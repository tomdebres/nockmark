#!/usr/bin/env python3
"""Drive ONE long-lived RunPod GPU pod for `tock ai-bench --gpu` (M6 B2b) and
`tock zk-bench --gpu` (M7 step 3).

`runpod_harness` is the API layer — deploy, ssh, terminate, stray sweep — and
is not reimplemented here. What this adds is the thing a *development* session
needs and a one-shot sweep does not: a pod that survives between invocations,
so a compile error costs one `build` (seconds to re-dispatch) instead of a
redeploy (minutes, plus a fresh cargo registry download).

    ./gpu_pod.py up                  # sweep this slot's strays, deploy, bootstrap, keep
    ./gpu_pod.py bootstrap           # re-run the bootstrap on the live pod
    ./gpu_pod.py push                # overlay the LOCAL tock/ onto the clone
    ./gpu_pod.py build               # cargo build --release --features gpu
    ./gpu_pod.py build-zk            # cargo build --release --features zk-gpu
    ./gpu_pod.py miner-jam           # build assets/miner.jam with hoonc at the pin
    ./gpu_pod.py prep-zk             # build-zk and miner-jam at once (one wait, not two)
    ./gpu_pod.py cuda-check          # 10 s: can this host's CUDA runtime see the GPU?
    ./gpu_pod.py prep-all            # one build with gpu+zk-gpu, miner-jam beside it,
                                     # and scripts/sweep_run.sh uploaded to /build
    ./gpu_pod.py run '<shell>'       # anything, with the toolchain on PATH
    ./gpu_pod.py pods                # what is live on the account, if anything
    ./gpu_pod.py down                # terminate this slot's pod + assert it is gone

One state file is one SLOT: one pod, named after the file
(`NOCKMARK_POD_STATE=~/.cache/nockmark/zk-4090.json` → pod `nockmark-zk-4090`).
Two sessions (or two cards) use two state files. A slot only ever sweeps or
terminates its own pod, never another slot's, so a second agent's pod on the
same account is safe. `NOCKMARK_GPU_TYPE` picks the card at `up`; the CUDA
arch flags then follow the card recorded in the slot (3090 sm_86, 4090 sm_89,
5090 sm_120), unless set explicitly. Deploys are community cloud only:
`NOCKMARK_POD_WAIT_S` (default 1800) bounds the wait for stock, and secure
cloud is never chosen silently — it costs 2–3x.

The safety trade this makes, stated plainly: `runpod_harness.deploy` registers
an atexit teardown the instant a pod exists, and `up` deliberately unregisters
it so the pod outlives the process. That is the ONLY place teardown is
disarmed, and three things fence it in — the unregister happens last, after
bootstrap, so any failure before it still tears the pod down; every `up`
pre-flight-sweeps `nockmark-` pods, so a leaked pod dies at the next
invocation; and `down` re-checks `list_pods()` and shouts if anything survived.

`push` exists because the branch under development is deliberately unpushed.
The pod clones nockmark at `main` for its layout and history, then this
overwrites `tock/` from the working tree, so what runs on the GPU is exactly
what is on this laptop's disk — no "did I remember to push?" failure mode.
"""

import base64
import io
import json
import os
import sys
import tarfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runpod_harness as rp  # noqa: E402

# The 3090 is the development GPU: $0.22/hr, Ampere. 4090 and 5090 cost 3-6x
# more per hour and tell you nothing a 3090 has not about whether the code
# COMPILES — they are for measuring.
GPU_TYPE = os.environ.get("NOCKMARK_GPU_TYPE", "NVIDIA GeForce RTX 3090")

