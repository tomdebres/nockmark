//! AI-PoW track, **canonical-MoE statement** (M6 Phase B1): grind + prove for
//! the block shape mainnet AI miners actually run.
//!
//! ## Why a second statement in the same track
//!
//! [`crate::aipow`] benchmarks the *dense* Pearl statement — a single 8×1024×8
//! tile, matrices synthesized from the challenge. That is a real AI-PoW
//! workload, but it is not the one `ai-pow-mine` submits: the production
//! gateway-free miner grinds the **canonical MoE (GROUPED_GEMM) block**
//! (`ai_pow_miner::canonical`), and the upcoming CUDA backend accelerates
//! exactly that. So this module adds the canonical statement as a peer of the
//! dense one inside the ONE AI track, not as a third leaderboard: consensus
//! itself compares heterogeneous AI work in MAC-equivalents (`ai_pow::
//! difficulty`, invariant D2 — expected MAC-equivalents per block is
//! `2^256 / T`, *independent of the shape the miner picked*), so
//! MAC-equivalents per second is precisely the unit in which a dense run and a
//! canonical-MoE run are commensurable.
//!
//! ## Two grind rules, deliberately kept apart
//!
//! | | dense ([`crate::aipow`]) | canonical MoE (here) |
//! |---|---|---|
//! | rule id | [`crate::aipow::AI_NONCE_RULE`] `extranonce-le8-v1` | [`AI_MOE_NONCE_RULE`] `canonical-ordinal-v3` |
//! | attempt selector | a u64 extranonce encoded as **8 LE bytes**, fed to the prover as the attempt nonce | a u32 **ordinal** that offsets the synthetic Pearl header's `timestamp` |
//! | what it perturbs | `pow_key_for_nonce(s_a, nonce)` | `sigma = header.to_bytes()` → `kappa` → noise seeds → the whole tile inference |
//! | threshold | raw: `jackpot ≤ T_b`, unscaled | scaled: `jackpot ≤ T_b · F` |
//!
//! There is NO 8-byte nonce anywhere on the canonical path — the canonical
//! jackpot is `keyed_hash(tile_state, s_a)` with `s_A` used directly, and
//! upstream asserts the nonce-folded key must NOT appear
//! (`canonical_jackpot_keyed_by_s_a_direct_not_nonce_folded`). Mixing the two
//! encodings would silently produce a different attempt, so the rules carry
//! different names and neither module's helpers are reachable from the other's
//! grind loop.
//!
//! ## Threshold semantics: SCALED, unlike the dense path
//!
//! The dense benchmark compares `jackpot ≤ T_b` with no shape factor —
//! `T_b` is the effective threshold directly. The canonical path does NOT:
//! `ai_pow_miner::run::canonical_grind_threshold` is
//! `effective_jackpot_threshold(target, F) = target · F`, the same value the
//! consensus verifier derives via `PearlPublicProofParams::
//! nockchain_adjusted_target` (pinned equal upstream by
//! `canonical_grind_threshold_matches_the_consensus_verifier`). So on this path
//! the registry's `T_b` prices ONE MAC-equivalent and expected attempts per win
//! is `2^256 / (T_b·F + 1)`, a factor `F = 2^16` fewer attempts than the same
//! `T_b` would mean on the dense path. Reading this backwards is not
//! hypothetical: M5 shipped a target off by 2^218 and ground for 23 h without a
//! win. [`expected_attempts_per_moe_win`] and its unit tests are the guard.
//!
//! Certificate proving stays OUTSIDE the measured grind window, exactly as on
//! the dense path (~25-30 s per win on CPU; a fixed overhead, not throughput).

use std::time::{Duration, Instant};

use ai_pow::difficulty::{dot_product_length, shape_work_factor};
use ai_pow::params::MatmulParams;
use ai_pow::pearl_compat::{
    derive_pearl_moe_work_commitments, PearlIncompleteBlockHeader, PearlMiningConfig,
    PearlWorkCommitments,
};
use ai_pow::pearl_moe_routing::{build_routing_data, RoutingData};
use ai_pow::synth::{synth_matrices, AI_POW_PROD_SYNTH_SEED};
use ai_pow::tile_hash::hash_le_target;
use ai_pow_miner::canonical::{
    canonical_mining_config, canonical_public_params, evaluate_canonical_moe_jackpot,
    prove_canonical_moe_block_at, PreparedCanonicalMoeTemplate,
};
use ai_pow_miner::certificate_noun::AiProofNode;
use ai_pow_miner::run::canonical_grind_threshold;
use ai_pow_zk::composite_public::CompositePublicInputs;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The canonical shape, mirrored from upstream's private constants
// ---------------------------------------------------------------------------

/// The canonical AI-PoW block shape: upstream's `CANONICAL_MATMUL_PARAMS`
/// (`crates/ai-pow-miner/src/run.rs`), which is `const` but private, so it is
/// restated here. `tests::canonical_shape_matches_upstream` pins it against the
/// crate's own behaviour rather than against a copied literal.
pub const AI_MOE_PARAMS: MatmulParams = MatmulParams {
    m: 64,
    k: 1024,
    n: 64,
    noise_rank: 64,
    tile: 8,
    spot_checks: 1,
    difficulty_bits: 0,
};

