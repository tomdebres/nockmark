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
4. **Mainnet's AI statement.** Add `dense-peak` (the #159 production shape) as a
   third statement on the AI board. Unknown: CPU prove time and peak RAM for
   that cert — measure before committing to it.
5. **GPU sweep, both tracks** (3090 / 4090 / 5090; RunPod balance $8.64, the
   whole M6 programme cost $0.57), then a write-up answering "which puzzle
   should this card mine?", then revisit the announcement.

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
