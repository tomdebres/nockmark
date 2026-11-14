# Nockmark

Verified mining benchmarks for [Nockchain](https://github.com/zorp-corp/nockchain).

Nockchain is mined two ways — a ZK proof-of-work and an AI proof-of-work —
and "what hardware mines fastest?" decides whether mining pays. Every public
answer so far has been self-reported. Nockmark is a registry where the
numbers are verified instead: your machine does real mining work against a
server-issued challenge, the registry checks every proof, and your rate is
computed from the server's own clock. The published number is a lower bound
that nobody can inflate — including you.

- Leaderboard: https://nockmark.xyz/ (JSON at `/leaderboard?track=zk` and `?track=ai`)
- Earnings estimates (every input published): https://nockmark.xyz/economics
- Write-up, which puzzle each card should mine (RTX 3090, 4090, 5090, M1 Max):
  [docs/writeups/2026-11-13-which-puzzle-should-this-card-mine.md](docs/writeups/2026-11-13-which-puzzle-should-this-card-mine.md)

## Get on the board

```sh
cd tock
./target/release/tock zk-bench --kernel assets/miner.jam \
  --submit https://nockmark.xyz
```

Build instructions (pinned toolchain, kernel jams) are in
[tock/README.md](tock/README.md); `tock/setup-bench.sh` provisions a fresh
Ubuntu box end-to-end in about 15 minutes.

`zk-bench` measures the machine first — ~2 s of hashing plus one throwaway
`%5` proof (~21 s on an M1 Max) — and asks the registry for a difficulty
**tier**: expected Tip5 hashes per win, sized so each win grinds for twice that
proof. It then grinds nonces for the challenge's block commitment with
upstream's own V5 digest, proves k winners (k = 16) through the mainnet
miner kernel, and submits them. On an M1 Max that is about 20 minutes.

On an NVIDIA GPU, `--gpu` grinds on upstream's open-source CUDA V5 search. It
needs a build with the `zk-gpu` feature, which requires `nvcc`, and the arch
of your card (86 for Ampere, 89 for Ada, 120 for Blackwell):

```sh
ZK_POW_CUDA_ARCH=86 cargo build --release --features zk-gpu
./target/release/tock zk-bench --kernel assets/miner.jam --gpu \
  --submit https://nockmark.xyz
```

Only the search moves to the device: every winner it reports is re-checked
against the CPU oracle and proved on the CPU, then verified by the registry
like any other.

There is a second track for Logos's AI-PoW puzzle (INT8 tiled matmul +
recursive STARK certificate — the first nockchain mining path with an
open-source miner). Same trustless model:

```sh
./target/release/tock ai-bench --submit https://nockmark.xyz
```

That track benchmarks three statements, on one board. The default is the
single-tile `dense` benchmark; `--statement canonical-moe` benchmarks the
canonical MoE block the gateway-free reference miner submits; and
`--statement dense-peak` benchmarks mainnet's production AI shape, the one
upstream's peak CUDA kernel grinds (m=4096, k=8192, n=32768, 16×16 tiles):

```sh
./target/release/tock ai-bench --statement canonical-moe --submit https://nockmark.xyz
./target/release/tock ai-bench --statement dense-peak --submit https://nockmark.xyz
```

A dense-peak certificate is heavy: 72–76 s and 12–15 GB of RAM on an M1 Max
(61–64 s once the prover setup is reused across wins). Its CPU grind is fast —
about 140,000 tiles/s, 2.9×10¹¹ MAC-equivalents/s on that machine — because one
transcript serves 2¹⁹ tiles. Dense-peak challenges therefore live 3 h instead
of 1 h.

On an NVIDIA GPU, `--gpu` grinds the canonical-MoE statement on upstream's
CUDA backend, and the dense-peak statement on upstream's #159 peak kernel —
which runs only on compute capability 12.0 (RTX 5090 class); upstream's kernel
refuses other cards. It needs a build with the `gpu` feature, which requires `nvcc`
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