/// Opened-tile side (`CANONICAL_HW`): the Pearl row/column patterns are both
/// `hw`-long, so the opened tile is `hw × hw` = 8×8.
pub const AI_MOE_HW: u32 = 8;
/// MoE expert count (`CANONICAL_E`).
pub const AI_MOE_E: usize = 2;
/// MoE routing width (`CANONICAL_TOP_K`): one expert per token.
pub const AI_MOE_TOP_K: usize = 1;

/// Pattern-expansion bound upstream uses everywhere on the canonical path
/// (`indices_with_offset_bounded(0, 4096)` in `canonical_moe_schedule`, and the
/// `max_pattern_len` argument of the node's MoE work precheck). The verifier
/// must use the same bound or an honest schedule can fail to re-derive.
pub const AI_MOE_MAX_PATTERN_LEN: usize = 4096;

/// Identifier for the ordinal→attempt rule, the canonical-path peer of
/// [`crate::aipow::AI_NONCE_RULE`]. `v3` names the production Pearl **V3**
/// statement this rule belongs to: the attempt selector is a u32 ordinal that
/// offsets the synthetic Pearl header timestamp
/// (`PreparedCanonicalMoeTemplate::header_for`). It is NOT an 8-byte
/// little-endian nonce and must never be encoded as one. Versioned; the
/// registry advertises it on the challenge and clients must match it.
pub const AI_MOE_NONCE_RULE: &str = "canonical-ordinal-v3";

/// Statement discriminator strings shared by client, registry and board.
/// Both statements rank together on the one AI board.
pub const STATEMENT_DENSE: &str = "dense";
pub const STATEMENT_CANONICAL_MOE: &str = "canonical-moe";

/// MAC-equivalents one canonical-MoE grind attempt costs — the shape work
/// factor `F` consensus prices this shape's attempts at.
///
/// Derivation (`ai_pow::difficulty`, `F = h · w · dot_product_length`):
///
/// ```text
///   h  = w  = AI_MOE_HW                     = 8     (8×8 opened tile)
///   dot_product_length(k, r) = k − (k mod r)
///                            = 1024 − (1024 mod 64)
///                            = 1024                 (r | k, so dot = k)
///   F  = 8 · 8 · 1024 = 65_536 = 2^16
/// ```
///
/// Numerically identical to the dense track's
/// [`crate::aipow::MAC_EQUIV_PER_ATTEMPT`] — both open an 8×8 tile over
/// `k = 1024` — but arrived at through a different shape, so it is derived and
/// pinned separately. [`moe_shape_work_factor`] recomputes it from the crate
/// functions and `tests::shape_work_factor_matches_the_crate` asserts the two agree,
/// so a re-pin that moves the canonical shape cannot silently drift this
/// constant.
pub const MAC_EQUIV_PER_MOE_ATTEMPT: f64 = 65_536.0;

/// `F` recomputed from the crate's own difficulty functions for the canonical
/// shape. The authoritative value; [`MAC_EQUIV_PER_MOE_ATTEMPT`] is its `f64`
/// mirror for the rate arithmetic.
pub fn moe_shape_work_factor() -> Result<u128, String> {
    let dot = dot_product_length(AI_MOE_PARAMS.k, AI_MOE_PARAMS.noise_rank as u32)
        .map_err(|e| format!("dot_product_length: {e:?}"))?;
    shape_work_factor(AI_MOE_HW, AI_MOE_HW, dot).map_err(|e| format!("shape_work_factor: {e:?}"))
}

// ---------------------------------------------------------------------------
// Threshold + economics
// ---------------------------------------------------------------------------

/// The effective jackpot threshold `Θ = T · F` for the canonical shape —
/// upstream's [`canonical_grind_threshold`], used unchanged so the benchmark's
/// accept predicate is byte-for-byte the miner's (and, transitively, the
/// consensus verifier's).
pub fn moe_threshold(target: &[u8; 32]) -> Result<[u8; 32], String> {
    canonical_grind_threshold(target).map_err(|e| e.to_string())
}

/// The calibrated canonical-MoE benchmark target `T_b = 2^224`.
///
/// Derivation — measured on this machine, not guessed. The measurement lives in
/// `tests::moe_grind_rate` (the peer of upstream's `canonical_mining_costs`) so a
/// future re-calibration re-runs the same code rather than re-deriving it:
///
/// ```text
///   measured grind rate (M1 Max, one thread, release)  0.2947 ms/attempt
///                                                    = 3394 attempts/sec
///   grind budget                                       10-20 s per win
///   attempts wanted        = 3394 · (10 … 20 s)      = 33_937 … 67_875
///   nearest power of two                             = 2^16 = 65_536  (19.3 s)
///   Θ = 2^256 / attempts   = 2^256 / 2^16            = 2^240
///   T = Θ / F              = 2^240 / 2^16            = 2^224
/// ```
///
/// `2^224` is byte 28 of the little-endian encoding set to `0x01` and every
/// other byte zero — a target you cannot misread. (M5's post-mortem: the first
/// deployed dense target was a big-endian-intended value that read as `2^11`
/// little-endian and ground for 23 h without a win. A target whose byte order
/// you have to squint at is a target you will get wrong.) Rounding to a power
/// of two also keeps `2^256/(Θ+1)` exact in `f64`.
///
/// Note this is a MAC-equivalent-priced target, so it is NOT comparable
/// digit-for-digit with the dense track's `NOCKMARK_AI_TARGET`: the same
/// number means `F = 2^16` times more attempts there. Production overrides via
/// `NOCKMARK_AI_MOE_TARGET`.
pub fn calibrated_moe_target() -> [u8; 32] {
    let mut t = [0u8; 32];
    t[CALIBRATION_BYTE] = CALIBRATION_VALUE;
    t
}

