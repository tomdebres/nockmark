//! Golden path: mint → prove k=2 against the nonce → submit → on the board.
//! Run: RUST_MIN_STACK=8388608 cargo test --release --test e2e -- --ignored --nocapture
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use tower::ServiceExt;

const K: u64 = 2;

#[tokio::test]
#[ignore]
async fn golden_path_and_anti_cheat() {
    let dir = tempfile::tempdir().unwrap();
    let reg_jam = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../tock/assets/registry.jam"));
    let miner_jam = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../tock/assets/miner.jam")).unwrap();
    let st = nockmark_registry::http::AppState::boot(reg_jam, dir.path()).await.unwrap();
    let app = nockmark_registry::http::router(st);

    // 1. mint
    let res = app.clone().oneshot(Request::post("/challenge").body(Body::empty()).unwrap()).await.unwrap();
    let ch: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    let nonce = ch["nonce"].as_str().unwrap().to_string();

    // 2. prove k=2 against the challenge (exactly what tock bench does)
    let serf = tock::miner::boot_kernel(miner_jam, nockapp::utils::NOCK_STACK_SIZE_TINY).await;
    let header = tock::nonce::seed_to_belts(&nonce, "header");
    let t0 = std::time::Instant::now();
    let mut proofs = Vec::new();
    for i in 0..K {
        let nb = tock::nonce::seed_to_belts(&format!("{nonce}/{i}"), "nonce");
        let out = tock::miner::run_prove(&serf, &header, &nb, 64).await;
        proofs.push(base64::engine::general_purpose::STANDARD.encode(&out.proof_jam));
    }
    let elapsed_ms = t0.elapsed().as_millis() as u64;

    // 3. submit — must be accepted
    let body = serde_json::json!({
        "nonce": nonce, "hardware": "e2e-test", "prover_version": "31b8a015",
        "elapsed_ms": elapsed_ms, "proofs": proofs
    });
    let res = app.clone().oneshot(Request::post("/run")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "golden path must be accepted");

    // 4. replay the same nonce — must be rejected by the kernel
    let res = app.clone().oneshot(Request::post("/run")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "replay must be rejected");

    // 5. leaderboard has exactly one verified run
    let res = app.oneshot(Request::get("/leaderboard").body(Body::empty()).unwrap()).await.unwrap();
    let board: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(board.as_array().unwrap().len(), 1);
    assert_eq!(board[0]["hardware"], "e2e-test");
}
