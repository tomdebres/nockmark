# Nockmark

Verified proving benchmarks for [Nockchain](https://github.com/zorp-corp/nockchain).

Nockchain is a zkPoW chain: miners earn NOCK by producing STARK proofs, so
"what hardware proves fastest?" is the question that decides whether mining
pays. Every public answer so far has been self-reported. Nockmark is a
registry where the numbers are verified instead: your machine proves real
mining workloads against a server-issued challenge, the registry checks
every STARK, and your rate is computed from the server's own clock. The
published number is a lower bound that nobody can inflate — including you.

- Leaderboard: https://nockmark.xyz/ (JSON at `/leaderboard`)
- Earnings estimates: https://nockmark.xyz/economics
- Write-up with cross-hardware results (M1, Graviton4, EPYC, Xeon):
  [docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md](docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md)

## Get on the board

```sh
cd tock
./target/release/tock bench --kernel assets/miner.jam \
  --submit https://nockmark.xyz
```

Build instructions (pinned toolchain, kernel jams) are in
[tock/README.md](tock/README.md); `tock/setup-bench.sh` provisions a fresh
Ubuntu box end-to-end in about 15 minutes. The benchmark itself is ~3
minutes of proving on an M1.

There is a second track for Logos's AI-PoW puzzle (INT8 tiled matmul +
recursive STARK certificate — the first nockchain mining path with an
open-source miner). Same trustless model:

```sh
./target/release/tock ai-bench --submit https://nockmark.xyz
```

That track benchmarks two statements, on one board. The default is the
single-tile `dense` benchmark; `--statement canonical-moe` benchmarks the
canonical MoE block the production gateway-free miner actually submits:

```sh
./target/release/tock ai-bench --statement canonical-moe --submit https://nockmark.xyz
```

On an NVIDIA GPU, `--gpu` grinds the canonical-MoE statement on upstream's
CUDA backend. It needs a build with the `gpu` feature, which requires `nvcc`
(the feature is off by default so CPU-only machines build unchanged), and the
arch flags for your card — `compute_86`/`sm_86` for Ampere, `compute_89`/
`sm_89` for Ada, `compute_120`/`sm_120` for Blackwell:

```sh
AI_POW_CUDA_ARCH=compute_86 AI_POW_CUDA_CODE=sm_86 \
  cargo build --release --features gpu
./target/release/tock ai-bench --statement canonical-moe --gpu \
  --submit https://nockmark.xyz
```

Only the jackpot **search** moves to the device. Every win the GPU proposes is
re-checked against the scalar oracle and certified by the same CPU prover a
`--gpu`-less run uses, so a GPU row is verified by the registry in exactly the
same way as a CPU one — the device is a filter, never a source of trust.
`scripts/gpu_pod.py` is the RunPod recipe this was developed and measured on.

Both rank together because rates are MAC-equivalents/sec — the unit consensus
itself uses to compare AI work of different tile shapes — convertible to
ZK-attempt-equivalents at the consensus exchange rate; details at
https://nockmark.xyz/api.

`ai-bench` measures your machine for ~2 s first and asks the registry for a
difficulty **tier** sized so the grind takes about a minute — otherwise fast
hardware finishes grinding in milliseconds and its ranked rate is nothing but
the fixed certificate-proving cost inside the measured window. Pass
`--attempts <n>` to pick the tier yourself (expected grind attempts per win) or
`--target <hex>` to pin the difficulty outright; either skips the calibration.
Choosing a tier cannot inflate a rate: an easy one earns less credit against
that same fixed proving cost, and a hard one has to actually be ground out,
attempt by verified attempt.

## How submissions are verified

1. `POST /challenge` returns a nonce; your proofs are derived from it, so
   nothing can be precomputed.
2. `tock` proves k=8 real mainnet workloads (proof version 3 since the Zoe
   fork, pow-len 64, miner kernel pinned at nockchain `1372f270` and
   fingerprinted by sha256).
3. `POST /run` submits the proofs. The registry verifies each STARK with a
   verifier kernel compiled from the same pinned nockchain tree, and
   computes proofs/sec from `submitted_at − issued_at` on its own clock.
   Client-reported timings are displayed but never ranked.

What is *not* verified: the hardware descriptor is self-reported. Anything
rate-related is.

## Layout

- `tock/` — bench harness and submit client (Rust; boots the mainnet miner
  kernel one instance per thread)
- `registry/` — the registry server (Rust/axum; verification, leaderboard,
  economics)
- `hoon/` — the registry's verifier kernel, compiled against the pinned
  nockchain tree by `scripts/build-registry-jam.sh`
- `bench-results/` — raw result JSONs behind the write-up
- `docs/` — write-up, design notes, deploy runbook
- `deploy/`, `Dockerfile`, `railway.json` — the deployed instance

Building requires a nockchain checkout at `1372f270` (path dependencies —
expected location and toolchain pinning in `tock/README.md`).

Runs from hardware not yet on the board are the most useful contribution —
especially modern desktop CPUs and big-core-count servers.
