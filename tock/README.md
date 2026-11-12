# tock — Nockmark bench harness

Benchmarks a machine at Nockchain mining: the ZK lottery (`zk-bench`: Tip5
hashes of proof object 0, winners proved through the **real mainnet miner
kernel**) and AI-PoW (`ai-bench`). Run standalone — no node, no networking
beyond the registry. Born as the M0 spike
(`docs/superpowers/specs/2026-07-15-m0-spike-findings.md`).

See also: [M2 Registry Deployment Runbook](../docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md)

## Prerequisites

- Nothing for the Rust build: the nockchain crates come by git, pinned to one
  `rev` in `Cargo.toml` (every nockchain line carries the same rev; bump them
  together with `NOCKCHAIN_PIN` in `src/miner.rs` on a re-pin). Cargo fetches
  them once into `~/.cargo/git`, so any checkout or worktree builds as-is.
- A nockchain checkout at that same commit only to compile the kernel jams
  below (`$NOCKCHAIN` here; anywhere on disk).
- `protoc` on PATH (`brew install protobuf` / `apt install protobuf-compiler`).
  The AI track's canonical-MoE client links `ai-pow-miner`'s `node` feature —
  the canonical grind/prove path lives behind it — which pulls `nockapp-grpc`.
- The pinned nightly toolchain (`nightly-2026-04-03`). On this machine it
  lives at `~/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin`
  (installed from static.rust-lang.org tarballs; there is no rustup here).

```sh
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
```

## One-time: compile the kernel jams

The prover/verifier are Hoon kernels, compiled with hoonc (built once in the
nockchain repo). hoonc quirks:

- It writes the output jam into the **current directory** (ignores the
  directory part of `--output`).
- `--new` needs an empty data dir, and a data dir cannot be reused: use a
  fresh one per kernel and per attempt.
- It parses every file under the tree it is given. Since `ed1edfcc` that
  includes deliberately unparseable fixtures in `hoon/tests/compiler-errors/`,
  which stop it **without a jam and with exit status 0**. Compile from a copy
  of the tree without them (`-L`: `hoon/common/hoon.hoon` is a symlink), and
  check the jam exists afterwards.

```sh
cd "$NOCKCHAIN"   # checked out at the rev pinned in Cargo.toml
cargo build --release -p hoonc
B=$(mktemp -d) && cp -RL hoon "$B/hoon" && rm -rf "$B/hoon/tests/compiler-errors"
cd "$B"
# miner kernel (the prover — the kernel mainnet mining uses), ~8 min at ed1edfcc
"$NOCKCHAIN/target/release/hoonc" --new --data-dir "$B/data-miner" \
  --output miner.jam hoon/apps/dumbnet/miner.hoon hoon
# roswell kernel (test harness exposing %verify-proof), ~30 min at ed1edfcc
"$NOCKCHAIN/target/release/hoonc" --new --data-dir "$B/data-roswell" \
  --output roswell.jam hoon/apps/roswell/roswell.hoon hoon
ls -l miner.jam roswell.jam && mv miner.jam roswell.jam <nockmark>/tock/assets/
# the registry kernel (hoon/registry.hoon), same treatment, from the repo root:
NOCKCHAIN="$NOCKCHAIN" scripts/build-registry-jam.sh
```

`deploy/assets/` holds the committed `roswell.jam` and `registry.jam` the
deployed registry runs; copy fresh builds there on a re-pin.

## ZK mining: `zk-bench`

Since block 147,500 (Anthropos, ZK proof version 5) a ZK mining attempt is one
Tip5 hash of proof object 0 for a nonce; a full STARK is built only for a nonce
whose hash meets the target, and is bound to it. `zk-bench` does that:

1. calibrate (unless `--attempts`): ~2 s of hashing, then one throwaway `%5`
   proof, to size the tier at twice this machine's proving time;
2. with `--submit`, mint a challenge at that tier (block commitment, target,
   k); otherwise grind a local dev commitment;
3. grind nonces with upstream's own `v5_pow_digest` on every core (or, with
   `--gpu`, upstream's CUDA search) until k meet the target;
4. prove each winner through the miner kernel with
   `[%5 commitment nonce target 64]` — the kernel re-checks the digest and
   refuses a loser;
