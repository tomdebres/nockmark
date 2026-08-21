//! AI-PoW track, **canonical-MoE statement**, server side (M6 Phase B1).
//!
//! The peer of [`crate::aipow`]'s dense verifier. Same trust story, same store,
//! same board — a different statement, verified by its own rules and priced in
//! the same unit. See [`tock::aipow_moe`] for the client half and for why the
//! two statements share one leaderboard.
//!
//! ## Which verify entrypoint, and why
//!
//! Three routes into a canonical-MoE certificate exist upstream:
//!
//! 1. `certificate_noun::verify_decoded_ai_pow_pearl_merge_compact_moe_artifact_…`
//!    — the consensus verify the `%ai-pow-verify` jet calls. It is
//!    `pub(crate)`, and it takes the *noun/jam artifact* wire form, so using it
//!    would mean shipping a jammed artifact over our HTTP API and reconstructing
//!    the node's `PearlMergeAiPowVerifierContext`. Same reason M5 rejected the
//!    `certificate_noun` route for the dense track.
//! 2. `zk_bridge::verify_pearl_moe_recursive_certificate` — the large-checkpoint
//!    regression path, `#[cfg(test)]` upstream and never compiled into a release
//!    binary.
//! 3. **`zk_bridge::verify_pearl_moe_compact_recursive_certificate`** — the
//!    proof half of route 1, public, and exactly what route 1 calls at its step
//!    (5). This is the one we use.
//!
//! Route 3 alone is not the whole accept decision, so [`verify_moe_submission`]
//! reassembles the rest of route 1's recipe around it:
//! `verify_pearl_moe_compatible_work` (MoE envelope + routing-consistency
//! binding + the difficulty gate on the authenticated jackpot), then route 1's
//! step (a) — binding `pis.hash_jackpot` to the statement's `hash_jackpot`,
//! without which the difficulty gate would not be about the proven tile.
//!
//! ## Why this is *stronger* than the node's version
//!
//! The node receives a statement and authenticates it: it checks the Pearl aux
//! inclusion proof binds the header's coinbase to the candidate block
//! commitment. We never receive a statement at all. The registry RE-DERIVES the
//! entire canonical statement — synthetic Pearl header, mining config, matrix
//! commitments, routing, opened indices, and the jackpot — from
//! `(challenge, ordinal)` with `canonical_moe_statement_parts`, and the
//! submission carries only the certificate and its Layer-0 public inputs. There
//! is no aux-binding step here because there is nothing prover-supplied left to
//! bind: the challenge is ours, the ordinal is ours, and everything downstream
//! of them is a pure function we compute ourselves.

use std::path::{Path, PathBuf};

use ai_pow::pearl_compat::verify_pearl_moe_compatible_work;
use ai_pow::zk_bridge::{
    prove_pearl_moe_compact_recursive_certificate, verify_pearl_moe_compact_recursive_certificate,
    PearlMoeCompactProveRun,
};
use ai_pow_miner::canonical::canonical_moe_statement_parts;
use ai_pow_zk::recursion::{
    compact_batch_verifier_key_digest_to_bytes, decode_compact_batch_recursive_certificate,
    encode_compact_batch_recursive_certificate, AiPowCompactBatchVerifierContext,
    AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES,
};
use tock::aipow_moe::{
    max_moe_target, moe_prove_inputs, AiMoeCertBlob, AI_MOE_E, AI_MOE_HW, AI_MOE_MAX_PATTERN_LEN,
    AI_MOE_PARAMS, AI_MOE_TOP_K,
};

/// Submission blob cap for the canonical-MoE statement.
///
/// The blob is a compact certificate (150 KB consensus cap) plus the Layer-0
/// public inputs and the ordinal — no statement metadata at all, since the
/// registry re-derives the statement. Same 154 KiB bound as the dense path;
/// enforced before any decode work.
pub const AI_MOE_SUBMISSION_BLOB_MAX: usize = 154 * 1024;

