# The 4090 ZK anomaly: luck or a device miss? (M7 step 5)

2026-11-13. Live run zk 8 (RTX 4090, tier 2²⁹, pipelined sweep) needed 1.81×
the expected hashes for its 16 wins (29.0 expected units, z = +3.25,
p ≈ 0.003). Every winner it reported was real, so the row could only be
understated. The open question: did upstream's CUDA V5 search on that card
miss winners the CPU oracle would have found, or was it bad luck?

**Result: bad luck. The card missed nothing.** Replayed on a 4090, zk 8's
search range holds exactly 16 winners at 2²⁹, and they are the 16 zk 8
found. Upstream's CUDA V5 search agreed with the CPU oracle on every winner
and every digest checked. The row stands as measured; there is nothing to
report upstream.

## The probe

`tock/examples/zk_v5_probe.rs` drives upstream's own primitives only:
`CudaSession::search` (the device) and `v5_pow_digest` + `digest_le` (the CPU
oracle). Every scan uses **zk 8's own challenge** (kernel nonce
2664258700595450701), so the device recomputes exactly the digests zk 8's
card computed. Inputs are in `inputs/` (`make_inputs.py` derives them from the
zk 8 report); `analyse.py` turns `out/` into `summary.json` and the tables
below.

1. **Replay.** The device enumerates every winner at tier 2²⁰ over zk 8's
   whole search range, nonces 0 … 15,567,367,629 (2^33.86): search 131,072
   nonces per launch and restart one past each winner, which is zk-bench's own
   loop. Every winner is re-checked on the CPU, and its CPU digest is kept.
   Winners at 2²⁹ are a subset of winners at 2²⁰ (the targets nest), so this
   gives the complete set of 2²⁹ winners in zk 8's range, to compare with
   the 16 zk 8 found. It is also the win-rate probe past 2³² nonces: 14,846
   wins expected at 2²⁰, and the depth of each digest gives the counts at
   2²¹ … 2³².
2. **Device vs CPU oracle**, 32 sampled ranges of 2²² nonces at tier 2¹⁴
   (8,192 wins expected): the start, straddles of 2³², 2³³ and 2³⁴, points
   inside zk 8's three longest gaps, and ten points spread to 2³⁴. The device
   enumerates every winner in each range at the production batch (262,144),
   so most launches hold several winners and the lowest-winner report is
   exercised. The CPU enumerates the same ranges, on the pod and again on an
   M1 Max (aarch64). The sets must be identical.
3. **zk 8's exact launches.** The 16 launches that found zk 8's winners
   (start = previous winner + 1 + m × 262,144), replayed at the 2²⁹ target.
   Each must return its winner.
4. **Digests.** Device digest vs CPU digest, all five limbs, for 383 nonces:
   0–63, 2^k − 1, 2^k and 2^k + 1 up to 2⁶³, p − 1 and p − 2, zk 8's winners,
   and points across its range and its longest gap.

## Results

| check | result |
|---|---|
| 2²⁹ winners in zk 8's range, nonces 0 … 15,567,367,629 | **16, identical to zk 8's 16**; none missed |
| win rate at 2²⁰ over the same range (2^33.86 nonces) | 14,931 vs 14,846 expected (z +0.70); 0 false positives |
| the same, per 2³⁰-nonce block (15 blocks) | χ² 10.6 on 14 dof (p ≈ 0.72) |
| device vs CPU oracle, 32 × 2²² sampled nonces at 2¹⁴ | device 8,229 = pod CPU 8,229 = M1 Max 8,229, the same nonces; 8,192 expected |
| zk 8's 16 exact launches at 2²⁹ | 16 / 16 return zk 8's winner |
| device vs CPU digests, all five limbs | 383 / 383 equal, nonces up to p − 1 |

Where the shortfall sits: the same 14,931 digests counted at each finer tier.

| tier | wins in zk 8's range | expected | z |
|---|---:|---:|---:|
| 2²⁰ | 14,931 | 14,846 | +0.70 |
| 2²² | 3,691 | 3,712 | −0.34 |
| 2²⁴ | 916 | 928 | −0.39 |
| 2²⁶ | 241 | 232 | +0.59 |
| 2²⁷ | 115 | 116 | −0.09 |
| 2²⁸ | 48 | 58 | −1.31 |
| **2²⁹** | **16** | **29.0** | **−2.41** |
| 2³⁰ | 6 | 14.5 | −2.23 |
| 2³¹ | 1 | 7.2 | −2.32 |

