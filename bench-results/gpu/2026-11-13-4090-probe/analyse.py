#!/usr/bin/env python3
"""Tables for the 4090 probe from the pod's out/ files (see README.md).

Exact integer arithmetic throughout: a digest is the base-p integer of its
five limbs, and a nonce wins at tier 2^a iff digest <= T_a, with
T_a = floor((p^5 - 1) / 2^a) - 1 (tock::zkv5::target_for_attempts).
"""
import json, math, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.environ.get('PROBE_OUT', os.path.join(HERE, 'out'))  # PROBE_OUT: red-checks on a copy
P = 2**64 - 2**32 + 1
MAX = P**5 - 1
def target(a): return MAX // 2**a - 1
def base_p(v): return [(v // P**i) % P for i in range(5)]
def as_int(limbs): return sum(l * P**i for i, l in enumerate(limbs))
# The board's zk 8 target is tier 2^29: the formula must reproduce it.
assert base_p(target(29)) == [34359738359] + [34359738360] * 4

run = json.load(open(os.path.join(HERE, '../2026-11-13-pipelined-sweep/rtx4090/zk-gpu.json')))
zk8 = [w['nonce'] for w in run['wins']]
N = zk8[-1] + 1

def rows(name):
    path = os.path.join(OUT, name)
    out, meta = [], []
    for line in open(path):
        line = line.strip()
        if not line: continue
        (meta if line.startswith('#') else out).append(line if line.startswith('#') else list(map(int, line.split())))
    return out, meta

def poisson_z(obs, exp): return (obs - exp) / math.sqrt(exp)

S = {}
# 1. Replay of zk 8's range at tier 2^20.
rep, meta = rows('replay-t20.txt')
S['replay_meta'] = meta
wins = [(r[1], as_int(r[3:8])) for r in rep if r[2] == 1]
false = [r[1] for r in rep if r[2] != 1]
assert all(n < N for n, _ in wins)
S['replay'] = {'nonces': N, 'tier_log2': 20, 'wins': len(wins), 'expected': N / 2**20,
               'z': poisson_z(len(wins), N / 2**20), 'false_positives': len(false)}
# Per 2^30-nonce block.
blocks = []
for b in range(math.ceil(N / 2**30)):
    lo, hi = b * 2**30, min((b + 1) * 2**30, N)
    obs = sum(1 for n, _ in wins if lo <= n < hi)
    blocks.append({'block': b, 'from': lo, 'to': hi, 'wins': obs, 'expected': (hi - lo) / 2**20})
chi2 = sum((x['wins'] - x['expected'])**2 / x['expected'] for x in blocks)
S['replay_blocks'] = blocks
S['replay_blocks_chi2'] = {'chi2': chi2, 'dof': len(blocks) - 1}
# Depth: wins at each finer tier within the same range.
depth = []
for a in range(20, 33):
    obs = sum(1 for _, d in wins if d <= target(a))
    depth.append({'tier_log2': a, 'wins': obs, 'expected': N / 2**a, 'z': poisson_z(obs, N / 2**a)})
S['depth'] = depth
w29 = sorted(n for n, d in wins if d <= target(29))
S['tier29'] = {'probe': w29, 'zk8': zk8, 'identical': w29 == zk8,
               'missed_by_zk8': sorted(set(w29) - set(zk8)),
               'zk8_not_found_by_probe': sorted(set(zk8) - set(w29))}

# 2. Device vs CPU oracle on the sampled ranges, tier 2^14.
cpu, cmeta = rows('sample-cpu-t14.txt')
dev, dmeta = rows('sample-dev-t14.txt')
cset = {(r[0], r[1]) for r in cpu}
dset = {(r[0], r[1]) for r in dev if r[2] == 1}
dfalse = [r for r in dev if r[2] != 1]
ranges = [list(map(int, l.split())) for l in open(os.path.join(HERE, 'inputs/sample.ranges')) if l.strip() and not l.startswith('#')]
total = sum(l for _, l in ranges)
m1, m1meta = rows('sample-cpu-t14.m1max.txt')
m1set = {(r[0], r[1]) for r in m1}
S['crosscheck_m1'] = {'m1_wins': len(m1set), 'identical_to_pod_cpu': m1set == cset, 'meta': m1meta,
                      'm1_only': sorted(m1set - cset), 'pod_only': sorted(cset - m1set)}
S['crosscheck'] = {'ranges': len(ranges), 'nonces': total, 'tier_log2': 14, 'cpu_wins': len(cset),
                   'device_wins': len(dset), 'expected': total / 2**14,
                   'device_only': sorted(dset - cset), 'cpu_only': sorted(cset - dset),
                   'device_false_positives': len(dfalse), 'cpu_meta': cmeta, 'device_meta': dmeta,
                   'identical': cset == dset and not dfalse}
per = []
for i, (s, l) in enumerate(ranges):
    per.append({'range': i, 'start': s, 'cpu': sum(1 for r in cset if r[0] == i), 'device': sum(1 for r in dset if r[0] == i)})
S['crosscheck_ranges'] = per

# 3. zk 8's exact launches at tier 2^29.
lines = [l for l in open(os.path.join(HERE, 'inputs/zk8.launches')) if l.strip() and not l.startswith('#')]
expect = [int(l.split('#')[1].split()[1]) for l in lines]
got, _ = rows('zk8-launches-t29.txt')
S['zk8_launches'] = {'launches': len(got), 'returned_expected_winner': sum(1 for g, e in zip(got, expect) if g[2] == 1 and g[3] == e and not any(g[4:8]))}
if os.path.exists(os.path.join(OUT, 'extra-launches-t29.txt')):
    S['extra_launches'] = rows('extra-launches-t29.txt')[0]

# 4. Digest agreement.
dig, dmeta2 = rows('digests.txt')
S['digests'] = {'nonces': len(dig), 'mismatches': sum(1 for r in dig if r[1] != 1), 'max_nonce': max(r[0] for r in dig)}

json.dump(S, open(os.path.join(HERE if 'PROBE_OUT' not in os.environ else OUT, 'summary.json'), 'w'), indent=1)
r = S['replay']
print(f"replay [0, {N}) at 2^20: {r['wins']} wins vs {r['expected']:.1f} expected (z {r['z']:+.2f}), {r['false_positives']} false positives")
print(f"blocks chi2 {chi2:.1f} on {len(blocks)-1} dof")
for d in depth: print(f"  2^{d['tier_log2']}: {d['wins']} vs {d['expected']:.1f} (z {d['z']:+.2f})")
t = S['tier29']; print(f"2^29 winners: probe {len(t['probe'])}, zk 8 {len(t['zk8'])}, identical {t['identical']}, missed by zk8 {t['missed_by_zk8']}, zk8 not found {t['zk8_not_found_by_probe']}")
c = S['crosscheck']; print(f"crosscheck: cpu {c['cpu_wins']} device {c['device_wins']} expected {c['expected']:.0f}; device-only {len(c['device_only'])}, cpu-only {len(c['cpu_only'])}, false {c['device_false_positives']}, identical {c['identical']}")
print(f"M1 Max oracle: {S['crosscheck_m1']['m1_wins']} wins, identical to the pod CPU: {S['crosscheck_m1']['identical_to_pod_cpu']}")
print(f"zk8 launches: {S['zk8_launches']}")
print(f"digests: {S['digests']}")
