//! M7 step 4: server-side verification of **dense-peak** wins — mainnet's
//! production AI shape (`tock::aipow_peak`).
//!
//! ## What the server runs
//!
//! The client submits each win as the jammed `%ai-pow` block artifact a node
//! receives. Per win, cheapest first:
//!
//! 1. size cap ([`AI_PEAK_SUBMISSION_BLOB_MAX`]) and the attempt index split
//!    into `(extranonce, ordinal)`;
//! 2. upstream's bounded decode (`decode_ai_pow_pearl_merge_artifact_jam_with_rules`,
//!    Hardened) — the jet's own cue and shape limits;
//! 3. **statement pins** ([`check_peak_statement`]): a dense (not MoE)
//!    artifact; the dense-peak mining config (16×16 tiles over k=8192,
//!    r=512 — which is what makes `F = 2^21`); m=4096, n=32768; the tile the
//!    claimed ordinal names; and the Pearl header THIS challenge has at the
//!    claimed extranonce. These bind the stored attempt index to the ticket and
//!    the row to the shape it is labelled with. Consensus itself pins none of
//!    them — a node accepts any admissible shape and header.
//! 4. **upstream's consensus verifier**:
//!    `verify_ai_pow_block_artifact_with_verification_rules(.., ConsensusBatch)`,
//!    the function behind the `%ai-pow-verify-v2` jet — admission limits,
//!    canonical FRI witnesses, the 2^19 trace cap, aux-inclusion binding of the
//!    challenge as the block commitment, `jackpot ≤ T·F`, and the compact
//!    recursive STARK — against the registry's own context and pinned
//!    verifier-key digest (so a Legacy-rules certificate fails on its digest).
//!
//! The matrices are miner-chosen in consensus (the verifier binds the
//! committed `H_A`/`H_B` and the opened strips, never the model), and so they
//! are here: a ticket over other matrices costs the same `F` MACs.
//!
//! ## The context
//!
//! Derived from the shape alone, the way ai-pow-jets builds its dense setup
//! (`build_verifier_setup_descriptor_dense`): `derive_compact_verifier_context_for_shape`
//! at `pearl_dense_canonical_trace_height(P, 0, 0)` = 2^17 under Hardened
//! rules. No proving and no matrices, but ~46 s of CPU, so it is persisted on
//! the volume (pin-scoped, swept at boot like the other contexts) and loaded
//! on the first dense-peak submission after a restart.

use std::path::{Path, PathBuf};

use ai_pow::pearl_compat::{PearlIncompleteBlockHeader, PearlPublicProofParams};
use ai_pow::zk_bridge::{pearl_dense_canonical_trace_height, zk_params_from_matmul};
use ai_pow_miner::certificate_noun::{
    decode_ai_pow_pearl_merge_artifact_jam_with_rules,
    verify_ai_pow_block_artifact_with_verification_rules, AiPowBlockVerifyOutcome,
    CertificateNounLimits,
};
use ai_pow_zk::proof_rules::{ProofRules, VerificationRules};
use ai_pow_zk::recursion::{
    compact_batch_verifier_key_digest_to_bytes, derive_compact_verifier_context_for_shape,
    AiPowCompactBatchVerifierContext, AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES,
};
use tock::aipow_peak::{
    expected_peak_header, peak_mining_config, split_attempt_index, tile_offsets,
    AI_PEAK_MAX_PATTERN_LEN, AI_PEAK_PARAMS, AI_PEAK_SUBMISSION_BLOB_MAX,
};

/// The registry's dense-peak verifier setup and the digest it pins.
pub struct AiPeakVerifier {
    context: AiPowCompactBatchVerifierContext,
    pinned_digest: [u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
}

impl AiPeakVerifier {
    /// Where the context lives on the volume; pin-scoped like the others, so
    /// a re-pin misses it and `aipow::sweep_stale_contexts` removes it.
    pub fn context_path(data_dir: &Path) -> PathBuf {
        data_dir.join(format!(
            "aipow-peak-verifier-context-{}.bin",
            tock::miner::NOCKCHAIN_PIN
        ))
    }

