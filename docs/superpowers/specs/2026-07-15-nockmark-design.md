# Nockmark — Trustless STARK Proving Benchmark Registry (Design)

**Date:** 2026-07-15
**Author:** Tom de Bres (with Claude)
**Status:** Draft for review

## Purpose

Nockchain is a zkPoW chain: miners earn NOCK by generating STARK proofs of Nock
execution. The ecosystem's most-asked question — *"what hardware proves fastest,
and what does it earn?"* — has no trustworthy answer. Pools (NockPool, Nockbox,
nockhash) show members their own rates; profitability calculators are generic;
no cross-hardware benchmark registry exists (verified 2026-07-15).

Nockmark is that registry, built as a NockApp. Its differentiator over any
leaderboard a pool could publish: **submissions are cryptographically verified,
not self-reported.** A benchmark board that cannot lie, on the chain whose whole
thesis is verifiable computation.

## Users and demand

- **Miners** (the majority of Nockchain's active users today): choose hardware,
  compare rigs, estimate NOCK/day before buying.
- **Prospective miners**: the "is it worth it on my machine" question, answered
  with real data instead of Discord folklore.
- **Traders/analysts** (secondary): proving-economics data (cost per proof,
  difficulty trend) as market context.

Competitive threat is the pools, who hold raw member data. Nockmark's moat is
neutrality (cross-pool), method (standardized workload), and verification
(challenge-response, below) — none of which a pool's self-reported member stats
provide.

## Naming

- **Nockmark** — the product/registry (ecosystem register: NockBlocks, NockPool,
  Nockscan).
- **`tock`** — the bench-harness CLI (core-repo register: honk, hatch, hoonc).
  Miners run `tock bench`, results go to Nockmark.

## The trust model (core design idea)

A verified STARK proof shows work was **done**, not how **fast**. Self-reported
timings are gameable. Nockmark therefore uses **challenge-response benchmarking**:

1. `tock` requests a challenge from the registry → registry pokes
   `new-challenge`, stores `{nonce, issued-at}`, returns the nonce.
2. `tock` runs the standardized workload: produce **k** STARK proofs whose
   inputs incorporate the nonce (so they cannot be precomputed).
3. `tock` submits the proof artefacts + hardware descriptor.
4. The driver verifies every proof (cheap — that's the STARK asymmetry) and the
   registry computes the rate from **server-side elapsed time**
   (`submitted-at − issued-at`), not from any client-reported number.

Result: published rates are cryptographic **lower bounds** (network latency only
slows you down; nothing can speed you up). Hardware descriptors remain
self-reported and are labelled as such.

## Architecture

Three components, one repo:

### 1. Hoon kernel (the registry) — `hoon/`

Pure state machine, durable via the NockApp framework. State:

- `challenges`: map of nonce → `{issued-at, status}`
- `runs`: append-only list of verified runs:
  `{run-id, nonce, hardware, prover-version, k, elapsed-ms, proofs-per-sec,
    artefact-hashes, submitted-at, verify-mode (%full or %sampled)}`
- `params`: current k, workload version (bumped when the prover changes enough
  to break comparability; runs are tagged with workload version)

Pokes:
- `new-challenge` — mint a challenge
- `submit-run` — record a run (driver only pokes this after verification passes)
- `set-params` — admin: rotate workload version / k

Peeks:
- `leaderboard` — runs ranked by proofs-per-sec, filterable by hardware class
  and workload version
- `run` — full detail for one run
- `economics` — proofs-per-sec → est. NOCK/day given current difficulty and
  emission (difficulty cached in state, refreshed by the driver from the
  official Block Explorer API)

### 2. Rust driver (HTTP + verification) — `src/`

Scaffolded from the `nockup` **http-server** template.

- JSON API: `POST /challenge`, `POST /run` (multipart: proofs + metadata),
  `GET /leaderboard`, `GET /runs/:id`, `GET /economics`
- **Verification gate:** on `POST /run`, verify each submitted proof using the
  verifier from the nockchain codebase before poking `submit-run`. Invalid or
  nonce-mismatched proofs → reject, nothing enters state.
- Difficulty refresher: periodic task pulling network difficulty/emission from
  the Block Explorer API, poking a cache update.

### 3. `tock` bench harness — separate binary in the same repo

- Detects hardware (CPU model, cores, RAM, GPU if relevant) — best-effort,
  clearly labelled self-reported.
- Fetches a challenge, runs the standardized proving workload k times via the
  real nockchain prover, streams progress, submits the bundle.
- Also runs fully offline (`tock bench --local`) printing results without
  submitting — useful standalone even if the registry never gains users.

## Standardized workload (spike required — see M0)

The workload must exercise the same STARK prover mainnet mining uses, on a
fixed, nonce-parameterized input, so runs are comparable across machines.
Candidate entry points in `nockchain/`: the miner kernel's proving path and
`zkvm-jetpack`. **M0 is a time-boxed spike (2–3 days) to find the cleanest way
to (a) invoke prove(input) directly and (b) invoke verify(proof) directly.**
If the prover cannot be invoked outside full-node mining context within the
spike box, fallback: drive a fakenet miner instance with a nonce-derived
genesis/candidate and measure block-proof production — uglier but workable.

## Milestones (each has standalone value)

- **M0 — Spike:** prove/verify entry points identified; `prove(nonce-input)` and
  `verify(proof)` runnable from a standalone Rust binary. Go/no-go gate.
- **M1 — `tock --local`:** harness benches the local machine against the real
  prover, prints proofs/sec. (Publishable artefact on its own: "first public
  Nockchain proving benchmarks" write-up.)
- **M2 — Registry live:** kernel + driver deployed on a VPS, seeded with runs
  from Tom's hardware + 2–3 cloud instance types; public read API + minimal
  static leaderboard page (http-static or plain HTML on the driver).
- **M3 — Trustless submissions:** challenge-response flow + verification gate
  on; `tock bench` submits end-to-end; announce to mining community
  (Discord/Telegram); economics peek live.

Post-v1 (explicitly out of scope now): AI-PoW pearl workloads if PR #137 lands,
GPU-specific workload variants, historical difficulty charts, signed hardware
attestation.

## Dependencies and stack

- `nockchain` repo crates (local fork at `../nockchain`): `nockapp` (framework),
  prover/verifier path found in M0, `hoonc` for kernel compilation.
- `nockup` for scaffolding (note: alpha; community template-hardening PRs
  pending — expect sharp edges, file issues upstream when hit).
- Rust (driver + harness), Hoon (kernel). Tom is new to both: kernel Hoon stays
  minimal (maps, lists, no clever type golf); driver leans on the template.
- Official Block Explorer API for difficulty/emission data.

## Risks

| Risk | Mitigation |
|------|------------|
| NockApp framework is developer-alpha; interfaces churn | Pin to a known-good nockchain commit; upgrade deliberately; file upstream issues when hit (dogfooding-driven contributions) |
| Prover not cleanly invokable standalone | M0 spike is a go/no-go gate with a fakenet fallback; if both fail, stop and rechoose |
| Pools publish their own stats | Ship M2 fast; neutrality + verification remain the moat |
| Cold start (nobody submits) | M1/M2 are valuable with zero external users (own benchmarks + write-up); community announce only at M3 |
| Workload invalidated by prover changes | Workload versioning in kernel params; leaderboards scoped per workload version |
| Hardware descriptors can be lied about | Documented as self-reported; rates themselves are verified — the load-bearing number is trustless |
| Hoon learning cliff | Kernel is deliberately the simplest component (registry CRUD); start from template kernel and grow |

## Testing

- Kernel: unit-test pokes/peeks via the framework's test harness
  (`nockapp_tests.bzl` pattern in upstream repo as reference).
- Driver: integration test — synthetic challenge → known-good proof fixture →
  run appears in leaderboard; known-bad proof → rejected.
- Harness: golden-path e2e against a local driver instance.
- Anti-cheat test: replayed/stale nonce and precomputed-proof submissions must
  be rejected.

## Success criteria

- M1: real proofs/sec numbers for ≥3 hardware configs, reproducible ±10%.
- M3: an external miner runs `tock bench` and lands a verified leaderboard
  entry with zero hand-holding.
- Ecosystem signal: linked from at least one community resource (mining guide,
  pool docs, or Discord pin).

## Open questions (to resolve in M0, not blockers for approval)

1. Exact prove/verify entry points and their input shapes (spike deliverable).
2. k and workload size: long enough to swamp network latency (target: minutes
   of proving), short enough that hobbyists bother.
3. Whether the registry VPS verifies proofs fast enough at k=chosen; if heavy,
   verify a random sample of the k proofs instead (still binds the rate).
4. Hosting: single VPS is fine for v1; domain (nockmark.xyz?) to be registered.
