# M1 Bench Results — first cross-hardware Nockchain proving benchmarks

**Date:** 2026-07-15
**Tool:** `tock` 0.1.0 (this repo), workload v2, pow-len 64,
kernel jam `sha256:2e762b175e9dfcdf…` (miner.hoon @ nockchain 31b8a015)
**Raw data:** `bench-results/*.json` (per-proof timings, hardware descriptors)

## Results

| hardware | single-thread per-proof | single-thread proofs/s | all-cores proofs/s | all-cores proofs/day |
|----------|------------------------|------------------------|--------------------|----------------------|
| Apple M1 (4P+4E, 8 GB), macOS | **20.5 s** | 0.048 | 0.153 (4 threads) | ~13,200 |
| AWS Graviton4, c8g.4xlarge (16c, 32 GB) | 30.4 s | 0.033 | **0.514** (16 threads) | **~44,400** |
| AMD EPYC 9R14, c7a.4xlarge (16c, 32 GB) | 34.0 s | 0.029 | 0.465 (16 threads) | ~40,100 |

Observations:

- **Apple M1 has the fastest single core** by a wide margin (20.5 s vs 30–34 s
  on the server parts) — the prover is dominated by single-core field
  arithmetic + hashing and rewards high IPC/clock over core count.
- **Throughput scales near-linearly with cores**: 15.6× on 16 Graviton cores,
  15.8× on 16 EPYC cores, 3.2× on the M1's 4 P-cores (its E-cores and
  8 GB RAM limit it). Per-proof latency rises only ~2–25% under full load.
- **Graviton4 beats EPYC 9R14 (Zen 4c)** per core and in aggregate at a lower
  hourly price — interesting for cloud-mining economics.
- **Reproducibility** is excellent: within-run per-proof spread is ±3% on
  macOS and under ±1% on the dedicated cloud boxes — far inside the ±10%
  M1 success criterion. (Cross-run reproducibility on the M1 Mac confirmed
  across two sessions: 20.5/21.4/21.2 s vs 20.4–21.6 s.)

## Method (short)

Standalone invocation of the mainnet miner kernel (see the M0 findings doc):
per-proof nonces derived as `seed/<i>` via the versioned rule
`fnv1a-splitmix64-v1`, target set to max so timing is difficulty-independent,
one booted kernel per thread, wall-clock measured around each kernel poke.
Cloud boxes were dedicated-vCPU instances provisioned by
`tock/setup-bench.sh`; kernel jam copied, not recompiled, so the workload is
bit-identical (verified by the jam sha256 recorded in every result).

## Cost

Both cloud runs together consumed ≈ **$0.85** of AWS credits
(c8g.4xlarge ~20 min, c7a.4xlarge ~45 min incl. one false start).

## Follow-ups

- More configs when convenient: a modern consumer Ryzen/Intel desktop and an
  M-series Pro/Max would bracket the miner-hardware spectrum.
- Sample the tail: 8-thread run on the M1 Mac (E-core contribution), and a
  32c+ EPYC box to check linear scaling holds at higher core counts.
- The proofs/day numbers feed the M2 `economics` peek (NOCK/day at current
  difficulty/emission).