    /// Load the persisted context from `data_dir`, else derive it (~46 s) and
    /// persist it (write, then rename). Blocking; call from `spawn_blocking`.
    pub fn load_or_build(data_dir: &Path) -> Result<Self, String> {
        let path = Self::context_path(data_dir);
        let context = match std::fs::read(&path) {
            Ok(bytes) => {
                let (context, _): (AiPowCompactBatchVerifierContext, usize) =
                    bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                        .map_err(|e| format!("decode {}: {e}", path.display()))?;
                context
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("aipow-peak: no verifier context at {} — deriving (~46 s)…", path.display());
                let context = derive_context()?;
                let bytes = bincode::serde::encode_to_vec(&context, bincode::config::standard())
                    .map_err(|e| format!("dense-peak context serialize: {e}"))?;
                let tmp = path.with_extension("bin.tmp");
                std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
                std::fs::rename(&tmp, &path)
                    .map_err(|e| format!("rename to {}: {e}", path.display()))?;
                eprintln!(
                    "aipow-peak: verifier context derived and persisted ({:.1} KB)",
                    bytes.len() as f64 / 1024.0
                );
                context
            }
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        Self::from_context(context)
    }

    /// Derive the context in memory only (tests).
    pub fn build() -> Result<Self, String> {
        Self::from_context(derive_context()?)
    }

    fn from_context(context: AiPowCompactBatchVerifierContext) -> Result<Self, String> {
        let digest = context
            .validate_setup_binding()
            .map_err(|e| format!("dense-peak verifier context failed setup binding: {e:?}"))?;
        crate::aipow::require_hardened(context.proof_rules())?;
        Ok(Self {
            pinned_digest: compact_batch_verifier_key_digest_to_bytes(&digest),
            context,
        })
    }

    pub fn context(&self) -> &AiPowCompactBatchVerifierContext {
        &self.context
    }

    pub fn pinned_digest(&self) -> &[u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES] {
        &self.pinned_digest
    }

    /// Verify one dense-peak win (blocking): see [`verify_peak_submission`].
    pub fn verify(
        &self,
        challenge: &[u8; 32],
        target: &[u8; 32],
        attempt_index: u64,
        artifact_jam: &[u8],
    ) -> Result<(), String> {
        verify_peak_submission(
            challenge,
            target,
            &self.context,
            &self.pinned_digest,
            attempt_index,
            artifact_jam,
        )
    }
}

/// The largest k whose dense-peak submission fits a `body_limit`-byte
/// request: k wins at the per-artifact cap, base64'd, plus 1 KiB per win and
/// 4 KiB overall for the JSON around them. 19 for the registry's 4 MiB.
pub fn max_k_for_body(body_limit: usize) -> u64 {
    let per_win = AI_PEAK_SUBMISSION_BLOB_MAX.div_ceil(3) * 4 + 1024;
    (body_limit.saturating_sub(4096) / per_win) as u64
}

/// The Hardened compact verifier context for the dense-peak trace height.
fn derive_context() -> Result<AiPowCompactBatchVerifierContext, String> {
    let trace_height = pearl_dense_canonical_trace_height(&AI_PEAK_PARAMS, 0, 0)
        .map_err(|e| format!("dense-peak trace height: {e:?}"))?;
    derive_compact_verifier_context_for_shape(
        &zk_params_from_matmul(&AI_PEAK_PARAMS),
        trace_height,
        ProofRules::Hardened,
    )
    .map_err(|e| format!("derive dense-peak verifier context: {e:?}"))
}

/// The statement pins (step 3 of the module docs) on an already-decoded
/// statement. Pure, so each pin is tested on its own.
pub fn check_peak_statement(
    public: &PearlPublicProofParams,
    expected_header: &PearlIncompleteBlockHeader,
    ordinal: u64,
) -> Result<(), String> {
    if public.mining_config != peak_mining_config() {
        return Err(
            "mining config is not dense-peak's (16x16 tiles over k=8192, r=512)".to_string(),
        );
    }
    if (public.m, public.n) != (AI_PEAK_PARAMS.m, AI_PEAK_PARAMS.n) {
        return Err(format!(
            "matrix dims {}x{} are not dense-peak's {}x{}",
            public.m, public.n, AI_PEAK_PARAMS.m, AI_PEAK_PARAMS.n
        ));
    }
    let tile = tile_offsets(ordinal).ok_or("tile ordinal out of range")?;
    if (public.t_rows, public.t_cols) != tile {
        return Err("opened tile does not match the claimed attempt index".to_string());
    }
    if public.block_header != *expected_header {
        return Err(
            "Pearl header is not this challenge's header at the claimed extranonce".to_string(),
        );
    }
    Ok(())
}

/// Verify one untrusted dense-peak win against a registry challenge: the
/// recipe in the module docs.
pub fn verify_peak_submission(
    challenge: &[u8; 32],
    target: &[u8; 32],
    context: &AiPowCompactBatchVerifierContext,
    pinned_digest: &[u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
    attempt_index: u64,
    artifact_jam: &[u8],
) -> Result<(), String> {
    // (1)
    if artifact_jam.len() > AI_PEAK_SUBMISSION_BLOB_MAX {
        return Err(format!(
            "artifact {} bytes exceeds {AI_PEAK_SUBMISSION_BLOB_MAX}",
            artifact_jam.len()
        ));
    }
    let (extranonce, ordinal) =
        split_attempt_index(attempt_index).ok_or("attempt index: extranonce does not fit u32")?;

    // (2) The jet's own bounded decode.
    let limits = CertificateNounLimits::default();
    let artifact =
        decode_ai_pow_pearl_merge_artifact_jam_with_rules(artifact_jam, limits, ProofRules::Hardened)
            .map_err(|e| format!("artifact decode: {e:?}"))?;

    // (3) Statement pins.
    if artifact.moe.is_some() {
        return Err("not a dense artifact (it carries a MoE tail)".to_string());
    }
    let header = PearlIncompleteBlockHeader::from_bytes(&artifact.statement.block_header)
        .map_err(|e| format!("statement header: {e:?}"))?;
    let public = PearlPublicProofParams::from_public_data(header, &artifact.statement.public_data)
        .map_err(|e| format!("statement public data: {e:?}"))?;
    let expected_header = expected_peak_header(challenge, extranonce)?;
    check_peak_statement(&public, &expected_header, ordinal)?;

    // (4) Consensus, against the context this server derived.
    if &compact_batch_verifier_key_digest_to_bytes(context.verifier_key_digest()) != pinned_digest {
        return Err("verifier context digest does not match pinned digest".to_string());
    }
    match verify_ai_pow_block_artifact_with_verification_rules(
        &artifact,
        limits,
        challenge,
        target,
        AI_PEAK_MAX_PATTERN_LEN,
        context,
        pinned_digest,
        VerificationRules::ConsensusBatch,
    )
    .map_err(|e| format!("consensus verify: {e:?}"))?
    {
        AiPowBlockVerifyOutcome::Dense(_) => Ok(()),
        AiPowBlockVerifyOutcome::Moe(_) => Err("routed to the MoE verifier".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_pow::pearl_compat::PearlPeriodicPattern;

    fn honest(ordinal: u64) -> (PearlPublicProofParams, PearlIncompleteBlockHeader) {
        let header = expected_peak_header(&[7; 32], 3).unwrap();
        let (t_rows, t_cols) = tile_offsets(ordinal).unwrap();
        (
            PearlPublicProofParams {
                block_header: header,
                mining_config: peak_mining_config(),
                hash_a: [1; 32],
                hash_b: [2; 32],
                hash_jackpot: [3; 32],
                m: AI_PEAK_PARAMS.m,
                n: AI_PEAK_PARAMS.n,
                t_rows,
                t_cols,
            },
            header,
        )
    }

    #[test]
    fn k_is_bounded_by_the_request_body() {
        let limit = 4 * 1024 * 1024;
        assert_eq!(max_k_for_body(limit), 19);
        // Production's k = 16 fits; k = 24 would draw a 413 after hours of work.
        assert!(16 <= max_k_for_body(limit) && max_k_for_body(limit) < 24);
        let per_win = AI_PEAK_SUBMISSION_BLOB_MAX.div_ceil(3) * 4 + 1024;
        assert!(19 * per_win + 4096 <= limit);
    }

    #[test]
    fn statement_pins_accept_the_honest_statement() {
        let (p, h) = honest(300_001);
        assert_eq!(check_peak_statement(&p, &h, 300_001), Ok(()));
    }

    /// Each pin on its own: a statement differing from the honest one in just
    /// that field is refused, with that pin's reason.
    #[test]
    fn statement_pins_each_refuse_their_field() {
        let ord = 300_001;
        let (p, h) = honest(ord);

        // Same F-relevant shape but 8x8 tiles: a different, admissible
        // statement consensus would count at 2^19, not 2^21.
        let mut q = p.clone();
        let eight = PearlPeriodicPattern {
            shape: [(1, 8), (8, 1), (8, 1)],
        };
        q.mining_config.rows_pattern = eight;
        q.mining_config.cols_pattern = eight;
        assert!(check_peak_statement(&q, &h, ord).unwrap_err().contains("mining config"));

        let mut q = p.clone();
        q.mining_config.common_dim = 4096;
        assert!(check_peak_statement(&q, &h, ord).unwrap_err().contains("mining config"));

        let mut q = p.clone();
        q.m = 512;
        assert!(check_peak_statement(&q, &h, ord).unwrap_err().contains("matrix dims"));
        let mut q = p.clone();
        q.n = 512;
        assert!(check_peak_statement(&q, &h, ord).unwrap_err().contains("matrix dims"));

        // The claimed ordinal names another tile.
        assert!(check_peak_statement(&p, &h, ord + 1).unwrap_err().contains("opened tile"));
        assert!(check_peak_statement(&p, &h, 1 << 19).unwrap_err().contains("out of range"));

        // The header of another extranonce, and of another challenge.
        let other_x = expected_peak_header(&[7; 32], 4).unwrap();
        assert!(check_peak_statement(&p, &other_x, ord).unwrap_err().contains("header"));
        let other_c = expected_peak_header(&[8; 32], 3).unwrap();
        assert!(check_peak_statement(&p, &other_c, ord).unwrap_err().contains("header"));
    }
}