`ai-bench` measures your machine first — ~2 s of grinding plus one throwaway
certificate prove (~7-26 s) — and asks the registry for a difficulty **tier**
sized so each win grinds for twice that measured prove time. Otherwise fast
hardware finishes grinding in milliseconds and its ranked rate is nothing but
the fixed certificate-proving cost inside the measured window. Sizing against
your own prove time is what makes the comparison fair across machines: every
host's ranked rate then recovers the same fraction of its true grind
throughput, instead of one that depends on whichever CPU did the proving.
Both clients prove each win while the grind goes on, so the window is the grind
plus one certificate and that fraction is about 97 % at k = 16 (about 2/3
for a run made the old way, grind first and prove after, which the board marks
`serial`). Pass
`--attempts <n>` to pick the tier yourself (expected grind attempts per win) or
`--target <hex>` to pin the difficulty outright; either skips the calibration,
including the prove.
Choosing a tier cannot inflate a rate: an easy one earns less credit against
that same fixed proving cost, and a hard one has to actually be ground out,
attempt by verified attempt.

## How submissions are verified

ZK track (`zk-bench`):

1. `POST /challenge?track=zk&attempts=N` returns a kernel-minted nonce, the
   block commitment derived from it (`blake3("nockmark-zk-v5" ‖ nonce)`
   written in base p), and the target for the tier. Nothing can be
   precomputed.
2. `tock` grinds nonces against that commitment and target, then proves k
   winners as `%5` proofs (pow-len 64, miner kernel pinned at nockchain
   `ed1edfcc`).
3. `POST /run?track=zk` submits the proofs. For each, the registry re-derives
   the Tip5 hash from the proof's own object 0 the way consensus does, checks
   the version (`%5`), the commitment, the length and the target, then
   verifies the STARK twice: upstream's native verifier and the consensus Hoon
   verifier. One nonce counts once, whatever the proof bytes. The rate is
   k × the tier's expected hashes, over `submitted_at − issued_at` on the
   server's own clock. Client-reported timings are displayed but never
   ranked.

The `%3` complete-proof submissions of earlier versions are retired: the
registry answers `410 Gone`.

AI-track wins are verified the same way, against mainnet's current rules:
`Hardened` proofs (block 154,500 on) and, for canonical MoE, exact routing
multiplicity (block 170,000 on). A legacy certificate is rejected. Dense-peak
wins arrive as the `%ai-pow` block artifact a node receives and go through
upstream's consensus verifier itself, after the registry pins the statement
(shape, tile, and this challenge's header).

The board shows ranked runs only. Runs under retired rules (complete proofs
before Anthropos, AI certificates under earlier pins, marked `rules: legacy`)
and runs superseded by the pipelined client stay in the JSON at
`/leaderboard`, unranked.

What is *not* verified: the hardware descriptor and `prover_version` are
self-reported. Anything rate-related is.

## Layout

- `tock/` — bench harness and submit client (Rust; grinds with upstream's V5
  digest or CUDA search, proves with the mainnet miner kernel)
- `registry/` — the registry server (Rust/axum; verification, leaderboard,
  economics)
- `hoon/` — the registry kernel (challenge minting, pre-Anthropos runs),
  compiled against the pinned nockchain tree by `scripts/build-registry-jam.sh`
- `bench-results/` — raw result JSONs behind the write-up
- `docs/` — write-up, design notes, deploy runbook; the M7 plan
  (`docs/superpowers/specs/2026-11-11-m7-anthropos.md`) records how the board
  moved to the Anthropos rules
- `deploy/`, `Dockerfile`, `railway.json` — the deployed instance

The nockchain crates are git dependencies pinned to one commit in
`tock/Cargo.toml` and `registry/Cargo.toml`, so any checkout builds without a
sibling nockchain tree; toolchain and kernel-jam steps are in `tock/README.md`.

`./verify --fast` (syntax checks, under a second) and `./verify` (adds the
tock and registry test suites) are the checks a change must pass before it
opens a PR.

Runs from hardware not yet on the board are the most useful contribution —
especially datacentre GPUs (H100, B200), which the write-up could not measure.
