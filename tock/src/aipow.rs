//! AI-PoW track (M5): grind + prove for the Logos AI-PoW puzzle.
//!
//! Embeds the ai-pow crates as libraries (same pattern as nockapp /
//! zkvm-jetpack for the ZK track). The challenge plays the
//! `block_commitment` role AND the matrix seed: matrices are
//! `synth_matrices(challenge, params)`, so the workload is challenge-fresh
//! and the registry can re-derive every commitment. Extranonces are ground
//! strictly ascending through the exact production attempt path
//! (`BlockContext::build` + `mine_with_context_at_target` — every attempt
//! rebuilds the full nonce-bound context; cheap reuse across nonces would be
//! a PoW soundness bug upstream and would inflate our rate).
//!
//! Certificate proving is OUTSIDE the measured grind window (fixed ~24 s/win
//! overhead, not throughput): the window closes when the k-th win is found,
//! and each winning attempt's context is rebuilt afterwards (~2 ms each,
//! negligible) for `prove_ai_pow_compact_recursive_certificate`.

use std::time::{Duration, Instant};

use ai_pow::params::MatmulParams;
use ai_pow::prover::{mine_with_context_at_target, BlockContext};
use ai_pow::synth::synth_matrices;
use ai_pow::zk_bridge::{
    prove_ai_pow_compact_recursive_certificate,
    prove_ai_pow_compact_recursive_certificate_with_prover_cache,
    AiPowCompactRecursiveProverCache,
};
use ai_pow_zk::composite_public::CompositePublicInputs;
use ai_pow_zk::recursion::encode_compact_batch_recursive_certificate;
use serde::{Deserialize, Serialize};

/// Canonical single-tile AI-PoW shape (m=8, k=1024, n=8, r=64, tile=8) —
/// fixed by protocol version; clients must use exactly this. The registry's
/// verifier and the aipow-spike share the same constant; a change here is a
/// workload-version bump.
pub const AI_PARAMS: MatmulParams = MatmulParams {
    m: 8,
    k: 1024,
    n: 8,
    noise_rank: 64,
    tile: 8,
    spot_checks: 1,
    difficulty_bits: 0,
};

/// Identifier for the extranonce→nonce-bytes rule, the AI analog of
/// `nonce::NONCE_RULE`: the attempt nonce is the extranonce as exactly
/// 8 little-endian bytes. Versioned; the registry checks it on challenges.
pub const AI_NONCE_RULE: &str = "extranonce-le8-v1";

/// MAC-equivalents per attempt for the canonical single-tile shape
/// (F = 2^16, ai-pow params / design doc).
pub const MAC_EQUIV_PER_ATTEMPT: f64 = 65536.0;

/// One AI-track workload: challenge (block-commitment role + matrix seed),
/// jackpot target, and the number of wins to find.
pub struct AiChallenge {
    /// 32-byte challenge minted by the registry (or a dev constant locally).
    pub challenge: [u8; 32],
    /// Benchmark jackpot target T_b: a **little-endian** 256-bit bound
    /// (upstream `hash_le_target`); a win is `jackpot ≤ T_b`, unscaled —
    /// this is the effective threshold, not a consensus-style `T` that
    /// `attempt_wins` would multiply by the shape factor.
    pub target: [u8; 32],
    /// Wins required before the grind window closes.
    pub k: u64,
}

/// The per-win submission blob the registry verifies (M5 Task 3 wire
/// format): the compact certificate plus its claimed statement metadata
/// (found tile, trace height, Layer-0 public inputs). Everything here is a
/// CLAIM — the registry re-derives every slot it can from
/// `(challenge, extranonce)` and cryptographically binds the rest via the
/// compact STARK verify. This is the Task-1 spike's `Submission` shape with
/// the field order preserved: postcard encodes fields in declaration order,
/// so reordering or retyping any field is a wire-format break.
#[derive(Serialize, Deserialize)]
pub struct AiCertBlob {
    /// Attempt nonce bytes: the extranonce as exactly 8 LE bytes
    /// ([`AI_NONCE_RULE`]).
    pub nonce: Vec<u8>,
    /// Claimed solved tile index (always 0 for the canonical single-tile
    /// shape; the registry re-derives it).
    pub found_idx: u32,
    /// Claimed Layer-0 trace height (checked against the schedule-derived
    /// value).
    pub trace_height: usize,
    /// Claimed Layer-0 public inputs (each re-derivable slot is compared).
    pub pis: CompositePublicInputs,
    /// Compact recursive certificate, canonical postcard bytes (< 150 KB
    /// consensus cap).
    pub cert_bytes: Vec<u8>,
}