# CUDA arch per card, for both kernels: (AI_POW_CUDA_ARCH, AI_POW_CUDA_CODE,
# ZK_POW_CUDA_ARCH). The ZK V5 build script takes bare numbers. A fixed sm_86
# default built SASS a 5090 (sm_120) cannot load, so the flags follow the card
# the slot was deployed on; an explicit env var still wins.
CUDA_ARCHES = {
    "NVIDIA GeForce RTX 3090": ("compute_86", "sm_86", "86"),
    "NVIDIA GeForce RTX 4090": ("compute_89", "sm_89", "89"),
    "NVIDIA GeForce RTX 5090": ("compute_120", "sm_120", "120"),
}

# The image already carries nvcc 12.8 and the driver; only the Rust side and
# the protobuf compiler (nockapp-grpc's build script) are missing.
IMAGE = "nvidia/cuda:12.8.1-devel-ubuntu24.04"

NOCKMARK_REPO = "https://github.com/tomdebres/nockmark.git"
TOOLCHAIN_DATE = "2026-04-03"
TRIPLE = "x86_64-unknown-linux-gnu"

# No nockchain checkout on the pod: tock's nockchain crates are git deps
# pinned by `rev` in tock/Cargo.toml, and cargo fetches them (CUDA sources
# included — ai-pow-miner's build.rs compiles them) into ~/.cargo/git. The pin
# therefore travels with `push`: whatever rev the local Cargo.toml/Cargo.lock
# say is what the pod builds.
ROOT = "/build"
NOCKMARK = f"{ROOT}/nockmark"
TOCK = f"{NOCKMARK}/tock"

STATE = os.path.expanduser(
    os.environ.get("NOCKMARK_POD_STATE", "~/.cache/nockmark/gpu-pod.json")
)
# The slot's pod name: the only pods this slot will ever sweep.
POD_NAME = rp.NAME_PREFIX + os.path.splitext(os.path.basename(STATE))[0]


def cuda_env(gpu):
    """The arch exports for `gpu`: the card's own, unless set explicitly."""
    ai_arch, ai_code, zk_arch = CUDA_ARCHES.get(gpu, CUDA_ARCHES["NVIDIA GeForce RTX 3090"])
    ai_arch = os.environ.get("AI_POW_CUDA_ARCH", ai_arch)
    ai_code = os.environ.get("AI_POW_CUDA_CODE", ai_code)
    zk_arch = os.environ.get("ZK_POW_CUDA_ARCH", zk_arch)
    return (f"export AI_POW_CUDA_ARCH={ai_arch} AI_POW_CUDA_CODE={ai_code}; "
            f"export ZK_POW_CUDA_ARCH={zk_arch}; ")


# Everything a remote command needs: the tarball-installed toolchain, nvcc,
# the arch flags upstream's build.rs reads (from `cuda_env`), and the 8 MB
# stack the ai-pow provers want (RUST_MIN_STACK, same as every other Nockmark
# build).
ENV = (
    "export PATH=/opt/rust/bin:/usr/local/cuda/bin:$PATH; "
    "export CUDA_HOME=/usr/local/cuda; "
    "export RUST_MIN_STACK=8388608; "
    "export CARGO_TERM_COLOR=never; "
)


def load_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except FileNotFoundError:
        sys.exit("no pod: run `gpu_pod.py up` first")


def save_state(state):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    with open(STATE, "w") as f:
        json.dump(state, f)


# Marker the remote script prints as its very last act. It must not appear in
# the script's own source, because a PTY echoes everything typed into it — see
# `remote` below for the full trap.
DONE = "NM_" + "DONE"


def _upload_lines(data: bytes, path: str):
    """Shell lines that reconstruct `data` at `path`, safe for a PTY.

    A PTY in canonical mode discards input past its ~4 KB line buffer, so one
    giant `echo` silently truncates; base64 in ≤1000-byte `printf`s does not.
    scp is not an alternative — OpenSSH's sftp mode against RunPod's proxy is
    the trap the July sweep already paid for.
    """
    blob = base64.b64encode(data).decode()
    chunks = [blob[i:i + 1000] for i in range(0, len(blob), 1000)]
    lines = [f"rm -f {path}.b64"]
    lines += [f"printf %s '{c}' >> {path}.b64" for c in chunks]
    lines.append(f"base64 -d {path}.b64 > {path}")
    return lines


