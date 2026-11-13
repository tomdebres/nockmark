#!/usr/bin/env python3
"""Inputs for the 4090 probe (tock/examples/zk_v5_probe.rs), from run zk 8.

replay.ranges     the whole range run zk 8 searched, [0, last winner]
sample.ranges     32 ranges of 2^22 nonces for the device-vs-CPU cross-check:
                  the start, 2^32/2^33/2^34 straddles, zk 8's three longest
                  gaps, and evenly spread points up to 2^34
zk8.launches      zk 8's exact launches that found its 16 winners (tier 2^29)
digests.nonces    nonces for the device-vs-CPU digest comparison
"""
import json, os
HERE = os.path.dirname(os.path.abspath(__file__))
P = 2**64 - 2**32 + 1
BATCH = 262_144
L = 2**22
run = json.load(open(os.path.join(HERE, '../2026-11-13-pipelined-sweep/rtx4090/zk-gpu.json')))
wins = [w['nonce'] for w in run['wins']]
assert run['run_id'] == 8 and len(wins) == 16 and wins == sorted(wins)
assert run['hashes'] == wins[-1] + 1, "zk 8 searched [0, last winner]"

def out(name, lines, head):
    with open(os.path.join(HERE, 'inputs', name), 'w') as f:
        f.write(''.join(f'# {h}\n' for h in head))
        f.write(''.join(f'{l}\n' for l in lines))

out('replay.ranges', [f'0 {wins[-1] + 1}'], ['run zk 8: challenge 2664258700595450701, tier 2^29, nonces 0..=15567367629'])

ranges = [0]
ranges += [2**e - L // 2 for e in (32, 33, 34)]
prev = [0] + [w + 1 for w in wins[:-1]]
gaps = sorted(((w - s, s, w) for s, w in zip(prev, wins)), reverse=True)[:3]
for (g, s, w), n in zip(gaps, (8, 6, 4)):
    ranges += [s + (g * (i + 1)) // (n + 1) - L // 2 for i in range(n)]
ranges += [(2**34 * (i + 1)) // 11 - L // 2 for i in range(10)]
assert len(ranges) == 32 and all(0 <= r < P for r in ranges)
out('sample.ranges', [f'{r} {L}' for r in ranges],
    ['start len: 32 x 2^22 nonces of challenge 2664258700595450701',
     f'zk 8 gaps sampled (nonces): {[(s, w) for g, s, w in gaps]}'])

launches = [f'{s + ((w - s) // BATCH) * BATCH} {BATCH}  # expect {w}' for s, w in zip(prev, wins)]
out('zk8.launches', launches, ['zk 8 exact launches at tier 2^29: start batch  # expected winner'])

nonces = set(range(64))
for k in range(1, 64):
    for d in (-1, 0, 1):
        if 0 <= 2**k + d < P:
            nonces.add(2**k + d)
nonces |= {P - 1, P - 2}
nonces |= set(wins)
nonces |= {(2**34 * i) // 64 + 12345 for i in range(64)}
g, s, w = gaps[0]
nonces |= {s + (g * i) // 64 for i in range(64)}
out('digests.nonces', sorted(nonces), [f'{len(nonces)} nonces for the device-vs-CPU digest check'])
print('gaps', gaps)
print('ranges', len(ranges), 'digests', len(nonces))
