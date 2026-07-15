# Nockmark M0 spike

Standalone prove/verify against Nockchain's real STARK prover — no node, no
networking. See `docs/superpowers/specs/2026-07-15-m0-spike-findings.md` for
the full writeup.

## Prerequisites

- The nockchain checkout at `../../../nockchain` (this crate uses path deps).
- The pinned nightly toolchain (`nightly-2026-04-03`). This machine has it at
  `~/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin` (installed
  from static.rust-lang.org tarballs; no rustup on this machine).

```sh
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
```

## One-time: compile the kernel jams

The prover/verifier are Hoon kernels; compile them with hoonc (built once in
the nockchain repo):

```sh
cd ../../../nockchain
cargo build --release -p hoonc
# miner kernel (the prover — same kernel mainnet mining uses)
target/release/hoonc --new --data-dir ../nockmark/m0-prover-spike/spike/assets/hoonc-data \
  --output ../nockmark/m0-prover-spike/spike/assets/miner.jam hoon/apps/dumbnet/miner.hoon hoon
# roswell kernel (test harness that exposes %verify-proof)
target/release/hoonc --data-dir ../nockmark/m0-prover-spike/spike/assets/hoonc-data \
  --output ../nockmark/m0-prover-spike/spike/assets/roswell.jam hoon/apps/roswell/roswell.hoon hoon
```

(First compile bootstraps the Hoon compiler and is slow; the shared
`--data-dir` caches build state for the second kernel. Drop `--new` on
reruns against an existing data dir.)

## Run

```sh
cargo build --release

# prove: nonce is any string; proof written to proof.jam
./target/release/nockmark-spike prove --nonce "hello-nockmark-1" \
  --kernel assets/miner.jam --out proof.jam

# verify: prints ACCEPT/REJECT + timing, exit code 1 on reject
./target/release/nockmark-spike verify --proof proof.jam \
  --kernel assets/roswell.jam
```