def remote(command, timeout_s=3600):
    """Run `command` on the saved pod, with the build environment loaded.

    The command is **uploaded and run as a file**, never typed at the remote
    shell, and that is not fastidiousness — it is the fix for a failure that
    reported success. RunPod's proxy only offers an INTERACTIVE shell over a
    PTY, and two things go wrong when a multi-line script is fed to one:

      * `set -e` in an interactive shell kills the session at the first
        non-zero status, so the remaining lines never run and their output
        never appears;
      * the PTY echoes the input before the shell consumes it, so
        `runpod_harness.ssh` finds its own `__NM_RC__$?__` sentinel in the
        ECHO, parses `$?` as "not a digit", and returns **rc 0**. A bootstrap
        that executed nothing at all therefore looked like a clean success,
        and only failed three steps later with "rustc: command not found".

    So: upload, run under a fresh non-interactive `bash` (where `set -e`
    behaves), and trust an explicit end marker with the real exit status
    rather than the transport's rc.

    `</dev/null` is the third non-negotiable. The remote shell is being driven
    THROUGH its stdin, so any command that reads stdin eats the lines queued
    behind it — `apt-get` swallowed the end marker and the harness's own
    `exit`, and the session then sat idle at a prompt until the ssh timeout,
    with the work long since finished. Detaching the payload's stdin is what
    makes "the command ended" and "the session ended" the same event.
    """
    state = load_state()
    script = "set -euo pipefail\n" + ENV + cuda_env(state["gpu"]) + "\n" + command + "\n"
    lines = _upload_lines(script.encode(), "/tmp/nm-cmd.sh")
    lines.append(f'bash /tmp/nm-cmd.sh </dev/null 2>&1; echo "{DONE}_$?_"')
    rc, out = rp.ssh(state["host_id"], "\n".join(lines), timeout_s=timeout_s)
    print(out, flush=True)
    # The marker appears twice (echoed input, then real output); the last one
    # is the executed copy, and only it carries a substituted status.
    marker = out.rfind(f"{DONE}_")
    status = out[marker + len(DONE) + 1:].split("_", 1)[0] if marker >= 0 else ""
    if not status.isdigit():
        print(f"[gpu_pod] no end marker (ssh rc={rc}) — treating as failure", flush=True)
        return rc or 1
    return int(status)


BOOTSTRAP = f"""
export DEBIAN_FRONTEND=noninteractive
# DPkg::Lock::Timeout, not a bare apt-get: RunPod's SSH proxy answers via
# its OWN agent, so a shell is reachable while the container's startup
# command is still running its apt-get to install sshd. Racing that lock is
# how the first sweep attempt died on every card —
# "E: Could not get lock /var/lib/dpkg/lock-frontend ... held by process
# 549 (apt-get)" — so wait for it instead. Ten minutes is far longer than
# the ~40 s the startup install takes, and costs nothing when it is free.
# DPkg::Lock::Timeout covers dpkg's lock only. `apt-get update` also takes
# /var/lib/apt/lists/lock, which the startup install holds too — and fails at
# once on it ("Could not get lock /var/lib/apt/lists/lock", seen 2026-11-12).
# So wait for any apt/dpkg to finish first, and retry the update regardless.
# (/proc, not pgrep: the CUDA image may not ship procps.)
for i in $(seq 1 120); do grep -qxE 'apt-get|apt|dpkg' /proc/[0-9]*/comm 2>/dev/null || break; sleep 5; done
for i in 1 2 3 4 5; do apt-get -o DPkg::Lock::Timeout=600 update -qq && break; sleep 10; done
apt-get -o DPkg::Lock::Timeout=600 install -y -qq --no-install-recommends \
    git protobuf-compiler build-essential curl xz-utils pkg-config libssl-dev ca-certificates
# Pinned nightly from static.rust-lang.org tarballs. The CUDA image has no
# rustup and installing one would be a second, unpinned source of truth; this
# is the same three-component install the registry Dockerfile does.
mkdir -p /opt/rust /tmp/rust-dl
cd /tmp/rust-dl
for c in rustc cargo rust-std; do
  curl -sSfLO "https://static.rust-lang.org/dist/{TOOLCHAIN_DATE}/${{c}}-nightly-{TRIPLE}.tar.xz"
  tar xf "${{c}}-nightly-{TRIPLE}.tar.xz"
  "./${{c}}-nightly-{TRIPLE}/install.sh" --prefix=/opt/rust --disable-ldconfig >/dev/null
done
cd / && rm -rf /tmp/rust-dl
mkdir -p {ROOT} && cd {ROOT}
[ -d {NOCKMARK} ] || git clone --quiet --filter=blob:none {NOCKMARK_REPO} {NOCKMARK}
git -C {NOCKMARK} checkout --quiet main
/opt/rust/bin/rustc --version
nvcc --version | tail -1
nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader
"""


