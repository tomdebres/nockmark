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
    /// AI track (M5): pending challenges + verified runs, JSONL-backed.
    pub ai: Arc<Mutex<crate::aipow::AiStore>>,
    /// AI compact-STARK verifier context, built/loaded lazily on the first
    /// AI submission (~505 ms from the persisted blob, ~25 s by proving).
    pub ai_verifier: Arc<tokio::sync::OnceCell<Arc<crate::aipow::AiVerifier>>>,
    /// Wins per AI submission (NOCKMARK_AI_K, default 4).
    pub ai_k: u64,
    /// Benchmark jackpot target T_b (NOCKMARK_AI_TARGET; generous default
    /// until Task 5 calibrates it).
    pub ai_target: [u8; 32],
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
        let ai = crate::aipow::AiStore::load(crate::aipow::AiStore::path_in(data_dir))
            .map_err(|e| nockapp::NockAppError::OtherError(format!("ai store load: {e}")))?;
        Ok(Self {
            kernel: Arc::new(Mutex::new(RegistryKernel::boot(jam, data_dir).await?)),
            verifier: Arc::new(Mutex::new(Verifier::boot().await?)),
            limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60))),
            k,
            econ: Arc::new(tokio::sync::RwLock::new(crate::economics::from_env())),
            data_dir: data_dir.to_path_buf(),
            ai: Arc::new(Mutex::new(ai)),
            ai_verifier: Arc::new(tokio::sync::OnceCell::new()),
            ai_k: crate::aipow::k_from_env(),
            ai_target: crate::aipow::target_from_env(),
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
    // Both tracks share these routes (`?track=ai` selects the AI track),
    // so the AI mint/verify paths sit under the same per-IP rate limit.
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
        // Explicit request-size bound (M2 carry-forward): k=8 ZK proofs
        // are ~1.2 MiB base64 and k=4 AI cert blobs ~0.8 MiB, so 4 MiB is
        // generous headroom for both tracks.
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(state)
}

/// `?track=ai` selects the M5 AI-PoW track on the shared routes.
fn is_ai_track(q: &std::collections::HashMap<String, String>) -> bool {
    q.get("track").map(String::as_str) == Some("ai")
}

