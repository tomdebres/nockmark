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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_nock_per_day: Option<f64>,
    /// This run's share of the estimated whole-network proving rate
    /// (difficulty / block time). Tiny by design — the network is pools
    /// of GPUs; the board is single machines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_share: Option<f64>,
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn to_entry(run: RunRecord, econ: Option<crate::economics::EconParams>) -> LeaderboardEntry {
    let server_window_ms =
        crate::kernel::da_diff_to_ms(run.issued_at, run.submitted_at).max(1);
    let proofs_per_sec = round4(run.k as f64 / (server_window_ms as f64 / 1000.0));
    let self_reported_pps = round4(run.k as f64 / (run.elapsed_ms as f64 / 1000.0));
    let est_nock_per_day =
        econ.map(|p| round4(crate::economics::nock_per_day(proofs_per_sec, &p)));
    let network_share =
        econ.map(|p| proofs_per_sec / crate::economics::network_pps(&p));
    LeaderboardEntry {
        run,
        server_window_ms,
        proofs_per_sec,
        self_reported_pps,
        est_nock_per_day,
        network_share,
    }
}

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Mutex<RegistryKernel>>,
    pub verifier: Arc<Mutex<Verifier>>,
    pub limiter: Arc<RateLimiter>,
    pub k: u64,
    pub econ: Arc<tokio::sync::RwLock<Option<crate::economics::EconParams>>>,
    pub data_dir: std::path::PathBuf,
}

impl AppState {
    /// Difficulty observations live beside the kernel state on the
    /// persistent volume.
    pub fn econ_history_path(&self) -> std::path::PathBuf {
        self.data_dir.join("econ-history.jsonl")
    }
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
            econ: Arc::new(tokio::sync::RwLock::new(crate::economics::from_env())),
            data_dir: data_dir.to_path_buf(),
        })
    }
}

/// Key = LAST X-Forwarded-For entry (the proxy-appended true client IP; the
/// last entry is trustworthy whether the edge proxy appends to or overwrites
/// a client-supplied header), else "direct" (oneshot tests, local curl).
/// Applied only to the POST routes — they mint kernel state / burn verifier
/// CPU.
async fn rate_limit_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let key = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next_back())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "direct".into());
    if let Err(retry_after_secs) = st.limiter.hit(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry_after_secs.to_string())],
            Json(json!({
                "error": "rate limit exceeded",
                "reason": "per-IP window on proof-verifying routes",
                "retry_after_secs": retry_after_secs,
            })),
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
        .route("/api", get(api_page))
        .route("/leaderboard", get(leaderboard))
        .route("/runs/:id", get(run_by_id))
        .route("/economics", get(economics))
        .route("/economics/history", get(economics_history))
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

async fn api_page() -> Html<&'static str> {
    Html(include_str!("../static/api.html"))
}

async fn leaderboard(State(st): State<AppState>) -> (StatusCode, Json<Vec<LeaderboardEntry>>) {
    let econ = *st.econ.read().await;
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let mut entries: Vec<LeaderboardEntry> =
                runs.into_iter().map(|run| to_entry(run, econ)).collect();
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
    let econ = *st.econ.read().await;
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let entry = runs.into_iter().find(|r| r.id == id).map(|run| to_entry(run, econ));
            match entry {
                Some(e) => (StatusCode::OK, Json(Some(e))),
                None => (StatusCode::NOT_FOUND, Json(None)),
            }
        }
        Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(None)),
    }
}

async fn economics(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(p) = *st.econ.read().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "economics not configured on this instance" })),
        );
    };
    let mut out = json!({
        "difficulty": p.difficulty,
        "block_reward_nock": p.block_reward_nock,
        "block_time_secs": crate::economics::block_time_secs(),
        "est_network_pps": crate::economics::network_pps(&p),
        "model": "est_nock_per_day = pps * 86400 / difficulty * block_reward_nock",
        "note": "difficulty = expected proof attempts per block; estimates only",
    });
    if let Some(pps) = q.get("pps").and_then(|s| s.parse::<f64>().ok()) {
        out["pps"] = json!(pps);
        out["est_nock_per_day"] = json!(crate::economics::nock_per_day(pps, &p));
        out["network_share"] = json!(pps / crate::economics::network_pps(&p));
    }
    (StatusCode::OK, Json(out))
}

/// Difficulty observations from this instance's refresh loop, oldest
/// first. `?hours=` bounds the lookback (default one week, max 90 days);
/// responses are thinned to ≤ 1000 points.
async fn economics_history(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let hours = q
        .get("hours")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(168)
        .clamp(1, 24 * 90);
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(hours * 3600);
    match crate::economics::read_history(&st.econ_history_path(), since, 1000) {
        Ok(points) => (
            StatusCode::OK,
            Json(json!({
                "hours": hours,
                "points": points.iter().map(|(t, d)| json!([t, d])).collect::<Vec<_>>(),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("history read failed: {e}") })),
        ),
    }
}
