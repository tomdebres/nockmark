//! AI-track golden path with a REAL grind + prove (M5 Task 3): mint an AI
//! challenge (all-FF target via env override, k=2), produce the wins with
//! the actual client module (`tock::aipow`), submit, and check the board
//! math; then check that a tampered certificate and a replay are rejected.
//!
//! Takes ~1–2 min: two compact-certificate proves (~24 s + cached) plus
//! the verifier-context build-by-proving (~25 s, first submission only —
//! the tamper attempt below deliberately runs first so the context build
//! is exercised exactly once, on the rejection path). Comparable to the
//! existing 52 s verifier suite. Run with RUST_MIN_STACK=8388608.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use tower::ServiceExt;

const K: u64 = 2;

async fn req_json(
    app: axum::Router,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let req = match body {
        Some(v) => Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    };
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::json!({})),
    )
}

#[tokio::test]
async fn ai_golden_path_tamper_and_replay() {
    // Env config must be set before AppState::boot reads it. This is the
    // only test in this binary, so no cross-test env races.
    std::env::set_var("NOCKMARK_AI_TARGET", "f".repeat(64)); // every attempt wins
    std::env::set_var("NOCKMARK_AI_K", K.to_string());

    let dir = tempfile::tempdir().unwrap();
    let jam = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    let st = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2)
        .await
        .unwrap();
    // leak: kernel checkpoints live here; dropping would delete the dir
    // under the running NockApp (SIGABRT)
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);
    let app = nockmark_registry::http::router(st.clone());

    // 1. Mint an AI challenge: derived challenge + track parameters.
    let (status, ch) = req_json(app.clone(), "POST", "/challenge?track=ai", Some(serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let nonce: u64 = ch["nonce"].as_str().unwrap().parse().unwrap();
    let challenge_hex = ch["challenge"].as_str().unwrap();
    assert_eq!(challenge_hex.len(), 64);
    assert_eq!(ch["target"].as_str().unwrap(), &"f".repeat(64));
    assert_eq!(ch["k"], K);
    assert_eq!(ch["nonce_rule"], tock::aipow::AI_NONCE_RULE);
    assert_eq!(ch["params"]["m"], 8);
    assert_eq!(ch["params"]["k"], 1024);
    assert_eq!(ch["params"]["n"], 8);
    assert_eq!(ch["params"]["noise_rank"], 64);
    assert_eq!(ch["params"]["tile"], 8);
    // The challenge is exactly the documented derivation of the nonce.
    assert_eq!(
        challenge_hex,
        tock::aipow::hex32(&nockmark_registry::aipow::challenge32(nonce))
    );

    // 2. Grind + prove with the real client module (~50 s: first prove
    //    builds the STARK setup, the second reuses the prover cache).
    let ai_ch = tock::aipow::AiChallenge {
        challenge: tock::aipow::parse_hex32(challenge_hex).unwrap(),
        target: [0xff; 32],
        k: K,
    };
    let summary = tokio::task::spawn_blocking(move || tock::aipow::run(&ai_ch))
        .await
        .unwrap();
    let extranonces: Vec<u64> = summary.wins.iter().map(|w| w.extranonce).collect();
    assert_eq!(extranonces, vec![0, 1], "max target wins on every attempt");
    let wins_json: Vec<serde_json::Value> = summary
        .wins
        .iter()
        .map(|w| {
            serde_json::json!({
                "extranonce": w.extranonce,
                "cert_b64": base64::engine::general_purpose::STANDARD.encode(&w.submission_bytes),
            })
        })
        .collect();
    let body = serde_json::json!({
        "nonce": nonce.to_string(),
        "hardware": "aipow-e2e",
        "prover_version": tock::miner::NOCKCHAIN_PIN,
        "grind_elapsed_ms": summary.grind_elapsed_ms,
        "wins": wins_json,
    });

    // 3. Tampered certificate first (the challenge stays unused after a
    //    rejection): flip one byte in the middle of win 1's blob — the
    //    cert body dominates the blob, so this lands in the certificate.
    //    This request also pays the one-time verifier-context
    //    build-by-proving (~25 s) before rejecting.
    let mut tampered_blob = summary.wins[1].submission_bytes.clone();
    let mid = tampered_blob.len() / 2;
    tampered_blob[mid] ^= 0x01;
    let mut tampered = body.clone();
    tampered["wins"][1]["cert_b64"] =
        serde_json::json!(base64::engine::general_purpose::STANDARD.encode(&tampered_blob));
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(tampered)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "tampered cert: {res}");
    assert!(res["error"].as_str().unwrap().contains("win 1"), "{res}");

    // Wrong-order wins are rejected before any verify work.
    let mut reordered = body.clone();
    let w = reordered["wins"].as_array().unwrap().clone();
    reordered["wins"] = serde_json::json!([w[1], w[0]]);
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(reordered)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("ascending"), "{res}");

    // 4. The honest submission is accepted (verifier context now cached).
    let t0 = std::time::Instant::now();
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "honest submit: {res}");
    let run_id = res["run_id"].as_u64().unwrap();
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(30),
        "cached-context verify of k=2 should be ~0.2 s, took {:?}",
        t0.elapsed()
    );
    // The context blob was persisted for the next boot's fast path.
    assert!(nockmark_registry::aipow::AiVerifier::context_path(&data_dir).exists());

    // 5. Replaying the nonce is rejected by the store.
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(res["error"], "nonce-used");

    // 6. Board math: with T = 2^256−1 (all-FF), expected attempts/win
    //    rounds to 1, so MAC-equivalents = k · 1 · 2^16 over each window.
    let (status, board) = req_json(app.clone(), "GET", "/leaderboard?track=ai", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = board.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"].as_u64().unwrap(), run_id);
    assert_eq!(row["hardware"], "aipow-e2e");
    assert_eq!(row["k"], K);
    assert_eq!(row["win_extranonces"], serde_json::json!([0, 1]));
    let window_ms = row["server_window_ms"].as_u64().unwrap();
    // The window covers grind + ~2 proves + the tamper/reorder round trips.
    assert!(window_ms >= summary.grind_elapsed_ms, "window {window_ms} ms");
    let mac_total = K as f64 * 65536.0;
    let verified = row["verified_mac_per_sec_lb"].as_f64().unwrap();
    let expect = mac_total / (window_ms as f64 / 1000.0);
    assert!(
        (verified - expect).abs() <= 0.001 * expect,
        "verified {verified} vs expected {expect} (4-significant-digit rounding)"
    );
    let grind = row["grind_mac_per_sec"].as_f64().unwrap();
    let expect_grind = mac_total / (summary.grind_elapsed_ms as f64 / 1000.0);
    assert!(
        (grind - expect_grind).abs() <= 0.001 * expect_grind,
        "grind {grind} vs expected {expect_grind}"
    );
    assert!(
        grind > verified,
        "the grind window excludes proving, so it must beat the lower bound"
    );
    let zk_equiv = row["zk_attempt_equiv_per_sec"].as_f64().unwrap();
    assert!((zk_equiv - verified / 25.75e9).abs() <= 0.001 * (verified / 25.75e9));
    assert!((row["sigma_frac"].as_f64().unwrap() - 1.0 / (K as f64).sqrt()).abs() < 1e-3);

    // 7. The ZK board is untouched by AI runs.
    let (status, zk_board) = req_json(app, "GET", "/leaderboard", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(zk_board.as_array().unwrap().len(), 0);
}
