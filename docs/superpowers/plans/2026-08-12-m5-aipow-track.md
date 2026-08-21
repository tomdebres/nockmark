# M5 — AI-PoW benchmark track

**Goal:** a second leaderboard dimension benchmarking Logos's AI-PoW puzzle
(INT8 tiled matmul + recursive STARK certificate), with the same trustless
property as the ZK track: every number computed server-side from
certificates the server verified, of a challenge the server issued.

**Status:** design. Approved by Tom 2026-08-11. Implementation follows the
M3 subagent-driven pattern.

## Why now

Logos (protocol 016, activation height 126,000) adds AI-PoW as a second
mining puzzle with **open-source CPU miner code in-tree** — the first time
any nockchain mining path is fully benchmarkable without reverse
engineering. The CUDA backend is a stub today; when it lands, this track
becomes the GPU leaderboard. Until then it ships labeled **"CPU
reference."**

## Measured facts the design rests on (M1 Max, 2026-08-11)

| Quantity | Value | Source |
|---|---|---|
| Grind rate (CPU) | ~1.7 ms/attempt (≈594/s on M2 Max) | upstream canonical_mining_costs |
| Certificate prove | 24.4 s wall, 4.3 GB peak RSS | release-gate script, this machine |
| Certificate size | 146 KB (consensus cap 150 KB) | same |
| Core compact verify | **~1 ms** (`l2_compact_verify_ms=1`) | upstream latency test, this machine |
| Consensus exchange rate | 25.75e9 MAC-equivalents / ZK attempt | tx-engine.hoon |
| Attempt work factor F | 2^16 MAC-equivalents (canonical 8×1024×8 tile) | ai-pow params |

**Task 1 spike CLOSED (2026-08-12), gate passed ~25×:** full registry-path
verify (submission decode → matrix re-synthesis from challenge → statement
re-derivation → canonical program commitment → compact STARK verify) is
**79 ms mean/cert** on the M1 Max — k=8 ≈ 0.6 s per submission, inline like
the ZK track. Tampered cert and wrong-nonce both rejected (CapMismatch /
PublicInputMismatch). Cost is dominated by the canonical-program build,
not the ~1 ms crypto core. Two verifier-boot objects: a 40-byte pinned
key digest (config constant) and a ~1.0 GB `AiPowCompactBatchVerifierContext`
(~730 MB resident; 505 ms to load from a pre-serialized blob — generate
offline per NOCKCHAIN_PIN, ship on the Railway volume). Steady-state
verify RAM ~1 GB — fine; avoid build-by-proving at boot (4 GB peak).
**Route decision:** the registry verifies via the `zk_bridge` statement
path (`verify_ai_pow_full_matmul_production_statement` +
`verify_compact_batch_recursive_certificate_with_context`) with deps only
`ai-pow (zk, parallel)` + `ai-pow-zk` — NOT the `certificate_noun.rs`
noun/jam entrypoints, which are Pearl-merge-specific, `pub(crate)`-walled,
and drag in the whole node feature tree. Reference implementation:
`aipow-spike/src/verifier.rs::verify_submission`.

## Design

### Challenge (registry)

`POST /challenge?track=ai` returns:

- `challenge` — 32 bytes (hex), minted by the registry kernel like the ZK
  nonce; plays the `block_commitment` role in `block_state(commitment,
  nonce)`. The kernel treats it as an opaque challenge id; no kernel
  changes beyond a track tag on the mint.
- `target` — benchmark jackpot target `T_b` (hex, 32 bytes). Fixed by the
  server, NOT chain difficulty: calibrated so a reference CPU finds a win
  every ~15–20 s of grinding. CORRECTED post-live-fire (2026-08-13): the
  mine/verify comparison is `hash ≤ T_b` **little-endian, unscaled** —
  T_b is the effective threshold, so expected attempts/win =
  2^256/(T_b+1), NOT 2^256/(T_b·F). The first deployed target (BE-encoded
  2^243, reads as 2^11 LE ⇒ 2^229 attempts/win) ground for 23 h without a
  win; production value is LE 2^243 (`…0800`), ~8191 attempts/win.
- `k` — wins required (start k = 8, same as ZK track).
- `params` — the canonical single-tile shape (m=8, k=1024, n=8, r=64,
  tile=8), fixed by protocol version; clients must use exactly these.
- `matrix_seed` — the challenge itself: matrices are
  `synth_matrices(challenge, params)`, so the workload is challenge-fresh
  and the server can re-derive commitments.

### Client (tock)

`tock bench --track ai --submit …`, embedding the ai-pow crates as
libraries (path deps, same pattern as nockapp/zkvm-jetpack):

