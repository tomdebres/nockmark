//! Dense-peak over HTTP (M7 step 4), on the real fixture artifacts
//! (`tests/fixtures/aipeak/`): the mint advertises the statement, its own tier
//! range and its 3 h window, and records the statement on the challenge; the
//! challenge and the submission must agree on dense-peak; the honest
//! submission is accepted and ranked on the AI board with the other
//! current-rules rows.
//!
//! The fixture challenge (`challenge32(5150)` at tier 2^12) is seeded into the
//! AI store before boot, which is how a registry that never minted it can take
//! the fixture proofs. Run with RUST_MIN_STACK=8388608.

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

fn fx(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aipeak")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[tokio::test]
async fn dense_peak_mint_bind_accept_and_rank() {
    std::env::set_var("NOCKMARK_AI_K", K.to_string());
    let manifest: serde_json::Value = serde_json::from_slice(&fx("manifest.json")).unwrap();
    let nonce = manifest["nonce"].as_u64().unwrap();
    let target_hex = manifest["target"].as_str().unwrap().to_string();
    let indices: Vec<u64> = manifest["indices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();

    // Seed: the fixture challenge as a dense-peak mint, and a second challenge
    // minted WITHOUT a statement (what a dense or canonical-MoE mint writes),
    // both issued just now.
    let dir = tempfile::tempdir().unwrap();
    let now = nockmark_registry::aipow::unix_ms();
    let unpinned_nonce = nonce + 1;
    std::fs::write(
        nockmark_registry::aipow::AiStore::path_in(dir.path()),
        format!(
            "{}\n{}\n",
            serde_json::json!({"ev": "challenge", "nonce": nonce, "issued_at_ms": now,
                               "target": target_hex, "statement": "dense-peak"}),
            serde_json::json!({"ev": "challenge", "nonce": unpinned_nonce, "issued_at_ms": now,
                               "target": target_hex}),
        ),
    )
    .unwrap();
    let jam = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    let st = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2)
        .await
        .unwrap();
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);
    let app = nockmark_registry::http::router(st);

    // 1. Mint. A tier above dense-peak's ceiling is clamped to 2^36 (not the
    //    other statements' 2^30); the challenge says how long it lives.
    let (status, ch) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai&statement=dense-peak&attempts=1099511627776",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ch}");
    assert_eq!(ch["statement"], "dense-peak");
    assert_eq!(ch["nonce_rule"], tock::aipow_peak::AI_PEAK_NONCE_RULE);
    assert_eq!(ch["attempts"], 1u64 << 36);
    assert_eq!(
        ch["target"].as_str().unwrap(),
        tock::aipow::hex32(&tock::aipow::pow2_target(235 - 36).unwrap())
    );
    assert_eq!(ch["window_ms"], 3 * 3_600_000u64);
    assert_eq!(ch["params"]["m"], 4096);
    assert_eq!(ch["params"]["n"], 32768);
    assert_eq!(ch["params"]["k"], 8192);
    assert_eq!(ch["params"]["tile"], 16);
    assert_eq!(ch["params"]["shape_work_factor"], 2_097_152.0);
    let minted: u64 = ch["nonce"].as_str().unwrap().parse().unwrap();
    // The store line carries the statement, so the window survives a reboot.
    let log = std::fs::read_to_string(nockmark_registry::aipow::AiStore::path_in(&data_dir)).unwrap();
    let line = log
        .lines()
        .find(|l| l.contains(&format!("\"nonce\":{minted}")))
        .expect("mint line");
    assert!(line.contains("\"statement\":\"dense-peak\""), "{line}");
    // A canonical-MoE mint still writes no statement and keeps the 2^30 cap.
    let (status, moe) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai&statement=canonical-moe&attempts=1099511627776",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moe["attempts"], 1u64 << 30);
    assert!(moe.get("window_ms").is_none(), "{moe}");

    // 2. The honest body.
    let wins: Vec<serde_json::Value> = indices[..K as usize]
        .iter()
        .enumerate()
        .map(|(i, idx)| {
            serde_json::json!({
                "extranonce": idx,
                "cert_b64": base64::engine::general_purpose::STANDARD.encode(fx(&format!("win-{i}.jam"))),
            })
        })
        .collect();
    let body = serde_json::json!({
        "nonce": nonce.to_string(),
        "hardware": "aipeak-http-test",
        "prover_version": tock::miner::NOCKCHAIN_PIN,
        "statement": "dense-peak",
        "grind_elapsed_ms": 1000,
        "wins": wins,
    });

    // 3. Binding, both directions, before any verify work.
    let mut unpinned = body.clone();
    unpinned["nonce"] = serde_json::json!(unpinned_nonce.to_string());
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(unpinned)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        res["error"].as_str().unwrap().contains("minted for dense or canonical-moe"),
        "{res}"
    );
    let mut as_moe = body.clone();
    as_moe["statement"] = serde_json::json!("canonical-moe");
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(as_moe)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("minted for dense-peak"), "{res}");

    // An attempt index whose extranonce is wider than u32.
    let mut wide = body.clone();
    wide["wins"][1]["extranonce"] =
        serde_json::json!((u64::from(u32::MAX) + 1) * tock::aipow_peak::PEAK_TICKETS_PER_EXTRANONCE);
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(wide)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("u32 extranonce"), "{res}");

    // An artifact over dense-peak's cap is refused at the HTTP layer, before
    // the verifier (whose own cap says "artifact", not "blob").
    let mut big = body.clone();
    big["wins"][0]["cert_b64"] = serde_json::json!(base64::engine::general_purpose::STANDARD
        .encode(vec![0u8; tock::aipow_peak::AI_PEAK_SUBMISSION_BLOB_MAX + 1]));
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(big)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("win 0: blob"), "{res}");

    // A win submitted under another win's index (the verifier's pins; this
    // request also pays the one-time context derivation).
    let mut swapped = body.clone();
    swapped["wins"][1]["extranonce"] = serde_json::json!(indices[1] + 1);
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(swapped)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].as_str().unwrap().contains("win 1"), "{res}");

    // 4. Honest: accepted.
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "honest submit: {res}");
    let run_id = res["run_id"].as_u64().unwrap();
    let (status, res) = req_json(app.clone(), "POST", "/run?track=ai", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(res["error"], "nonce-used");

    // 5. Board: a Hardened dense-peak row, ranked, credited 2^12 attempts per
    //    win at F = 2^21.
    let (status, board) = req_json(app, "GET", "/leaderboard?track=ai", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = board.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"].as_u64().unwrap(), run_id);
    assert_eq!(row["statement"], "dense-peak");
    assert_eq!(row["rules"], "hardened");
    assert_eq!(row["rank"], 1);
    assert_eq!(row["expected_attempts_per_win"], 4096.0);
    assert_eq!(row["win_extranonces"], serde_json::json!(indices[..K as usize]));
    let window_ms = row["server_window_ms"].as_u64().unwrap();
    let expect = K as f64 * 4096.0 * 2_097_152.0 / (window_ms as f64 / 1000.0);
    let verified = row["verified_mac_per_sec_lb"].as_f64().unwrap();
    assert!((verified - expect).abs() <= 0.001 * expect, "{verified} vs {expect}");

    // 6. A registry whose k cannot fit k artifacts in one request body
    //    refuses to mint dense-peak at all (the other statements still mint).
    std::env::set_var("NOCKMARK_AI_K", "24");
    let dir = tempfile::tempdir().unwrap();
    let st = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2)
        .await
        .unwrap();
    std::mem::forget(dir);
    let app = nockmark_registry::http::router(st);
    let (status, res) = req_json(
        app.clone(),
        "POST",
        "/challenge?track=ai&statement=dense-peak",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{res}");
    assert!(res["error"].as_str().unwrap().contains("k <= 19"), "{res}");
    let (status, _) = req_json(
        app,
        "POST",
        "/challenge?track=ai&statement=canonical-moe",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
