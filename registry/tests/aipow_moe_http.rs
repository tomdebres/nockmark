//! Canonical-MoE golden path with a REAL grind + prove (M6 Phase B1), the
//! peer of `aipow_http.rs`: mint a `statement=canonical-moe` challenge (loosest
//! scalable target via env override, k=2), produce the wins with the actual
//! client module (`tock::aipow_moe`), submit, and check the board math; then
//! check that a tampered certificate, a wrong ordinal and a replay are
//! rejected.
//!
//! Takes ~2 min: two canonical MoE proves (~25-30 s each — this path has no
//! prover-cache reuse) plus the one-time canonical-MoE verifier-context build
//! (~25-30 s, on the rejection path so it is paid exactly once). Run with
//! RUST_MIN_STACK=8388608.

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
async fn canonical_moe_golden_path_tamper_and_replay() {
    // Env config must be set before AppState::boot reads it. This is the only
    // test in this binary, so no cross-test env races.
    //
    // NOTE the MoE target is NOT all-FF: `T · F` is computed fail-closed, so
    // an all-FF target is an ERROR here rather than "everything wins". This is
    // the loosest target the canonical shape can actually scale.
    std::env::set_var(
        "NOCKMARK_AI_MOE_TARGET",
        tock::aipow::hex32(&tock::aipow_moe::max_moe_target()),
    );
    std::env::set_var("NOCKMARK_AI_K", K.to_string());

    let dir = tempfile::tempdir().unwrap();
    let jam = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    let st = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2)
        .await
        .unwrap();
    // leak: kernel checkpoints live here; dropping would delete the dir under
    // the running NockApp (SIGABRT)
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);
    let app = nockmark_registry::http::router(st.clone());

    // 1. Mint a canonical-MoE challenge: the shared challenge derivation, this
    //    statement's shape, target and grind rule.
    let (status, ch) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai&statement=canonical-moe",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let nonce: u64 = ch["nonce"].as_str().unwrap().parse().unwrap();
    let challenge_hex = ch["challenge"].as_str().unwrap();
    assert_eq!(challenge_hex.len(), 64);
    assert_eq!(ch["statement"], "canonical-moe");
    assert_eq!(ch["k"], K);
    assert_eq!(ch["nonce_rule"], tock::aipow_moe::AI_MOE_NONCE_RULE);
    assert_eq!(ch["nonce_rule"], "canonical-ordinal-v3");
    assert_ne!(
        ch["nonce_rule"], tock::aipow::AI_NONCE_RULE,
        "the ordinal rule must not be advertised as the dense LE8 rule"
    );
    // The canonical shape, including the MoE half the dense challenge has no
    // concept of.
    assert_eq!(ch["params"]["m"], 64);
    assert_eq!(ch["params"]["k"], 1024);
    assert_eq!(ch["params"]["n"], 64);
    assert_eq!(ch["params"]["noise_rank"], 64);
    assert_eq!(ch["params"]["hw"], 8);
    assert_eq!(ch["params"]["e"], 2);
    assert_eq!(ch["params"]["top_k"], 1);
    assert_eq!(ch["params"]["shape_work_factor"], 65536.0);
    assert_eq!(ch["params"]["target_is_scaled_by_shape_work_factor"], true);
    assert_eq!(
        ch["target"].as_str().unwrap(),
        tock::aipow::hex32(&tock::aipow_moe::max_moe_target())
    );
    // The challenge is the same documented derivation both statements share.
    assert_eq!(
        challenge_hex,
        tock::aipow::hex32(&nockmark_registry::aipow::challenge32(nonce))
    );

    // A dense challenge on the same route still answers with the dense rule —
    // the statements are selected, not merged.
    let (status, dense_ch) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dense_ch["statement"], "dense");
    assert_eq!(dense_ch["nonce_rule"], tock::aipow::AI_NONCE_RULE);
    assert_eq!(dense_ch["params"]["m"], 8);
    // An unknown statement is rejected rather than silently scored as dense.
    let (status, bad_ch) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai&statement=sparse",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(bad_ch["error"].as_str().unwrap().contains("sparse"));

    // 2. Grind + prove with the real client module (~60 s: two full canonical
    //    MoE proves).
    let ai_ch = tock::aipow_moe::AiMoeChallenge {
        challenge: tock::aipow::parse_hex32(challenge_hex).unwrap(),
        target: tock::aipow_moe::max_moe_target(),
        k: K,
    };
    let summary = tokio::task::spawn_blocking(move || tock::aipow_moe::run(&ai_ch))
        .await
        .unwrap();
    let ordinals: Vec<u32> = summary.wins.iter().map(|w| w.ordinal).collect();
    assert_eq!(ordinals, vec![0, 1], "the loosest target wins every attempt");
    let wins_json: Vec<serde_json::Value> = summary
        .wins
        .iter()
        .map(|w| {
            serde_json::json!({
                "extranonce": w.ordinal,
                "cert_b64": base64::engine::general_purpose::STANDARD.encode(&w.submission_bytes),
            })
        })
        .collect();
    let body = serde_json::json!({
        "nonce": nonce.to_string(),
        "hardware": "aipow-moe-e2e",
        "prover_version": tock::miner::NOCKCHAIN_PIN,
        "statement": "canonical-moe",
        "grind_elapsed_ms": summary.grind_elapsed_ms,
        "wins": wins_json,
    });

    // 3. Tampered certificate first (the challenge stays unused after a
    //    rejection): flip one byte in the middle of win 1's blob, which the
    //    certificate body dominates. This request also pays the one-time
    //    canonical-MoE verifier-context build (~25-30 s) before rejecting.
    let mut tampered_blob = summary.wins[1].submission_bytes.clone();
    let mid = tampered_blob.len() / 2;
    tampered_blob[mid] ^= 0x01;
    let mut tampered = body.clone();
    tampered["wins"][1]["cert_b64"] =
        serde_json::json!(base64::engine::general_purpose::STANDARD.encode(&tampered_blob));
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(tampered)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "tampered cert: {res}");
    assert!(res["error"].as_str().unwrap().contains("win 1"), "{res}");

    // A certificate submitted under the wrong ordinal is rejected: the whole
    // statement is re-derived from (challenge, ordinal), so win 0's proof
    // cannot stand in for ordinal 5.
    let mut wrong_ordinal = body.clone();
    wrong_ordinal["wins"][1]["extranonce"] = serde_json::json!(5);
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(wrong_ordinal)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("win 1"), "{res}");

    // A canonical-MoE win cannot claim an ordinal outside the u32 grind space.
    let mut too_wide = body.clone();
    too_wide["wins"][1]["extranonce"] = serde_json::json!(u64::from(u32::MAX) + 1);
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(too_wide)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("u32"), "{res}");

    // Submitting canonical-MoE wins as `dense` is rejected: the dense verifier
    // re-derives an entirely different statement from the same challenge.
    let mut mislabelled = body.clone();
    mislabelled["statement"] = serde_json::json!("dense");
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(mislabelled)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{res}");

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
        "cached-context verify of k=2 should be well under a second, took {:?}",
        t0.elapsed()
    );
    // The canonical-MoE context blob was persisted for the next boot, under
    // its OWN pin-scoped path. (The dense blob exists here too — the
    // mislabelled-statement rejection above forced the dense verifier to boot;
    // that the two are separate files is the point.)
    let moe_ctx = nockmark_registry::aipow_moe::AiMoeVerifier::context_path(&data_dir);
    let dense_ctx = nockmark_registry::aipow::AiVerifier::context_path(&data_dir);
    assert!(moe_ctx.exists(), "{}", moe_ctx.display());
    assert_ne!(
        moe_ctx, dense_ctx,
        "each statement owns a separate pin-scoped setup"
    );

    // 5. Replaying the nonce is rejected by the store.
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(res["error"], "nonce-used");

    // 6. Board math. At the loosest scalable target, Θ = 2^256 − 2^16, so
    //    expected attempts per win rounds to 1 and MAC-equivalents = k · 1 · F.
    let (status, board) = req_json(app.clone(), "GET", "/leaderboard?track=ai", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = board.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"].as_u64().unwrap(), run_id);
    assert_eq!(row["hardware"], "aipow-moe-e2e");
    assert_eq!(row["k"], K);
    // The statement is on the row — this is what the board column reads.
    assert_eq!(row["statement"], "canonical-moe");
    assert_eq!(row["win_extranonces"], serde_json::json!([0, 1]));
    let window_ms = row["server_window_ms"].as_u64().unwrap();
    assert!(window_ms >= summary.grind_elapsed_ms, "window {window_ms} ms");
    let mac_total = K as f64 * tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT;
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

    // The single-run view carries the statement too.
    let (status, one) = req_json(
        app.clone(),
        "GET",
        &format!("/runs/{run_id}?track=ai"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["statement"], "canonical-moe");

    // 7. The ZK board is untouched by AI runs.
    let (status, zk_board) = req_json(app, "GET", "/leaderboard", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(zk_board.as_array().unwrap().len(), 0);
}
