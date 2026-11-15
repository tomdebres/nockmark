#!/usr/bin/env python3
"""Every figure in docs/writeups/2026-11-13-which-puzzle-should-this-card-mine.md.

Inputs, all in this directory unless noted:
  economics.json            nockmark.xyz/economics (difficulties, reward, price)
  nockscan-*.json           nockscan.com/api/v1 (network rates, block shares)
  leaderboard-{zk,ai}.json  nockmark.xyz/leaderboard, the verified rows
  ../../gpu/2026-11-13-pipelined-sweep/   calibrated rates, luck-1 recovery, power samples
Fetched at the time in fetched_at.txt.
"""
import json, os, re, statistics
H = os.path.dirname(os.path.abspath(__file__))
SW = os.path.join(H, '../../gpu/2026-11-13-pipelined-sweep')
E = json.load(open(os.path.join(H, 'economics.json')))
NS = json.load(open(os.path.join(H, 'nockscan-mining-metrics.json')))
LB = {t: {r['id']: r for r in json.load(open(os.path.join(H, f'leaderboard-{t}.json'))) if r.get('rank') is not None}
      for t in ('zk', 'ai')}
SUM = {(s['host'], s['run']): s for s in json.load(open(os.path.join(SW, 'summary.json')))}

R, D_ZK, D_AI, USD = E['block_reward_nock'], E['difficulty'], E['ai_difficulty_mac_equiv_per_block'], E['nock_usd']
X = E['mac_equiv_per_zk_attempt']
def zk_day(hps): return hps * 86400 / D_ZK * R
def ai_day(macps): return macps * 86400 / D_AI * R

# EIA Electric Power Monthly, Table 5.6.A, August 2026 (released 2026-10-23):
# U.S. average residential price 18.16 cents/kWh.
TARIFF = 0.1816
RENT = {'rtx5090': 0.69, 'rtx4090': 0.34, 'rtx3090': 0.22}  # community $/h, from the sweep's pods

def power(host, run):
    """Mean GPU board power (nvidia-smi power.draw) over samples at >= 90% utilisation."""
    pw = []
    for l in open(os.path.join(SW, host, f'{run}.samples.txt')):
        m = re.search(r'gpu\[([^\]]*)\]', l)
        try:
            u, _, p, *_ = [x.strip() for x in m.group(1).split(',')]
            if float(u) >= 90: pw.append(float(p))
        except Exception:
            pass
    return statistics.mean(pw)