/// `T_b = 2^224`: byte 28 of the LE encoding, value 1.
const CALIBRATION_BYTE: usize = 28;
const CALIBRATION_VALUE: u8 = 0x01;
/// `2^256 / (T_b · F)` at [`calibrated_moe_target`] — the expected attempts per
/// win the calibration was sized for. Public so the registry can state the
/// grind budget it is advertising without recomputing it.
pub const EXPECTED_ATTEMPTS_PER_WIN: f64 = 65_536.0;
/// The single-thread CPU grind rate [`calibrated_moe_target`] was computed
/// from (M1 Max, release, `tests::moe_grind_rate`).
pub const MEASURED_ATTEMPTS_PER_SEC: f64 = 3394.0;

/// The loosest canonical-MoE target that still scales: `T = 2^240 − 1`, the
/// largest `T` with `T · F ≤ 2^256 − 1` at this shape's `F = 2^16`. Every
/// attempt wins (`Θ = 2^256 − 2^16`), which is what the integration tests and
/// `--target max` local runs want.
///
/// Note there is no "all-FF" MoE target: `effective_jackpot_threshold` is
/// fail-closed, so `[0xff; 32]` is not "everything wins", it is an ERROR — the
/// exact trap [`moe_threshold`] exists to surface. (The dense path, which never
/// scales, does take `[0xff; 32]`.)
pub fn max_moe_target() -> [u8; 32] {
    let mut t = [0xffu8; 32];
    t[30] = 0;
    t[31] = 0;
    t
}

/// Expected grind attempts per canonical-MoE win at registry target `T`:
/// `2^256 / (Θ + 1)` where `Θ = T · F`.
///
/// Contrast `crate::aipow`'s dense form `2^256 / (T + 1)`: same jackpot
/// distribution, different threshold semantics. Expressed over `Θ` rather than
/// `T · F` directly so an overflow-clamped `Θ` (fail-closed upstream) is
/// impossible to reach here — [`moe_threshold`] errors first.
pub fn expected_attempts_per_moe_win(target: &[u8; 32]) -> Result<f64, String> {
    let theta = moe_threshold(target)?;
    let mut t = 0.0f64;
    for &b in theta.iter().rev() {
        t = t * 256.0 + b as f64;
    }
    Ok(2f64.powi(256) / (t + 1.0))
}

// ---------------------------------------------------------------------------
// Client types
// ---------------------------------------------------------------------------

/// One canonical-MoE workload: the challenge (which plays the node's
/// `nock_block_commitment` role — it is bound into the synthetic Pearl
/// coinbase's aux commitment, hence into the header's merkle root), the
/// registry target, and the wins required.
pub struct AiMoeChallenge {
    /// 32-byte challenge minted by the registry (or a dev constant locally).
    /// Passed to upstream as `nock_commit`.
    pub challenge: [u8; 32],
    /// Registry target `T_b`. **Scaled** at compare time by `F` — see the
    /// module docs. The matrices are NOT seeded from this challenge on the
    /// canonical path (upstream fixes them at `AI_POW_PROD_SYNTH_SEED`); the
    /// challenge binds through the aux commitment instead, which is what the
    /// production node checks.
    pub target: [u8; 32],
    /// Wins required before the grind window closes.
    pub k: u64,
}

/// The per-win submission blob the registry verifies.
///
/// Deliberately smaller than the dense [`crate::aipow::AiCertBlob`]: on the
/// canonical path the ENTIRE statement — header, mining config, matrix
/// commitments, routing, opened indices, jackpot — is a deterministic function
/// of `(challenge, ordinal)`, so the registry re-derives all of it with
/// `canonical_moe_statement_parts` and needs nothing from the prover but the
/// certificate and its Layer-0 public inputs. Even `trace_height` is omitted:
/// the verify path recomputes it from the re-derived opened schedule, so
/// carrying a claimed one would only add trust surface.
///
/// Postcard encodes fields in declaration order — reordering or retyping any
/// field is a wire-format break.
#[derive(Serialize, Deserialize)]
pub struct AiMoeCertBlob {
    /// The grind ordinal this certificate is for ([`AI_MOE_NONCE_RULE`]).
    /// Bound against the win's claimed ordinal by the registry.
    pub ordinal: u32,
    /// Claimed Layer-0 public inputs. Every re-derivable slot is re-checked
    /// (`COMMITMENT_HASH`/`HASH_A`/`HASH_B`/`JOB_KEY`) and the rest is bound
    /// cryptographically by the compact verify.
    pub pis: CompositePublicInputs,
    /// Compact recursive certificate, canonical postcard bytes.
    pub cert_bytes: Vec<u8>,
}

/// One canonical-MoE jackpot win with its compact recursive certificate.
pub struct AiMoeWin {
    pub ordinal: u32,
    pub cert_bytes: Vec<u8>,
    /// Postcard bytes of [`AiMoeCertBlob`] — base64 this into `cert_b64`.
    pub submission_bytes: Vec<u8>,
    pub prove_ms: u64,
}

