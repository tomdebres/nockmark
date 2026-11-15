# ZK v5 GPU sweep: RTX 3090 / 4090 / 5090 (RunPod, live registry)

2026-11-12, M7 step 5 (ZK half). One `tock zk-bench --gpu --submit
https://nockmark.xyz` per card: self-calibrated, k = 16, tock at `1a704cd`,
nockchain `ed1edfcc`. Each pod built miner.jam with hoonc at the pin; its sha256
`1f6c1f32…387c0e` was identical to the local jam on every pod. The live registry
verified every proof twice (the native verifier and the consensus Hoon
verifier) and ranked each run.

The 3090 row is step 3's run 2 (PR #34). The 4090 and 5090 runs were
instrumented. Every zk-bench output line is timestamped, and `nvidia-smi` plus
the tock process's `/proc` CPU ticks were sampled every 5 s. The files are
`rtx*/live.txt` (raw) and `rtx*/summary.json` (derived by the same script for
both cards).

| card (pod host CPU) | run | calibrated | sustained | verified | verified / calibrated | proof | tier | hashes / expected | NOCK/day | pod |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| RTX 3090 (EPYC 7663) | 2 | 6.11 MH/s | 5.18 | 4.82 | 0.789 | 39.5 s | 2²⁹ | 0.676 | 5.89 | $0.22/h, in step 3's $0.31 |
| RTX 4090 (Threadripper PRO 5975WX) | 3 | 14.65 MH/s | 14.03 | **10.03** | 0.685 | 29.8 s | 2³⁰ | 0.989 | 12.25 | $0.34/h × 51 min = $0.29 |
| RTX 5090 (EPYC 9654) | 4 | 17.83 MH/s | 17.30 | **9.53** | 0.535 | 33.7 s | 2³⁰ | 1.246 | 11.64 | $0.69/h × 53 min = $0.61 |

- **Calibrated:** 2 s at target 0, before the challenge exists.
- **Sustained:** hashes actually computed ÷ grind time, i.e. the device's rate over about 20 minutes.
- **Verified:** the board's ranked figure, k × 2^tier ÷ server window. The server window runs from mint to commit, after both server verifiers.
- **NOCK/day:** as the board showed at 20:44 UTC; it moves with difficulty.
- **Upstream claims:** PR #192 claimed 4090 ≈ 14.5 and 5090 ≈ 18 MH/s. The calibrations are within 1 % of both.

## Why verified is below raw

verified / calibrated = luck × sustained/calibrated × grind/window

| card | luck (expected ÷ actual hashes) | sustained / calibrated | grind / window | product | the same run at luck 1 |
|---|---:|---:|---:|---:|---:|
| 3090 | 1.479 | 0.848 | 0.629 | 0.789 | 0.61 (3.70 MH/s) |
| 4090 | 1.011 | 0.958 | 0.707 | 0.685 | 0.68 (9.96 MH/s) |
| 5090 | 0.802 | 0.971 | 0.687 | 0.535 | 0.62 (11.03 MH/s) |

Where the server window went (sampler means per phase):

| phase | 4090 | GPU, CPU | 5090 | GPU, CPU |
|---|---:|---|---:|---|
| grind until 16 wins | 1,210.8 s | 100 %, 2,508 MHz, 384 W (at the 400 W cap); 1.0 core | 1,237.2 s | 100 %, 2,644 MHz, 520 W (at the cap); 1.0 core |
| prove the 16 winners, one at a time | 476.5 s | **0 %**; **1.0 core of 64** | 539.7 s | **0 %**; **1.0 core of 192** |
| submit (upload, both server verifiers, commit) | 24.8 s | 0 % | 25.1 s | 0 % |
| server window | 1,712.1 s | | 1,801.9 s | |