/// The instance's canonical-MoE target: `NOCKMARK_AI_MOE_TARGET` if set and
/// valid hex32, else [`tock::aipow_moe::calibrated_moe_target`] (`2^224`,
/// ≈ 65 536 attempts ≈ 19 s of single-thread CPU grinding per win — see that
/// function for the measurement the number comes from).
///
/// A canonical-MoE target is NOT interchangeable with the dense track's
/// `NOCKMARK_AI_TARGET`: this one prices ONE MAC-equivalent and is scaled by
/// `F = 2^16` before the jackpot is compared, so the same 32 bytes mean `F`
/// times more grinding on the dense path. Hence a separate variable rather than
/// a shared one.
pub fn moe_target_from_env() -> [u8; 32] {
    std::env::var("NOCKMARK_AI_MOE_TARGET")
        .ok()
        .and_then(|s| tock::aipow::parse_hex32(&s).ok())
        .unwrap_or_else(tock::aipow_moe::calibrated_moe_target)
}

/// `[u32; 8]` little-endian digest words → 32 bytes. Upstream's
/// `digest_words_to_bytes` (private to `certificate_noun`), restated: the
/// public inputs carry digests as LE u32 words and the statement carries them
/// as bytes, so the jackpot binding has to cross that boundary.
fn digest_words_to_bytes(words: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, word) in words.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Verifier: canonical-MoE compact setup + full submission verify
// ---------------------------------------------------------------------------

/// The verifier-owned compact setup for the canonical-MoE statement, plus the
/// 40-byte verifier-key digest re-derived from it.
///
/// Structurally identical to [`crate::aipow::AiVerifier`] and built the same
/// way (lazily, once, on the first submission; persisted; pin-scoped). It is
/// nonetheless a SEPARATE context: the compact setup encodes the recursion
/// circuit for a Layer-0 trace bucket, and while the canonical MoE shape opens
/// the same 8×8×1024 tile as the dense benchmark, "the two contexts happen to
/// coincide" is not a property either crate promises. Keeping them apart costs
/// one file on the volume and removes a whole class of silent cross-statement
/// breakage at re-pin time.
pub struct AiMoeVerifier {
    context: AiPowCompactBatchVerifierContext,
    pinned_digest: [u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
}

impl AiMoeVerifier {
    /// Where the serialized canonical-MoE context lives on the persistent
    /// volume. Pin-scoped for the same reason as the dense one: the context
    /// encodes the AIR at the pinned nockchain commit, so a re-pin must miss
    /// this path and rebuild rather than load a stale blob.
    pub fn context_path(data_dir: &Path) -> PathBuf {
        data_dir.join(format!(
            "aipow-moe-verifier-context-{}.bin",
            tock::miner::NOCKCHAIN_PIN
        ))
    }