def sweep_slot():
    """Terminate leftovers of THIS slot only (pods named `POD_NAME`).

    `runpod_harness.sweep_strays` kills every `nockmark-` pod on the account,
    which is right for a one-shot sweep and wrong when another session is
    measuring on its own pod at the same time.
    """
    mine = [p for p in rp.list_pods() if p["name"] == POD_NAME]
    for p in mine:
        print(f"[gpu_pod] sweeping this slot's stray pod {p['id']} (${p['costPerHr']}/hr)", flush=True)
        rp.terminate(p["id"])
    return len(mine)


def deploy_community():
    """One community-cloud pod, retrying while stock is out.

    `attempts=1` keeps `runpod_harness.deploy` off its secure-cloud fallback
    (which fires on the last of several attempts): secure costs 2-3x, and
    paying it is a decision, not a retry policy.
    """
    wait_s = float(os.environ.get("NOCKMARK_POD_WAIT_S", "1800"))
    deadline = time.time() + wait_s
    while True:
        try:
            return rp.deploy(GPU_TYPE, POD_NAME, image=IMAGE, container_disk=60, attempts=1)
        except rp.RunPodError as e:
            if time.time() + 60 > deadline:
                sys.exit(f"[gpu_pod] no community {GPU_TYPE} within {wait_s:.0f}s; last: {e}")
            print(f"[gpu_pod] no community stock yet ({e}); retrying in 60 s", flush=True)
            time.sleep(60)


def up():
    if os.path.exists(STATE):
        sys.exit(f"[gpu_pod] {STATE} already holds a pod; `down` it first (or use another "
                 f"NOCKMARK_POD_STATE)")
    swept = sweep_slot()
    if swept:
        print(f"[gpu_pod] swept {swept} stray pod(s) before deploying", flush=True)
    pod = deploy_community()
    host = rp.host_id(pod["id"])
    # `remote` reads the slot, so it is written before the bootstrap; any
    # failure from here on clears it, because the atexit hook is about to
    # terminate the pod it names.
    save_state({"pod_id": pod["id"], "host_id": host, "gpu": GPU_TYPE,
                "cost_per_hr": pod["costPerHr"], "deployed_at": time.time()})
    try:
        rp.wait_for_ssh_ready(host)
        print(f"[gpu_pod] bootstrapping {pod['id']} ({host})", flush=True)
        if remote(BOOTSTRAP, timeout_s=1800) != 0:
            sys.exit("[gpu_pod] bootstrap failed — pod will be torn down")
    except BaseException:
        os.remove(STATE)
        raise
    # Last statement in the happy path: everything above still tears the pod
    # down on failure, because the atexit hook is armed until this line.
    rp.atexit.unregister(rp.terminate)
    print(f"[gpu_pod] pod {pod['id']} is UP and will persist; `gpu_pod.py down` to stop billing",
          flush=True)


