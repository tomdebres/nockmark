# M7 — catching up with Anthropos

Approved by Tom 2026-11-11 (full scope; nockscan primary, NockBlocks fallback).
Research: upstream master `ed1edfcc` (2026-10-26), 20 commits past our pin
`c8d6b13e`; live chain at height 181,400 on 2026-11-11.

## What changed under the board

| Height | Upgrade | What it means for nockmark |
|---|---|---|
| 147,500 | **Anthropos**, protocol 017, ZK proof `%5` | A ZK attempt is now one Tip5 hash of proof object 0 (`[%puzzle commitment nonce len p]`, `p` a hash over 64 sponge belts — no STARK). A full proof is built only for a winning nonce, and is bound to it. **The ZK board's unit, complete proofs/sec, stopped being the mining unit.** Block allocation flipped to ~30% ZK (500 s ideal) / ~70% AI (214 s). |
| 154,500 | ASERT reset; AI `ProofRules::Hardened` | Exchange rate 14,285,714 MAC-equivalents per Tip5 hash. AI certificates proved under the legacy program (ours) are no longer admissible on mainnet. |
| 170,000 | ASERT reset (ZK ref 100 GH/s, AI 20 EMAC/s); exact MoE routing multiplicity | Our `canonical-moe` shape (e=2, top_k=1) should satisfy it; unverified until the re-pin runs. |
| 170,501 | Eon 4 | Reward 2048 → 1536 NOCK; miner share 1228.8. |

