# GPU sweep: GoldenMiner CUDA prover across four cards (RunPod)

2026-07-18, one ~15-minute pool-mode run per card. SELF-REPORTED numbers
(the pool prover's own logs) — NOT verified, NOT in a unit known to equal
tock's STARK proofs. Same method as the 2026-07-15 4090 spike
(`../rtx4090-goldenminer-v0.4.3.md`): golden-miner-pool-prover v0.4.3
(closed source), GPU mode, mining live pool work with the nockmark burner
pubkey, `nvidia-smi` sampled every 15 s. Raw logs per card in the
subdirectories (`console.log` is the full prover + utilization capture).

| card | steady "p/s" | avg W | "p/s"/W | cloud, $/hr | pod cost |
|------|-------------:|------:|--------:|------------:|---------:|
| RTX 3090 (community) | 161.0 ±2.4 | 269 | 0.60 | 0.22 | $0.06 |
| A100 SXM4 80GB (community) | 170.7 ±1.6 | 277 | 0.62 | 1.39 | $0.36 |
| RTX 4090 (secure) | 315.7 ±2.2 | 398 | 0.79 | 0.69 | $0.18 |
| RTX 5090 (secure) | 431.2 ±1.7 | 526 | 0.82 | 0.99 | $0.26 |

Steady "p/s" is the mean of the second half of each run (n=26 samples);
all four cards sat at 98–100% GPU utilization throughout.

Observations, in the pool's own unit:

- **The 4090 number reproduces.** 315.7 here vs 326–334 on 2026-07-15 —
  within ~4% across different hosts and days.
- **Consumer cards beat the datacenter card on value.** The A100 80GB
  matches a 3090 (~170 vs ~161) while renting for 6× the price. This
  workload doesn't reward HBM or tensor throughput the way ML does.
- **The 5090 is the efficiency and throughput leader**: ~37% faster than
  the 4090 at ~32% more power — per-watt the best of the four, but the
  gain is incremental, not generational.
- Earnings at 2026-07-18 economics (network ~3M "p/s" per the
  year-in-review unit, ~1.18M NOCK/day emission, NOCK ≈ $0.019):
  3090 ≈ 63 NOCK/day ($1.20), A100 ≈ 67 ($1.27), 4090 ≈ 124 ($2.36),
  5090 ≈ 170 ($3.22) — before electricity. Consumer cards roughly clear
  a residential power bill; the A100 only makes sense if the machine is
  free.

The caveat from the write-up stands: these are numbers from a closed
binary in an unverifiable unit. The registry verifies CPU proofs today;
verified GPU numbers need a prover that can be driven with a challenge
nonce and emits its proof artifacts (fake-pool work injection is the
candidate approach — see the M1 follow-ups).