    /// Load the canonical-MoE compact verifier context from `data_dir`, else
    /// build it by proving one throwaway canonical block (~25-30 s, multi-GB
    /// peak) and persist it. Blocking and CPU/RAM heavy: call from
    /// `spawn_blocking`, once, lazily (first canonical-MoE submission).
    pub fn load_or_build(data_dir: &Path) -> Result<Self, String> {
        let path = Self::context_path(data_dir);
        let context = match std::fs::read(&path) {
            Ok(bytes) => {
                let (context, _): (AiPowCompactBatchVerifierContext, usize) =
                    bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                        .map_err(|e| format!("decode {}: {e}", path.display()))?;
                context
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => build_context_by_proving(&path)?,
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let digest = context
            .validate_setup_binding()
            .map_err(|e| format!("moe verifier context failed setup binding: {e:?}"))?;
        Ok(Self {
            pinned_digest: compact_batch_verifier_key_digest_to_bytes(&digest),
            context,
        })
    }

    /// Verify one canonical-MoE win end-to-end (blocking): see
    /// [`verify_moe_submission`].
    pub fn verify(
        &self,
        challenge: &[u8; 32],
        target: &[u8; 32],
        ordinal: u32,
        blob_bytes: &[u8],
    ) -> Result<(), String> {
        verify_moe_submission(
            challenge,
            target,
            &self.context,
            &self.pinned_digest,
            ordinal,
            blob_bytes,
        )
    }
}

/// Build the canonical-MoE compact verifier context by proving one throwaway
/// canonical block, then persist it for the next boot.
///
/// Upstream's `prove_canonical_moe_block_at` discards the verifier context it
/// produced and the variant that returns it is `#[cfg(test)] pub(crate)`, so
/// the context is reached through `zk_bridge`'s prover with the canonical
/// inputs assembled by [`moe_prove_inputs`]. Before the context is persisted it
/// is used to verify its own certificate through the full
/// [`verify_moe_submission`] path — if the input assembly had drifted from the
/// crate's, this fails loudly at build time instead of rejecting every honest
/// submission later.
fn build_context_by_proving(path: &Path) -> Result<AiPowCompactBatchVerifierContext, String> {
    eprintln!(
        "aipow-moe: no verifier context at {} — building by proving (~25-30 s)…",
        path.display()
    );
    let challenge = *blake3::hash(b"nockmark-ai-moe-v1 verifier-context-build").as_bytes();
    let ordinal = 0u32;
    let inputs = moe_prove_inputs(&challenge, ordinal)?;
    let run = prove_pearl_moe_compact_recursive_certificate(
        &inputs.params,
        &inputs.a,
        &inputs.b,
        &inputs.commitments.kappa,
        &inputs.commitments.h_a,
        &inputs.commitments.h_b,
        &inputs.routing,
        0,
        &inputs.inner,
        &inputs.local_b,
        inputs.n_e,
    )
    .map_err(|e| format!("moe context prove: {e:?}"))?;
    let PearlMoeCompactProveRun {
        compact_cert,
        verifier_context,
        pis,
        ..
    } = run;

    // Self-check: verify the freshly proved certificate through the real
    // submission path against a target every jackpot clears. This is what turns
    // a drifted `moe_prove_inputs` into a boot-time error.
    let cert_bytes = encode_compact_batch_recursive_certificate(&compact_cert)
        .map_err(|e| format!("moe context encode cert: {e:?}"))?;
    let blob = postcard::to_allocvec(&AiMoeCertBlob {
        ordinal,
        pis,
        cert_bytes,
    })
    .map_err(|e| format!("moe context blob encode: {e}"))?;
    let digest = verifier_context
        .validate_setup_binding()
        .map_err(|e| format!("moe verifier context failed setup binding: {e:?}"))?;
    verify_moe_submission(
        &challenge,
        &max_moe_target(),
        &verifier_context,
        &compact_batch_verifier_key_digest_to_bytes(&digest),
        ordinal,
        &blob,
    )
    .map_err(|e| format!("moe context self-verify failed (inputs drifted from upstream?): {e}"))?;

    let bytes = bincode::serde::encode_to_vec(&verifier_context, bincode::config::standard())
        .map_err(|e| format!("moe context serialize: {e}"))?;
    // Write-then-rename so a crash mid-write cannot leave a torn blob for the
    // next boot's fast path.
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))?;
    eprintln!(
        "aipow-moe: verifier context built and persisted ({:.1} MB)",
        bytes.len() as f64 / (1024.0 * 1024.0)
    );
    Ok(verifier_context)
}

/// Verify one untrusted canonical-MoE win end-to-end against a registry
/// challenge.
///
/// Accept recipe — the node's MoE accept path with its statement-authentication
/// prefix replaced by statement RE-DERIVATION (see the module docs):
///
/// 1. Size-cap and decode the blob; bind its ordinal to the win's claimed
///    ordinal.
/// 2. Re-derive the whole canonical statement from `(challenge, ordinal)` —
///    header (whose coinbase carries this challenge as the aux commitment),
///    mining config, `H_A`/`H_B`, routing, opened indices, and the real
///    `hash_jackpot`. Nothing here comes from the client.
/// 3. `verify_pearl_moe_compatible_work`: MoE envelope, routing-consistency
///    binding, and the difficulty gate `jackpot ≤ target · F` — the SCALED
///    comparison, unlike the dense track's raw one.
/// 4. Bind `pis.hash_jackpot` to the statement's jackpot, so the gate in (3) is
///    about the tile the certificate actually proves.
/// 5. Pin check: both the certificate and the verifier context must declare the
///    registry's own 40-byte setup digest.
/// 6. `verify_pearl_moe_compact_recursive_certificate`: routing-spliced `s_A`
///    and public-input binding, the opened-schedule program-commitment fold
///    (never the prover's program), then the compact cryptographic verify.
pub fn verify_moe_submission(
    challenge: &[u8; 32],
    target: &[u8; 32],
    context: &AiPowCompactBatchVerifierContext,
    pinned_digest: &[u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
    ordinal: u32,
    blob_bytes: &[u8],
) -> Result<(), String> {
    // (1) Decode the blob and bind it to the claimed ordinal.
    if blob_bytes.len() > AI_MOE_SUBMISSION_BLOB_MAX {
        return Err(format!(
            "submission blob {} bytes exceeds {AI_MOE_SUBMISSION_BLOB_MAX}",
            blob_bytes.len()
        ));
    }
    let sub: AiMoeCertBlob =
        postcard::from_bytes(blob_bytes).map_err(|e| format!("submission decode: {e}"))?;
    if sub.ordinal != ordinal {
        return Err("blob ordinal does not match the win's ordinal".to_string());
    }

    // (2) Re-derive the statement from trusted data only.
    let (public, moe_art) = canonical_moe_statement_parts(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        *challenge,
        ordinal,
    )
    .map_err(|e| format!("statement re-derivation: {e}"))?;

    // (3) The node's MoE work precheck: envelope + routing binding + the
    //     jackpot difficulty gate at `target · F`.
    let work = verify_pearl_moe_compatible_work(
        &public,
        &moe_art.moe,
        &moe_art.routing_data,
        target,
        AI_MOE_MAX_PATTERN_LEN,
    )
    .map_err(|e| format!("moe work precheck: {e:?}"))?;

    // (4) The gated jackpot must be the PROVEN one.
    if digest_words_to_bytes(&sub.pis.hash_jackpot) != public.hash_jackpot {
        return Err("certificate public inputs do not carry the statement's jackpot".to_string());
    }

    // (5) Pin checks: never trust a prover-supplied setup, and never verify
    //     against a context this server did not boot with.
    let cert = decode_compact_batch_recursive_certificate(&sub.cert_bytes)
        .map_err(|e| format!("certificate decode: {e}"))?;
    if &compact_batch_verifier_key_digest_to_bytes(cert.verifier_key_digest()) != pinned_digest {
        return Err("certificate verifier-key digest does not match pinned digest".to_string());
    }
    if &compact_batch_verifier_key_digest_to_bytes(context.verifier_key_digest()) != pinned_digest {
        return Err("verifier context digest does not match pinned digest".to_string());
    }

    // (6) Proof half.
    verify_pearl_moe_compact_recursive_certificate(
        context,
        cert,
        &sub.pis,
        &AI_MOE_PARAMS,
        &work.commitments.kappa,
        &work.commitments.h_a,
        &work.commitments.h_b,
        &public.mining_config,
        &moe_art.moe,
        public.m,
        public.n,
        public.t_rows,
        public.t_cols,
        &moe_art.routing_data,
        AI_MOE_MAX_PATTERN_LEN,
    )
    .map_err(|e| format!("compact certificate verify: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_words_round_trip_is_little_endian() {
        let words = [0x0403_0201u32, 0, 0, 0, 0, 0, 0, 0x1211_100f];
        let bytes = digest_words_to_bytes(&words);
        assert_eq!(&bytes[0..4], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&bytes[28..32], &[0x0f, 0x10, 0x11, 0x12]);
    }

    /// The MoE target env override, and the fact that it is a DIFFERENT
    /// variable from the dense one. (Env is process-global; this test restores
    /// what it sets.)
    #[test]
    fn moe_target_env_default() {
        std::env::remove_var("NOCKMARK_AI_MOE_TARGET");
        assert_eq!(
            moe_target_from_env(),
            tock::aipow_moe::calibrated_moe_target()
        );
        // Setting the DENSE variable must not move the MoE target.
        std::env::set_var("NOCKMARK_AI_TARGET", &"f".repeat(64));
        assert_eq!(
            moe_target_from_env(),
            tock::aipow_moe::calibrated_moe_target()
        );
        std::env::remove_var("NOCKMARK_AI_TARGET");
        std::env::set_var(
            "NOCKMARK_AI_MOE_TARGET",
            tock::aipow::hex32(&max_moe_target()),
        );
        assert_eq!(moe_target_from_env(), max_moe_target());
        std::env::remove_var("NOCKMARK_AI_MOE_TARGET");
    }

}
