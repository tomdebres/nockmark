//! Mainnet's current AI admission rules, checked on REAL canonical-MoE
//! certificates rather than by reading upstream code (M7 step 2, pin
//! ed1edfcc).
//!
//! - Height 154,500: `ProofRules::Hardened`. Legacy certificates are
//!   inadmissible.
//! - Height 170,000: consensus batch adds exact MoE routing multiplicity
//!   (every token routed exactly `top_k` times). Our `canonical-moe` shape is
//!   e = 2, top_k = 1.
//!
//! One block is proved with upstream's reference prover (the call `tock`
//! makes) and run through upstream's own consensus verifier (the code behind
//! the `%ai-pow-verify-v2` jet) under `VerificationRules::ConsensusBatch`,
//! against the registry's own verifier context. A legacy-rules block is
//! proved the same way and must be rejected by both that verifier and the
//! registry's submission path.
//!
//! One test, sequential: three proves of ~25-30 s and several GB each, so
//! they are not run side by side. Run with RUST_MIN_STACK=8388608.

use ai_pow_miner::certificate_noun::{
    build_ai_pow_pearl_merge_moe_artifact_noun_from_node_with_rules,
    validate_ai_pow_block_artifact_admission,
    verify_ai_pow_block_artifact_jam_with_verification_rules, AiPowBlockVerifyOutcome,
    AiProofNode, CertificateNounLimits, PearlMergeAiPowArtifactShape, PearlMergeMoeArtifact,
};
use ai_pow_miner::reference::{prove_reference_moe_block_at_with_rules, ReferenceBlock};
use ai_pow_zk::proof_rules::{ProofRules, VerificationRules};
use nockmark_registry::aipow_moe::{verify_moe_submission, AiMoeVerifier};
use tock::aipow_moe::{
    max_moe_target, AiMoeCertBlob, AI_MOE_E, AI_MOE_HW, AI_MOE_MAX_PATTERN_LEN, AI_MOE_PARAMS,
    AI_MOE_TOP_K,
};

fn challenge() -> [u8; 32] {
    *blake3::hash(b"nockmark-m7 mainnet-rules test").as_bytes()
}

fn prove(rules: ProofRules) -> ReferenceBlock {
    let block = prove_reference_moe_block_at_with_rules(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        challenge(),
        0,
        rules,
    )
    .unwrap_or_else(|e| panic!("prove canonical MoE block under {rules:?}: {e}"));
    assert_eq!(block.rules, rules);
    block
}

/// The `%ai-pow` artifact a node would receive for this block, jammed.
fn artifact_jam(block: &ReferenceBlock) -> Vec<u8> {
    let c = &block.certificate;
    build_ai_pow_pearl_merge_moe_artifact_noun_from_node_with_rules(
        &block.statement,
        &block.aux_inclusion,
        &block.moe_art,
        &c.zk_params,
        c.found_idx,
        c.trace_height,
        &c.commitments,
        &c.public_inputs,
        &c.certificate,
        block.rules,
    )
    .expect("build %ai-pow MoE artifact noun")
    .jam()
    .to_vec()
}

/// Upstream's consensus verify of a jammed artifact, against OUR context.
fn consensus_verify(
    v: &AiMoeVerifier,
    jam: &[u8],
    rules: VerificationRules,
) -> Result<AiPowBlockVerifyOutcome, String> {
    verify_ai_pow_block_artifact_jam_with_verification_rules(
        jam,
        CertificateNounLimits::default(),
        &challenge(),
        &max_moe_target(),
        AI_MOE_MAX_PATTERN_LEN,
        v.context(),
        v.pinned_digest(),
        rules,
    )
    .map_err(|e| format!("{e:?}"))
}

/// The registry's submission blob for this block (what `tock` posts).
fn blob(block: &ReferenceBlock) -> Vec<u8> {
    let AiProofNode::Bytes(cert_bytes) = &block.certificate.certificate else {
        panic!("compact certificate must use the canonical byte node");
    };
    postcard::to_allocvec(&AiMoeCertBlob {
        ordinal: 0,
        pis: block.certificate.public_inputs.clone(),
        cert_bytes: cert_bytes.clone(),
    })
    .unwrap()
}

#[test]
fn canonical_moe_meets_mainnet_rules_and_legacy_is_inadmissible() {
    assert_eq!((AI_MOE_E, AI_MOE_TOP_K), (2, 1), "the shape this test is about");

    // The registry's own context, built the way the live server builds it.
    let dir = tempfile::tempdir().unwrap();
    let v = AiMoeVerifier::load_or_build(dir.path()).expect("build MoE verifier context");
    assert_eq!(v.context().proof_rules(), ProofRules::Hardened);

    // 1. A Hardened block passes mainnet's consensus verify at the current
    //    rules (>= 170,000), and the registry accepts the same certificate.
    let block = prove(ProofRules::Hardened);
    let outcome = consensus_verify(&v, &artifact_jam(&block), VerificationRules::ConsensusBatch)
        .expect("upstream consensus verify under ConsensusBatch (height >= 170,000)");
    assert!(matches!(outcome, AiPowBlockVerifyOutcome::Moe(_)));
    verify_moe_submission(
        &challenge(),
        &max_moe_target(),
        v.context(),
        v.pinned_digest(),
        0,
        &blob(&block),
    )
    .expect("registry accepts the Hardened certificate");

    // 2. The multiplicity rule is live in that verifier: it passes the real
    //    routing, and rejects the same block with one token routed twice and
    //    another never (same length, so only multiplicity can catch it).
    //    Pre-170,000 (Hardened) admission has no such rule and lets it by.
    let routing = &block.moe_art.routing_data;
    assert_eq!(routing.len(), AI_MOE_PARAMS.m as usize * AI_MOE_TOP_K);
    let shape = |routing_data: Vec<u32>| PearlMergeAiPowArtifactShape {
        statement: block.statement.clone(),
        aux_inclusion: block.aux_inclusion.clone(),
        certificate: block.certificate.clone(),
        moe: Some(PearlMergeMoeArtifact {
            moe: block.moe_art.moe.clone(),
            routing_data,
        }),
    };
    validate_ai_pow_block_artifact_admission(
        &shape(routing.clone()),
        VerificationRules::ConsensusBatch,
    )
    .expect("real routing has exact multiplicity");
    let mut doubled = routing.clone();
    assert_ne!(doubled[0], doubled[1]);
    doubled[1] = doubled[0];
    validate_ai_pow_block_artifact_admission(&shape(doubled.clone()), VerificationRules::Hardened)
        .expect("Hardened admission has no multiplicity rule");
    let err = validate_ai_pow_block_artifact_admission(
        &shape(doubled),
        VerificationRules::ConsensusBatch,
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("Multiplicity"), "{err:?}");

    // 3. A Legacy certificate for the same statement is inadmissible: the
    //    consensus verifier and the registry both reject it, on the setup.
    let legacy = prove(ProofRules::Legacy);
    let err = consensus_verify(&v, &artifact_jam(&legacy), VerificationRules::ConsensusBatch)
        .unwrap_err();
    eprintln!("legacy certificate, consensus verify: {err}");
    let err = verify_moe_submission(
        &challenge(),
        &max_moe_target(),
        v.context(),
        v.pinned_digest(),
        0,
        &blob(&legacy),
    )
    .unwrap_err();
    assert!(err.contains("verifier-key digest"), "{err}");
}