(Counts are nested, so the z values are not independent; `summary.json` has
every tier from 2²⁰ to 2³².)

- **The device saw every nonce.** Down to 2²⁷ the digests of this challenge
  are exactly as common as they should be. A device that skipped work or
  dropped winners would thin every tier alike.
- **The shortfall is in the challenge's deep tail.** Only at 2²⁸ and below
  do the counts fall away. This challenge's digests simply held few very
  small values in that range, and any correct device grinding it would have
  needed the same 1.81× the expected hashes.
- **How rare.** For one run, p ≈ 0.003. Across the eight ZK v5 runs on the
  board, the chance that at least one is this unlucky is about 1 in 40.
- **zk 8's own host is not needed for the conclusion.** The probe ran on a
  different 4090 (driver 570.211.01 against zk 8's 580.95.05). The 2²⁹
  winner set is a property of the challenge, not of a card, and this card
  was checked against the CPU oracle three ways above.

### Not covered

- A fault on zk 8's own card that only shows on that card. It cannot have
  cost zk 8 a winner, because none existed to miss, but it was not tested.
- Other challenges at 2²⁹ and above. The win rate at the deep tiers was
  checked on this one challenge only (2³⁰: 6 vs 14.5 here); #36's 48 GPU wins
  at 2²⁹–2³⁰ on other challenges were consistent (z −0.21).

## How it was run

On the pod, from `/build/nockmark/tock` after `cargo build --release
--features zk-gpu --example zk_v5_probe` (`ZK_POW_CUDA_ARCH=89`), with
`C=2664258700595450701` and `I=inputs`:

```sh
P=target/release/examples/zk_v5_probe
$P --challenge $C --tier-log2 20 device-scan --ranges $I/replay.ranges --batch 131072 --out replay-t20.txt
$P --challenge $C --tier-log2 14 cpu-scan --ranges $I/sample.ranges --threads 9 --out sample-cpu-t14.txt
$P --challenge $C --tier-log2 14 device-scan --ranges $I/sample.ranges --batch 262144 --out sample-dev-t14.txt
$P --challenge $C --tier-log2 29 launches --file $I/zk8.launches --out zk8-launches-t29.txt
$P --challenge $C digests --file $I/digests.nonces --out digests.txt
```

The first two ran side by side (19 min; the replay at 13.6 M nonces/s, the
card at its 450 W cap), the rest after (3 min). The M1 Max ran the same
`cpu-scan` locally (`out/sample-cpu-t14.m1max.txt`, 153 k H/s on 8 threads).
`out/host.txt` has the host, driver and binary hash; `out/gpu-samples.txt`
the card's utilisation and power every 10 s.

One slip, no effect on the results: the pod's CPU quota was 11.05 CPUs
although `nproc` said 96, and the first CPU scan started 94 threads. That
starved the replay's driver thread for about a minute (GPU at 19 %). The scan
was restarted with 9 threads.

## Pod and cost

**$0.223 of the $0.50 cap** (balance $5.151 → $4.928 once billing settled;
it read $4.941 straight after teardown), community cloud,
4090 at $0.34/h. The API then showed `pods: []` at $0/h.

| host | driver | outcome | minutes |
|---|---|---|---:|
| #1–#3 | 580.178.04 | failed the CUDA check before any build; taken straight down | 1.3, 1.2, 1.2 |
| #4 | 570.211.01, EPYC 7642 (11.05-CPU quota) | `cuInit` OK; build (2 min 48 s), replay, cross-checks | 35.2 |

The first 4090 was offered after 17 min of `SUPPLY_CONSTRAINT`. Hosts on
driver 580.178.04 join 580.173.02 and 620.70.06 (#41) as community hosts that
fail `cuInit` in our image; their error code was not captured. On #4 the
diagnostic showed `/dev/nvidia-uvm` present and `cuInit=0`.