def push():
    """Overlay the local `tock/` onto the pod's clone."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        # `stub`: the empty kernels-open-miner that tock/Cargo.toml patches in.
        # `examples`: probes such as zk_v5_probe.
        for member in ("Cargo.toml", "Cargo.lock", "src", "stub", "examples"):
            path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tock", member)
            tar.add(os.path.abspath(path), arcname=member)
    payload = buf.getvalue()
    print(f"[gpu_pod] pushing {len(payload)} bytes of tock/ to {TOCK}", flush=True)
    sys.exit(remote(
        "\n".join(_upload_lines(payload, "/tmp/tock.tgz")
                  + [f"tar xzf /tmp/tock.tgz -C {TOCK}", "echo pushed"]),
        timeout_s=900,
    ))


def pinned_rev():
    """The nockchain rev every git dep in tock/Cargo.toml is pinned to."""
    import re
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tock", "Cargo.toml")
    revs = set(re.findall(r'rev = "([0-9a-f]{40})"', open(path).read()))
    if len(revs) != 1:
        sys.exit(f"[gpu_pod] expected one pinned nockchain rev in tock/Cargo.toml, found {revs}")
    return revs.pop()


def miner_jam_script(rev):
    """Build miner.jam on the pod with hoonc at `rev` (tock/README.md recipe).

    18 MB does not travel well through RunPod's PTY proxy (see `_upload_lines`),
    and the jam is a pure function of the pinned hoon tree, so the pod builds
    its own and prints its sha256 to compare with the local one. hoonc parses
    every file under the tree it is given, and upstream ships deliberately
    unparseable fixtures in hoon/tests/compiler-errors/, which stop it with no
    jam and exit 0 — so it compiles a copy without them, and the jam's
    existence is checked.
    """
    nc = f"{ROOT}/nockchain"
    return f"""
[ -d {nc} ] || git clone --quiet --filter=blob:none https://github.com/nockchain/nockchain.git {nc}
git -C {nc} fetch --quiet origin {rev} || true
git -C {nc} checkout --quiet {rev}
cd {nc} && cargo build --release -p hoonc 2>&1 | tail -3
rm -rf {ROOT}/hoon-build && mkdir -p {ROOT}/hoon-build
cp -RL {nc}/hoon {ROOT}/hoon-build/hoon
rm -rf {ROOT}/hoon-build/hoon/tests/compiler-errors
cd {ROOT}/hoon-build && rm -f miner.jam
{nc}/target/release/hoonc --new --data-dir {ROOT}/hoonc-data-$$ --output miner.jam \
    hoon/apps/dumbnet/miner.hoon hoon 2>&1 | tail -5
