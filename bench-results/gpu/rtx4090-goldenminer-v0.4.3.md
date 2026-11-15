# GPU spike: GoldenMiner CUDA prover on RTX 4090 (RunPod)

2026-07-15, ~15 min pool-mode run. SELF-REPORTED numbers (pool prover's own
logs) — NOT verified, NOT in a unit known to equal tock's STARK proofs.

- Prover: golden-miner-pool-prover v0.4.3 (closed source), "Pure GPU mode"
- Card: NVIDIA GeForce RTX 4090 (128 SM, 23.5 GB), RunPod community cloud
- Reported speed: **326–334 "p/s"**, slowly rising over the window
- GPU utilization: 100%, ~405–431 W, ~13.0 GB VRAM
- Pool: Golden Miner (real work: "New task received, height: 105300")
- Burner payout address: XaJktdYiva2QsDL2pyJxmzQCeNdUgELdNvfDjy6JNiCCTZHH4KjMnh

Unit caveat (the whole point): tock measures full STARK proofs (~30 s each
on a server core → 0.033 proofs/s/core). If GoldenMiner's "p/s" were the
same unit, one 4090 = ~10,000 CPU cores — implausible for STARK-on-GPU
(typical gains 10–50×). More likely "p/s" counts PoW *attempts* or sub-proof
units, matching the network's headline "~3M proofs/s" (which would then be
~9,000 4090-equivalents — a believable network size). Chain height in the
task log (~105,300) also post-dates the Aletheia activation block (65,500),
i.e. current emission era is 2,048 NOCK/block.

Rough pool-unit economics at face value: 334/3,000,000 of 295k NOCK/day
(post-activation emission) ≈ 33 NOCK/day ≈ $1.3/day at $0.04 — against
~$1–1.5/day of electricity at 420 W. Consumer-GPU NOCK mining is roughly
break-even at today's price, and pool "p/s" units are unverifiable —
exactly the opacity Nockmark exists to cut through.
