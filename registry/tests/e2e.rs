//! Golden path over real HTTP using the client code `tock zk-bench --submit`
//! runs: fetch a ZK v5 challenge → grind k=3 winners, proving each through the
//! miner kernel on two concurrent provers while the grind goes on → submit →
//! ranked by the server window. Both server verifiers check every pipelined
//! proof. Needs a compiled `tock/assets/miner.jam`. Run:
//!   RUST_MIN_STACK=8388608 cargo test --release --test e2e -- --ignored --nocapture
use std::future::IntoFuture;

use base64::Engine;
use tock::zkv5;

const K: u64 = 3; // ~1 min with 2 provers; production k is 16
const PROVERS: usize = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn golden_path_and_anti_cheat() {
    let dir = tempfile::tempdir().unwrap();
    let reg_jam = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    let miner_jam = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/miner.jam"
    ))
    .unwrap();
    let st = nockmark_registry::http::AppState::boot_with_k(reg_jam, dir.path(), K)
        .await
        .unwrap();
    let app = nockmark_registry::http::router(st);

    // Serve on a real port so tock's reqwest-based client is what we test.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());
    let mut serfs = Vec::new();
    for _ in 0..PROVERS {
        serfs.push(
            tock::miner::boot_kernel(miner_jam.clone(), nockapp::utils::NOCK_STACK_SIZE_TINY).await,
        );
    }

    // 1. challenge — via the miner-facing client, which checks it
    let t0 = std::time::Instant::now();
    let ch = tock::client::fetch_zk_challenge(&base, Some(zkv5::ZK_ATTEMPTS_MIN))
        .await
        .unwrap();
    assert_eq!(ch.k, K);
    assert_eq!(ch.attempts, zkv5::ZK_ATTEMPTS_MIN);

    // 2. grind and prove at once, exactly as zk-bench does
    let opts = tock::pipeline::PipelineOptions::new(
        K as usize,
        PROVERS,
        tock::pipeline::Proving::Pipelined,
    );
    let rt = tokio::runtime::Handle::current();
    let (commitment, target) = (ch.commitment, ch.target);
    let g = tokio::task::block_in_place(|| {
        zkv5::run_pipelined(&serfs, &rt, commitment, target, opts, |sink| {
            zkv5::grind_cpu_stage(sink, commitment, target, 4)
        })
    })
    .unwrap();
    assert_eq!(g.wins.len(), K as usize);
    let nonces: Vec<zkv5::Belts5> = g.wins.iter().map(|w| w.win).collect();
    assert!(zkv5::nonces_strictly_ascending(&nonces), "submitted in the registry's order");
    assert!(
        g.wins.iter().any(|w| w.started_at < g.grind_elapsed),
        "a proof ran while the grind went on"
    );
    let proofs: Vec<String> = g
        .wins
        .iter()
        .map(|w| base64::engine::general_purpose::STANDARD.encode(&w.proof.proof_jam))
        .collect();
    let true_elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0).ceil() as u64;

    // 3. a lying grind time (1 ms) is ACCEPTED — it is display-only and
    //    cannot move the ranked rate (step 6).
    let sub = tock::client::ZkSubmission {
        nonce: ch.nonce.clone(),
        hardware: "e2e-test".into(),
        prover_version: tock::miner::NOCKCHAIN_PIN.into(),
        grind_elapsed_ms: 1,
        proofs: proofs.clone(),
        proving: Some(tock::pipeline::Proving::Pipelined.as_str()),
    };
    let run_id = tock::client::submit_zk_run(&base, &sub).await.unwrap();

    // 4. replaying the submission is refused: the challenge is spent
    let err = tock::client::submit_zk_run(&base, &sub).await.unwrap_err();
    assert!(err.contains("nonce-used"), "got: {err}");

    // 5. the same proofs against a fresh challenge: bound to the old one
    let ch2 = tock::client::fetch_zk_challenge(&base, Some(zkv5::ZK_ATTEMPTS_MIN))
        .await
        .unwrap();
    let sub2 = tock::client::ZkSubmission {
        nonce: ch2.nonce,
        ..sub
    };
    let err = tock::client::submit_zk_run(&base, &sub2).await.unwrap_err();
    assert!(
        err.contains("commitment"),
        "cross-challenge proofs must be refused: {err}"
    );

    // 6. the board ranks by the server window: the 1 ms lie shows up in the
    //    grind-window rate, but the ranked rate is bounded by reality.
    let board: serde_json::Value = reqwest::get(format!("{base}/leaderboard?track=zk"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = board.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"].as_u64().unwrap(), run_id);
    assert_eq!(rows[0]["rank"].as_u64(), Some(1));
    assert_eq!(rows[0]["hardware"], "e2e-test");
    assert_eq!(rows[0]["proving"], "pipelined");
    let ranked = rows[0]["verified_zk_hashes_per_sec_lb"].as_f64().unwrap();
    let honest = K as f64 * zkv5::ZK_ATTEMPTS_MIN as f64 / (true_elapsed_ms as f64 / 1000.0);
    assert!(
        ranked <= honest * 1.01,
        "ranked {ranked} must not exceed honest {honest}"
    );
    assert!(
        rows[0]["grind_zk_hashes_per_sec"].as_f64().unwrap() > 1e6,
        "the lie is visible in the grind-window rate"
    );
    // The window opens at the mint (just after t0) and closes at the submit
    // (after the proving), so it covers the run less one round trip.
    assert!(rows[0]["server_window_ms"].as_u64().unwrap() + 1_000 >= true_elapsed_ms);
    eprintln!(
        "e2e: {} wins, {} hashes, grind {:?}, wall {:?}, ranked {ranked} H/s, honest bound {honest:.0}",
        g.wins.len(),
        g.hashes,
        g.grind_elapsed,
        g.wall
    );
}
