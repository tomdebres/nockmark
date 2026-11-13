#!/usr/bin/env python3
"""Summarise a pipelined sweep (M7 step 5, part 2b).

    sweep_summary.py <dir> [<registry base URL>]

<dir> holds one sub-directory per host (`rtx5090/`, `m1-max/`, …), each with
the `<name>.json` reports `tock --report` wrote and, for pod runs, the
`<name>.live.txt` headers `scripts/sweep_run.sh` wrote (pod $/h is read from
`pod.json` beside them when present). For every submitted run it fetches the
board's own row and prints, and writes to `<dir>/summary.json`:

  calibrated, sustained and verified rates; recovery = verified / calibrated,
  and the same at luck 1; wins vs expected (luck); host CPU; $/h; wall time.

Recovery at luck 1 replaces the grind with the expected work over the
sustained rate and keeps the run's measured tail (window − grind). That tail
itself depends on luck (a lucky run bunches wins and queues proofs), so the
figure is an estimate, stated as one.
"""
import json
import math
import os
import re
import sys
import urllib.request

UA = {"User-Agent": "Mozilla/5.0 nockmark-sweep-summary"}


def fetch(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def header(path):
    """Host facts from a sweep_run.sh live.txt header."""
    h = {}
    if not os.path.exists(path):
        return h
    for line in open(path, errors="replace"):
        if line.startswith("====TLOG"):
            break
        if line.startswith("model name"):
            h["cpu"] = line.split(":", 1)[1].strip()
        elif line.startswith("nproc"):
            h["nproc"] = int(line.split()[1])
        elif line.startswith("cgroup_cpu_max"):
            parts = line.split()[1:]
            if len(parts) == 2 and parts[0].isdigit():
                h["cpu_quota"] = round(int(parts[0]) / int(parts[1]), 1)
        elif line.startswith("NVIDIA"):
            h["gpu"] = line.strip()
        elif line.startswith("start "):
            h["start"] = float(line.split()[1])
        elif line.startswith("exit "):
            h["exit_rc"], h["end"] = int(line.split()[1]), float(line.split()[2])
    if "start" not in h:
        # A local run's log: every line is "<unix ts> <tock output>".
        ts = [float(l.split()[0]) for l in open(path, errors="replace")
              if re.match(r"^\d+\.\d+ ", l)]
        if ts:
            h["start"], h["end"] = ts[0], ts[-1]
    return h


def main():
    d = sys.argv[1]
    base = (sys.argv[2] if len(sys.argv) > 2 else "https://nockmark.xyz").rstrip("/")
    rows = []
    for host in sorted(os.listdir(d)):
        hd = os.path.join(d, host)
        if not os.path.isdir(hd):
            continue
        pod = {}
        if os.path.exists(os.path.join(hd, "pod.json")):
            pod = json.load(open(os.path.join(hd, "pod.json")))
        for f in sorted(os.listdir(hd)):
            if not f.endswith(".json") or f == "pod.json":
                continue
            name = f[:-5]
            r = json.load(open(os.path.join(hd, f)))
            h = header(os.path.join(hd, name + ".live.txt"))
            ai = r["track"] == "ai"
            cal = r.get("calibrated_attempts_per_sec" if ai else "calibrated_hashes_per_sec")
            sus = r.get("sustained_attempts_per_sec" if ai else "sustained_hashes_per_sec")
            attempts = r.get("attempts" if ai else "hashes")
            expected = r.get("expected_attempts" if ai else "expected_hashes")
            row = {
                "host": host, "run": name, "track": r["track"], "statement": r.get("statement"),
                "proving": r["proving"], "provers": r["provers"], "k": r["k"],
                "tier_log2": round(math.log2(r["tier_attempts"]), 2) if ai else r["tier_log2"],
                "calibrated": cal, "calibrated_prove_s": r.get("calibrated_prove_s"),
                "sustained": sus, "luck": (expected / attempts) if attempts else None,
                "grind_s": r["grind_s"], "client_wall_s": r["wall_s"],
                "prove_mean_s": sum(w["prove_s"] for w in r["wins"]) / len(r["wins"]),
                "failures": r.get("failures", 0), "run_id": r.get("run_id"),
                "hardware": r.get("hardware"), **{k: h[k] for k in ("cpu", "nproc", "cpu_quota", "gpu") if k in h},
            }
            if "start" in h and "end" in h:
                row["wall_min"] = round((h["end"] - h["start"]) / 60, 1)
                if pod.get("usd_per_hr"):
                    row["usd_per_hr"] = pod["usd_per_hr"]
                    row["usd"] = round(pod["usd_per_hr"] * (h["end"] - h["start"]) / 3600, 3)
            if r.get("run_id"):
                b = fetch(f"{base}/runs/{r['run_id']}?track={r['track']}")
                window = b["server_window_ms"] / 1000
                ver = b["verified_mac_per_sec_lb"] if ai else b["verified_zk_hashes_per_sec_lb"]
                # MAC-equivalents per attempt (consensus's F for the statement);
                # a ZK attempt is one Tip5 hash.
                F = {"canonical-moe": 65536.0, "dense-peak": 2097152.0, "dense": 65536.0}.get(
                    r.get("statement"), 1.0) if ai else 1.0
                ver_att = ver / F
                tail = window - r["grind_s"]
                e_grind = expected / sus if sus else None
                row.update({
                    "server_window_s": window, "verified": ver, "verified_attempts_per_s": ver_att,
                    "rank": b.get("rank"), "board_proving": b.get("proving"),
                    "est_nock_per_day": b.get("est_nock_per_day"),
                    "tail_s": round(tail, 1),
                    "recovery": ver_att / cal if cal else None,
                    "recovery_luck1": (expected / (e_grind + tail)) / cal if (cal and e_grind) else None,
                    "F": F,
                })
            rows.append(row)
    json.dump(rows, open(os.path.join(d, "summary.json"), "w"), indent=1)
    fmt = lambda v, p=3: "—" if v is None else (f"{v:.{p}g}" if isinstance(v, float) else str(v))
    print("| host | run | proving | tier | calibrated | sustained | verified (att/s) | recovery | at luck 1 | luck | tail s | prove s | rank |")
    print("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for r in rows:
        print(f"| {r['host']} | {r['run']} | {r['proving']}/{r['provers']} | 2^{fmt(r['tier_log2'])} | "
              f"{fmt(r['calibrated'])} | {fmt(r['sustained'])} | {fmt(r.get('verified_attempts_per_s'))} | "
              f"{fmt(r.get('recovery'))} | {fmt(r.get('recovery_luck1'))} | {fmt(r['luck'])} | "
              f"{fmt(r.get('tail_s'))} | {fmt(r['prove_mean_s'])} | {fmt(r.get('rank'))} |")


if __name__ == "__main__":
    main()
