# Pipelined sweep: every GPU row re-measured (M7 step 5, part 2b)

2026-11-13, 00:54–02:22 UTC. Every board row was re-measured with the
pipelined client: tock `58fe623` (PR #40), nockchain `ed1edfcc`, k = 16, all
runs self-calibrated against the live registry (`https://nockmark.xyz`).
Each pod built `miner.jam` with hoonc at the pin, and its sha256
`1f6c1f32…387c0e` matched the local jam on every pod. The registry verified
every win and ranked every run. Community cloud only.

- **Per host:** `<host>/<run>.json` is tock's `--report` (calibration and the
  per-win found/prove timeline).
- **Pod runs** also have `<run>.live.txt` (host header plus a timestamped tock
  log; `scripts/sweep_run.sh`) and `<run>.samples.txt` (nvidia-smi and tock's
  CPU ticks every 5 s).
- **Derived tables:** `summary.json` and `summary.md`, from
  `scripts/sweep_summary.py`.
- **Board snapshot:** `board-zk.json` and `board-ai.json`, taken 02:26 UTC.

## The rows

| card / host | statement | run | calibrated | sustained | verified | luck | verified ÷ calibrated | at luck 1 | proof | tail | host CPU (quota) | pod $/h | wall | NOCK/day |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|
| RTX 5090 | dense-peak (#159 kernel) | ai 12 | 1.072×10⁸ tiles/s | 1.010×10⁸ | 1.114×10⁸ = **2.34×10¹⁴ MAC-eq/s** | 1.23 | 1.04 | 0.86 | 23.6 s | 63.5 s | EPYC 7663 (31.4) | 0.69 | 10.8 min | 7.22 |
| RTX 5090 | canonical-moe | ai 13 | 2.87 M att/s | 2.85 M | 2.30 M = 1.51×10¹¹ | 0.87 | 0.80 | 0.92 | 7.2 s | 16.0 s | EPYC 7663 (31.4) | 0.69 | 4.1 min | 0.0047 |
| RTX 4090 | canonical-moe | ai 14 | 2.00 M att/s | 2.00 M | 2.10 M = 1.38×10¹¹ | 1.13 | 1.05 | 0.94 | 9.2 s | 19.0 s | i5-14600KF | 0.34 | 4.5 min | 0.0043 |
| RTX 3090 | canonical-moe | ai 10 | 0.924 M att/s | 0.912 M | 0.949 M = 6.22×10¹⁰ | 1.10 | 1.03 | 0.94 | 9.0 s | 15.5 s | TR PRO 5995WX (17.9) | 0.22 | 4.9 min | 0.0019 |
| TR PRO 5995WX CPU (3090 pod) | dense-peak | ai 15 | 28.0 k tiles/s | 23.5 k | 14.9 k = 3.13×10¹⁰ | 0.67 | 0.53 | 0.77 | 127 s | 120 s | 64 workers on a 17.9 quota | 0.22 | 38.0 min | 0.00097 |
| M1 Max CPU | canonical-moe | ai 11 | 3,472 att/s | 2,846 | 2,940 = 1.93×10⁸ | 1.08 | 0.85 | 0.78 | 28.3 s | 33.8 s | M1 Max (10) | — | 11.9 min | 6×10⁻⁶ |
| RTX 5090 | ZK v5 | zk 7 | 17.28 MH/s | 16.72 | **17.32** | 1.11 | 1.00 | 0.91 | 37.8 s | 62.8 s | EPYC 7663 (31.4) | 0.69 | 17.4 min | 20.1 |
| RTX 4090 | ZK v5 | zk 8 | 14.69 MH/s | 14.46 | 7.66 | **0.55** | 0.52 | 0.92 | 19.1 s | 45.0 s | i5-14600KF | 0.34 | 19.2 min | 8.87 |
| RTX 3090 | ZK v5 | zk 6 | 6.28 MH/s | 6.18 | 6.42 | 1.13 | 1.02 | 0.91 | 30.4 s | 54.1 s | TR PRO 5995WX (17.9) | 0.22 | 11.9 min | 7.44 |
| M1 Max CPU | ZK v5 | zk 5 | 175.9 kH/s | 166.4 | 236.8 | 1.91 | 1.35 | 0.80 | 26.3 s | 144 s | M1 Max (10) | — | 9.8 min | 0.27 |

- **Calibrated:** the client's 2 s measurement before the mint. **Sustained:** attempts actually made over the grind.
- **Verified:** the board's ranked figure, k × tier ÷ the server window, also given in attempts/s.
- **Luck:** expected ÷ actual attempts. Above 1 means lucky.
- **At luck 1:** the expected work over the sustained rate, plus the run's measured tail (server window − grind), divided into k × tier and over calibrated. The tail itself depends on luck (a lucky run bunches its wins and queues proofs), so this is an estimate.
- **Proof:** mean proof time during the run. **Tail:** server window minus grind: the last proof(s), the upload and both server verifiers.
- **NOCK/day:** as the board showed it at 02:26 UTC (ZK difficulty 9.17×10¹³, AI 3.43×10²¹ MAC-eq/block, reward 1228.8, NOCK $0.01266). It moves with difficulty.

## Against the serial rows

| card | ZK at luck 1, serial (#34 / #36) | pipelined (this sweep) | predicted in #36 |
|---|---:|---:|---:|
| RTX 3090 | 0.61 (3.70 MH/s) | 0.91 (5.73 MH/s) | 4.97 MH/s* |
| RTX 4090 | 0.68 (9.96 MH/s) | 0.92 (13.4 MH/s) | 13.4 MH/s |
| RTX 5090 | 0.62 (11.0 MH/s) | 0.91 (15.8 MH/s) | 16.3 MH/s |

\* That prediction was for the step 3 host, whose sustained rate was 5.18 MH/s. This sweep's 3090 host sustained 6.18.

- **ZK:** the GPU rows now recover 0.91–0.92 of calibrated at luck 1, up from 0.61–0.68. The 4090 and 5090 match the #36 predictions to within 4 %. The remaining gap is:
  - the tail, ~45–63 s: the last win's proof, the upload and both verifiers;
  - the power-capped sustained rate, 1–4 % under calibration;
  - tiers that round down to R = 1.4–1.8. That still fits the k·R·p + p model: 0.95 × sustained/calibrated, less the upload.
- **canonical-MoE on GPU:** 0.92–0.94 at luck 1. The legacy serial GPU rows (M6) recovered 0.57–0.65 at the same R = 2. A certificate takes 7–9 s on these hosts, so the tail is 15–19 s.
- **dense-peak on the 5090:** 0.86 at luck 1. The sustained rate was 6 % under calibration (500 W cap, 97–98 % utilisation), and the tail was 63.5 s: the last 24 s certificate, the upload of 16 × 147 KB artifacts, and consensus verification.

## CPU-only hosts: the trade-off, measured

Pipelining helps when the prover has idle cores to use. On a CPU-only host,
the grinder and the provers compete for the same cores.

| host | statement | proving | sustained | proof beside the grind | proof alone | after the last win | at luck 1 |
|---|---|---|---:|---:|---:|---:|---:|
| TR PRO 5995WX, 17.9-CPU quota | dense-peak | pipelined (row ai 15) | 23.5 k tiles/s | 127 s (to 172 s) | 30.8 s | 104.5 s | 0.77 |
| same pod, same tier, no submit | dense-peak | serial | 30.2 k tiles/s | — | 28.3 s | 453.1 s | 0.76 |
| M1 Max | canonical-moe (row ai 11) | pipelined | 2,846 att/s (calibrated 3,472) | 28.3 s | 26.2 s | 30.4 s | 0.78 |
| M1 Max | ZK v5 (row zk 5) | pipelined | 166.4 kH/s (calibrated 175.9) | 29.6 s | 20.6 s | 120 s | 0.80 |

- **dense-peak on a CPU: a wash** (0.77 pipelined vs 0.76 serial at luck 1).
  - upstream's `CpuSearchBackend` starts one worker per *physical* core (64), whatever the pod's CPU quota (17.9). The prover's rayon pool honours the quota.
  - Beside the grind, a certificate took about 5× as long, and the grind lost 22 %. Both are fully parallel and CPU-bound, so overlapping them buys nothing.
  - The pipelined row (ai 15) is honest and ranked. Its lower verified figure against the old serial row 9 (5.49×10¹⁰ on an EPYC 7663 at a 23.8 quota) is mostly luck (0.67 against 1.28) and a different host.
- **canonical-moe on the M1:** the single grind thread ran 18 % under its calibration beside the 10-thread prover. The prover cost almost nothing in return (28.3 s vs 26.2 s), and the tail was one certificate.
- **ZK on the M1:** the row was lucky (luck 1.91). Wins bunched faster than one contended prover (29.6 s beside the grind, 20.6 s alone) could clear them, which left a 120 s queue at the end. At luck 1 the queue would be shorter, so 0.80 understates it. The local k = 8 run at luck 0.85 recovered 0.87 (`bench-results/cpu/2026-11-12-m1-pipelining/`).

## Open item: the 4090 ZK row's luck

- **The anomaly:** run zk 8 needed 1.81× the expected hashes for 16 wins at 2²⁹ (29.0 expected units, z = +3.25 under the exponential model, p ≈ 0.003).
- **What was checked:**
  - no proof failed or was replaced;
  - the device sustained its calibration (14.46 vs 14.69 MH/s), busy and power-capped throughout;
  - the long gaps are spread across the whole 15.6 G-nonce range, not one stall;
  - every winner was re-checked on the CPU and verified twice by the registry.
- **What that means:** false positives are ruled out, and the row cannot be over-credited. A device that *missed* winners on this host (driver 580.95.05, i5-14600KF) cannot be ruled out by this run. It would only understate the row.
- **Earlier runs were normal:** the #36 4090 (driver 575.51) at 2³⁰ came in at z = −0.04, and the 3090 and 5090 runs here at −0.46 and −0.38.
- **To settle it:** a win-rate probe on a 4090 past 2³² nonces at a low tier, about $0.20 with a re-roll. Not run.

## Pods and cost

**$1.542 in total, under the $5.50 cap** (balance $6.707 → $5.164). The API then showed `pods: []` at $0/h.

| slot | hosts tried | live minutes | $ |
|---|---|---:|---:|
| 5090 | #1: CUDA error 999, found only after its 18 min build; #2: ran everything | 20.9 + 48.7 | 0.241 + 0.560 |
| 4090 | #1: CUDA error 999 after its build; #2–#4: failed the 10 s CUDA check (#4 also used for the `cuInit` diagnosis); #5: ran everything | 16.0 + 1.3 + 1.5 + 2.7 + 46.0 | 0.091 + 0.007 + 0.008 + 0.015 + 0.261 |
| 3090 | one host (TR PRO 5995WX): MoE, ZK, dense-peak CPU, then the serial comparison | 94.1 | 0.345 |

- **CUDA error 999 hosts.** Community hosts on driver 580.173.02 (every 4090 offered for about 30 min) and 620.70.06 (one 5090) fail `cuInit` itself with error 999 in the `nvidia/cuda:12.8.1` image, even though `nvidia-smi` works. Hosts on 580.65.06 and 580.95.05 ran fine.
- **The fix:** the re-roll script now compiles and runs a `cudaGetDeviceCount` check before the 10–20 min build, and takes a host that fails it straight down.
- **Prep time** (tock with both CUDA backends, plus the hoonc jam) took 10–20 min, depending on the host's CPU quota.
