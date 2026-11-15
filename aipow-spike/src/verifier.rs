//! Registry-side verifier for AI-PoW compact recursive certificates.
//!
//! This is the M5 Task-1 spike of the module the Nockmark registry will run on
//! every untrusted submission. It follows the node-side accept recipe (the
//! `derive_ai_pow_statement` path — see nockchain-v3
//! `crates/ai-pow/src/zk_bridge.rs` and the node verify in
//! `crates/ai-pow-miner/src/certificate_noun.rs`):
//!
//! 1. Decode the submission blob (postcard) and the compact certificate bytes
//!    (`ai_pow_zk::recursion::decode_compact_batch_recursive_certificate`,
//!    canonical-form-checked, 150 KB consensus cap).
//! 2. Check the certificate's verifier-key digest against the registry-pinned
//!    40-byte digest (never trust a prover-supplied setup).
//! 3. Re-derive the statement from trusted data only: re-synthesize the
//!    matrices from the challenge, re-derive kappa / matrix commitments /
//!    noise seeds / pow_key, and bind every public input via
//!    `ai_pow::zk_bridge::verify_ai_pow_full_matmul_production_statement`
//!    (rejects multi-tile params, wrong found_idx, wrong trace height, any
//!    PI mismatch, and HASH_JACKPOT > target).
//! 4. Re-derive the canonical Layer-0 program commitment from the opened
//!    schedule (never the prover's program) and run the compact
//!    cryptographic verify against the verifier-owned context.
//!
//! Nothing carried in the submission is trusted except as a claim to check.

use ai_pow::commit::matrix_commitment;
use ai_pow::fiat_shamir::{block_state, canonical_noise_seeds_from_matrix_commitments, commitment_key};
use ai_pow::params::MatmulParams;
use ai_pow::prover::params_tag;
use ai_pow::synth::synth_matrices;
use ai_pow::zk_bridge::{
    expected_layer0_rows_for_strip_schedule, verify_ai_pow_full_matmul_production_statement,
    zk_params_from_matmul, ZkPublicCommitments,
};
use ai_pow_zk::canonical::{canonical_program_for_strip_schedule, BlockPublic, StripIndexSchedule};
use ai_pow_zk::composite_public::CompositePublicInputs;
use ai_pow_zk::recursion::{
    canonical_l0_program_commitment_vals, compact_batch_verifier_key_digest_to_bytes,
    decode_compact_batch_recursive_certificate, verify_compact_batch_recursive_certificate_with_context,
    AiPowCompactBatchVerifierContext, AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES,
};
use ai_pow_zk::CircuitConfig;
use serde::{Deserialize, Serialize};

/// One win as submitted by a client: the extranonce plus the certificate and
/// its claimed statement metadata. Everything here is untrusted.
#[derive(Serialize, Deserialize)]
pub struct Submission {
    /// Client nonce (the M5 extranonce, little-endian bytes).
    pub nonce: Vec<u8>,
    /// Claimed solved tile index (must re-derive; 0 for the single-tile shape).
    pub found_idx: u32,
    /// Claimed Layer-0 trace height (checked against the schedule-derived one).
    pub trace_height: usize,
    /// Claimed Layer-0 public inputs (each slot re-derived and compared).
    pub pis: CompositePublicInputs,
    /// Compact recursive certificate, canonical postcard bytes (< 150 KB).
    pub cert_bytes: Vec<u8>,
}

/// Verify one untrusted submission end-to-end against a registry challenge.
///
/// `challenge` plays the `block_commitment` role AND the matrix seed
/// (M5 design: matrices are `synth_matrices(challenge, params)`).
/// `pinned_digest` is the registry-pinned 40-byte compact verifier-key digest.
/// `context` is the verifier-owned compact setup (built once at boot).
pub fn verify_submission(
    challenge: &[u8; 32],
    target: &[u8; 32],
    params: &MatmulParams,
    context: &AiPowCompactBatchVerifierContext,
    pinned_digest: &[u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
    submission_bytes: &[u8],
) -> Result<(), String> {
    // (1) Decode the submission and the certificate bytes.
    let sub: Submission =
        postcard::from_bytes(submission_bytes).map_err(|e| format!("submission decode: {e}"))?;
    let cert = decode_compact_batch_recursive_certificate(&sub.cert_bytes)
        .map_err(|e| format!("certificate decode: {e}"))?;

    // (2) Pin check: the certificate must declare the registry-pinned setup.
    let digest_bytes = compact_batch_verifier_key_digest_to_bytes(cert.verifier_key_digest());
    if &digest_bytes != pinned_digest {
        return Err("certificate verifier-key digest does not match pinned digest".to_string());
    }

    // (3) Re-derive the statement from trusted data only.
    let (a, b) = synth_matrices(challenge, params);
    let kappa = commitment_key(&block_state(challenge, &sub.nonce), &params_tag(params));
    let a_bytes: Vec<u8> = a.iter().map(|&v| v as u8).collect();
    let b_bytes: Vec<u8> = b.iter().map(|&v| v as u8).collect();
    let commitments = ZkPublicCommitments {
        h_a_chunk: matrix_commitment(&a_bytes, &kappa),
        h_b_chunk: matrix_commitment(&b_bytes, &kappa),
    };
    verify_ai_pow_full_matmul_production_statement(
        challenge,
        &sub.nonce,
        params,
        target,
        sub.found_idx,
        &commitments,
        &sub.pis,
        sub.trace_height,
    )
    .map_err(|e| format!("statement re-derivation: {e:?}"))?;

    // (4) Canonical L0 program commitment from the opened schedule, then the
    //     compact cryptographic verify against the verifier-owned context.
    let zk_params = zk_params_from_matmul(params);
    let (tile_i, tile_j) = params.tile_coords(sub.found_idx as u64);
    let schedule = StripIndexSchedule::from_tile(&zk_params, tile_i, tile_j)
        .map_err(|e| format!("strip schedule: {e}"))?;
    // Defense-in-depth: the statement check above already pinned trace_height
    // to the schedule-derived value; recompute so this module stands alone.
    let trace_height = expected_layer0_rows_for_strip_schedule(params, &schedule)
        .map_err(|e| format!("trace height: {e:?}"))?
        .required_trace_len();
    if trace_height != sub.trace_height {
        return Err("trace height does not match opened schedule".to_string());
    }
    let (s_a, s_b) = canonical_noise_seeds_from_matrix_commitments(
        &kappa,
        &commitments.h_a_chunk,
        &commitments.h_b_chunk,
    );
    let block_public = BlockPublic {
        tile_i,
        tile_j,
        kappa,
        s_a,
        s_b,
    };
    let program =
        canonical_program_for_strip_schedule(&zk_params, &schedule, &block_public, trace_height)
            .map_err(|e| format!("canonical program: {e}"))?;
    let profile = CircuitConfig::for_layer0_trace(trace_height);
    let l0_commitment = canonical_l0_program_commitment_vals(&zk_params, &profile, &program);

    verify_compact_batch_recursive_certificate_with_context(context, cert, &sub.pis, &l0_commitment)
        .map_err(|e| format!("compact certificate verify: {e:?}"))
}
