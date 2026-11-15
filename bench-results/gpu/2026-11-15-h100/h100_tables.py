#!/usr/bin/env python3
"""The H100 NVL's ZK row in the 2026-11-13 write-up's terms.

Inputs, all in this directory unless noted:
  summary.json              scripts/sweep_summary.py over this directory
  economics.json            nockmark.xyz/economics at fetched_at.txt
  h100-nvl/zk-gpu.samples.txt   nvidia-smi every 5 s during the run
  ../../econ/2026-11-13-snapshot/economics.json   the write-up's snapshot
  ../2026-11-13-pipelined-sweep/                  the consumer cards' rows

NOCK/day = rate x 86,400 / difficulty x reward, as the board computes it.
"""
import json, os, re, statistics

H = os.path.dirname(os.path.abspath(__file__))
SW = os.path.join(H, '../2026-11-13-pipelined-sweep')
NOW = json.load(open(os.path.join(H, 'economics.json')))
WU = json.load(open(os.path.join(H, '../../econ/2026-11-13-snapshot/economics.json')))
ROW = {(r['host'], r['run']): r for r in json.load(open(os.path.join(H, 'summary.json')))}
OLD = {(r['host'], r['run']): r for r in json.load(open(os.path.join(SW, 'summary.json')))}

TARIFF = 0.1816  # $/kWh: EIA Electric Power Monthly, Table 5.6.A, Aug 2026 (the write-up's)
# Community $/h: this run's pod, and the 2026-11-13 sweep's.
CARDS = [('H100 NVL', H, 'h100-nvl', ROW, 2.59),
         ('RTX 5090', SW, 'rtx5090', OLD, 0.69),
         ('RTX 4090', SW, 'rtx4090', OLD, 0.34),
         ('RTX 3090', SW, 'rtx3090', OLD, 0.22)]


def power(base, host):
    """Mean board power over the ZK run's samples at >= 90 % utilisation."""
    pw = []
    for l in open(os.path.join(base, host, 'zk-gpu.samples.txt')):
        m = re.search(r'gpu\[([^\]]*)\]', l)
        try:
            u, _, p, *_ = [x.strip() for x in m.group(1).split(',')]
            if float(u) >= 90:
                pw.append(float(p))
        except Exception:
            pass
    return statistics.mean(pw), len(pw)


def day(hps, e):
    return hps * 86400 / e['difficulty'] * e['block_reward_nock']


print(f"write-up snapshot: ZK difficulty {WU['difficulty']:.4e}, reward {WU['block_reward_nock']}, NOCK ${WU['nock_usd']}")
print(f"today ({open(os.path.join(H, 'fetched_at.txt')).read().strip()}): ZK difficulty {NOW['difficulty']:.4e}, "
      f"reward {NOW['block_reward_nock']}, NOCK ${NOW['nock_usd']}, AI difficulty {NOW['ai_difficulty_mac_equiv_per_block']:.4e}")
print()
print('| card | calibrated MH/s | verified MH/s | luck | at luck 1 MH/s | NOCK/day, write-up snapshot (verified / luck 1) '
      '| NOCK/day, today (verified / luck 1) | $/h | MH/s per $/h (luck 1) | board W | kH/J (luck 1) |')
print('|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|')
out = {}
for label, base, host, rows, usd_h in CARDS:
    r = rows[(host, 'zk-gpu')]
    ver, cal = r['verified'], r['calibrated']
    l1 = r['recovery_luck1'] * cal
    w, n = power(base, host)
    o = {'calibrated': cal, 'verified': ver, 'luck': r['luck'], 'luck1': l1,
         'nock_day_writeup': [day(ver, WU), day(l1, WU)], 'nock_day_today': [day(ver, NOW), day(l1, NOW)],
         'usd_per_hr': usd_h, 'mhs_per_usd_hr_luck1': l1 / 1e6 / usd_h, 'board_w': w, 'power_samples': n,
         'kh_per_joule_luck1': l1 / 1e3 / w}
    out[label] = o
    print(f"| {label} | {cal/1e6:.2f} | {ver/1e6:.2f} | {r['luck']:.2f} | {l1/1e6:.2f} | "
          f"{o['nock_day_writeup'][0]:.1f} / {o['nock_day_writeup'][1]:.1f} | "
          f"{o['nock_day_today'][0]:.1f} / {o['nock_day_today'][1]:.1f} | {usd_h:.2f} | "
          f"{o['mhs_per_usd_hr_luck1']:.1f} | {w:.0f} | {o['kh_per_joule_luck1']:.1f} |")

h, f = out['H100 NVL'], out['RTX 5090']
print()
print(f"H100 NVL vs RTX 5090: calibrated {h['calibrated']/f['calibrated']:.2f}x, at luck 1 {h['luck1']/f['luck1']:.2f}x, "
      f"per rent dollar {h['mhs_per_usd_hr_luck1']/f['mhs_per_usd_hr_luck1']:.2f}x, per joule {h['kh_per_joule_luck1']/f['kh_per_joule_luck1']:.2f}x")

print()
print('Does it pay? (H100 NVL, ZK)')
for tag, e in (('write-up snapshot', WU), ('today', NOW)):
    v, l1 = day(h['verified'], e), day(h['luck1'], e)
    print(f"  {tag}: earns ${v * e['nock_usd']:.3f} / ${l1 * e['nock_usd']:.3f} a day (verified / luck 1) "
          f"against ${h['usd_per_hr'] * 24:.2f} rent ({l1 * e['nock_usd'] / (h['usd_per_hr'] * 24) * 100:.2f} % at luck 1)")
kwh = h['board_w'] * 24 / 1000
l1_usd_wu = day(h['luck1'], WU) * WU['nock_usd']
print(f"  owned, board power only: {kwh:.1f} kWh/day = ${kwh * TARIFF:.2f}/day at {TARIFF * 100:.2f} c/kWh; "
      f"breaks even below {l1_usd_wu / kwh * 100:.1f} c/kWh or above a NOCK price of "
      f"${kwh * TARIFF / day(h['luck1'], WU):.3f} (write-up snapshot, luck 1)")

# Upstream's only H100 AI figure is in unmerged PR #164 (Gemma inference
# mining): 159.0 and 258.97 TMAC/s "isolated production mining". Not measured
# here; priced only to bound what an H100 could earn on AI if that landed.
for tmacs in (159.0e12, 258.97e12):
    for tag, e in (('write-up snapshot', WU), ('today', NOW)):
        d = tmacs * 86400 / e['ai_difficulty_mac_equiv_per_block'] * e['block_reward_nock']
        print(f"  upstream #164 claim {tmacs/1e12:.0f} TMAC/s on AI -> {d:.2f} NOCK/day ({tag})")

json.dump(out, open(os.path.join(H, 'tables.json'), 'w'), indent=1)
