use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path as AxumPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::kernel::{RegistryKernel, RunRecord};
use crate::ratelimit::RateLimiter;
use crate::verifier::Verifier;

/// Proofs per submission. 8 ≈ 3 minutes on an M1 Mac (21 s/proof) — the
/// design spec's "minutes of proving" target; the spike value was 2.
pub const K_DEFAULT: u64 = 8;

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardEntry {
    #[serde(flatten)]
    pub run: RunRecord,
    /// submitted_at − issued_at, the server-observed window.
    pub server_window_ms: u64,
    /// k / server window — the trustless, ranked rate (a lower bound).
    pub proofs_per_sec: f64,
    /// k / client-reported elapsed_ms — informational only.
    pub self_reported_pps: f64,
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn to_entry(run: RunRecord) -> LeaderboardEntry {
    let server_window_ms =
        crate::kernel::da_diff_to_ms(run.issued_at, run.submitted_at).max(1);
    let proofs_per_sec = round4(run.k as f64 / (server_window_ms as f64 / 1000.0));
    let self_reported_pps = round4(run.k as f64 / (run.elapsed_ms as f64 / 1000.0));
    LeaderboardEntry { run, server_window_ms, proofs_per_sec, self_reported_pps }
}

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Mutex<RegistryKernel>>,
    pub verifier: Arc<Mutex<Verifier>>,
    pub limiter: Arc<RateLimiter>,
    pub k: u64,
}

impl AppState {
    pub async fn boot(jam: &Path, data_dir: &Path) -> Result<Self, nockapp::NockAppError> {
        Self::boot_with_k(jam, data_dir, K_DEFAULT).await
    }

    pub async fn boot_with_k(
        jam: &Path,
        data_dir: &Path,
        k: u64,
    ) -> Result<Self, nockapp::NockAppError> {
        Ok(Self {
            kernel: Arc::new(Mutex::new(RegistryKernel::boot(jam, data_dir).await?)),
            verifier: Arc::new(Mutex::new(Verifier::boot().await?)),
            limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60))),
            k,
        })
    }
}

/// Key = first X-Forwarded-For entry (Railway always sets it); "direct"
/// otherwise (oneshot tests, local curl). Applied only to the POST routes —
/// they mint kernel state / burn verifier CPU.
async fn rate_limit_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let key = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "direct".into());
    if !st.limiter.check(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "rate limit exceeded — try again in a minute" })),
        )
            .into_response();
    }
    next.run(req).await
}

pub fn router(state: AppState) -> Router {
    let limited = Router::new()
        .route("/challenge", post(new_challenge))
        .route("/run", post(submit_run))
        .route_layer(middleware::from_fn_with_state(state.clone(), rate_limit_mw));
    Router::new()
        .route("/", get(index_page))
        .route("/leaderboard", get(leaderboard))
        .route("/runs/:id", get(run_by_id))
        .merge(limited)
        // Explicit request-size bound (M2 carry-forward): k=8 proofs are
        // ~1.2 MiB base64, so 4 MiB is generous headroom.
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(state)
}

async fn new_challenge(State(st): State<AppState>) -> Json<serde_json::Value> {
    let nonce = st
        .kernel
        .lock()
        .await
        .mint_challenge()
        .await
        .expect("mint_challenge failed");
    Json(json!({
        "nonce": nonce.to_string(),
        "pow_len": tock::miner::DEFAULT_POW_LEN,
        "k": st.k,
        "nonce_rule": tock::nonce::NONCE_RULE,
    }))
}

#[derive(serde::Deserialize)]
struct RunSubmission {
    nonce: String,
    hardware: String,
    prover_version: String,
    elapsed_ms: u64,
    proofs: Vec<String>,
}

async fn submit_run(
    State(st): State<AppState>,
    Json(sub): Json<RunSubmission>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    use base64::Engine;

    fn bad(msg: String) -> (StatusCode, Json<serde_json::Value>) {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
    }

    let Ok(nonce) = sub.nonce.parse::<u64>() else {
        return bad("nonce must be a decimal u64".into());
    };
    // elapsed_ms = 0 would make proofs_per_sec = Infinity, which serde_json
    // serializes as JSON null — corrupting the leaderboard at rank #1.
    if sub.elapsed_ms == 0 {
        return bad("elapsed_ms must be greater than zero".into());
    }
    if sub.hardware.len() > 128 {
        return bad("hardware string too long (max 128 bytes)".into());
    }
    if sub.prover_version.len() > 64 {
        return bad("prover_version string too long (max 64 bytes)".into());
    }
    if sub.proofs.len() as u64 != st.k {
        return bad(format!("expected {} proofs, got {}", st.k, sub.proofs.len()));
    }
    // decode + bind + verify every proof BEFORE touching kernel state
    for (i, b64) in sub.proofs.iter().enumerate() {
        let Ok(jam) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            return bad(format!("proof {i}: invalid base64"));
        };
        if let Err(e) = crate::binding::check_binding(&jam, &sub.nonce, i as u64, tock::miner::DEFAULT_POW_LEN) {
            return bad(format!("proof {i}: {e}"));
        }
        // `Verifier::verify`'s future is not `Send` (it holds a raw-pointer
        // `NockStack` across an internal await inside `roswell`), which
        // axum's `Handler` blanket impl requires of the whole handler
        // future. Run it to completion on a blocking-pool thread via a
        // nested `block_on` so the non-Send state never needs to cross a
        // cooperative-scheduling boundary.
        let verifier = st.verifier.clone();
        let jam_owned = jam;
        let verify_result = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current()
                .block_on(async move { verifier.lock().await.verify(&jam_owned).await })
        })
        .await;
        match verify_result {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return bad(format!("proof {i}: STARK verification failed")),
            Ok(Err(e)) => return bad(format!("proof {i}: verifier error: {e}")),
            Err(join_err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("proof {i}: verify task failed: {join_err}") })),
                )
            }
        }
    }
    match st.kernel.lock().await
        .submit_run(nonce, &sub.hardware, &sub.prover_version, st.k, sub.elapsed_ms)
        .await
    {
        Ok(Ok(id)) => (StatusCode::OK, Json(json!({ "run_id": id }))),
        Ok(Err(reason)) => bad(reason),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("kernel error: {e}") })),
        ),
    }
}

async fn index_page() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn leaderboard(State(st): State<AppState>) -> (StatusCode, Json<Vec<LeaderboardEntry>>) {
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let mut entries: Vec<LeaderboardEntry> = runs.into_iter().map(to_entry).collect();
            entries.sort_by(|a, b| b.proofs_per_sec.partial_cmp(&a.proofs_per_sec).unwrap_or(std::cmp::Ordering::Equal));
            (StatusCode::OK, Json(entries))
        }
        Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(vec![])),
    }
}

async fn run_by_id(
    State(st): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> (StatusCode, Json<Option<LeaderboardEntry>>) {
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let entry = runs.into_iter().find(|r| r.id == id).map(to_entry);
            match entry {
                Some(e) => (StatusCode::OK, Json(Some(e))),
                None => (StatusCode::NOT_FOUND, Json(None)),
            }
        }
        Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(None)),
    }
}