/// Grind phase output.
pub struct MoeGrindResult {
    /// Winning ordinals, strictly ascending by construction.
    pub win_ordinals: Vec<u32>,
    /// Grind start to the k-th win found (prove time excluded by design).
    pub elapsed_ms: u64,
    /// Attempts evaluated, i.e. last winning ordinal + 1.
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

/// Full canonical-MoE ai-bench summary.
pub struct AiMoeBenchSummary {
    pub wins: Vec<AiMoeWin>,
    pub grind_elapsed_ms: u64,
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

/// Fixed dev challenge for fully-local canonical-MoE runs. Distinct from
/// [`crate::aipow::dev_challenge`] so a local MoE run and a local dense run are
/// never the same workload.
pub fn dev_challenge() -> [u8; 32] {
    ai_pow::commit::matrix_commitment(b"nockmark-m6-ai-moe-bench-dev-challenge-v1", &[0u8; 32])
}

// ---------------------------------------------------------------------------
// Grind
// ---------------------------------------------------------------------------

/// Grind ordinals 0, 1, 2, … strictly ascending until `k` jackpot wins.
///
/// Uses [`PreparedCanonicalMoeTemplate`] — upstream's own search template,
/// which hoists the schedule and the (challenge-independent) matrices and
/// recomputes every attempt-dependent value: `kappa`, both matrix commitments,
/// both noise seeds, both noised strips, the tile matmul and the jackpot. There
/// is no cheap re-roll; upstream pins that as an anti-reuse invariant
/// (`canonical_extranonce_forces_fresh_tile_inference`).
///
/// Every winner is re-checked against the scalar oracle
/// [`evaluate_canonical_moe_jackpot`] before it is allowed to reach the prover,
/// mirroring `grind_canonical_block_with_backend` — the same guard that keeps a
/// (future, Phase B2) GPU backend from reporting a jackpot the CPU disagrees
/// with.
pub fn grind_moe(ch: &AiMoeChallenge) -> MoeGrindResult {
    assert!(ch.k >= 1, "k must be at least 1");
    let threshold = moe_threshold(&ch.target).expect("canonical grind threshold");
    let template = PreparedCanonicalMoeTemplate::new(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        ch.challenge,
    )
    .expect("PreparedCanonicalMoeTemplate::new");
    let mut scratch = template.scratch();

    let t0 = Instant::now();
    let mut win_ordinals: Vec<u32> = Vec::with_capacity(ch.k as usize);
    let mut ordinal: u32 = 0;
    let mut attempts: u64 = 0;
    while (win_ordinals.len() as u64) < ch.k {
        let out = template.evaluate(ordinal, &mut scratch);
        attempts += 1;
        if hash_le_target(&out.jackpot_hash, &threshold) {
            // Scalar-oracle recheck: the template's batched transcript must
            // agree with the standalone evaluator the prover will certify.
            let oracle = evaluate_canonical_moe_jackpot(
                &AI_MOE_PARAMS,
                AI_MOE_HW,
                AI_MOE_E,
                AI_MOE_TOP_K,
                ch.challenge,
                ordinal,
            )
            .expect("canonical scalar jackpot");
            assert_eq!(
                oracle, out.jackpot_hash,
                "template jackpot disagrees with the canonical scalar oracle at ordinal {ordinal}"
            );
            win_ordinals.push(ordinal);
            eprintln!(
                "  win {}/{} at ordinal {ordinal} ({:.1}s)",
                win_ordinals.len(),
                ch.k,
                t0.elapsed().as_secs_f64()
            );
        }
        ordinal = ordinal
            .checked_add(1)
            .expect("canonical ordinal space (u32) exhausted: the target is too hard for this shape");
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    MoeGrindResult {
        win_ordinals,
        // Round UP like the ZK/dense benches: never claim faster than measured.
        elapsed_ms: (elapsed_s * 1000.0).ceil().max(1.0) as u64,
        total_attempts: attempts,
        attempts_per_sec: attempts as f64 / elapsed_s.max(1e-9),
    }
}

/// Measure this machine's canonical-MoE attempt rate: evaluate a throwaway
/// template for `budget` and count attempts.
///
/// The peer of [`crate::aipow::calibrate_attempts_per_sec`] and subject to the
/// same rule — run it BEFORE minting a challenge, because the server window
/// opens at mint and a calibration inside it would be charged to the rate it
/// exists to measure. No jackpot comparison at all here: on this path
/// `template.evaluate` IS the attempt, so its cost is the whole measurement
/// (and skipping the comparison removes any chance of a throwaway win reaching
/// the prover).
pub fn calibrate_moe_attempts_per_sec(budget: Duration) -> f64 {
    let template = PreparedCanonicalMoeTemplate::new(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        dev_challenge(),
    )
    .expect("PreparedCanonicalMoeTemplate::new");
    let mut scratch = template.scratch();
    let t0 = Instant::now();
    let mut attempts: u64 = 0;
    let mut ordinal: u32 = 0;
    while t0.elapsed() < budget {
        let _ = template.evaluate(ordinal, &mut scratch);
        attempts += 1;
        // Wrapping, not checked: a calibration that ran through the whole u32
        // ordinal space would simply measure the same attempts again, which is
        // still an honest rate.
        ordinal = ordinal.wrapping_add(1);
    }
    attempts as f64 / t0.elapsed().as_secs_f64().max(1e-9)
}

/// Measure what ONE canonical-MoE certificate costs this machine: prove a
/// single throwaway block and time it.
///
/// The peer of [`crate::aipow::calibrate_prove_secs`] and the second half of
/// the same pre-mint calibration — it must finish before the challenge is
/// minted, for the reason [`calibrate_moe_attempts_per_sec`] gives.
/// [`crate::aipow::tier_for_prove_ratio`] is denominated in this number.
///
/// Simpler than the dense peer in one respect: [`prove_canonical_moe_block_at`]
/// takes no target at all — the canonical statement is a function of
/// `(challenge, ordinal)` alone — so the throwaway needs no contrived win, just
/// [`dev_challenge`] at ordinal 0. The block is dropped on return.
///
/// This is also the measurement a `--gpu` run uses, unchanged: the device
/// grinds, the CPU proves, so the per-certificate cost is a property of the
/// host and not of the backend.
pub fn calibrate_moe_prove_secs() -> f64 {
    let t0 = Instant::now();
    prove_canonical_moe_block_at(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        dev_challenge(),
        0,
    )
    .expect("prove throwaway canonical-MoE calibration block");
    t0.elapsed().as_secs_f64().max(1e-9)
}

// ---------------------------------------------------------------------------
// Prove
// ---------------------------------------------------------------------------

/// Prove a canonical MoE block per winning ordinal (~25-30 s each on CPU).
///
/// Uses upstream's [`prove_canonical_moe_block_at`] verbatim — the same call
/// the gateway-free production miner makes on a jackpot hit — so the certified
/// statement is byte-identical to a real submission's. Note it exposes no
/// prover-cache reuse (unlike the dense path's
/// `…_with_prover_cache`), so every win pays the full setup; that is a fixed
/// per-win cost outside the measured window, not a throughput term.
pub fn prove_moe_wins(ch: &AiMoeChallenge, win_ordinals: &[u32]) -> Vec<AiMoeWin> {
    let mut wins = Vec::with_capacity(win_ordinals.len());
    for &ordinal in win_ordinals {
        let t0 = Instant::now();
        let block = prove_canonical_moe_block_at(
            &AI_MOE_PARAMS,
            AI_MOE_HW,
            AI_MOE_E,
            AI_MOE_TOP_K,
            ch.challenge,
            ordinal,
        )
        .expect("prove canonical MoE block");
        let prove_ms = t0.elapsed().as_millis() as u64;
        let AiProofNode::Bytes(cert_bytes) = &block.certificate.certificate else {
            panic!("production compact certificate must use the canonical byte node");
        };
        eprintln!(
            "  cert for ordinal {ordinal}: {:.1}s ({} bytes)",
            prove_ms as f64 / 1000.0,
            cert_bytes.len()
        );
        let blob = AiMoeCertBlob {
            ordinal,
            pis: block.certificate.public_inputs.clone(),
            cert_bytes: cert_bytes.clone(),
        };
        let submission_bytes = postcard::to_allocvec(&blob).expect("encode MoE submission blob");
        wins.push(AiMoeWin {
            ordinal,
            cert_bytes: cert_bytes.clone(),
            submission_bytes,
            prove_ms,
        });
    }
    wins
}

/// The full local canonical-MoE workload: grind to k wins, then certify each.
pub fn run(ch: &AiMoeChallenge) -> AiMoeBenchSummary {
    eprintln!("grinding to {} canonical-MoE win(s)…", ch.k);
    let grind = grind_moe(ch);
    eprintln!(
        "grind window closed: {} attempts in {:.1}s",
        grind.total_attempts,
        grind.elapsed_ms as f64 / 1000.0
    );
    eprintln!("proving {} certificate(s)…", grind.win_ordinals.len());
    let wins = prove_moe_wins(ch, &grind.win_ordinals);
    AiMoeBenchSummary {
        wins,
        grind_elapsed_ms: grind.elapsed_ms,
        total_attempts: grind.total_attempts,
        attempts_per_sec: grind.attempts_per_sec,
    }
}

// ---------------------------------------------------------------------------
// Prove inputs (verifier-context construction only)
// ---------------------------------------------------------------------------

/// The inputs `ai_pow::zk_bridge::prove_pearl_moe_compact_recursive_certificate`
/// needs for the canonical block at `(challenge, ordinal)`.
///
/// Exists for exactly one caller: the registry's build-by-proving of its
/// canonical-MoE compact verifier context. Upstream's `canonical_moe_inputs` is
/// private and `prove_canonical_moe_block_at` discards the verifier context it
/// produced, so the context cannot be obtained through the public miner API.
///
/// Everything fragile is still taken from the crate rather than restated: the
/// synthetic Pearl header (with its aux commitment and coinbase merkle root —
/// the part that binds our challenge) comes from
/// [`canonical_public_params`], the mining config from
/// [`canonical_mining_config`], the commitments from
/// `derive_pearl_moe_work_commitments`. Only the routing/pattern schedule is
/// re-expressed, and `tests::prove_inputs_match_the_crate` pins that against the
/// crate's own ticket.
pub struct MoeProveInputs {
    pub params: MatmulParams,
    pub config: PearlMiningConfig,
    pub header: PearlIncompleteBlockHeader,
    pub a: Vec<i8>,
    pub b: Vec<i8>,
    pub commitments: PearlWorkCommitments,
    pub routing: RoutingData,
    pub inner: Vec<u32>,
    pub local_b: Vec<u32>,
    pub n_e: usize,
    pub m: usize,
}

/// Build [`MoeProveInputs`] for the canonical block at `(challenge, ordinal)`.
pub fn moe_prove_inputs(challenge: &[u8; 32], ordinal: u32) -> Result<MoeProveInputs, String> {
    let params = AI_MOE_PARAMS;
    let m = params.m as usize;
    let n = params.n as usize;
    if AI_MOE_E == 0 || n % AI_MOE_E != 0 {
        return Err(format!("n={n} not divisible by e={AI_MOE_E}"));
    }
    let n_e = n / AI_MOE_E;
    let config = canonical_mining_config(&params, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K);
    // Round-robin token→expert assignment, upstream's `canonical_moe_schedule`.
    let topk: Vec<u32> = (0..m).map(|t| (t % AI_MOE_E) as u32).collect();
    let routing = build_routing_data(&topk, m, AI_MOE_TOP_K, AI_MOE_E)
        .map_err(|e| format!("routing: {e:?}"))?;
    let inner = config
        .rows_pattern
        .indices_with_offset_bounded(0, AI_MOE_MAX_PATTERN_LEN)
        .map_err(|e| format!("inner: {e:?}"))?;
    let local_b = config
        .cols_pattern
        .indices_with_offset_bounded(0, AI_MOE_MAX_PATTERN_LEN)
        .map_err(|e| format!("local_b: {e:?}"))?;
    // The canonical matrices are FIXED at the production synth seed — on this
    // path the challenge binds through the header's aux commitment, not the
    // matrices (the dense path is the other way round).
    let (a, b) = synth_matrices(AI_POW_PROD_SYNTH_SEED, &params);
    let header =
        canonical_public_params(&params, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, *challenge, ordinal)
            .map_err(|e| format!("canonical public params: {e}"))?
            .block_header;
    let mu = config.to_bytes().map_err(|e| format!("config bytes: {e:?}"))?;
    let commitments = derive_pearl_moe_work_commitments(
        &header.to_bytes(),
        &mu,
        &a,
        &b,
        params.m,
        n_e as u32,
        &routing.routing_data_le_bytes(),
        &routing.routing_offsets_le_bytes(),
    );
    Ok(MoeProveInputs {
        params,
        config,
        header,
        a,
        b,
        commitments,
        routing,
        inner,
        local_b,
        n_e,
        m,
    })
}

#[cfg(test)]
mod tests {
    use ai_pow_miner::canonical::{canonical_moe_statement_parts, evaluate_canonical_moe_ticket};

    use super::*;

    /// **F must come from the crate, not from a copied literal.**
    ///
    /// Recomputes `F` two independent ways — through `ai_pow::difficulty`'s
    /// `shape_work_factor_for` decomposition, and through the mining config the
    /// canonical statement actually carries (`PearlMiningConfig::
    /// shape_work_factor`, which is what the consensus verifier re-parses) —
    /// and pins both against [`MAC_EQUIV_PER_MOE_ATTEMPT`]. A re-pin that moves
    /// the canonical shape breaks this instead of silently rescaling every
    /// canonical-MoE row on the board.
    #[test]
    fn shape_work_factor_matches_the_crate() {
        let f = moe_shape_work_factor().expect("canonical shape is admissible");
        // The derivation spelled out: dot = k − (k mod r) = 1024, F = 8·8·1024.
        assert_eq!(
            dot_product_length(AI_MOE_PARAMS.k, AI_MOE_PARAMS.noise_rank as u32).unwrap(),
            1024
        );
        assert_eq!(f, 1 << 16, "F = h·w·dot = 8·8·1024 = 2^16");
        assert_eq!(f as f64, MAC_EQUIV_PER_MOE_ATTEMPT);

        // Same number, derived from the authenticated statement's own config —
        // the verifier's route. Upstream pins miner == verifier; this pins
        // OUR constant == both.
        let config =
            canonical_mining_config(&AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K);
        assert_eq!(config.shape_work_factor().expect("config factor"), f);
        let public = canonical_public_params(
            &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, [0x5a; 32], 0,
        )
        .expect("canonical public params");
        assert_eq!(public.difficulty_adjustment_factor().expect("verifier factor"), f);
    }

    /// The canonical shape constants restated here must be the ones upstream
    /// mines: if `AI_MOE_PARAMS`/hw/e/top_k drifted from
    /// `CANONICAL_MATMUL_PARAMS`, the derived statement would not be the one a
    /// node accepts. Checked through observable behaviour — the opened schedule
    /// and the routing the crate itself produces.
    #[test]
    fn canonical_shape_matches_upstream() {
        let (public, moe_art) = canonical_moe_statement_parts(
            &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, [0x42; 32], 0,
        )
        .expect("statement parts");
        // 64 tokens, 2 experts, top-1 → expert 0 opens 8 of its 32 rows…
        assert_eq!(public.m, 64);
        // …over n_e = n/e = 32 columns per expert.
        assert_eq!(public.n, 32);
        assert_eq!(moe_art.moe.expert_idx, 0);
        assert_eq!(moe_art.moe.outer_indices.len(), AI_MOE_HW as usize);
        assert_eq!(moe_art.routing_data.len(), 64);
        // Dense-only statement fields stay zero on the MoE route.
        assert_eq!((public.t_rows, public.t_cols), (0, 0));
    }

    /// **Threshold semantics: SCALED here, RAW on the dense path.**
    ///
    /// `canonical_grind_threshold(T)` must equal `T · F` and must equal the
    /// consensus verifier's `nockchain_adjusted_target(T)` — never `T` itself.
    /// The dense benchmark compares against `T` unscaled, so the same `T` means
    /// two different amounts of work on the two statements; conflating them is
    /// the M5 bug that ground for 23 h.
    #[test]
    fn threshold_is_scaled_by_f_unlike_the_dense_path() {
        let mut target = [0u8; 32];
        target[24] = 0x01; // T = 2^192
        let theta = moe_threshold(&target).expect("threshold");
        // Θ = 2^192 · 2^16 = 2^208 (byte 26 set), strictly looser than T.
        let mut expected = [0u8; 32];
        expected[26] = 0x01;
        assert_eq!(theta, expected, "Θ must be T·F, not T");
        assert_ne!(theta, target, "the canonical threshold is NOT the raw target");

        // …and it is exactly what the consensus verifier derives.
        let public = canonical_public_params(
            &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, [0x5a; 32], 0,
        )
        .expect("public params");
        assert_eq!(public.nockchain_adjusted_target(&target).expect("verifier"), theta);

        // A jackpot in the band (T, Θ] is a real win here and would be thrown
        // away by a raw-target comparison.
        let mut jackpot = target;
        jackpot[0] = 0x01; // 2^192 + 1
        assert!(!hash_le_target(&jackpot, &target));
        assert!(hash_le_target(&jackpot, &theta));
    }

    /// Expected-attempts arithmetic, pinned at the values the registry ships.
    ///
    /// The shipped canonical-MoE target is a power of two `T = 2^b`; because
    /// the canonical threshold is SCALED, expected attempts per win is
    /// `2^256 / (T·F)` = `2^(256 − b − 16)`, not `2^(256 − b)`. Both forms are
    /// asserted so the factor-of-F gap is visible in the test itself.
    #[test]
    fn expected_attempts_for_the_production_moe_target() {
        let prod = calibrated_moe_target();
        // The target reads as a single set bit at byte CALIBRATION_BYTE.
        assert_eq!(prod[CALIBRATION_BYTE], CALIBRATION_VALUE);
        assert!(prod.iter().enumerate().all(|(i, &b)| i == CALIBRATION_BYTE || b == 0));

        let attempts = expected_attempts_per_moe_win(&prod).unwrap();
        assert_eq!(attempts, EXPECTED_ATTEMPTS_PER_WIN);
        // ~10-20 s of grinding on the reference CPU at the measured rate.
        assert!(
            (10.0..=20.0).contains(&(attempts / MEASURED_ATTEMPTS_PER_SEC)),
            "calibration drifted: {attempts} attempts at {MEASURED_ATTEMPTS_PER_SEC}/s"
        );

        // The reading that would reproduce the M5 disaster: applying the DENSE
        // (unscaled) formula to this target overstates the grind by exactly F.
        let mut raw = 0.0f64;
        for &b in prod.iter().rev() {
            raw = raw * 256.0 + b as f64;
        }
        assert_eq!(
            (2f64.powi(256) / (raw + 1.0)) / attempts,
            MAC_EQUIV_PER_MOE_ATTEMPT,
            "dense-style arithmetic on a canonical target is wrong by F"
        );

        // The loosest scalable target wins on (essentially) every attempt…
        assert!((expected_attempts_per_moe_win(&max_moe_target()).unwrap() - 1.0).abs() < 1e-6);
        // …and one step past it is fail-closed, not accept-everything.
        assert!(moe_threshold(&[0xff; 32]).is_err(), "T·F must fail closed");
    }

    /// Measure this machine's canonical-MoE grind rate — the input to
    /// [`calibrated_moe_target`]. The peer of upstream's `canonical_mining_costs`.
    /// Ignored (~10 s); run with:
    ///   cargo test --release moe_grind_rate -- --ignored --nocapture
    #[test]
    #[ignore]
    fn moe_grind_rate() {
        let template = PreparedCanonicalMoeTemplate::new(
            &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, [0x5a; 32],
        )
        .expect("template");
        let mut scratch = template.scratch();
        let _ = template.evaluate(0, &mut scratch); // warm
        let attempts = 4_000u32;
        let t0 = std::time::Instant::now();
        for ordinal in 0..attempts {
            let _ = template.evaluate(ordinal, &mut scratch);
        }
        let per = t0.elapsed().as_secs_f64() / attempts as f64;
        println!(
            "canonical-MoE grind: {:.4} ms/attempt ({:.0} attempts/sec), F = 2^{}",
            per * 1e3,
            1.0 / per,
            moe_shape_work_factor().unwrap().ilog2(),
        );
        for secs in [10.0f64, 15.0, 20.0] {
            let attempts = secs / per;
            println!(
                "  {secs:>4.0} s of grinding = {attempts:.0} attempts = 2^{:.1} \
                 => T = 2^{:.1}",
                attempts.log2(),
                256.0 - attempts.log2() - 16.0,
            );
        }
    }

    /// The two grind rules must stay separable: different names, and the
    /// canonical ordinal enters ONLY as a header-timestamp offset — there is no
    /// LE8 nonce anywhere on this path.
    #[test]
    fn ordinal_rule_is_not_the_dense_le8_nonce_rule() {
        assert_ne!(AI_MOE_NONCE_RULE, crate::aipow::AI_NONCE_RULE);
        assert_eq!(AI_MOE_NONCE_RULE, "canonical-ordinal-v3");

        let template = PreparedCanonicalMoeTemplate::new(
            &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, [0x33; 32],
        )
        .expect("template");
        let h0 = template.header_for(0);
        for ordinal in [1u32, 7, 4096, u32::MAX] {
            let h = template.header_for(ordinal);
            assert_eq!(
                h.timestamp.wrapping_sub(h0.timestamp),
                ordinal,
                "the ordinal is a timestamp offset, nothing else"
            );
            // Everything else in the header — including the merkle root that
            // carries our challenge's aux commitment — is ordinal-invariant.
            assert_eq!(h.version, h0.version);
            assert_eq!(h.prev_block, h0.prev_block);
            assert_eq!(h.merkle_root, h0.merkle_root);
            assert_eq!(h.nbits, h0.nbits);
        }
        // The dense rule's encoding is 8 bytes wide; the canonical selector is
        // a u32 and does not fit that mould.
        assert_eq!(crate::aipow::extranonce_nonce(1).len(), 8);
        assert_eq!(std::mem::size_of::<u32>(), 4);
    }

    /// The replicated prove-inputs must reproduce the crate's own transcript.
    /// Cheap (no certificate): one template evaluation's worth of work.
    #[test]
    fn prove_inputs_match_the_crate() {
        let challenge = [0x42u8; 32];
        for ordinal in [0u32, 1, 7] {
            let inputs = moe_prove_inputs(&challenge, ordinal).expect("prove inputs");
            let ticket = evaluate_canonical_moe_ticket(
                &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, challenge, ordinal,
            )
            .expect("crate ticket");
            let (public, moe_art) = canonical_moe_statement_parts(
                &AI_MOE_PARAMS, AI_MOE_HW, AI_MOE_E, AI_MOE_TOP_K, challenge, ordinal,
            )
            .expect("statement parts");

            assert_eq!(inputs.commitments.h_a, public.hash_a, "H_A");
            assert_eq!(inputs.commitments.h_b, public.hash_b, "H_B");
            assert_eq!(inputs.commitments.s_a, ticket.s_a, "s_A");
            assert_eq!(inputs.commitments.s_b, ticket.s_b, "s_B");
            assert_eq!(inputs.config, public.mining_config, "mining config");
            assert_eq!(inputs.header, public.block_header, "header");
            assert_eq!(inputs.n_e as u32, public.n, "n_e");
            assert_eq!(inputs.m as u32, public.m, "m");
            assert_eq!(
                inputs.routing.routing_data, moe_art.routing_data,
                "routing data"
            );
            assert_eq!(
                inputs.routing.routing_offsets, moe_art.moe.routing_offsets,
                "routing offsets"
            );
        }
    }

    /// **The same tier is a different 32 bytes on this statement.**
    ///
    /// tier → target → tier must round-trip across the whole clamp range here
    /// too, and the target must sit exactly `log2(F) = 16` bits BELOW the dense
    /// target for the same tier — because the canonical threshold is `T · F`,
    /// not `T`. If these two ever coincided, one of the statements would be
    /// grinding `F` times the work it was credited for.
    #[test]
    fn moe_tier_target_round_trip_across_the_clamp_range() {
        use crate::aipow::{
            expected_attempts_per_win, pow2_target, target_for_attempts, AI_ATTEMPTS_MAX,
            AI_ATTEMPTS_MIN,
        };
        let moe_expected = |t: &[u8; 32]| expected_attempts_per_moe_win(t).unwrap_or(1.0);
        for a in AI_ATTEMPTS_MIN.ilog2()..=AI_ATTEMPTS_MAX.ilog2() {
            let tier = 1u64 << a;
            let target = target_for_attempts(tier, moe_expected)
                .unwrap_or_else(|| panic!("no target for canonical-MoE tier 2^{a}"));
            assert_eq!(
                expected_attempts_per_moe_win(&target).unwrap(),
                tier as f64,
                "canonical-MoE tier 2^{a} does not round-trip"
            );
            // Θ = T·F, so the tier's target is 2^(240−a), not 2^(256−a).
            assert_eq!(target, pow2_target(240 - a).unwrap(), "MoE tier 2^{a}");
            let dense = target_for_attempts(tier, expected_attempts_per_win).unwrap();
            assert_ne!(dense, target, "the two statements must not share a target");
            // Scoring the MoE tier's target with the dense formula overstates
            // the work by exactly F — the M5 mistake, pinned here per tier.
            assert_eq!(
                expected_attempts_per_win(&target) / tier as f64,
                MAC_EQUIV_PER_MOE_ATTEMPT
            );
        }
        // The shipped calibrated target is itself the 2^16 tier.
        assert_eq!(
            target_for_attempts(1 << 16, moe_expected).unwrap(),
            calibrated_moe_target()
        );
    }

    /// The MoE calibration measures attempt cost and reports a positive rate.
    /// Short budget — this is a unit test, not the measurement (that is
    /// `moe_grind_rate`).
    #[test]
    fn moe_calibration_measures_a_positive_rate() {
        let rate = calibrate_moe_attempts_per_sec(Duration::from_millis(150));
        assert!(rate > 0.0 && rate.is_finite(), "rate {rate}");
    }

    /// A generous target wins on the first ordinals, and the winners survive
    /// the scalar-oracle recheck. No proving (that is the registry integration
    /// test's job, ~25-30 s per certificate).
    #[test]
    fn grind_at_max_target_wins_immediately() {
        let ch = AiMoeChallenge {
            challenge: dev_challenge(),
            target: max_moe_target(),
            k: 2,
        };
        let out = grind_moe(&ch);
        assert_eq!(out.win_ordinals, vec![0, 1]);
        assert_eq!(out.total_attempts, 2);
        assert!(out.attempts_per_sec > 0.0);
    }
}