5. with `--submit`, send the proofs; the registry verifies and ranks them.

```sh
cargo build --release
./target/release/tock zk-bench --kernel assets/miner.jam                 # local
./target/release/tock zk-bench --kernel assets/miner.jam -k 2 --attempts 1048576
./target/release/tock zk-bench --kernel assets/miner.jam --submit https://nockmark.xyz
```

`--threads N` caps the CPU grind (default: every core). Proving is one kernel,
one proof at a time, whatever it says: the tier is sized against that, so the
registry's window recovers the same 2/3 of the true hash rate on every host.

GPU (NVIDIA, CUDA 12.8+): build with the `zk-gpu` feature and your card's
arch, then add `--gpu`. Only the search moves to the device; every winner is
re-checked on the CPU and proved there.

```sh
ZK_POW_CUDA_ARCH=86 cargo build --release --features zk-gpu   # 89 Ada, 120 Blackwell
./target/release/tock zk-bench --kernel assets/miner.jam --gpu --submit https://nockmark.xyz
```

`scripts/gpu_pod.py` (`up`, `push`, `prep-zk`, `run`, `down`) is the RunPod
recipe. `prep-zk` runs `build-zk` and `miner-jam` side by side: `miner-jam`
builds the jam on the pod with hoonc at the pinned rev rather than uploading
18 MB through RunPod's PTY proxy. The CUDA arch follows the card
(`NOCKMARK_GPU_TYPE`). Each `NOCKMARK_POD_STATE` file is its own pod slot, so
two cards, or two sessions, can run at once without touching each other's
pods.

The nockchain `kernels-open-miner` crate, which `zk-pow-miner` depends on,
embeds a miner jam at build time and fails the build without one. Nothing here
uses that embedded kernel (tock reads `--kernel` at run time), so
`Cargo.toml` patches it to an empty stub (`stub/kernels-open-miner`).

## AI track: dense-peak

`ai-bench --statement dense-peak` benchmarks mainnet's production AI shape
(`ai_pow_miner::PEAK_PRODUCTION_PARAMS`: m=4096, k=8192, n=32768, r=512, 16×16
tiles) through upstream's own code: `PreparedReferenceDenseTemplate`, the CPU
`CpuSearchBackend::search_dense` (or, with `--features gpu --gpu`, the #159
peak kernel via `MultiGpuPeakSearchBackend::search_peak`), and
`prove_with_rules(Hardened)`, with the first win's prover setup reused for the
rest. Each win is submitted as the jammed `%ai-pow` artifact.

```sh
cargo build --release
./target/release/tock ai-bench --statement dense-peak -k 1          # local, self-calibrated
./target/release/tock ai-bench --statement dense-peak --submit https://nockmark.xyz
```

An attempt is one 16×16 tile (F = 2²¹ MAC-equivalents); an extranonce holds
2¹⁹ of them, and the attempt index is `extranonce · 2^19 + tile`. Measured on
an M1 Max (10 cores, 32 GB) at ed1edfcc: grind 136–142 k tiles/s (2.9×10¹¹
MAC-equivalents/s, ~2.5×10¹¹ including the per-extranonce transcript), 0.6 s
per transcript, certificates 72–76 s cold and 61–64 s warm, peak RSS 12 GB
(15 GB holding the reusable setup), artifacts ~147 KB. The calibration proves
one throwaway certificate before minting, so budget ~2 minutes and that
memory before the run starts.

## Complete-proof bench (pre-Anthropos `%3`)

`tock bench` times complete `%3` proofs — the ZK mining unit until block
147,500. It is still a fair CPU proving benchmark, but no longer a mining
rate, and the registry no longer accepts its submissions (`--submit` exits
with a pointer to `zk-bench`).

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

## Submitting to the public registry

`zk-bench --submit https://nockmark.xyz` (above) is the ZK path; `ai-bench
--submit` the AI one. The registry supplies k, the tier's target and the
commitment, and `zk-bench` checks the commitment and target against its own
derivation before grinding. The published rate is computed **server-side**
from the challenge-issue → submission window, so it is a verified lower bound
on your machine's hash rate: nothing you report can inflate it. Hardware
strings are self-reported and labelled as such.
