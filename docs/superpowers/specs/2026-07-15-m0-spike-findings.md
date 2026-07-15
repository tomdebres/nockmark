# M0 Spike Findings — Standalone STARK prove/verify

**Date:** 2026-07-15
**Status:** IN PROGRESS — this header will be replaced with GO / NO-GO once timings land.
**Nockchain commit:** 31b8a015 (Tom's fork, master, post native-compiler merge)

## Question

Can Nockchain's STARK prover and verifier be invoked standalone — outside
full-node mining — with a caller-controlled nonce? (Go/no-go gate for the
whole Nockmark design.)

## Answer so far: YES on entry points (code compiling; timings pending)

Both prove and verify are reachable without a node, a chain, or networking.
The key realization: Nockchain's prover isn't a Rust function — it's a **Hoon
computation run inside a NockVM kernel, accelerated by Rust jets**
(zkvm-jetpack). "Standalone invocation" therefore means: boot a small kernel
in a `SerfThread` (an in-process NockVM instance) and poke it. The mining
driver itself already does exactly this — mining does not go through the
node's main kernel at all.

## Entry points found

### Prove — the miner kernel

- **Rust template:** `nockchain/crates/nockchain/src/mining.rs` —
  `create_mining_driver` boots `SerfThread::<SaveableCheckpoint>::new(KERNEL, …)`
  (line ~353) and pokes it per attempt (`start_mining_attempt`, line ~478).
  This is the whole mainnet proving path; it needs no node.
- **Kernel:** `nockchain/hoon/apps/dumbnet/miner.hoon`. Poke cause:
  `[%2 header=noun-digest:tip5 nonce=noun-digest:tip5 target=bignum pow-len=@]`
  (version tag %0/%1/%2 = proof version; mainnet is currently %2).
- **Hoon call chain:** `prove-block-inner` in `nockchain/hoon/common/pow.hoon`
  → `prove:np` in `hoon/common/nock-prover.hoon` → STARK prover in
  `hoon/common/stark/prover.hoon`, jetted by `crates/zkvm-jetpack`
  (`produce_prover_hot_state()`).
- **Result:** effect `[%mine-result %& hash %command %pow proof dig header nonce]`
  — the proof noun is right there in the effect; jam it and you have the
  artefact.
- **Nonce shape:** `noun-digest:tip5` = 5 field elements (u64 < Goldilocks
  prime p = 2^64−2^32+1), as a right-nested 5-tuple. Any arbitrary input can
  be mapped into this (hash → 5 belts), which is exactly the
  challenge-nonce-incorporation Nockmark needs.
- **Getting the proof out:** the kernel only includes the proof in the effect
  when `check-target` passes (`hoon/common/pow.hoon:5`). For benchmarking, set
  target = max-tip5-atom = p⁵−1 (bignum `[%bn (list u32)]`, LSB-first limbs) —
  every proof passes, so prove time is measured independent of difficulty.
- **pow-len:** mainnet constant 64 (`hoon/common/ztd/eight.hoon:893`).

### Verify — the roswell kernel (or nock-verifier from any kernel)

- **Hoon:** `verify` in `nockchain/hoon/common/nock-verifier.hoon` (wraps
  `verify:verifier` in `hoon/common/stark/verifier.hoon`). This is the same
  gate the node applies to incoming blocks: `hoon/apps/dumbnet/inner.hoon:1073`
  `(verify:nv u.pow ~ eny)`.
- **Ready-made harness:** the **roswell** test kernel
  (`hoon/apps/roswell/roswell.hoon`) exposes poke
  `[%verify-proof p=(unit (unit proof))]` → effect `[%exit code]` (0 =
  accept). The Rust crate `crates/roswell` even has `check-proof` CLI and a
  `Roswell::check_proof(&Proof)` library function.
- **Bonus:** roswell also has `%prove` / `%test-custom` / `%proof-snapshot-for`
  pokes taking arbitrary header+nonce ("not yet implemented on the rust side"
  per kernel comment — but the kernel side exists; poking it from Rust is
  trivial noun construction).

## Spike binary

`spike/` in this repo — `nockmark-spike` with two subcommands:

- `prove --nonce <any string> --kernel miner.jam --out proof.jam` — boots the
  miner kernel exactly like the mining driver, derives 5 nonce belts from the
  string, pokes with max target, extracts + jams the proof, prints wall-clock.
- `verify --proof proof.jam --kernel roswell.jam` — boots the roswell kernel,
  pokes `%verify-proof`, prints ACCEPT/REJECT + wall-clock.

Build/run (see spike/README.md): kernels must first be compiled from Hoon
with `hoonc` (one-time, cached).

## Timings (this machine: Apple Silicon, Darwin 25.3.0)

| step | wall-clock | notes |
|------|-----------|-------|
| miner kernel boot | 1.6 s | one-time per SerfThread; excluded from prove time |
| **prove** (pow-len 64, v2) | **20.5 s / 21.4 s / 21.2 s** (3 nonces) | single-threaded, one attempt; ±3% |
| verify | _pending_ | |

Proof artefact: **116,039 bytes jammed** (`proof.jam`). Digest (tip5 hash of
proof): `0x4343…80e6`. Kernel compile times (one-time): miner.jam 6m15s cold,
roswell.jam _pending_ (warm cache); jams are 17 MB (miner).

## Sharp edges hit

1. **Toolchain:** repo pins `nightly-2026-04-03` (rustc 1.96); stable 1.94
   fails on `core::hint::cold_path`. This machine had no rustup binary (only
   an orphaned `~/.rustup` with stable); installed the standalone nightly
   toolchain into `~/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin`
   via static.rust-lang.org tarballs (no rustup needed).
2. **Kernel jams are not prebuilt.** `assets/*.jam` is gitignored; only the
   hoon-compiler bootstrap jam ships. `just build-kernel-assets` (or direct
   `hoonc` invocations) compiles them. First hoonc run bootstraps the Hoon
   compiler — slow.
3. **`kernels-*` crates need the jams at compile time** (`include_bytes!`),
   so the spike reads jam files at runtime instead of depending on those
   crates — also keeps the nockchain checkout untouched.

## Recommendation

_Pending timings; entry-point evidence strongly points GO._