async fn new_challenge(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let nonce = st
        .kernel
        .lock()
        .await
        .mint_challenge()
        .await
        .expect("mint_challenge failed");
    if is_ai_track(&q) {
        // AI track: the kernel's mint/uniqueness machinery is reused
        // untouched; the 32-byte challenge is DERIVED from the minted
        // nonce, and the AI store records the server-observed issue time
        // (the AI window runs on the server clock, not kernel @da state).
        let challenge = crate::aipow::challenge32(nonce);
        st.ai
            .lock()
            .await
            .record_challenge(nonce, crate::aipow::unix_ms())
            .expect("ai store append failed");
        let p = &tock::aipow::AI_PARAMS;
        return Json(json!({
            "nonce": nonce.to_string(),
            "challenge": tock::aipow::hex32(&challenge),
            "target": tock::aipow::hex32(&st.ai_target),
            "k": st.ai_k,
            "params": {
                "m": p.m, "k": p.k, "n": p.n,
                "noise_rank": p.noise_rank, "tile": p.tile,
            },
            "nonce_rule": tock::aipow::AI_NONCE_RULE,
        }));
    }
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

fn bad(msg: String) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

async fn submit_run(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<serde_json::Value>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    use base64::Engine;

    if is_ai_track(&q) {
        return submit_ai_run(st, body).await;
    }
    let sub: RunSubmission = match serde_json::from_value(body) {
        Ok(sub) => sub,
        Err(e) => return bad(format!("bad submission body: {e}")),
    };

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

#[derive(serde::Deserialize)]
struct AiWinSubmission {
    extranonce: u64,
    /// base64 of the postcard `tock::aipow::AiCertBlob` (certificate +
    /// claimed statement metadata).
    cert_b64: String,
}

#[derive(serde::Deserialize)]
struct AiRunSubmission {
    nonce: String,
    hardware: String,
    prover_version: String,
    grind_elapsed_ms: u64,
    wins: Vec<AiWinSubmission>,
}

/// `POST /run?track=ai` — same shape as the ZK path: validate cheaply,
/// verify every certificate BEFORE touching any state, then commit.
async fn submit_ai_run(
    st: AppState,
    body: serde_json::Value,
) -> (StatusCode, Json<serde_json::Value>) {
    use base64::Engine;

    let sub: AiRunSubmission = match serde_json::from_value(body) {
        Ok(sub) => sub,
        Err(e) => return bad(format!("bad submission body: {e}")),
    };
    let Ok(nonce) = sub.nonce.parse::<u64>() else {
        return bad("nonce must be a decimal u64".into());
    };
    // grind_elapsed_ms = 0 would make grind_mac_per_sec = Infinity, which
    // serde_json serializes as JSON null (same M2 concern as elapsed_ms).
    if sub.grind_elapsed_ms == 0 {
        return bad("grind_elapsed_ms must be greater than zero".into());
    }
    if sub.hardware.len() > 128 {
        return bad("hardware string too long (max 128 bytes)".into());
    }
    if sub.prover_version.len() > 64 {
        return bad("prover_version string too long (max 64 bytes)".into());
    }
    if sub.wins.len() as u64 != st.ai_k {
        return bad(format!("expected {} wins, got {}", st.ai_k, sub.wins.len()));
    }
    let extranonces: Vec<u64> = sub.wins.iter().map(|w| w.extranonce).collect();
    if !crate::aipow::extranonces_strictly_ascending(&extranonces) {
        return bad("win extranonces must be strictly ascending".into());
    }
    // Decode + size-cap every blob before any expensive work.
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(sub.wins.len());
    for (i, win) in sub.wins.iter().enumerate() {
        let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(&win.cert_b64) else {
            return bad(format!("win {i}: invalid base64"));
        };
        if blob.len() > crate::aipow::AI_SUBMISSION_BLOB_MAX {
            return bad(format!(
                "win {i}: blob {} bytes exceeds {}",
                blob.len(),
                crate::aipow::AI_SUBMISSION_BLOB_MAX
            ));
        }
        blobs.push(blob);
    }
    // Challenge must be pending, unexpired, unused — checked cheaply now
    // (before burning verifier CPU) and re-checked at commit.
    if let Err(reason) = st
        .ai
        .lock()
        .await
        .challenge_status(nonce, crate::aipow::unix_ms())
    {
        return bad(reason);
    }
    // Verifier context: built/loaded once, lazily, on the blocking pool
    // (first submission pays ~505 ms from the persisted blob or ~25 s
    // build-by-proving; every later submission reuses the OnceCell).
    let verifier = {
        let data_dir = st.data_dir.clone();
        match st
            .ai_verifier
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || {
                    crate::aipow::AiVerifier::load_or_build(&data_dir).map(Arc::new)
                })
                .await
                .map_err(|e| format!("verifier init task failed: {e}"))?
            })
            .await
        {
            Ok(v) => v.clone(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("ai verifier unavailable: {e}") })),
                )
            }
        }
    };
    // Verify every win (CPU-bound, ~79 ms each) on the blocking pool. The
    // challenge is re-derived from the stored nonce — nothing
    // challenge-shaped is taken from the client.
    let challenge = crate::aipow::challenge32(nonce);
    for (i, (win, blob)) in sub.wins.iter().zip(blobs).enumerate() {
        let verifier = verifier.clone();
        let target = st.ai_target;
        let extranonce = win.extranonce;
        let verify_result = tokio::task::spawn_blocking(move || {
            verifier.verify(&challenge, &target, extranonce, &blob)
        })
        .await;
        match verify_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return bad(format!("win {i}: {e}")),
            Err(join_err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("win {i}: verify task failed: {join_err}") })),
                )
            }
        }
    }
    // Commit: re-checks pending/unexpired/unused under the store lock,
    // marks the nonce used, appends the run.
    match st.ai.lock().await.commit_run(
        nonce,
        &sub.hardware,
        &sub.prover_version,
        &st.ai_target,
        sub.grind_elapsed_ms,
        extranonces,
        crate::aipow::unix_ms(),
    ) {
        Ok(Ok(run)) => (StatusCode::OK, Json(json!({ "run_id": run.id }))),
        Ok(Err(reason)) => bad(reason),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("ai store error: {e}") })),
        ),
    }
}

