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

## How submissions are verified

1. `POST /challenge` returns a nonce; your proofs are derived from it, so
   nothing can be precomputed.
2. `tock` proves k=8 real mainnet workloads (proof version 2, pow-len 64,
   miner kernel pinned at nockchain `31b8a015` and fingerprinted by sha256).
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

Building requires a nockchain checkout at `31b8a015` (path dependencies —
expected location and toolchain pinning in `tock/README.md`).

Runs from hardware not yet on the board are the most useful contribution —
especially modern desktop CPUs and big-core-count servers.