/// One jackpot win with its compact recursive certificate.
pub struct AiWin {
    pub extranonce: u64,
    /// Canonical postcard bytes of the compact certificate (< 150 KB cap).
    pub cert_bytes: Vec<u8>,
    /// Postcard bytes of the full [`AiCertBlob`] (certificate + statement
    /// metadata) — base64 this into `cert_b64` for `POST /run?track=ai`.
    pub submission_bytes: Vec<u8>,
    pub prove_ms: u64,
}

/// Grind phase output: which extranonces won and how fast attempts ran.
pub struct GrindResult {
    /// Winning extranonces, strictly ascending by construction.
    pub win_extranonces: Vec<u64>,
    /// Grind start to the k-th win found (prove time excluded by design).
    pub elapsed_ms: u64,
    /// Attempts evaluated, i.e. last winning extranonce + 1.
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

/// Full ai-bench summary: grind window plus per-win certificates.
pub struct AiBenchSummary {
    pub wins: Vec<AiWin>,
    pub grind_elapsed_ms: u64,
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

/// Fixed dev challenge for fully-local runs, derived like the spike's:
/// ai-pow's own keyed commitment primitive over a versioned label.
pub fn dev_challenge() -> [u8; 32] {
    ai_pow::commit::matrix_commitment(b"nockmark-m5-ai-bench-dev-challenge-v1", &[0u8; 32])
}

/// The v1 nonce encoding: extranonce as exactly 8 little-endian bytes.
/// Changing this means bumping [`AI_NONCE_RULE`].
pub fn extranonce_nonce(extranonce: u64) -> [u8; 8] {
    extranonce.to_le_bytes()
}

/// Inverse of [`extranonce_nonce`]; `None` unless exactly 8 bytes.
pub fn nonce_extranonce(nonce: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(nonce.try_into().ok()?))
}

/// Parse a 32-byte hex string (optionally 0x-prefixed) for --challenge /
/// --target.
pub fn parse_hex32(s: &str) -> Result<[u8; 32], String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars (32 bytes), got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
            .map_err(|e| format!("bad hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

pub fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Difficulty tiers (M6 Phase B2a) — statement-INDEPENDENT plumbing
// ---------------------------------------------------------------------------
//
// ## The problem tiers solve
//
// The AI board ranks `verified_mac_per_sec_lb`, which divides MAC-equivalents
// by the SERVER window — and that window contains ~25 s of certificate proving
// per win, a fixed per-win overhead that is not throughput. At the
// CPU-calibrated target a GPU finishes grinding in ~0.02 s, so its ranked rate
// is almost entirely proving and barely differs from a laptop's.
//
// The fix falls straight out of the arithmetic. Total MAC-equivalents for a run
// is `k · (2^256/(Θ+1)) · F`, so a harder target grants proportionally more MAC
// against the SAME fixed proving overhead. Let the client pick the target and
// the grind, not the proving, dominates the window — and the measured rate
// converges on the machine's true throughput.
//
// ## Why this is not a way to inflate a number
//
// Both directions are closed, and neither by a check that could be forgotten:
//
//   * An EASY tier scores LOWER. Fewer expected attempts per win means less
//     MAC-equivalent credit against the same k × ~25 s of proving, so there is
//     no incentive to ask for one — the cheapest tier is the worst score.
//   * A HARD tier cannot lift a machine above its real rate, because every
//     attempt it is credited for is genuinely performed: the credit is
//     `2^256/(Θ+1)` expected attempts per win, and a win only exists if the
//     jackpot actually cleared `Θ`. The registry verifies each win against the
//     target IT derived and IT issued, over a window IT observed. A machine
//     that asks for 2^30 and cannot do 2^30 attempts simply never submits.
//   * The top end is capped by the 1 h challenge expiry, expressed as
//     [`AI_ATTEMPTS_MAX`].
//
// The one thing a tier does change is the ratio of grind to proving inside the
// window, which is exactly the measurement error it exists to remove.

/// Tier floor: the easiest difficulty a client may request, as expected grind
/// attempts per win. 2^12 = 4096 attempts is a fraction of a second on any CPU
/// — useful for smoke tests and integration tests, and harmless to allow
/// because asking for it can only LOWER the resulting score.
pub const AI_ATTEMPTS_MIN: u64 = 1 << 12;

/// Tier ceiling: 2^30 ≈ 1.07e9 expected grind attempts per win.
///
/// The bound is the challenge expiry. AI challenges go stale 1 h after they are
/// minted (`aipow::AI_WINDOW_MS` server-side), and at ~3 M attempts/s — the
/// order a GPU backend is expected to reach on this shape — 2^30 attempts is
/// ~6 min of grinding per win, so even k=4 wins plus k certificate proves fits
/// inside the hour with a wide margin. A higher ceiling would let a client mint
/// a challenge it cannot possibly submit against, which reads as a server bug
/// rather than a client mistake.
///
/// The canonical-MoE statement has a second, independent bound: its grind rule
/// numbers attempts with a u32 ordinal, so `k ·` expected-attempts must stay
/// under 2^32 there whatever this constant says.
pub const AI_ATTEMPTS_MAX: u64 = 1 << 30;

/// How long the client's self-calibration grinds before choosing a tier.
/// Long enough to average over scheduler noise, short enough that nobody minds
/// paying it before the real run starts.
pub const AI_CALIBRATION_SECS: f64 = 2.0;

/// The grind budget a self-calibrating client sizes its tier for: ~60 s of
/// expected grinding across ALL k wins.
///
/// Chosen against the fixed ~25 s-per-win certificate proving the same server
/// window contains. At k=4 that is ~100 s of proving, so a ~60 s grind puts the
/// throughput term at ~40% of the window instead of the ~0% a GPU sees at the
/// CPU-calibrated target. Larger would be more accurate and slower; the 1 h
/// challenge expiry is the hard ceiling and [`AI_ATTEMPTS_MAX`] keeps every
/// tier far below it.
pub const AI_TIER_GRIND_BUDGET_SECS: f64 = 60.0;

/// Expected grind attempts per jackpot win at **little-endian** target `T`
/// under the DENSE (raw) comparison: `2^256 / (T+1)` — the jackpot hash is
/// uniform over 2^256 and a win is `hash ≤ T`, compared LE (see
/// [`AiChallenge::target`]). f64 precision (~1e-16 relative) is far below the
/// ±1σ Poisson noise floor of a k-win sample.
///
/// The canonical-MoE peer is [`crate::aipow_moe::expected_attempts_per_moe_win`],
/// which is `2^256/(T·F+1)` — the SCALED comparison. Conflating the two is a
/// factor-`F` error in every rate the board prints.
pub fn expected_attempts_per_win(target: &[u8; 32]) -> f64 {
    let mut t = 0.0f64;
    for &b in target.iter().rev() {
        t = t * 256.0 + b as f64;
    }
    2f64.powi(256) / (t + 1.0)
}

/// The tier granted for a requested expected-attempts-per-win: clamped to
/// [`AI_ATTEMPTS_MIN`]..=[`AI_ATTEMPTS_MAX`], then rounded to the NEAREST power
/// of two in log space (so `1.5 · 2^n` rounds up).
///
/// Powers of two only, because the target realizing a tier is then a single set
/// bit: `2^256/(2^b + 1)` is exactly `2^(256−b)` in f64 for every `b` in the
/// tier range (the `+1` vanishes below the mantissa), so tier → target → score
/// round-trips exactly instead of drifting with rounding. It also makes the
/// tier legible on the board as `2^N`.
///
/// Clamping is silent by design but never invisible: the registry echoes the
/// granted value back on the challenge as `attempts`, so a client can always
/// see what it actually got.
pub fn grant_attempts(requested: u64) -> u64 {
    let n = requested.clamp(AI_ATTEMPTS_MIN, AI_ATTEMPTS_MAX);
    if n.is_power_of_two() {
        return n;
    }
    let lo = 1u64 << n.ilog2();
    let hi = lo << 1;
    // Nearest in log space: n is closer to hi iff n² ≥ lo·hi. u128 keeps the
    // squares exact (n ≤ 2^30 here, but the intent is what matters).
    if (n as u128) * (n as u128) >= (lo as u128) * (hi as u128) {
        hi
    } else {
        lo
    }
}

/// The little-endian 32-byte target `2^bit` — a single set bit. `None` for
/// `bit ≥ 256`.
pub fn pow2_target(bit: u32) -> Option<[u8; 32]> {
    if bit >= 256 {
        return None;
    }
    let mut t = [0u8; 32];
    t[(bit / 8) as usize] = 1 << (bit % 8);
    Some(t)
}

/// Invert a statement's OWN expected-attempts function: the single-bit target
/// that yields exactly `attempts` expected grind attempts per win.
///
/// `expected` is passed in rather than selected here, so ONE inversion serves
/// both statements and both crates. That is the whole point. The dense rule
/// compares the jackpot against `T` raw and the canonical-MoE rule against
/// `T · F`, so the same tier is a different 32 bytes on each — but the tier can
/// never disagree with the score, because the target is derived by inverting
/// the very function the score is computed with. Re-deriving `T = 2^(256−a)`
/// (dense) and `T = 2^(240−a)` (MoE) inline would be two more places for the
/// factor-`F` asymmetry to be got wrong; M5 already lost 23 h to getting it
/// wrong once.
///
/// `attempts` must be a granted tier (see [`grant_attempts`]); anything else
/// returns `None` rather than a nearby target.
pub fn target_for_attempts(
    attempts: u64,
    expected: impl Fn(&[u8; 32]) -> f64,
) -> Option<[u8; 32]> {
    if attempts != grant_attempts(attempts) {
        return None;
    }
    let want = attempts as f64;
    // Expected attempts falls monotonically as the target grows, and within the
    // tier range exactly one single-bit target lands on a given power-of-two
    // tier — so scan for the exact hit. 256 evaluations of a few-ns function,
    // once per challenge.
    (0..256).find_map(|bit| {
        let t = pow2_target(bit)?;
        (expected(&t) == want).then_some(t)
    })
}

/// Pick a tier for a machine measured at `attempts_per_sec`, sized so the
/// expected grind across all `k` wins takes about
/// [`AI_TIER_GRIND_BUDGET_SECS`]. Always a granted tier.
pub fn tier_for_rate(attempts_per_sec: f64, k: u64) -> u64 {
    let per_win = attempts_per_sec * AI_TIER_GRIND_BUDGET_SECS / k.max(1) as f64;
    // `as u64` saturates on overflow; the NaN guard is the only case it does
    // not cover, and [`grant_attempts`] clamps both ends regardless.
    grant_attempts(if per_win.is_finite() && per_win > 0.0 {
        per_win as u64
    } else {
        0
    })
}

/// Measure this machine's DENSE attempt rate: grind a throwaway workload for
/// `budget` and count attempts.
///
/// Deliberately separate from [`grind`], and meant to be run BEFORE a challenge
/// is minted. The server window opens at mint, so a calibration performed after
/// it would be charged to the very rate it exists to measure; likewise it must
/// finish before [`grind`] starts its own clock, which it does by being a
/// different function called earlier. The throwaway target is zero —
/// unreachable, so the loop is pure attempt cost with no win/prove branch — and
/// the throwaway challenge is [`dev_challenge`], since attempt cost does not
/// depend on which 32 bytes seed the matrices.
pub fn calibrate_attempts_per_sec(budget: Duration) -> f64 {
    let challenge = dev_challenge();
    let (a, b) = synth_matrices(&challenge, &AI_PARAMS);
    // T = 0: a win would need jackpot ≤ 0, which never happens.
    let unreachable = [0u8; 32];
    let t0 = Instant::now();
    let mut attempts: u64 = 0;
    while t0.elapsed() < budget {
        let nonce = extranonce_nonce(attempts);
        let ctx = BlockContext::build(&challenge, &nonce, &a, &b, &AI_PARAMS)
            .expect("BlockContext::build");
        mine_with_context_at_target(&ctx, &challenge, &nonce, &unreachable)
            .expect("attempt evaluation");
        attempts += 1;
    }
    attempts as f64 / t0.elapsed().as_secs_f64().max(1e-9)
}

/// Grind extranonces 0, 1, 2, … strictly ascending (the fixed rule — the AI
/// analog of the ZK track's nonce-derivation rule) until `k` jackpot wins.
/// Panics on ai-pow errors: with the canonical params those only mean the
/// pinned crates moved under us and the workload must be re-pinned anyway.
pub fn grind(ch: &AiChallenge) -> GrindResult {
    assert!(ch.k >= 1, "k must be at least 1");
    assert_eq!(
        AI_PARAMS.num_tiles(),
        1,
        "canonical shape must be single-tile"
    );

    let (a, b) = synth_matrices(&ch.challenge, &AI_PARAMS);

    let t0 = Instant::now();
    let mut win_extranonces: Vec<u64> = Vec::with_capacity(ch.k as usize);
    let mut extranonce: u64 = 0;
    while (win_extranonces.len() as u64) < ch.k {
        let nonce = extranonce_nonce(extranonce);
        let ctx = BlockContext::build(&ch.challenge, &nonce, &a, &b, &AI_PARAMS)
            .expect("BlockContext::build");
        let won = mine_with_context_at_target(&ctx, &ch.challenge, &nonce, &ch.target)
            .expect("attempt evaluation")
            .is_some();
        if won {
            win_extranonces.push(extranonce);
            eprintln!(
                "  win {}/{} at extranonce {extranonce} ({:.1}s)",
                win_extranonces.len(),
                ch.k,
                t0.elapsed().as_secs_f64()
            );
        }
        extranonce = extranonce
            .checked_add(1)
            .expect("extranonce space exhausted");
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    let total_attempts = extranonce;
    GrindResult {
        win_extranonces,
        // Round UP like the ZK bench: never claim faster than measured.
        elapsed_ms: (elapsed_s * 1000.0).ceil().max(1.0) as u64,
        total_attempts,
        attempts_per_sec: total_attempts as f64 / elapsed_s.max(1e-9),
    }
}

/// Prove a compact recursive certificate for each winning extranonce.
/// The first prove builds the STARK setup (~24 s, ~4.3 GB peak RSS); it is
/// consumed into a prover cache and reused for the remaining wins.
pub fn prove_wins(ch: &AiChallenge, win_extranonces: &[u64]) -> Vec<AiWin> {
    // Single-tile grid: the eligible attempt tile is always index 0.
    let found_idx = 0u32;
    let (a, b) = synth_matrices(&ch.challenge, &AI_PARAMS);

    let mut cache: Option<AiPowCompactRecursiveProverCache> = None;
    let mut wins = Vec::with_capacity(win_extranonces.len());
    for &extranonce in win_extranonces {
        let nonce = extranonce_nonce(extranonce);
        let ctx = BlockContext::build(&ch.challenge, &nonce, &a, &b, &AI_PARAMS)
            .expect("BlockContext::build");
        let t0 = Instant::now();
        let run = match &cache {
            Some(cache) => prove_ai_pow_compact_recursive_certificate_with_prover_cache(
                &ctx, &AI_PARAMS, &nonce, &ch.target, found_idx, cache,
            ),
            None => prove_ai_pow_compact_recursive_certificate(
                &ctx, &AI_PARAMS, &nonce, &ch.target, found_idx,
            ),
        }
        .expect("prove compact recursive certificate");
        let prove_ms = t0.elapsed().as_millis() as u64;
        let cert_bytes = encode_compact_batch_recursive_certificate(run.certificate())
            .expect("encode compact certificate");
        eprintln!(
            "  cert for extranonce {extranonce}: {:.1}s ({} bytes)",
            prove_ms as f64 / 1000.0,
            cert_bytes.len()
        );
        let blob = AiCertBlob {
            nonce: nonce.to_vec(),
            found_idx,
            trace_height: run.trace_height(),
            pis: run.public_inputs().clone(),
            cert_bytes: cert_bytes.clone(),
        };
        let submission_bytes = postcard::to_allocvec(&blob).expect("encode submission blob");
        if cache.is_none() {
            cache = run.into_prover_cache();
        }
        wins.push(AiWin {
            extranonce,
            cert_bytes,
            submission_bytes,
            prove_ms,
        });
    }
    wins
}

/// The full local workload: grind to k wins, then certify each win.
pub fn run(ch: &AiChallenge) -> AiBenchSummary {
    eprintln!("grinding to {} win(s)…", ch.k);
    let grind = grind(ch);
    eprintln!(
        "grind window closed: {} attempts in {:.1}s",
        grind.total_attempts,
        grind.elapsed_ms as f64 / 1000.0
    );
    eprintln!("proving {} certificate(s)…", grind.win_extranonces.len());
    let wins = prove_wins(ch, &grind.win_extranonces);
    AiBenchSummary {
        wins,
        grind_elapsed_ms: grind.elapsed_ms,
        total_attempts: grind.total_attempts,
        attempts_per_sec: grind.attempts_per_sec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the v1 nonce encoding: exactly 8 bytes, little-endian. If this
    /// breaks, AI_NONCE_RULE must be bumped (plus the registry's rule).
    #[test]
    fn extranonce_nonce_round_trip_v1() {
        for x in [0u64, 1, 0xff, 0x0123_4567_89ab_cdef, u64::MAX] {
            assert_eq!(nonce_extranonce(&extranonce_nonce(x)), Some(x));
        }
        assert_eq!(extranonce_nonce(1), [1, 0, 0, 0, 0, 0, 0, 0], "LE order");
        assert_eq!(nonce_extranonce(&[0u8; 7]), None, "short nonce rejected");
        assert_eq!(nonce_extranonce(&[0u8; 9]), None, "long nonce rejected");
    }

    #[test]
    fn hex32_round_trip_and_rejects() {
        let ch = dev_challenge();
        assert_eq!(parse_hex32(&hex32(&ch)).unwrap(), ch);
        assert_eq!(parse_hex32(&format!("0x{}", hex32(&ch))).unwrap(), ch);
        assert!(parse_hex32("ff").is_err(), "wrong length");
        assert!(parse_hex32(&"zz".repeat(32)).is_err(), "bad digits");
    }

    /// Max target ⇒ every attempt clears the jackpot bound, so the grind must
    /// win on extranonces 0..k with exactly k attempts. (Certificate proving
    /// is NOT exercised here — ~24 s / 4.3 GB per cert; the end-to-end
    /// integration test lands with the registry in Task 4.)
    #[test]
    fn grind_at_max_target_wins_immediately() {
        let ch = AiChallenge {
            challenge: dev_challenge(),
            target: [0xff; 32],
            k: 2,
        };
        let out = grind(&ch);
        assert_eq!(out.win_extranonces, vec![0, 1]);
        assert_eq!(out.total_attempts, 2);
        assert!(out.attempts_per_sec > 0.0);
    }

    /// Tiers are powers of two, clamped at both ends, and clamping is what a
    /// request outside the range gets — never an error and never the raw value.
    #[test]
    fn grant_attempts_rounds_to_pow2_and_clamps_both_ends() {
        assert_eq!(AI_ATTEMPTS_MIN, 4096);
        assert_eq!(AI_ATTEMPTS_MAX, 1 << 30);
        // Below the floor (including 0) and above the ceiling.
        assert_eq!(grant_attempts(0), AI_ATTEMPTS_MIN);
        assert_eq!(grant_attempts(1), AI_ATTEMPTS_MIN);
        assert_eq!(grant_attempts(4095), AI_ATTEMPTS_MIN);
        assert_eq!(grant_attempts(u64::MAX), AI_ATTEMPTS_MAX);
        assert_eq!(grant_attempts((1 << 30) + 1), AI_ATTEMPTS_MAX);
        // Exact tiers pass through untouched, across the whole range.
        for a in 12..=30u32 {
            assert_eq!(grant_attempts(1 << a), 1 << a, "2^{a}");
        }
        // Nearest in LOG space: the midpoint of [2^13, 2^14] is 2^13.5 ≈ 11585.
        assert_eq!(grant_attempts(11_584), 1 << 13);
        assert_eq!(grant_attempts(11_586), 1 << 14);
        // …and the arithmetic midpoint 12288 is therefore rounded UP.
        assert_eq!(grant_attempts(12_288), 1 << 14);
    }

    /// tier → target → tier round-trips exactly for the DENSE semantics across
    /// the whole clamp range. (The canonical-MoE half of this lives in
    /// `aipow_moe::tests`, and both statements are round-tripped together
    /// registry-side in `nockmark_registry::aipow::tests`.)
    #[test]
    fn dense_tier_target_round_trip_across_the_clamp_range() {
        for a in 12..=30u32 {
            let tier = 1u64 << a;
            let target = target_for_attempts(tier, expected_attempts_per_win)
                .unwrap_or_else(|| panic!("no target for dense tier 2^{a}"));
            assert_eq!(
                expected_attempts_per_win(&target),
                tier as f64,
                "dense tier 2^{a} does not round-trip"
            );
            // Dense compares the jackpot raw, so the tier's target is 2^(256−a).
            assert_eq!(target, pow2_target(256 - a).unwrap(), "dense tier 2^{a}");
        }
        // Only granted tiers derive a target; a near-miss is rejected rather
        // than silently rounded into something else.
        assert_eq!(target_for_attempts(3000, expected_attempts_per_win), None);
        assert_eq!(target_for_attempts(12_288, expected_attempts_per_win), None);
        assert_eq!(target_for_attempts(1 << 31, expected_attempts_per_win), None);
    }

    #[test]
    fn pow2_target_is_little_endian_and_bounded() {
        assert_eq!(pow2_target(0).unwrap()[0], 1);
        assert_eq!(pow2_target(248).unwrap()[31], 1);
        // 2^248 is the dense default target, expected 256 attempts per win.
        assert_eq!(expected_attempts_per_win(&pow2_target(248).unwrap()), 256.0);
        assert_eq!(pow2_target(256), None);
    }

    /// The calibration heuristic: a tier sized so k wins expect ~60 s of grind,
    /// from a synthetic rate (no grinding here — the measurement itself is the
    /// slow part and is exercised by the real local run).
    #[test]
    fn tier_for_rate_targets_the_grind_budget() {
        // A CPU at 15 000 attempts/s, k=4: 15000·60/4 = 225 000 ⇒ nearest tier
        // 2^18 = 262 144, i.e. ~70 s of expected grinding for the four wins.
        let tier = tier_for_rate(15_000.0, 4);
        assert_eq!(tier, 1 << 18);
        let grind_secs = tier as f64 * 4.0 / 15_000.0;
        assert!(
            (40.0..=100.0).contains(&grind_secs),
            "{grind_secs}s is not within a rounding step of the 60 s budget"
        );
        // A GPU-class rate at k=1 wants far more work, and gets it: 3e6·60 =
        // 1.8e8, whose nearest tier in log space is 2^27 (2^27.4).
        assert_eq!(tier_for_rate(3.0e6, 1), 1 << 27);
        assert_eq!(tier_for_rate(3.0e8, 1), AI_ATTEMPTS_MAX, "clamped, not wild");
        // A hopeless machine still gets a runnable tier rather than 0 or NaN.
        assert_eq!(tier_for_rate(1.0, 4), AI_ATTEMPTS_MIN);
        assert_eq!(tier_for_rate(0.0, 1), AI_ATTEMPTS_MIN);
        assert_eq!(tier_for_rate(f64::NAN, 1), AI_ATTEMPTS_MIN);
        // Every rate yields a tier that has a target.
        for rate in [1.0, 3_394.0, 15_000.0, 3.0e6, 1.0e12] {
            let t = tier_for_rate(rate, 4);
            assert!(target_for_attempts(t, expected_attempts_per_win).is_some());
        }
    }

    /// The calibration grind measures attempt cost and nothing else: it must
    /// never find a win (the throwaway target is unreachable) and must report a
    /// positive rate. Short budget — this is a unit test, not the measurement.
    #[test]
    fn calibration_measures_a_positive_rate() {
        let rate = calibrate_attempts_per_sec(Duration::from_millis(150));
        assert!(rate > 0.0 && rate.is_finite(), "rate {rate}");
    }
}