1. Fetch challenge; synth matrices from it.
2. Grind extranonces **0, 1, 2, … strictly ascending** (the fixed rule —
   the AI analog of the ZK track's nonce-derivation rule) via
   `CpuSearchBackend`, recording the grind window client-side.
3. On each jackpot win, prove a compact certificate
   (`prove_ai_pow_compact_recursive_certificate_with_prover_cache`).
   Proving is OUTSIDE the measured window (fixed ~24 s/win overhead, not
   throughput).
4. Submit after k wins: `{challenge, wins: [{extranonce, cert_b64}] × k,
   hardware, prover_version, grind_elapsed_ms}`.

### Server (registry)

`POST /run?track=ai`:

1. Challenge exists, unexpired, unused (kernel state, as today).
2. Extranonces strictly ascending, no duplicates.
3. For each win: re-derive the statement from `(challenge, extranonce)` —
   κ, commitments (matrices re-synthesized from the challenge), pow_key,
   canonical program — then verify the certificate against it and check
   `HASH_JACKPOT ≤ T_b`. Never trust any cert-carried metadata
   (`derive_ai_pow_statement` path; plain `MatmulProof` is rejected).
4. Rate = measured from **grind semantics**: the server window runs from
   challenge issue to submission, minus k × (certificate prove allowance).
   Simpler and more honest v1: server window as-is, and the board reports
   BOTH `grind_mac_per_sec` (k · 2^256/T_b / client grind window,
   display-only, like self_reported_pps) and `verified_mac_per_sec_lb`
   (same numerator over the full server window — a hard lower bound, the
   ranked number). Exactly the ZK track's verified/self-reported split.
5. Store as a run with `track: ai`; `/leaderboard?track=ai`; board page
   grows a second table.

### Board semantics

- Ranked: `verified_mac_per_sec_lb` (lower bound, server window).
- Context: ZK-attempt-equiv/s = MAC/s ÷ 25.75e9; network share once a
  network AI rate source exists.
- Confidence: k wins is Poisson — show ±1σ (≈ rate/√k) on both figures.
- Era: prover_version = NOCKCHAIN_PIN (shared constant).
- Label: **CPU reference** until CUDA kernels exist upstream.

### Explicitly out of scope for M5

- GPU mining (upstream stub).
- Chain-difficulty-linked AI economics (needs an AI-difficulty source;
  revisit with task 8's nockchain-api node).
- Multi-tile / full-matmul statements (fail closed upstream).
- Registry kernel hoon changes beyond the track tag on challenges/runs.

## Tasks

1. **Verify-path spike**: registry-side harness calling the node verify
   path on a cert from the release-gate test; measure end-to-end ms.
   Gate: < 2 s/cert. Also confirms which setup tables (if any) the
   verify path needs and their size — deploy blocker if multi-GB.
2. **tock: ai module** — synth + grind + prove + submit, `--track ai`.
3. **Registry: ai verify + endpoints** — challenge track tag, verify
   flow, leaderboard filter, board table. Target calibration constant.
4. **Tests** — unit (statement re-derivation mismatch cases, extranonce
   rules) + integration (challenge→grind→prove→submit→board with a
   generous T_b so the test grinds in seconds).
5. **Dockerfile/deploy** — protoc already handled; add any setup-table
   provisioning from Task 1's findings; deploy + live-fire M1 run.
6. **Docs** — /api additions, README section, board labeling.

Risks: recursion-fork proof format marked "subject to change" (pin
insulates us); prove RAM (4.3 GB) × concurrent submissions on the client
only — server never proves; Railway RAM for verify path TBD in Task 1.

## M6 Phase A (2026-08-14)

Re-pinned nockchain 1372f270 → **c8d6b13e** (fork branch
`m6-pearl-v3-pin`), picking up the production Pearl V3 statement.

- **V3 seed change**: `canonical_noise_seeds_from_matrix_commitments`
  now salts the noise seeds with the matmul shape — two new trailing
  args `(m, n)`; we pass `AI_PARAMS.m, AI_PARAMS.n`. Only signature
  change in our call graph.
- **AIR hardening**: upstream fixed the urange8 lookup constraint
  between the pins. Era-1 runs (prover_version 1372f270) were verified
  under the older constraint system; workload cost is unchanged, so
  cross-era rate comparisons stand. Caveat documented on /api.
- **Verifier-context rotation**: the compact context encodes the AIR,
  so the old 1 GB blob must never be loaded at the new pin. The context
  path is now pin-scoped (`aipow-verifier-context-<PIN>.bin`); a re-pin
  misses the file and rebuilds by proving (~25 s) on the first
  submission — rotation is automatic, no volume surgery.
- **No ZK kernel rebuild**: zkvm-jetpack/roswell/hoon consensus is
  unchanged since 1372f270; existing jams carry over.

## M6 Phase B1 (2026-08-21) — canonical-MoE statement, CPU-validated

The AI track now benchmarks the statement mainnet GPU miners actually run,
alongside the M5 dense one, on **one leaderboard**. Not a third board:
`ai_pow::difficulty` invariant D2 says expected MAC-equivalents per block is
`2^256/T` *independent of the tile shape the miner picked*, so
MAC-equivalents/s is the unit in which the two are commensurable — the same
unit consensus uses for AI fork-choice weight. Ranking them apart would invent
a distinction consensus does not make.

- **Shape** (upstream `CANONICAL_MATMUL_PARAMS` + `CANONICAL_HW/E/TOP_K`,
  `crates/ai-pow-miner/src/run.rs`): m=64, k=1024, n=64, r=64, tile=8, hw=8,
  e=2, top_k=1 — an 8×8 opened tile, 2 experts, top-1 routing.
- **Work factor F = 2^16**, derived not guessed:
  `dot_product_length(1024, 64) = 1024 − (1024 mod 64) = 1024`, so
  `F = h·w·dot = 8·8·1024 = 65 536`. `tock::aipow_moe::moe_shape_work_factor`
  recomputes it from `ai_pow::difficulty`, and unit tests pin it against BOTH
  `PearlMiningConfig::shape_work_factor` (the miner's route) and
  `PearlPublicProofParams::difficulty_adjustment_factor` (the verifier's), so a
  re-pin that moves the shape breaks loudly.
- **Threshold semantics: SCALED, unlike the dense path.**
  `canonical_grind_threshold(T) = effective_jackpot_threshold(T, F) = T·F`, and
  equals the consensus verifier's `nockchain_adjusted_target(T)`. The dense
  benchmark compares `jackpot ≤ T` raw. So the same 32 bytes mean `F = 2^16`
  times more grinding on the dense statement, and expected attempts per win is
  `2^256/(T·F+1)` here. A test pins the gap explicitly. Corollary: an all-FF
  MoE target is not "everything wins", it is a fail-closed ERROR
  (`tock::aipow_moe::max_moe_target` is the loosest usable one).
- **Grind rule `canonical-ordinal-v3`** (`AI_MOE_NONCE_RULE`), kept clearly
  apart from the dense `extranonce-le8-v1`: the attempt selector is a u32
  ordinal that offsets the synthetic Pearl header timestamp — there is no
  8-byte nonce anywhere on this path (upstream asserts the nonce-folded jackpot
  key must NOT appear).
- **Target calibration**: measured 0.2947 ms/attempt (3394 attempts/s, M1 Max,
  one thread, release — `tock::aipow_moe::tests::moe_grind_rate`, the peer of
  upstream's `canonical_mining_costs`). 2^16 = 65 536 attempts ≈ 19.3 s, so
  `T_b = Θ/F = 2^240/2^16 = 2^224` (byte 28 = 0x01). Env
  `NOCKMARK_AI_MOE_TARGET`; deliberately a separate variable from the dense
  `NOCKMARK_AI_TARGET` because the two are not interchangeable.
- **Verify route**: `zk_bridge::verify_pearl_moe_compact_recursive_certificate`
  — the public proof half of the consensus accept path
  (`certificate_noun::verify_decoded_ai_pow_pearl_merge_compact_moe_artifact_…`,
  which is `pub(crate)` and takes the noun/jam wire form). The registry
  reassembles the rest of that recipe around it: `verify_pearl_moe_compatible_work`
  (envelope + routing binding + the `jackpot ≤ T·F` gate) and the
  `pis.hash_jackpot == statement.hash_jackpot` binding. **Stronger than the
  node's**: the node authenticates a submitted statement via Pearl aux
  inclusion; we never receive one — the whole statement is re-derived from
  `(challenge, ordinal)` via `canonical_moe_statement_parts`, and the blob
  carries only the certificate and its Layer-0 public inputs.
- **Verifier context**: a second, separate, lazily-built, pin-scoped setup
  (`aipow-moe-verifier-context-<PIN>.bin`, ~1.0 GB, ~27 s to build by proving).
  Built through `prove_pearl_moe_compact_recursive_certificate` because
  `prove_canonical_moe_block_at` discards its context and the variant that
  returns it is `#[cfg(test)] pub(crate)`; the assembled inputs are unit-tested
  against the crate's own ticket AND the build self-verifies its own
  certificate before persisting. Volume cost is now ~2 GB of contexts.
- **Wire**: `statement` (`"dense"` | `"canonical-moe"`) on the challenge, the
  submission, the stored run and the board row. Absent = dense everywhere, so
  M5 clients and M5 `aipow-track.jsonl` rows are unchanged and replay
  identically.
- **Build cost**: `ai-pow-miner`'s `canonical`/`run` modules are behind its
  `node` feature, so both crates now link the gRPC/node tree. tock therefore
  needs `protoc` at build time — added to `tock/setup-bench.sh`; the registry
  Dockerfile already installed `protobuf-compiler`.
- **Out of scope, Phase B2**: the CUDA backend. Nothing here touches it; the
  grind goes through `PreparedCanonicalMoeTemplate` + the scalar-oracle
  recheck, which is exactly the seam a GPU `SearchBackend` slots into.
