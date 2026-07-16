//! Golden path over real HTTP using the same client code `tock bench
//! --submit` runs: fetch challenge → prove k=2 → submit → ranked by the
//! server window. Run:
//!   RUST_MIN_STACK=8388608 cargo test --release --test e2e -- --ignored --nocapture
use std::future::IntoFuture;

use base64::Engine;

const K: u64 = 2; // spike k keeps this test ~45 s; prod default is 8

#[tokio::test]
#[ignore]
async fn golden_path_and_anti_cheat() {
    let dir = tempfile::tempdir().unwrap();
    let reg_jam = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    let miner_jam =
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../tock/assets/miner.jam")).unwrap();
    let st = nockmark_registry::http::AppState::boot_with_k(reg_jam, dir.path(), K)
        .await
        .unwrap();
    let app = nockmark_registry::http::router(st);

    // Serve on a real port so tock's reqwest-based client is what we test.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());

    // 1. challenge — via the miner-facing client
    let ch = tock::client::fetch_challenge(&base).await.unwrap();
    assert_eq!(ch.k, K);
    assert_eq!(ch.nonce_rule, tock::nonce::NONCE_RULE);

    // 2. prove k proofs against the challenge (what tock bench does)
    let serf = tock::miner::boot_kernel(miner_jam, nockapp::utils::NOCK_STACK_SIZE_TINY).await;
    let header = tock::nonce::seed_to_belts(&ch.nonce, "header");
    let t0 = std::time::Instant::now();
    let mut proofs = Vec::new();
    for i in 0..K {
        let nb = tock::nonce::seed_to_belts(&format!("{}/{i}", ch.nonce), "nonce");
        let out = tock::miner::run_prove(&serf, &header, &nb, ch.pow_len).await;
        proofs.push(base64::engine::general_purpose::STANDARD.encode(&out.proof_jam));
    }
    let true_elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0).ceil() as u64;

    // 3. an inflated claim (elapsed lower than reality) is ACCEPTED as
    //    consistent — but it cannot move the ranked rate (step 6).
    let sub = tock::client::Submission {
        nonce: ch.nonce.clone(),
        hardware: "e2e-test".into(),
        prover_version: "31b8a015".into(),
        elapsed_ms: 1, // maximal lie
        proofs: proofs.clone(),
    };
    let run_id = tock::client::submit_run(&base, &sub).await.unwrap();

    // 4. replaying the nonce is rejected by the kernel
    let err = tock::client::submit_run(&base, &sub).await.unwrap_err();
    assert!(err.contains("nonce-used"), "got: {err}");

    // 5. a fresh challenge with an over-window elapsed claim is rejected
    //    before any proof work matters (binding runs first and these proofs
    //    bind the OLD nonce — so assert on that rejection path too).
    let ch2 = tock::client::fetch_challenge(&base).await.unwrap();
    let sub2 = tock::client::Submission {
        nonce: ch2.nonce,
        hardware: "e2e-test".into(),
        prover_version: "31b8a015".into(),
        elapsed_ms: 3_600_000,
        proofs,
    };
    let err = tock::client::submit_run(&base, &sub2).await.unwrap_err();
    assert!(
        err.contains("nonce"),
        "binding must reject cross-nonce proofs: {err}"
    );

    // 6. the board ranks by the server window: the elapsed_ms=1 lie shows
    //    up in self_reported_pps but the ranked rate is bounded by reality.
    let board: serde_json::Value = reqwest::get(format!("{base}/leaderboard"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = board.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"].as_u64().unwrap(), run_id);
    assert_eq!(rows[0]["hardware"], "e2e-test");
    let ranked = rows[0]["proofs_per_sec"].as_f64().unwrap();
    let honest = K as f64 / (true_elapsed_ms as f64 / 1000.0);
    assert!(
        ranked <= honest * 1.01,
        "ranked rate {ranked} must not exceed the honest rate {honest}"
    );
    assert!(
        rows[0]["self_reported_pps"].as_f64().unwrap() > 100.0,
        "the lie is visible as self-reported"
    );
    assert!(rows[0]["server_window_ms"].as_u64().unwrap() >= true_elapsed_ms);
}
