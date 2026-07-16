use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // add tower = "0.5" to [dependencies]

// Booting a kernel+verifier pair allocates a huge NockStack per instance;
// doing that concurrently across this file's #[tokio::test] functions (the
// default test runner runs them in parallel threads) reliably aborts the
// process. Serialize the whole test bodies on a single async lock so only
// one kernel/verifier pair is ever booted at a time.
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn state() -> nockmark_registry::http::AppState {
    let dir = tempfile::tempdir().unwrap();
    let jam = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../tock/assets/registry.jam"));
    let state = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2).await.unwrap();
    // leak: kernel checkpoints live here; dropping would delete the dir under
    // the running NockApp (SIGABRT)
    std::mem::forget(dir);
    state
}

#[tokio::test]
async fn challenge_returns_nonce() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::post("/challenge").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["nonce"].as_str().unwrap().parse::<u64>().unwrap() > 0);
    assert_eq!(v["nonce_rule"], "fnv1a-splitmix64-v1");
}

const GOOD: &[u8] = include_bytes!("fixtures/proof-good.jam");

async fn post_json(app: axum::Router, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(
        Request::post(path).header("content-type", "application/json")
            .body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::json!({})))
}

#[tokio::test]
async fn run_rejects_unbound_proof() {
    use base64::Engine;
    let _guard = test_lock().lock().await;
    let st = state().await;
    let app = nockmark_registry::http::router(st.clone());
    // mint a real challenge
    let (_, ch) = post_json(app.clone(), "/challenge", serde_json::json!({})).await;
    let nonce = ch["nonce"].as_str().unwrap();
    // proof-good.jam binds to "fixture-challenge/0", NOT to this nonce
    let b64 = base64::engine::general_purpose::STANDARD.encode(GOOD);
    let (status, body) = post_json(app, "/run", serde_json::json!({
        "nonce": nonce, "hardware": "hw", "prover_version": "31b8a015",
        "elapsed_ms": 60000, "proofs": [b64, b64]
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("nonce"));
}

#[tokio::test]
async fn run_rejects_unknown_challenge() {
    use base64::Engine;
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let b64 = base64::engine::general_purpose::STANDARD.encode(GOOD);
    let (status, body) = post_json(app, "/run", serde_json::json!({
        "nonce": "12345", "hardware": "hw", "prover_version": "x",
        "elapsed_ms": 1, "proofs": [b64, b64]
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // binding fails before the kernel is ever consulted
    assert!(body["error"].as_str().is_some());
}

#[tokio::test]
async fn run_rejects_zero_elapsed_ms() {
    use base64::Engine;
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let b64 = base64::engine::general_purpose::STANDARD.encode(GOOD);
    // elapsed_ms = 0 must be rejected before binding/verify: it would make
    // proofs_per_sec = Infinity, which serde_json serializes as JSON null.
    let (status, body) = post_json(app, "/run", serde_json::json!({
        "nonce": "12345", "hardware": "hw", "prover_version": "x",
        "elapsed_ms": 0, "proofs": [b64, b64]
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("elapsed_ms"));
}

#[tokio::test]
async fn leaderboard_empty_initially() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::get("/leaderboard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn index_page_ok() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("Nockmark"));
    assert!(html.contains("<table"));
}

#[tokio::test]
async fn leaderboard_sorts_by_proofs_per_sec_desc() {
    let _guard = test_lock().lock().await;
    let st = state().await;
    let app = nockmark_registry::http::router(st.clone());

    // Mint two real nonces via the kernel (submit_run requires a known
    // nonce), then poke submit_run directly on the same kernel handle.
    // This bypasses proof verification (legitimately, for this READ-path
    // test) and lets us control elapsed_ms precisely to assert ordering.
    // Windows are real (enforced by the kernel now): each run's claimed
    // elapsed_ms must fit inside its own mint→submit window, with a >=3x
    // margin so scheduling jitter can't flip a claim into rejection.
    // slow-hw: ~2400 ms window / 800 ms claim; fast-hw: ~600 ms window /
    // 200 ms claim. fast-hw ranks first under both client-pps and
    // server-window ranking (Task 2 flips the ranked source; this
    // scenario survives it), and the 1800 ms gap between the two windows
    // keeps the ordering robust under load.
    let nonce_slow = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2_400)).await;
    st.kernel
        .lock()
        .await
        .submit_run(nonce_slow, "slow-hw", "v", 2, 800)
        .await
        .unwrap()
        .unwrap();

    let nonce_fast = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    st.kernel
        .lock()
        .await
        .submit_run(nonce_fast, "fast-hw", "v", 2, 200)
        .await
        .unwrap()
        .unwrap();

    let res = app
        .oneshot(Request::get("/leaderboard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Ranked by the SERVER-WINDOW rate (the trustless number), not the
    // client-reported elapsed_ms.
    assert_eq!(rows[0]["hardware"].as_str().unwrap(), "fast-hw");
    assert_eq!(rows[1]["hardware"].as_str().unwrap(), "slow-hw");
    for row in rows {
        let window = row["server_window_ms"].as_u64().unwrap();
        assert!(window >= 300, "window {window} ms implausibly small");
        let pps = row["proofs_per_sec"].as_f64().unwrap();
        let expect = 2.0 / (window as f64 / 1000.0);
        assert!(
            (pps - expect).abs() <= 0.05 * expect,
            "proofs_per_sec {pps} not derived from server window {window} ms"
        );
        // Client claims are faster than the window here, so the
        // self-reported rate must exceed the ranked rate.
        assert!(row["self_reported_pps"].as_f64().unwrap() > pps);
    }
}

#[tokio::test]
async fn run_by_id_not_found() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::get("/runs/999").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn challenge_is_rate_limited_per_ip() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    // Limit is 10/min/IP on POST /challenge and POST /run. The rate-limit
    // key is the LAST XFF entry (the proxy-appended true client IP); a
    // different spoofed FIRST entry each request must not mint a fresh
    // bucket as long as the trusted last entry stays the same.
    for i in 0..10 {
        let res = app.clone()
            .oneshot(Request::post("/challenge")
                .header("x-forwarded-for", format!("10.0.0.{i}, 203.0.113.7"))
                .body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "request {i} should pass");
    }
    let res = app.clone()
        .oneshot(Request::post("/challenge")
            .header("x-forwarded-for", "10.0.0.99, 203.0.113.7")
            .body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // A different IP (different LAST entry) is unaffected.
    let res = app
        .oneshot(Request::post("/challenge")
            .header("x-forwarded-for", "10.0.0.1, 203.0.113.8")
            .body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn run_rejects_oversized_body() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app.oneshot(Request::post("/run")
        .header("content-type", "application/json")
        .body(Body::from(vec![b'x'; 5 * 1024 * 1024])).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn run_rejects_overlong_strings() {
    use base64::Engine;
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let b64 = base64::engine::general_purpose::STANDARD.encode(GOOD);
    let (status, body) = post_json(app.clone(), "/run", serde_json::json!({
        "nonce": "12345", "hardware": "h".repeat(129), "prover_version": "x",
        "elapsed_ms": 1, "proofs": [b64.clone(), b64.clone()]
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("hardware"));
    let (status, body) = post_json(app, "/run", serde_json::json!({
        "nonce": "12345", "hardware": "hw", "prover_version": "p".repeat(65),
        "elapsed_ms": 1, "proofs": [b64.clone(), b64]
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("prover_version"));
}

#[tokio::test]
async fn default_k_is_eight_and_test_states_override_it() {
    let _guard = test_lock().lock().await;
    assert_eq!(nockmark_registry::http::K_DEFAULT, 8);
    // state() boots with k=2 so the 116 KB fixture pair keeps working.
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::post("/challenge").body(Body::empty()).unwrap())
        .await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["k"], 2);
}

#[tokio::test]
async fn economics_unconfigured_returns_503() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    let res = app
        .oneshot(Request::get("/economics").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn economics_estimates_and_annotates_the_board() {
    let _guard = test_lock().lock().await;
    let st = state().await;
    *st.econ.write().await = Some(nockmark_registry::economics::EconParams {
        difficulty: 6_000.0,
        block_reward_nock: 2_048.0,
    });
    let app = nockmark_registry::http::router(st.clone());

    // /economics?pps= computes the estimate
    let res = app.clone()
        .oneshot(Request::get("/economics?pps=10").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["difficulty"], 6_000.0);
    let est = v["est_nock_per_day"].as_f64().unwrap();
    assert!((est - 10.0 * 86_400.0 / 6_000.0 * 2_048.0).abs() < 1e-6);

    // leaderboard rows carry est_nock_per_day when configured
    let nonce = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    st.kernel.lock().await
        .submit_run(nonce, "hw", "v", 2, 400).await.unwrap().unwrap();
    let res = app
        .oneshot(Request::get("/leaderboard").body(Body::empty()).unwrap())
        .await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v[0]["est_nock_per_day"].as_f64().unwrap() > 0.0);
}
