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
    let state = nockmark_registry::http::AppState::boot(jam, dir.path()).await.unwrap();
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
