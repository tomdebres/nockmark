//! M5 Task 1 — AI-PoW certificate VERIFY-path timing spike.
//!
//! Proves ONE compact recursive certificate for the canonical single-tile
//! shape (m=8, k=1024, n=8, r=64, tile=8) with an all-ones target (first
//! attempt wins), then times the registry-side verify path end-to-end:
//! submission decode + cert decode + statement re-derivation from
//! (challenge, nonce) + canonical program commitment + compact STARK verify.

mod verifier;

use std::time::Instant;

use ai_pow::params::MatmulParams;
use ai_pow::prover::BlockContext;
use ai_pow::synth::synth_matrices;
use ai_pow::zk_bridge::prove_ai_pow_compact_recursive_certificate;
use ai_pow_zk::recursion::{
    compact_batch_verifier_key_digest_to_bytes, encode_compact_batch_recursive_certificate,
    AiPowCompactBatchVerifierContext,
};

use verifier::{verify_submission, Submission};

/// Canonical single-tile AI-PoW shape (M5 design / upstream canonical costs).
const PARAMS: MatmulParams = MatmulParams {
    m: 8,
    k: 1024,
    n: 8,
    noise_rank: 64,
    tile: 8,
    spot_checks: 1,
    difficulty_bits: 0,
};

/// Peak RSS (bytes) via getrusage. macOS reports ru_maxrss in bytes.
fn max_rss_bytes() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    ru.ru_maxrss as u64
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    let challenge: [u8; 32] = *blake3_of(b"nockmark-m5-aipow-spike-challenge-v1");
    let nonce: Vec<u8> = 0u64.to_le_bytes().to_vec(); // extranonce 0, LE
    let target = [0xFFu8; 32]; // max target: every attempt wins
    let found_idx = 0u32; // single-tile grid: the attempt tile is always 0

    println!("== aipow-spike: prove ==");
    let t = Instant::now();
    let (a, b) = synth_matrices(&challenge, &PARAMS);
    println!("synth_matrices: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let ctx = BlockContext::build(&challenge, &nonce, &a, &b, &PARAMS)
        .expect("BlockContext::build");
    println!("BlockContext::build: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let run = prove_ai_pow_compact_recursive_certificate(&ctx, &PARAMS, &nonce, &target, found_idx)
        .expect("prove compact recursive certificate");
    let prove_s = t.elapsed().as_secs_f64();
    println!(
        "prove_ai_pow_compact_recursive_certificate: {:.2} s \
         (l1_circuit_build {} ms, l1_outer_cert {} ms, l2_prep {} ms, \
         l2_prove {} ms, l2_compact {} ms, upstream l2_compact_verify {} ms)",
        prove_s,
        run.l1_circuit_build_ms(),
        run.l1_outer_cert_ms(),
        run.l2_prep_ms(),
        run.l2_prove_ms(),
        run.l2_compact_ms(),
        run.l2_compact_verify_ms(),
    );
    println!("peak RSS after prove: {:.0} MB", mb(max_rss_bytes()));

    // Build the untrusted submission blob exactly as a client would ship it.
    let cert_bytes = encode_compact_batch_recursive_certificate(run.certificate())
        .expect("encode compact certificate");
    println!("certificate size: {} bytes", cert_bytes.len());
    let submission = Submission {
        nonce: nonce.clone(),
        found_idx,
        trace_height: run.trace_height(),
        pis: run.public_inputs().clone(),
        cert_bytes: cert_bytes.clone(),
    };
    let submission_bytes = postcard::to_allocvec(&submission).expect("encode submission");
    println!("submission size: {} bytes", submission_bytes.len());

    println!("\n== verifier setup (server boot objects) ==");
    let pinned_digest =
        compact_batch_verifier_key_digest_to_bytes(run.certificate().verifier_key_digest());
    println!("pinned verifier-key digest: {} bytes", pinned_digest.len());

    // Serialize the verifier-owned context (its serde form is the
    // verifier-only projection) to gauge its size and boot-load cost.
    let t = Instant::now();
    let context_bytes =
        bincode::serde::encode_to_vec(run.verifier_context(), bincode::config::standard())
            .expect("serialize verifier context");
    println!(
        "verifier context serialize: {:.1} ms, {:.1} MB",
        t.elapsed().as_secs_f64() * 1e3,
        mb(context_bytes.len() as u64)
    );
    let rss_before = max_rss_bytes();
    let t = Instant::now();
    let (boot_context, _): (AiPowCompactBatchVerifierContext, usize) =
        bincode::serde::decode_from_slice(&context_bytes, bincode::config::standard())
            .expect("deserialize verifier context");
    println!(
        "verifier context deserialize (boot-load): {:.1} ms, maxrss delta {:.1} MB",
        t.elapsed().as_secs_f64() * 1e3,
        mb(max_rss_bytes().saturating_sub(rss_before))
    );
    drop(context_bytes);

    println!("\n== verify (registry path, boot-loaded context) ==");
    // Warmup.
    verify_submission(&challenge, &target, &PARAMS, &boot_context, &pinned_digest, &submission_bytes)
        .expect("warmup verify");
    let mut times_ms: Vec<f64> = Vec::with_capacity(10);
    for _ in 0..10 {
        let t = Instant::now();
        verify_submission(
            &challenge, &target, &PARAMS, &boot_context, &pinned_digest, &submission_bytes,
        )
        .expect("verify");
        times_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let min = times_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = times_ms.iter().cloned().fold(0.0f64, f64::max);
    let mean = times_ms.iter().sum::<f64>() / times_ms.len() as f64;
    println!("verify end-to-end over 10 iters: min {min:.1} ms, mean {mean:.1} ms, max {max:.1} ms");
    println!(
        "per-iter ms: {}",
        times_ms.iter().map(|v| format!("{v:.1}")).collect::<Vec<_>>().join(" ")
    );

    println!("\n== negative cases ==");
    // Tampered certificate: flip one byte in the middle of the cert body.
    let mut tampered = submission;
    tampered.cert_bytes[cert_bytes.len() / 2] ^= 0x01;
    let tampered_bytes = postcard::to_allocvec(&tampered).expect("encode tampered");
    match verify_submission(
        &challenge, &target, &PARAMS, &boot_context, &pinned_digest, &tampered_bytes,
    ) {
        Err(e) => println!("tampered cert rejected: {e}"),
        Ok(()) => println!("FAIL: tampered cert ACCEPTED"),
    }
    // Wrong nonce: same cert/pis, claimed under extranonce 1.
    let mut wrong_nonce = tampered;
    wrong_nonce.cert_bytes = cert_bytes;
    wrong_nonce.nonce = 1u64.to_le_bytes().to_vec();
    let wrong_nonce_bytes = postcard::to_allocvec(&wrong_nonce).expect("encode wrong-nonce");
    match verify_submission(
        &challenge, &target, &PARAMS, &boot_context, &pinned_digest, &wrong_nonce_bytes,
    ) {
        Err(e) => println!("wrong-nonce submission rejected: {e}"),
        Ok(()) => println!("FAIL: wrong-nonce submission ACCEPTED"),
    }

    println!("\npeak RSS at exit: {:.0} MB", mb(max_rss_bytes()));
}

/// Tiny embedded helper so the spike needs no extra hashing dep: derive the
/// 32-byte challenge from a label via ai-pow's own keyed commitment primitive.
fn blake3_of(label: &[u8]) -> Box<[u8; 32]> {
    Box::new(ai_pow::commit::matrix_commitment(label, &[0u8; 32]))
}