async fn index_page() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn api_page() -> Html<&'static str> {
    Html(include_str!("../static/api.html"))
}

/// One AI-track board row: the stored run plus the rates computed from it
/// (`aipow::rates` — see the module docs for the formulas).
#[derive(Debug, Clone, Serialize)]
pub struct AiLeaderboardEntry {
    #[serde(flatten)]
    pub run: crate::aipow::AiRunRecord,
    /// submitted_at − issued_at, the server-observed window (certificate
    /// proving included — that is why the ranked rate is a lower bound).
    pub server_window_ms: u64,
    #[serde(flatten)]
    pub rates: crate::aipow::AiRates,
}

fn to_ai_entry(run: crate::aipow::AiRunRecord) -> AiLeaderboardEntry {
    let server_window_ms = run.submitted_at_ms.saturating_sub(run.issued_at_ms).max(1);
    // The per-run persisted target; an unparseable one (torn line) falls
    // back to the max target, which can only UNDERSTATE the rate.
    let target = tock::aipow::parse_hex32(&run.target).unwrap_or([0xff; 32]);
    let rates = crate::aipow::rates(run.k, &target, server_window_ms, run.grind_elapsed_ms);
    AiLeaderboardEntry {
        run,
        server_window_ms,
        rates,
    }
}

async fn leaderboard(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if is_ai_track(&q) {
        let mut entries: Vec<AiLeaderboardEntry> = st
            .ai
            .lock()
            .await
            .runs()
            .iter()
            .cloned()
            .map(to_ai_entry)
            .collect();
        entries.sort_by(|a, b| {
            b.rates
                .verified_mac_per_sec_lb
                .partial_cmp(&a.rates.verified_mac_per_sec_lb)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        return (StatusCode::OK, Json(entries)).into_response();
    }
    let econ = *st.econ.read().await;
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let mut entries: Vec<LeaderboardEntry> =
                runs.into_iter().map(|run| to_entry(run, econ)).collect();
            entries.sort_by(|a, b| b.proofs_per_sec.partial_cmp(&a.proofs_per_sec).unwrap_or(std::cmp::Ordering::Equal));
            (StatusCode::OK, Json(entries)).into_response()
        }
        Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<LeaderboardEntry>::new())).into_response(),
    }
}

async fn run_by_id(
    State(st): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if is_ai_track(&q) {
        let entry = st
            .ai
            .lock()
            .await
            .runs()
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .map(to_ai_entry);
        return match entry {
            Some(e) => (StatusCode::OK, Json(Some(e))).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(None::<AiLeaderboardEntry>),
            )
                .into_response(),
        };
    }
    let econ = *st.econ.read().await;
    match st.kernel.lock().await.leaderboard().await {
        Ok(runs) => {
            let entry = runs.into_iter().find(|r| r.id == id).map(|run| to_entry(run, econ));
            match entry {
                Some(e) => (StatusCode::OK, Json(Some(e))).into_response(),
                None => (StatusCode::NOT_FOUND, Json(None::<LeaderboardEntry>)).into_response(),
            }
        }
        Err(_e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(None::<LeaderboardEntry>)).into_response()
        }
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
