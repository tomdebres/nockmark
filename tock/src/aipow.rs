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

use std::time::Instant;

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
}