Upstream also merged an **open-source CUDA V5 ZK miner** (#192: device does the
object-0 digest and target compare, a CPU Serf proves the winner; PR figures
5090 ≈ 18 MH/s, 4090 ≈ 14.5 MH/s) and made the **dense peak CUDA kernel** the
production AI path (#159: m=4096 k=8192 n=32768, 5090 ≈ 335 TMAC/s). Gemma
inference mining (#164) is still an open PR.

Network on 2026-11-11 (nockscan `mining-metrics`): ZK ≈ 1.9e11 Tip5 hashes/s,
AI ≈ 1.5e19 MAC/s; AI is ~84% of the work.

## Steps

1. **Economics** — done (PR #32, deployed 2026-11-11). Per-track units and constants: ZK 500 s, AI 214 s,
   reward read from the chain (× 0.8), exchange rate 14,285,714. nockscan
   `mining-metrics` is the primary source (both difficulties, reward, the
   chain's own exchange rate, no key); NockBlocks RPC is the fallback. ZK
   proofs/sec rows carry no estimate. AI rows get NOCK/day and USD/day for the
   first time (from the unmerged August `econ-roi` work, adapted).
2. **Re-pin to `ed1edfcc`** — done (PR #33, 2026-11-12). Git deps make
   it a `rev` bump plus the
   `ai_pow_miner::canonical` → `reference` renames (`canonical_*` →
   `reference_*`, `PreparedCanonicalMoeTemplate` → `PreparedReferenceMoeTemplate`,
   `canonical_grind_threshold` → `reference_grind_threshold`). Rebuild miner,
   roswell and registry jams; the AI verifier context gains a `rules` field, so
   cached contexts rebuild (already pin-scoped). Bump `NOCKCHAIN_PIN`, the
   Dockerfile and `scripts/gpu_pod.py`; drop the Dockerfile's now-unused
   nockchain clone. Check the 170,000 multiplicity rule against our MoE certs.
   Outcome: the registry boots only Hardened verifier contexts and admits MoE
   wins under consensus-batch limits (exact routing multiplicity);
   `registry/tests/aipow_mainnet_rules.rs` runs a real canonical-MoE
   certificate through upstream's consensus verifier under `ConsensusBatch`
   (passes; e=2, top_k=1 routes each token once) and shows a Legacy
   certificate rejected. AI rows carry a server-attested `rules`
   (`hardened`, or `legacy` for every earlier row); ZK rows carry
   `rules: pre-anthropos`. Ranking stays all-rows until step 3 (see
   Decisions).
3. **ZK v5 track** — done (PR #34, 2026-11-12). Built as designed below. Outcome:
   - `tock zk-bench` grinds with upstream's own `v5_pow_digest`/`digest_le`
     (`zk-pow-miner`, its embedded-kernel crate patched to an empty stub) or,
     with `--features zk-gpu --gpu`, upstream's `CudaSession::search`; proves
     winners through miner.jam with `[%5 commitment nonce T_b 64]`.
   - Commitment: C = `blake3("nockmark-zk-v5" ‖ LE8(nonce))` written in base p
     (injective; limb 4 is almost always 0 because p⁴ ≈ 2²⁵⁶(1 − 2⁻³⁰)).
   - Server: everything in the list below, plus object 0 must be the canonical
     puzzle object for its own fields (consensus-path digest of the submitted
     object = upstream `v5_pow_digest`), `+canonical-pow-proof` on every
     `%poly`, and the STARK verified twice (native and the consensus Hoon
     verifier via roswell). Runs live in `zkv5-track.jsonl` beside the AI
     store; registry.hoon is unchanged.
   - Credit per win p⁵/(T_b+1) (= 2^a exactly at tier 2^a). Tiers
     [2¹², 2³¹], k = 16.
   - Live after deploy (k = 16, self-calibrated): RTX 3090 (CUDA V5,
     6.1 MH/s; 39.5 s proofs on the pod CPU, tier 2²⁹) → run 2, 4.82 MH/s
     verified, 5.81 NOCK/day; M1 Max (CPU, 175 k H/s on 10 threads, 21.2 s
     proofs, tier 2²³) → run 1, 91.0 kH/s verified. A 3090 is ≈ 35× an
     M1 Max on this digest.
   - Win-rate check on the 3090: 2³² nonces at tier 2¹⁸ gave 16,415 wins vs
     16,384 expected (+0.2σ); top limb uniform down to 2²⁴. RunPod spend
     $0.31.
   - `%3` submissions retired (410 Gone); both boards rank only current-rules
     rows (see Decisions).

   Design as approved:
   - challenge C → `block-commitment` = C reduced to 5 based belts;
   - the client grinds nonces against a benchmark target T_b with native
     `v5_pow_digest` (CPU) or `CudaSession::search` (GPU), proves k winners
     through the miner kernel (`[%5 header nonce target 64]`);
   - the server, per win: re-derives pow-v5 from the submitted proof's object
     0 (never trusts the claimed digest), checks commitment = C, `len` = 64,
     version `%5` (a `%3` proof relabelled `%5` passes `verify` alone), digest
     ≤ T_b, then `zkvm_jetpack::form::verify::verify`;
   - **dedupe on (C, nonce)**, not the proof hash — one object 0 admits many
     valid witness suffixes (#188);
   - rank Tip5 hashes/s = k × expected-attempts(T_b) / server window; tier sized
     against measured prove time (R = 2), as on the AI track;
   - earnings = hashes/s × 86400 / difficulty × reward.
4. **Mainnet's AI statement** — done (PR #38, deployed 2026-11-12). `dense-peak` (the
   #159 production shape, `PEAK_PRODUCTION_PARAMS`: m=4096 k=8192 n=32768
   r=512, 16×16 tiles) is the third statement on the AI board.
   - **Gate 1, measured on the M1 Max** (10 cores, 32 GB; upstream's own path:
     `PreparedReferenceDenseTemplate` → CPU `search_dense` →
     `prove_with_rules(Hardened)`; two runs, CPU 75–78% idle, no builds):
     grind 135.8 k / 142.5 k tiles/s = 2.85 / 2.99 × 10¹¹ MAC-equivalents/s
     (≈ 2.5 × 10¹¹ with the 0.6 s per-extranonce transcript); certificate
     75.8 s / 71.9 s cold, 64.1 s / 61.1 s with the prover setup reused; peak
     RSS 12.0 GB (15.4 GB holding the setup); artifact 147 KB; all three certs
     pass upstream's consensus verifier under `ConsensusBatch` (0.7 s each).
   - **Consensus accounting** (upstream's, from the statement's own mining
     config): F = 16·16·8192 = 2²¹ MAC-equivalents per tile, 2¹⁹ tiles per
     extranonce, trace height 2¹⁷ (cap 2¹⁹); `ConsensusBatch` adds nothing
     for dense (MoE multiplicity only). Matrices are miner-chosen in consensus.
   - **Gate outcome:** k = 16 × (R = 2 grind + prove) ≈ 48 certificates ≈
     54 min against the 1 h AI window; about 1 run in 4 would expire. Tom
     chose (2026-11-12): dense-peak rows from **rented CPUs** (no M1 board
     row), and dense-peak challenges live **3 h** (k = 16 and R = 2 kept, so
     its rows recover 2/3 with ±25% like every other row).
   - **Client** (`tock ai-bench --statement dense-peak`): upstream's CPU grind,
     or `--features gpu --gpu` for the #159 kernel (`search_peak`, every winner
     re-derived on the CPU); attempt index `extranonce · 2^19 + tile`
     (`peak-extranonce-tile-v1`); first win proved cold, the rest with its
     prover setup; tiers [2¹², 2³⁶]; each win submitted as the jammed
     `%ai-pow` artifact.
   - **Server:** bounded consensus decode; pins (dense, the dense-peak config
     and m×n, the claimed tile, this challenge's header at the claimed
     extranonce); then upstream's
     `verify_ai_pow_block_artifact_with_verification_rules(.., ConsensusBatch)`
     against a context derived from the shape
     (`derive_compact_verifier_context_for_shape`, ~20–46 s once, persisted
     pin-scoped, 1.2 KB) with its digest pinned. A dense-peak challenge
     records its statement: it gets the 3 h window and takes only dense-peak
     wins. Real-artifact fixtures (`registry/tests/fixtures/aipeak/`), every
     new rejection seen red.
   - **Live** (after deploy): run 9 on a RunPod RTX 3090 host's CPU (EPYC
     7663, 23.8-CPU quota; the GPU idle), self-calibrated: 31.2 k tiles/s,
     30.3 s per certificate → tier 2²¹, k = 16. 16 wins in 808.6 s (26.3 M
     tiles, 0.78× expected), certificates 27.0–30.1 s. Ranked 1 among the
     Hardened AI rows at **5.49 × 10¹⁰ MAC-eq/s verified** (grind window
     8.70 × 10¹⁰), 0.0017 NOCK/day. That host proves 2.5× faster than the M1
     Max. Pod spend $0.11.
   - **The #159 kernel runs only on compute capability 12.0** (upstream's
     `ai_pow_v3_peak.cu` refuses anything else: a 3090 returns CUDA status
     801 at preflight). `--gpu` dense-peak builds and fails cleanly off a
     5090; dense-peak GPU rows need an RTX 5090 (step 5). The M1 Max local
     row (k = 4, tier 2²⁰) was accepted too; there is no M1 board row, by
     Tom's choice.
5. **GPU sweep, both tracks** (3090 / 4090 / 5090; RunPod balance $8.64, the
   whole M6 programme cost $0.57), then a write-up answering "which puzzle
   should this card mine?", then revisit the announcement.
   - **ZK half: done (PR #36, 2026-11-12).** Live runs, self-calibrated,
     k = 16, client unchanged from step 3 (tock `1a704cd`). Logs and the
     full analysis are in `bench-results/gpu/2026-11-12-zk-v5-sweep/`.

     | card (pod CPU) | calibrated | sustained | verified (rank) | proof | tier | hashes / expected |
     |---|---:|---:|---:|---:|---:|---:|
     | 4090 (TR PRO 5975WX) | 14.65 MH/s | 14.03 | 10.03 (1) | 29.8 s | 2³⁰ | 0.989 |
     | 5090 (EPYC 9654) | 17.83 MH/s | 17.30 | 9.53 (2) | 33.7 s | 2³⁰ | 1.246 |
     | 3090 (EPYC 7663), step 3 | 6.11 MH/s | 5.18 | 4.82 (3) | 39.5 s | 2²⁹ | 0.676 |

     The calibrations are within 1 % of upstream's #192 claims (14.5 and
     18 MH/s).
   - **Why verified is below raw:** verified ÷ calibrated = luck ×
     sustained/calibrated × grind/window.
     - 3090: 1.48 × 0.85 × 0.63 = 0.79.
     - 4090: 1.01 × 0.96 × 0.71 = 0.69.
     - 5090: 0.80 × 0.97 × 0.69 = 0.54.

     At luck 1 the three runs recover 0.61, 0.68 and 0.62 of calibrated.
     - The GPU idles while proving. zk-bench grinds all 16 wins, then proves
       them one at a time on one core (1.0 of 64 or 192 cores busy, GPU
       0 %): 28–35 % of the window. That is the R = 2 tier rule's
       R/(R+1) cap, shifted per card by power-of-two rounding.
     - Submit and both server verifiers take ~25 s (1.4 %).
     - The 4090 and 5090 ran 3–4 % below their 2 s calibration, power-capped
       at 400 W and 520 W.
     - The 5090 ranks below the 4090 on luck alone (±25 % at k = 16): at
       luck 1 it would be 11.0 vs 10.0 MH/s.
   - **Follow-up fix, not done here** (rows must stay comparable):
     - Pipeline zk-bench as upstream's miner does: prove each winner while
       the GPU keeps grinding.
     - The window becomes grind + one proof + submit, so recovery is
       ≈ 1 − 1/(kR + 1) ≈ 97 %, still host-independent.
     - Predicted at luck 1: 4090 13.4 MH/s, 5090 16.3, 3090 4.97.
     - Probed on the same pods: 8 (4090 host) or 16 (5090 host) concurrent
       proofs each ran within 4 % of a lone proof, and the 4090's grind was
       14.50 MH/s alone vs 14.49 MH/s beside 8 proofs.
     - Re-measure every row at once with the new client.
   - **Digest uniformity at 2²⁹–2³⁰:** the 48 GPU wins took 46.58 expected
     units (z −0.21; KS against an exponential, p ≈ 0.36). 2³¹ was not
     exercised: no card's tier rounded up to it.
   - **RunPod spend:** $0.90 (balance $7.75 → $6.85), community cloud,
     4090 at $0.34/h and 5090 at $0.69/h, about 52 min each.
   - **Part 2a: pipelined clients (this PR).** Tom chose 2026-11-12: fix the
     clients first, then re-measure every row in one sweep.
     - Both clients now prove while grinding (`tock::pipeline`): the grinder
       hands each win to a prover pool the moment it finds it and keeps
       going, and pauses only once k wins are in hand. `zk-bench` runs a pool
       of miner kernels (default 4 beside a GPU, 1 beside a CPU grind);
       `ai-bench` canonical-moe and dense-peak, CPU and GPU, one certificate
       at a time by default (upstream's AI provers share one process-wide
       rayon pool). A failed proof gives its slot back and the grind finds a
       replacement; the failed win is never submitted. `--serial` keeps the
       old client for comparison.
     - Submission is unchanged: one batch of k wins, as before. The registry
       still credits k × expected-attempts(T_b) over its own mint-to-commit
       window and trusts no client timing.
     - **Server rules re-checked under pipelining.** The tier rule (R = 2)
       still makes the proof time cancel: the window is k·R·p + p, so recovery
       is kR/(kR + 1) ≈ 97 % at k = 16 on every host. The 1 h expiry and
       dense-peak's 3 h window are only looser now (runs are about a third
       shorter). None of the three was wrong, so none changed.
     - **Comparability.** Every run committed before 2026-11-13 00:00 UTC
       (server clock) came from the serial client. Once a board has a
       current-rules row committed after that, it ranks only those; the
       older rows stay listed, labelled `serial`, `rank: null`. Runs also
       carry the client's own `"proving"` label, which ranks nothing. See
       Decisions.
     - **Measured on the M1 Max CPU** (ZK v5, tier 2²³, k = 8, one machine,
       one hour; `bench-results/cpu/2026-11-12-m1-pipelining/`): the time
       after the last win fell from 169.3 s (8 proofs) to 31.9 s (one proof
       made beside the grind), and the server window from 492.7 s to 392.5 s.
       At luck 1 that is 0.69 → 0.87 of the uncontended rate, as the model
       predicts. The CPU grind lost 4.8 % to the prover beside it; nine grind
       threads instead of ten changed nothing.
   - **Part 2b: the sweep (2026-11-13).** Every row was re-measured with the
     pipelined client (tock `58fe623`), k = 16, self-calibrated, live. Full
     tables and logs are in `bench-results/gpu/2026-11-13-pipelined-sweep/`.
     RunPod spend $1.542 (balance $6.707 → $5.164), community only; all pods
     terminated.

     | card / host | statement | verified | at luck 1 | NOCK/day |
     |---|---|---:|---:|---:|
     | 5090 | dense-peak (#159 kernel) | 2.34×10¹⁴ MAC-eq/s | 0.86 | 7.22 |
     | 5090 | canonical-moe | 1.51×10¹¹ | 0.92 | 0.0047 |
     | 4090 | canonical-moe | 1.38×10¹¹ | 0.94 | 0.0043 |
     | 3090 | canonical-moe | 6.22×10¹⁰ | 0.94 | 0.0019 |
     | 3090 pod CPU (TR PRO, 17.9-CPU quota) | dense-peak | 3.13×10¹⁰ | 0.77 | 0.00097 |
     | M1 Max | canonical-moe | 1.93×10⁸ | 0.78 | 6×10⁻⁶ |
     | 5090 | ZK v5 | 17.32 MH/s | 0.91 | 20.1 |
     | 4090 | ZK v5 | 7.66 MH/s (luck 0.55) | 0.92 | 8.87 |
     | 3090 | ZK v5 | 6.42 MH/s | 0.91 | 7.44 |
     | M1 Max | ZK v5 | 236.8 kH/s (luck 1.91) | 0.80 | 0.27 |

     - **GPU rows now recover 0.86–0.94 of calibrated at luck 1**, up from
       0.61–0.68 (ZK) and 0.57–0.65 (legacy canonical-MoE). The 4090 and
       5090 ZK figures (13.4 and 15.7 MH/s at luck 1) match #36's
       predictions within 4 %.
     - **CPU-only hosts gain little or nothing.** On a 17.9-CPU pod,
       dense-peak pipelined recovered 0.77 and serial 0.76 at luck 1: the
       grind lost 22 % and certificates took 5× as long beside it. On the M1
       the single canonical-MoE grind thread lost 18 % to the prover.
     - **The 4090 ZK row needed 1.81× the expected hashes** (p ≈ 0.003). No
       failures, sustained = calibrated, gaps spread evenly; it can only be
       understated. Settled by the probe below: luck.
     - **RunPod:** hosts on driver 580.173.02 / 620.70.06 fail `cuInit` with
       error 999; the re-roll now checks CUDA before the 10–20 min build.
   - **Part 2c: the 4090 probe (2026-11-13). Luck, not a device miss.**
     Tom chose "probe the 4090, then draft". `tock/examples/zk_v5_probe.rs`
     replayed run zk 8's own challenge on an RTX 4090 with upstream's CUDA V5
     search and checked it against the CPU oracle (`v5_pow_digest`).
     Details in `bench-results/gpu/2026-11-13-4090-probe/`.
     - **zk 8 missed nothing.** The device enumerated every winner at 2²⁰
       over zk 8's whole range (nonces 0 … 15,567,367,629, 2^33.86). The
       2²⁹ winners among them are exactly zk 8's 16.
     - **The win rate past 2³² is normal:** 14,931 wins at 2²⁰ against
       14,846 expected (z +0.70), even across 2³⁰-nonce blocks (χ² 10.6 on
       14 dof). Tiers down to 2²⁷ are on expectation. The shortfall is only
       in this challenge's deep tail (2²⁹: 16 vs 29.0), so any correct
       device would have needed the same hashes.
     - **Device = oracle.** In 32 sampled ranges of 2²² nonces at 2¹⁴
       (straddling 2³², 2³³ and 2³⁴, and inside zk 8's longest gaps) the
       device, the pod CPU and an M1 Max found the same 8,229 winners. zk 8's
       16 exact launches each return its winner, and 383 digests match in
       all five limbs.
     - The probe ran on another 4090 (driver 570.211.01; zk 8's host had
       580.95.05). The winner set belongs to the challenge, not the card. A
       fault on zk 8's own card was not tested, but it cannot have cost a
       winner.
     - **RunPod:** $0.211 of the $0.50 cap (balance $5.151 → $4.941),
       community. Three hosts on driver 580.178.04 failed `cuInit` and were
       taken down within 1.3 min each; all pods terminated.
   - **Write-up:** next, in its own PR, merged only after Tom approves the
     text.

## Decisions

- Old rows stay in the data, labelled with the rules they were measured under;
  ranking only current-era rows comes with steps 2–3, when current-era rows
  exist (until then the boards would be empty). Step 2 labels; the switch to
  current-era ranking lands in step 3, when the ZK board gets its first v5
  rows and the AI board has had time to gather Hardened ones.
- Each step is its own branch and PR from `main`; steps 2–4 touch the same
  files, so they go in sequence.
- Each PR updates the docs its change makes stale. The dated write-ups stay as
  written, with a dated note on top.
- **Step 3 (2026-11-12): the registry no longer accepts new `%3` ZK runs.**
  Untracked `POST /challenge` and `POST /run` answer 410 Gone with a pointer
  to `?track=zk`. A complete-proofs/sec row measures work mainnet has refused
  since height 147,500 and could never be ranked. The 9 existing rows stay
  listed, unranked, in their own table.
- **Current-era ranking is live from step 3.** ZK: only `rules: anthropos`
  (v5) rows carry a `rank`. AI: only `hardened` rows do; legacy rows follow,
  `rank: null`, however fast. On the live board that leaves one ranked AI row
  until GPU rows are re-measured under Hardened rules (step 5).
- **ZK k = 16**, the AI track's k, for ±25% Poisson noise (k = 8 gave ±35%;
  the first M1 Max row at k = 8 landed at 38% of its calibrated rate on 2.55×
  bad luck). Tier ceiling 2³¹ so 16 wins fit the 1 h expiry on a 5090.
- **Serial rows after pipelining (step 5, part 2a, 2026-11-13).** Kept, not
  deleted, and labelled `serial`. A serial row understates its machine by
  about 1.4× against a pipelined one, so a mixed ranking would order cards by
  client version. The cut is server-attested: rows committed before
  2026-11-13 00:00 UTC (`PIPELINED_CLIENT_SINCE_MS`; the last live row before
  it was at 2026-11-12 22:18) stay ranked only until their board has a newer
  current-rules row, then follow the ranked rows with `rank: null`. Every
  ranked row is re-measured in the step 5 sweep, so the switch costs no
  coverage. The client's `proving` label is recorded and shown but ranks
  nothing. The first draft ranked on that label; the adversarial review
  (29 agents, 2026-11-13) showed one false "pipelined" row could then unrank
  every honest row on a board, so it was replaced before merge.
- **Submission stays one batch of k** under pipelining. Per-win submission
  would let the server verify while the client grinds and save the ~25 s
  upload-and-verify tail (≈1.4 %), but it needs partial-run state on the
  server and a new attack surface. Not worth it.