ROWS = [  # (label, host, zk run, ai run, board ids)
    ('RTX 5090', 'rtx5090', 'zk-gpu', 'peak-gpu', 7, 12),
    ('RTX 4090', 'rtx4090', 'zk-gpu', 'moe-gpu', 8, 14),
    ('RTX 3090', 'rtx3090', 'zk-gpu', 'moe-gpu', 6, 10),
    ('M1 Max (CPU)', 'm1-max', 'zk-cpu', 'moe-cpu', 5, 11),
]
out = {}
print(f"snapshot: {open(os.path.join(H, 'fetched_at.txt')).read().strip()}; econ observed {E['difficulty_observed_at']}")
print(f"ZK difficulty {D_ZK:.4e} Tip5 hashes/block (2^{NS['log2Difficulty']:.2f}); AI {D_AI:.4e} MAC-eq/block (2^{NS['aiLog2Difficulty']:.2f}); reward {R}; NOCK ${USD}")
print(f"network: ZK {E['est_network_zk_hashes_per_sec']:.3e} H/s, AI {E['est_ai_network_mac_per_sec']:.3e} MAC/s = {E['est_ai_network_mac_per_sec']/X:.3e} hash-eq/s")
zkw, aiw = E['est_network_zk_hashes_per_sec'], E['est_ai_network_mac_per_sec'] / X
print(f"work share: ZK {zkw/(zkw+aiw):.3f}, AI {aiw/(zkw+aiw):.3f}; per-block work ZK {D_ZK:.3e}, AI {D_AI/X:.3e} hash-eq ({D_AI/X/D_ZK:.2f}x)")
print("nockscan last-100 window:", {k: (v['blockShare'], v['workShare']) for k, v in NS['puzzles'].items()})
print()
print('| card | ZK verified | luck | at luck 1 | NOCK/day | AI statement | AI verified | luck | at luck 1 | NOCK/day | ZK ÷ AI (verified, luck 1) |')
for label, host, zr, ar, zid, aid in ROWS:
    z, a = SUM[(host, zr)], SUM[(host, ar)]
    zb, ab = LB['zk'][zid], LB['ai'][aid]
    zv, av = zb['verified_zk_hashes_per_sec_lb'], ab['verified_mac_per_sec_lb']
    z1 = z['recovery_luck1'] * z['calibrated']
    a1 = a['recovery_luck1'] * a['calibrated'] * a['F']
    r = dict(zk=zv, zk1=z1, zk_day=zk_day(zv), zk1_day=zk_day(z1), zk_luck=z['luck'],
             ai=av, ai1=a1, ai_day=ai_day(av), ai1_day=ai_day(a1), ai_luck=a['luck'], statement=a['statement'],
             board_zk_day=zb['est_nock_per_day'], board_ai_day=ab['est_nock_per_day'])
    out[label] = r
    print(f"| {label} | {zv:.4g} | {z['luck']:.2f} | {z1:.4g} | {r['zk_day']:.3g} | {a['statement']} | {av:.3g} | {a['luck']:.2f} | {a1:.3g} | {r['ai_day']:.3g} | {r['zk_day']/r['ai_day']:.3g} / {r['zk1_day']/r['ai1_day']:.3g} |")
# CPU-only dense-peak row (no ZK row on that host).
p = SUM[('rtx3090-cpu', 'peak-cpu')]
pv = LB['ai'][15]['verified_mac_per_sec_lb']; p1 = p['recovery_luck1'] * p['calibrated'] * p['F']
print(f"| TR PRO 5995WX CPU (17.9-CPU quota) | not measured | | | | dense-peak | {pv:.3g} | {p['luck']:.2f} | {p1:.3g} | {ai_day(pv):.3g} | |")
print()
print('| card | rent $/h | $/day rented | GPU W (measured) | kWh/day | $/day power @18.16c | NOCK/day ZK (verified, luck 1) | $/day ZK | break-even c/kWh (luck 1) | break-even NOCK price (owned, luck 1) |')
for label, host, zr, *_ in ROWS[:3]:
    r = out[label]; w = power(host, zr); kwh = w * 24 / 1000
    usd, usd1 = r['zk_day'] * USD, r['zk1_day'] * USD
    print(f"| {label} | {RENT[host]} | {RENT[host]*24:.2f} | {w:.0f} | {kwh:.2f} | {kwh*TARIFF:.2f} | {r['zk_day']:.3g} / {r['zk1_day']:.3g} | {usd:.3f} / {usd1:.3f} | {usd1/kwh*100:.2f} | {kwh*TARIFF/r['zk1_day']:.3f} |")
w = power('rtx5090', 'peak-gpu'); print(f"5090 dense-peak power {w:.0f} W; AI $/day {out['RTX 5090']['ai_day']*USD:.3f}")
print()
n5z = E['est_network_zk_hashes_per_sec'] / out['RTX 5090']['zk']
n5a = E['est_ai_network_mac_per_sec'] / out['RTX 5090']['ai']
print(f"network in 5090s (verified rates): ZK {n5z:,.0f}, AI {n5a:,.0f}; at upstream's 335 TMAC/s: {E['est_ai_network_mac_per_sec']/335e12:,.0f}")
print(f"5090 MAC per hash: {out['RTX 5090']['ai']/out['RTX 5090']['zk']:.3e} vs exchange {X:,.0f}")
json.dump(out, open(os.path.join(H, 'tables.json'), 'w'), indent=1)
