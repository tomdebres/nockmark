use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // add tower = "0.5" to [dependencies]

async fn state() -> nockmark_registry::http::AppState {
    let dir = tempfile::tempdir().unwrap();
    let jam = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../tock/assets/registry.jam"));
    nockmark_registry::http::AppState::boot(jam, dir.path()).await.unwrap()
}

#[tokio::test]
async fn challenge_returns_nonce() {
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
