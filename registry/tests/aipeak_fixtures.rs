//! Regenerates `tests/fixtures/aipeak/`: real dense-peak `%ai-pow` artifacts
//! for `tests/aipeak.rs`. Ignored by default — it proves five certificates
//! (~5 min, ~15 GB peak RSS on an M1 Max):
//!
//! ```text
//! cd registry && RUST_MIN_STACK=8388608 cargo test --test aipeak_fixtures -- --ignored --nocapture
//! ```
//!
//! Made through tock's own client path (`aipow_peak::grind_peak_cpu`,
//! `prove_peak_wins`), so regenerating them also exercises the client.
//!
//! * `win-{0,1,2}.jam` — three Hardened wins of challenge `challenge32(5150)`
//!   at tier 2^12 (the first proved cold, the others with the cached setup);
//! * `legacy-win-0.jam` — win 0's ticket proved under the Legacy proof rules;
//! * `moe.jam` — a Hardened canonical-MoE artifact for the same challenge;
//! * `manifest.json` — nonce, tier, target and the wins' attempt indices.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai_pow_miner::certificate_noun::{
    build_ai_pow_pearl_merge_moe_artifact_noun_from_node_with_rules, decode_ai_pow_pearl_merge_artifact_jam_with_rules,
    CertificateNounLimits,
};
use ai_pow_miner::reference::prove_reference_moe_block_at_with_rules;
use ai_pow_miner::search::CpuSearchBackend;
use ai_pow_zk::proof_rules::ProofRules;
use tock::aipow_moe::{AI_MOE_E, AI_MOE_HW, AI_MOE_PARAMS, AI_MOE_TOP_K};
use tock::aipow_peak::{self, AiPeakChallenge, PeakMatrices};

pub const FIXTURE_NONCE: u64 = 5150;
pub const FIXTURE_ATTEMPTS: u64 = aipow_peak::AI_PEAK_ATTEMPTS_MIN;

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aipeak")
}

#[test]
#[ignore = "proves five certificates (~5 min, ~15 GB peak RSS)"]
fn regenerate_aipeak_fixtures() {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let challenge = nockmark_registry::aipow::challenge32(FIXTURE_NONCE);
    let target = aipow_peak::peak_target_for_attempts(FIXTURE_ATTEMPTS).unwrap();
    let matrices = PeakMatrices::synth();
    let backend = CpuSearchBackend::default();
    let template = matrices.template(challenge);

    let ch = AiPeakChallenge {
        challenge,
        target,
        k: 3,
    };
    let grind = aipow_peak::grind_peak_cpu(&template, &ch, &backend);
    let indices: Vec<u64> = grind.wins.iter().map(|w| w.index).collect();
    eprintln!("grind: {} attempts, wins at {indices:?}", grind.total_attempts);
    let first = &grind.wins[0];
    let (x0, o0) = aipow_peak::split_attempt_index(first.index).unwrap();
    let wins = aipow_peak::prove_peak_wins(&template, &matrices, grind.wins);
    for (i, w) in wins.iter().enumerate() {
        std::fs::write(dir.join(format!("win-{i}.jam")), &w.artifact_jam).unwrap();
        eprintln!("win {i}: attempt {} proved in {} ms", w.index, w.prove_ms);
    }

    // Win 0's ticket again, proved under the Legacy rules mainnet dropped at
    // height 154,500.
    let prepared = Arc::new(template.prepare(x0).unwrap());
    let attempt = template.checked_winner(&prepared, o0, &target).unwrap();
    drop(prepared);
    let block = template.prove_with_rules(attempt, ProofRules::Legacy).unwrap();
    let legacy =
        aipow_peak::artifact_jam(&block.attempt, &block.aux_inclusion, &matrices, &block.run)
            .unwrap();
    // Legacy artifacts still decode (the program differs, not the noun).
    decode_ai_pow_pearl_merge_artifact_jam_with_rules(
        &legacy,
        CertificateNounLimits::default(),
        ProofRules::Legacy,
    )
    .expect("legacy artifact decodes under Legacy rules");
    std::fs::write(dir.join("legacy-win-0.jam"), &legacy).unwrap();

    // A canonical-MoE artifact for the same challenge (valid mainnet work of
    // another statement).
    let moe = prove_reference_moe_block_at_with_rules(
        &AI_MOE_PARAMS,
        AI_MOE_HW,
        AI_MOE_E,
        AI_MOE_TOP_K,
        challenge,
        0,
        ProofRules::Hardened,
    )
    .unwrap();
    let c = &moe.certificate;
    let moe_jam = build_ai_pow_pearl_merge_moe_artifact_noun_from_node_with_rules(
        &moe.statement,
        &moe.aux_inclusion,
        &moe.moe_art,
        &c.zk_params,
        c.found_idx,
        c.trace_height,
        &c.commitments,
        &c.public_inputs,
        &c.certificate,
        moe.rules,
    )
    .unwrap()
    .jam()
    .to_vec();
    std::fs::write(dir.join("moe.jam"), &moe_jam).unwrap();

    let manifest = serde_json::json!({
        "nonce": FIXTURE_NONCE,
        "attempts": FIXTURE_ATTEMPTS,
        "target": tock::aipow::hex32(&target),
        "indices": wins.iter().map(|w| w.index).collect::<Vec<_>>(),
        "pin": tock::miner::NOCKCHAIN_PIN,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
}
