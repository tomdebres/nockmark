# tock — Nockmark bench harness

Benchmarks a machine against Nockchain's **real mainnet STARK prover** — the
same miner kernel mining uses, run standalone (no node, no networking). Born
as the M0 spike (`docs/superpowers/specs/2026-07-15-m0-spike-findings.md`);
this is the M1 `tock bench --local` milestone.

## Prerequisites

- The nockchain checkout at `../../../nockchain` (this crate uses path deps).
- The pinned nightly toolchain (`nightly-2026-04-03`). On this machine it
  lives at `~/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin`
  (installed from static.rust-lang.org tarballs; there is no rustup here).

```sh
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
```

## One-time: compile the kernel jams

The prover/verifier are Hoon kernels, compiled with hoonc (built once in the
nockchain repo). Two hoonc quirks: it writes the output jam into the
**current directory** (ignores the directory part of `--output`), and a data
dir cannot be reused — use a fresh one per kernel.

```sh
cd ../../../nockchain
cargo build --release -p hoonc
# miner kernel (the prover — the kernel mainnet mining uses), ~6 min
target/release/hoonc --new --data-dir /tmp/hoonc-miner \
  --output miner.jam hoon/apps/dumbnet/miner.hoon hoon
mv miner.jam ../nockmark/m0-prover-spike/tock/assets/
# roswell kernel (test harness exposing %verify-proof), ~17 min
target/release/hoonc --new --data-dir /tmp/hoonc-roswell \
  --output roswell.jam hoon/apps/roswell/roswell.hoon hoon
mv roswell.jam ../nockmark/m0-prover-spike/tock/assets/
```

## Bench

```sh
cargo build --release

# single-threaded (clean hardware-comparison number)
./target/release/tock bench --kernel assets/miner.jam

# all-cores (realistic mining-rate number), JSON output
./target/release/tock bench --kernel assets/miner.jam -k 16 -t 8 --json
```

The result records proofs/sec, per-proof ms, a hardware descriptor
(self-reported), the SHA-256 of the kernel jam (workload identity), and the
nonce-derivation rule version (`fnv1a-splitmix64-v1`, golden-tested in
`src/nonce.rs`). Per-proof nonces are derived as `<seed>/<i>` so no two
proofs share an input; `--seed` is where a registry challenge will land at M3.

## Prove / verify primitives (from the M0 spike)

```sh
# one proof, arbitrary nonce string
./target/release/tock prove --nonce "hello" --kernel assets/miner.jam --out proof.jam
# verify: ACCEPT/REJECT + timing, exit code 1 on reject
./target/release/tock verify --proof proof.jam --kernel assets/roswell.jam
```

Reference timings (Tom's Apple M1, 8 GB): prove ~21 s/proof single-threaded
(and ~21 s wall-clock for 2 proofs on 2 threads), verify ~0.5 s, proofs
~116 KB jammed.