[ -s miner.jam ] || {{ echo "hoonc wrote no miner.jam"; exit 1; }}
mkdir -p {TOCK}/assets && mv miner.jam {TOCK}/assets/miner.jam
sha256sum {TOCK}/assets/miner.jam
"""


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd = sys.argv[1]
    if cmd == "up":
        up()
    elif cmd == "bootstrap":
        # Idempotent, and split out from `up` so a bootstrap that fails
        # halfway can be retried without paying for a fresh pod.
        sys.exit(remote(BOOTSTRAP, timeout_s=1800))
    elif cmd == "push":
        push()
    elif cmd == "build":
        sys.exit(remote(f"cd {TOCK} && cargo build --release --features gpu 2>&1 | tail -60"))
    elif cmd == "build-zk":
        sys.exit(remote(f"cd {TOCK} && cargo build --release --features zk-gpu 2>&1 | tail -60"))
    elif cmd == "miner-jam":
        sys.exit(remote(miner_jam_script(pinned_rev()), timeout_s=3600))
    elif cmd == "prep-zk":
        # The tock build and the hoonc build + jam compile are independent:
        # run them side by side, then show each one's tail and status.
        sys.exit(remote(
            f"(cd {TOCK} && cargo build --release --features zk-gpu) > {ROOT}/build-zk.log 2>&1 & b=$!\n"
            f"( {miner_jam_script(pinned_rev())} ) > {ROOT}/miner-jam.log 2>&1 & j=$!\n"
            "rb=0; wait $b || rb=$?; rj=0; wait $j || rj=$?\n"
            f"tail -5 {ROOT}/build-zk.log; tail -4 {ROOT}/miner-jam.log\n"
            'echo "build-zk rc=$rb, miner-jam rc=$rj"; [ $rb = 0 ] && [ $rj = 0 ]',
            timeout_s=3600))
    elif cmd == "cuda-check":
        # nvidia-smi working proves nothing: on community hosts with driver
        # 580.173.02 or 620.70.06 (2026-11-13) cuInit itself fails with error
        # 999 in this image. Run this before a 10-20 min build; on failure,
        # `down` and `up` again.
        sys.exit(remote(
            "cd /tmp && printf '%s\\n' '#include <cstdio>' '#include <cuda_runtime.h>' "
            "'int main(){int n=0; cudaError_t e=cudaGetDeviceCount(&n); "
            "printf(\"CUDA_CHECK err=%d n=%d\\n\", (int)e, n); return e != cudaSuccess || n < 1;}' "
            "> cuda_check.cu && nvcc -o cuda_check cuda_check.cu 2>/dev/null && ./cuda_check",
            timeout_s=600))
    elif cmd == "prep-all":
        # Both CUDA backends in one binary (ai-bench --gpu and zk-bench --gpu),
        # the jam beside the build, and the instrumented run script the sweep
        # launches detached (`setsid nohup bash /build/sweep_run.sh <name> ...`).
        here = os.path.dirname(os.path.abspath(__file__))
        runner = open(os.path.join(here, "sweep_run.sh"), "rb").read()
        sys.exit(remote(
            "\n".join(_upload_lines(runner, f"{ROOT}/sweep_run.sh")) + "\n"
            f"(cd {TOCK} && cargo build --release --features gpu,zk-gpu) > {ROOT}/build-all.log 2>&1 & b=$!\n"
            f"( {miner_jam_script(pinned_rev())} ) > {ROOT}/miner-jam.log 2>&1 & j=$!\n"
            "rb=0; wait $b || rb=$?; rj=0; wait $j || rj=$?\n"
            f"tail -5 {ROOT}/build-all.log; tail -4 {ROOT}/miner-jam.log\n"
            'echo "build-all rc=$rb, miner-jam rc=$rj"; [ $rb = 0 ] && [ $rj = 0 ]',
            timeout_s=3600))
    elif cmd == "run":
        sys.exit(remote(" ".join(sys.argv[2:]), timeout_s=5400))
    elif cmd == "pods":
        pods = rp.list_pods()
        print(json.dumps(pods, indent=2) if pods else "[gpu_pod] no live pods")
    elif cmd == "down":
        state = load_state()
        rp.terminate(state["pod_id"])
        time.sleep(5)
        left = rp.list_pods()
        if any(p["id"] == state["pod_id"] for p in left):
            sys.exit(f"[gpu_pod] pod {state['pod_id']} is STILL LISTED — billing; check RunPod")
        os.remove(STATE)
        if "deployed_at" in state:
            hours = (time.time() - state["deployed_at"]) / 3600
            print(f"[gpu_pod] {state['pod_id']} gone after {hours * 60:.1f} min at "
                  f"${state['cost_per_hr']}/hr (≈ ${hours * state['cost_per_hr']:.3f})")
        others = [p for p in left if p["name"] != POD_NAME]
        print("[gpu_pod] no live pods — clean" if not left else
              f"[gpu_pod] this slot is clean; other pods on the account (not touched): "
              f"{json.dumps(others)}")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