1. **Yes, the GPU idles while the CPU proves winners serially.** zk-bench grinds all k wins, then proves them one by one, on one core. That is 28 % of the window on the 4090, 30 % on the 5090 and about 35 % on the 3090. It is the tier rule working as designed: about 2 s of grinding per 1 s of proving (R = 2), which caps recovery at R/(R+1). Power-of-two tier rounding moves the effective R per card: 2.57 for the 4090, 1.84 for the 5090, 2.62 for the 3090.
2. **Luck, ±25 % at k = 16, explains the board's order.** The 5090 needed 1.246× the expected hashes (its first win alone took 7.2× the mean gap) and the 4090 needed 0.989×. At luck 1 the 5090 would sit above the 4090, at 11.0 vs 10.0 MH/s.
3. **Sustained vs calibration** costs 4 % on the 4090 and 3 % on the 5090. Both ran at their power limit for the whole grind (`nvidia-smi` throttle reason `0x4`, SW power cap). The 3090 lost 15 %; that run was not sampled, so the cause is not established.
4. **Submit** is about 25 s, 1.4 % of the window.
5. **The host CPU sets the proof time**, from single-core speed: 29.8 s, 33.7 s and 39.5 s on the three hosts. With the tier sized from it, the host changes the tier rather than recovery, except through the rounding in item 1.

## Parallel proving and grinding (probe after each live run, same pod)

N concurrent `zk-bench --attempts 4096 -k 1 -t 1` processes, each proving one
`%5` winner with its own kernel (`rtx*/parallel-proving.txt`):

| N | 4090 host (32 cores) | 5090 host (96 cores) |
|---:|---:|---:|
| 1 | 29.3 s | 33.8 s |
| 8 | 30.6 s each | 33.7 s each |
| 16 | 40 s each | 33.8 s each |
| 32 | — | 47.1 s each |

The 4090's GPU grind (tier 2²⁸, 4 wins, the same nonces both times) ran at
14.50 MH/s alone and at 14.49 MH/s with 8 proofs running beside it.

## Proposed fix (follow-up; the client was not changed for this sweep)

Pipeline zk-bench the way upstream's own miner works: GPU workers keep
searching while a Serf prover proves the winners. Hand each winner to a prover
thread as it is found, and keep the device grinding. The window then becomes
grind + one proof + submit.

- **Predicted recovery at luck 1:** 0.92 of calibrated on the 4090 (13.4 MH/s), 0.92 on the 5090 (16.3 MH/s) and 0.81 on the 3090 (4.97 MH/s), up from 0.68, 0.62 and 0.61.
- **One prover keeps up.** A proof takes about 30 s and the grind per win takes 60–100 s. A small prover pool costs these hosts nothing (probe above) and shortens the tail.
- **Recovery stays host-independent.** It becomes 1 − 1/(kR + 1) ≈ 97 % at k = 16, R = 2, and the proof time still cancels.
- **Every row must be re-measured with the new client at once.** A mixed board would favour re-run rows by about 1.4×.

Luck is a separate matter: ±25 % at k = 16, and today's 4090/5090 order is
inside it. Halving it needs k = 64, about 4× the run, which is over the 1 h
expiry at today's tiers.

## Win tally: digest uniformity at 2²⁹–2³⁰

The GPU walks nonces up from 0, so the gaps between a run's winning nonces (on
the board) are its hashes per win. They should be exponential with mean
2^tier.

| run | tier | 16 wins took | z |
|---|---:|---:|---:|
| 3090 (run 2) | 2²⁹ | 10.82 × 2²⁹ | −1.30 |
| 4090 (run 3) | 2³⁰ | 15.82 × 2³⁰ | −0.04 |
| 5090 (run 4) | 2³⁰ | 19.94 × 2³⁰ | +0.98 |
| pooled, 48 wins | | 46.58 expected units | −0.21 |

- **Distribution:** KS against Exp(1) gives D = 0.131, p ≈ 0.36.
- **Largest gap:** 7.18, the 5090's first win. P(max of 48 ≥ 7.18) = 0.036.
- **Verdict:** consistent with a uniform digest at 2²⁹ and 2³⁰. 2³¹ was not exercised, since no card's tier rounded up to it.

## Cost

RunPod **$0.900** (balance $7.746 → $6.846), both pods community cloud.

- **4090:** deployed after 2 minutes of `SUPPLY_CONSTRAINT` retries. Bootstrap took 1 min, build + jam 10 min, the live run 29 min and the probe 8 min.
- **5090:** deployed after 3 minutes of retries. Build + jam took 10 min, the live run 30 min and the probe 3 min.
- **Teardown:** both pods were terminated by id, and the API then showed `pods: []` at $0/h.
