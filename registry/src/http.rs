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

/// Wins per ZK v5 submission (`--k`): 16, the AI track's k
/// (`NOCKMARK_AI_K=16` since M6), for the same reason. k wins is Poisson, so a
/// row carries ±1/√k: ±25% at 16 against ±35% at 8, and at 8 two cards 1.4×
/// apart rank the wrong way round too often. The first M1 Max row at k = 8
/// came in at 38% of its calibrated rate on 2.55× bad luck. Each win is one
/// ~21 s proof on an M1 Max plus twice that of grinding, so a self-calibrated
/// k = 16 run is ~20 min there.
pub const K_DEFAULT: u64 = 16;

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
    /// The consensus era the row's unit belongs to. Every ZK row so far
    /// measures complete %3 STARK proofs/sec, the mining unit before
    /// Anthropos (height 147,500) made a ZK attempt one Tip5 hash, so all of
    /// them are `pre-anthropos`, whatever pin proved them.
    pub rules: &'static str,
    /// Always `None`: since M7 step 3 the ZK board ranks only rows measured
    /// under current consensus rules (Tip5 hashes/s, `rules: anthropos`).
    /// These rows stay listed, after the ranked ones, unranked.
    pub rank: Option<u64>,
    // No earnings fields. Every row here is complete %3 proofs/sec from a
    // pre-Anthropos pin, and since height 147,500 a ZK mining attempt is one
    // Tip5 hash: there is nothing to price it against
    // (`economics::ZK_PROOF_ROWS_UNPRICED`, published on /economics). Until
    // M7 these rows printed est_nock_per_day ≈ 0 and a network_share; both
    // were artefacts of dividing proofs by hashes.
}

/// [`LeaderboardEntry::rules`] for every ZK proofs/sec row.
pub const ZK_PROOF_ROW_RULES: &str = "pre-anthropos";

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn to_entry(run: RunRecord) -> LeaderboardEntry {
    let server_window_ms =
        crate::kernel::da_diff_to_ms(run.issued_at, run.submitted_at).max(1);
    let proofs_per_sec = round4(run.k as f64 / (server_window_ms as f64 / 1000.0));
    let self_reported_pps = round4(run.k as f64 / (run.elapsed_ms as f64 / 1000.0));
    LeaderboardEntry {
        run,
        server_window_ms,
        proofs_per_sec,
        self_reported_pps,
        rules: ZK_PROOF_ROW_RULES,
        rank: None,
    }
}

/// One ZK v5 board row: the stored run, the rates computed from it, its rank,
/// and earnings from the RANKED rate.
#[derive(Debug, Clone, Serialize)]
pub struct ZkV5LeaderboardEntry {
    #[serde(flatten)]
    pub run: crate::zkv5::ZkV5RunRecord,
    /// submitted_at − issued_at, server-observed (proving included).
    pub server_window_ms: u64,
    #[serde(flatten)]
    pub rates: crate::zkv5::ZkV5Rates,
    /// 1-based rank among current-rules rows. Every v5 row is current-rules.
    pub rank: Option<u64>,
    /// NOCK/day at the observed ZK difficulty, from the ranked
    /// `verified_zk_hashes_per_sec_lb`. Absent while economics is unconfigured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_nock_per_day: Option<f64>,
    /// `est_nock_per_day × spot NOCK/USD`, absent without a price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_usd_per_day: Option<f64>,
}

fn to_zk_entry(
    run: crate::zkv5::ZkV5RunRecord,
    econ: Option<&crate::economics::EconParams>,
) -> ZkV5LeaderboardEntry {
    let server_window_ms = run.submitted_at_ms.saturating_sub(run.issued_at_ms).max(1);
    let rates = crate::zkv5::rates(run.k, &run.target, server_window_ms, run.grind_elapsed_ms);
    // The published, rounded rate goes in, so a reader can redo the sum from
    // what the row prints (the AI board's rule).
    let est_nock_per_day = econ.map(|p| {
        crate::economics::signif4(crate::economics::nock_per_day(
            rates.verified_zk_hashes_per_sec_lb,
            p,
        ))
    });
    let est_usd_per_day = est_nock_per_day
        .zip(econ)
        .and_then(|(nock, p)| crate::economics::usd_per_day(nock, p));
    ZkV5LeaderboardEntry {
        run,
        server_window_ms,
        rates,
        rank: None,
        est_nock_per_day,
        est_usd_per_day,
    }
}

/// A row of the ZK board: a ranked v5 row or an unranked pre-Anthropos one.
/// The two carry different units, so `rules` (and the rate field present)
/// says which a row is.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ZkBoardRow {
    V5(ZkV5LeaderboardEntry),
    PreAnthropos(LeaderboardEntry),
}

fn by_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

/// The ZK board: v5 rows ranked by verified Tip5 hashes/s, then the
/// pre-Anthropos proofs/sec rows, unranked, fastest first.
fn zk_board(
    v5: Vec<ZkV5LeaderboardEntry>,
    legacy: Vec<LeaderboardEntry>,
) -> Vec<ZkBoardRow> {
    let mut v5 = v5;
    v5.sort_by(|a, b| {
        by_desc(
            a.rates.verified_zk_hashes_per_sec_lb,
            b.rates.verified_zk_hashes_per_sec_lb,
        )
    });
    for (i, e) in v5.iter_mut().enumerate() {
        e.rank = Some(i as u64 + 1);
    }
    let mut legacy = legacy;
    legacy.sort_by(|a, b| by_desc(a.proofs_per_sec, b.proofs_per_sec));
    v5.into_iter()
        .map(ZkBoardRow::V5)
        .chain(legacy.into_iter().map(ZkBoardRow::PreAnthropos))
        .collect()
}

/// The AI board: rows verified under the current rules (`hardened`) ranked
/// by verified MAC-equivalents/s, then every earlier row, unranked.
fn ai_board(entries: Vec<AiLeaderboardEntry>) -> Vec<AiLeaderboardEntry> {
    let (mut current, mut earlier): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|e| e.run.rules == crate::aipow::CURRENT_AI_RULES);
    current.sort_by(|a, b| {
        by_desc(a.rates.verified_mac_per_sec_lb, b.rates.verified_mac_per_sec_lb)
    });
    for (i, e) in current.iter_mut().enumerate() {
        e.rank = Some(i as u64 + 1);
    }
    earlier.sort_by(|a, b| {
        by_desc(a.rates.verified_mac_per_sec_lb, b.rates.verified_mac_per_sec_lb)
    });
    current.extend(earlier);
    current
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
    /// Canonical-MoE compact-STARK verifier context — a SEPARATE lazily-built,
    /// pin-scoped setup from the dense one (see `aipow_moe::AiMoeVerifier`).
    pub ai_moe_verifier: Arc<tokio::sync::OnceCell<Arc<crate::aipow_moe::AiMoeVerifier>>>,
    /// Canonical-MoE jackpot target (NOCKMARK_AI_MOE_TARGET). Priced in
    /// MAC-equivalents and scaled by F at compare time, so it is NOT
    /// interchangeable with `ai_target`.
    pub ai_moe_target: [u8; 32],
    /// ZK v5 track (M7 step 3): pending challenges + verified runs, JSONL.
    pub zk: Arc<Mutex<crate::zkv5::ZkV5Store>>,
    /// Dense-peak verifier setup (M7 step 4), derived from the shape on the
    /// first dense-peak submission (`aipow_peak::AiPeakVerifier`).
    pub ai_peak_verifier: Arc<tokio::sync::OnceCell<Arc<crate::aipow_peak::AiPeakVerifier>>>,
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
        let zk = crate::zkv5::ZkV5Store::load(crate::zkv5::ZkV5Store::path_in(data_dir))
            .map_err(|e| nockapp::NockAppError::OtherError(format!("zk v5 store load: {e}")))?;
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
            ai_moe_verifier: Arc::new(tokio::sync::OnceCell::new()),
            ai_moe_target: crate::aipow_moe::moe_target_from_env(),
            zk: Arc::new(Mutex::new(zk)),
            ai_peak_verifier: Arc::new(tokio::sync::OnceCell::new()),
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
        // generous headroom for both tracks. Dense-peak artifacts are
        // ~147 KB each; `aipow_peak::max_k_for_body` bounds its k by this.
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Which track a request is for (`?track=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Track {
    /// `?track=ai`: the AI-PoW track.
    Ai,
    /// `?track=zk`: the ZK v5 hash-rate track.
    Zk,
    /// No `track`: what every pre-M7 ZK client sent. Reads get the ZK board
    /// (and `/runs/:id` the pre-Anthropos kernel run, whose links old clients
    /// printed); writes get 410 Gone, because the `%3` proofs/sec submission
    /// they would make is retired.
    Untracked,
}

fn track_from_query(q: &std::collections::HashMap<String, String>) -> Result<Track, String> {
    match q.get("track").map(String::as_str) {
        None => Ok(Track::Untracked),
        Some("ai") => Ok(Track::Ai),
        Some("zk") => Ok(Track::Zk),
        Some(other) => Err(format!("unknown track {other:?} (expected \"zk\" or \"ai\")")),
    }
}

/// What an untracked `POST /challenge` or `POST /run` gets.
///
/// Those are the `%3` complete-proofs/sec submissions of every pre-M7 tock.
/// Since height 147,500 mainnet takes only `%5` ZK proofs and a ZK mining
/// attempt is one Tip5 hash, so a new proofs/sec row would measure work no
/// miner does and could never be ranked. Old rows stay on the board, unranked.
const ZK_V3_RETIRED: &str = "ZK %3 proofs/sec submissions are retired: since height 147,500 (Anthropos) \
     mainnet takes only %5 ZK proofs and a ZK attempt is one Tip5 hash. Use \
     POST /challenge?track=zk — `tock zk-bench --submit`. Earlier rows stay on the \
     board, unranked.";

fn gone() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::GONE, Json(json!({ "error": ZK_V3_RETIRED })))
}

/// `?statement=` selects which AI-PoW statement, within the one AI track.
/// Absent means `dense`, so every M5 request keeps its exact meaning.
/// Request-body cap for every route.
pub const BODY_LIMIT: usize = 4 * 1024 * 1024;

/// The dense-peak target for an untiered challenge: tier 2^24 (~2 min of
/// M1 Max CPU grinding per win). Clients normally ask for a tier.
fn default_peak_target() -> [u8; 32] {
    tock::aipow_peak::peak_target_for_attempts(1 << 24).expect("2^24 is a granted dense-peak tier")
}

fn statement_from_query(
    q: &std::collections::HashMap<String, String>,
) -> Result<crate::aipow::Statement, String> {
    match q.get("statement") {
        Some(s) => crate::aipow::Statement::parse(s),
        None => Ok(crate::aipow::Statement::Dense),
    }
}

/// `?attempts=` requests a difficulty TIER on the AI track: the expected grind
/// attempts per WIN the client wants to be targeted at. Absent means the
/// instance's configured target, i.e. exactly M5/B1 behaviour.
///
/// A malformed value is rejected rather than ignored. A client that believes it
/// is grinding a 2^24 tier while the server quietly handed it 2^13 would report
/// a rate three orders of magnitude wrong, and would have no way to tell.
fn attempts_from_query(
    q: &std::collections::HashMap<String, String>,
) -> Result<Option<u64>, String> {
    match q.get("attempts") {
        None => Ok(None),
        Some(s) => s
            .parse::<u64>()
            .map(Some)
            .map_err(|e| format!("attempts must be a decimal u64: {e}")),
    }
}

async fn new_challenge(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match track_from_query(&q) {
        Err(e) => return bad(e),
        Ok(Track::Untracked) => return gone(),
        Ok(Track::Zk) => return new_zk_challenge(st, &q).await,
        Ok(Track::Ai) => {}
    }
    // AI track from here. Validate the query and derive everything pure
    // BEFORE minting: an unknown statement or a malformed tier must not
    // consume a nonce (or, worse, silently hand back a dense one).
    let (statement, tier) = {
        let statement = match statement_from_query(&q) {
            Ok(s) => s,
            Err(e) => return bad(e),
        };
        let requested = match attempts_from_query(&q) {
            Ok(a) => a,
            Err(e) => return bad(e),
        };
        // k dense-peak artifacts must fit one request body, or the client
        // grinds for up to 3 h and then meets a 413. Refuse before minting.
        let peak_k_max = crate::aipow_peak::max_k_for_body(BODY_LIMIT);
        if statement == crate::aipow::Statement::DensePeak && st.ai_k > peak_k_max {
            return bad(format!(
                "dense-peak needs k <= {peak_k_max} to fit {} MiB; this registry asks for k = {}",
                BODY_LIMIT >> 20,
                st.ai_k
            ));
        }
        // The tier's target is a pure function of (statement, granted tier), so
        // deriving it here — before the mint — keeps a derivation failure from
        // stranding a nonce. `grant_attempts` clamps and rounds first, so the
        // derivation cannot actually fail; the error arm is the honest way to
        // say so without an `expect` in a request handler.
        let tier = match requested {
            None => None,
            Some(n) => {
                let granted = statement.grant_attempts(n);
                match statement.target_for_attempts(granted) {
                    Some(target) => Some((granted, target)),
                    None => {
                        return bad(format!(
                            "no {} target for a tier of {granted} expected attempts per win",
                            statement.as_str()
                        ))
                    }
                }
            }
        };
        (statement, tier)
    };
    let nonce = st
        .kernel
        .lock()
        .await
        .mint_challenge()
        .await
        .expect("mint_challenge failed");
    {
        // AI track: the kernel's mint/uniqueness machinery is reused
        // untouched; the 32-byte challenge is DERIVED from the minted
        // nonce, and the AI store records the server-observed issue time
        // (the AI window runs on the server clock, not kernel @da state).
        let challenge = crate::aipow::challenge32(nonce);
        // Per-statement workload parameters. The challenge, k and window are
        // shared; the shape, the target and the grind rule are not.
        let (params, instance_target) = match statement {
            crate::aipow::Statement::Dense => {
                let p = &tock::aipow::AI_PARAMS;
                (
                    json!({
                        "m": p.m, "k": p.k, "n": p.n,
                        "noise_rank": p.noise_rank, "tile": p.tile,
                    }),
                    st.ai_target,
                )
            }
            crate::aipow::Statement::CanonicalMoe => {
                let p = &tock::aipow_moe::AI_MOE_PARAMS;
                (
                    json!({
                        "m": p.m, "k": p.k, "n": p.n,
                        "noise_rank": p.noise_rank, "tile": p.tile,
                        // The MoE half of the shape: the opened tile side and
                        // the expert routing. Clients must use exactly these.
                        "hw": tock::aipow_moe::AI_MOE_HW,
                        "e": tock::aipow_moe::AI_MOE_E,
                        "top_k": tock::aipow_moe::AI_MOE_TOP_K,
                        // The jackpot is compared against target * F, not
                        // against target — say so on the wire.
                        "shape_work_factor": tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT,
                        "target_is_scaled_by_shape_work_factor": true,
                    }),
                    st.ai_moe_target,
                )
            }
            crate::aipow::Statement::DensePeak => {
                let p = &tock::aipow_peak::AI_PEAK_PARAMS;
                (
                    json!({
                        "m": p.m, "k": p.k, "n": p.n,
                        "noise_rank": p.noise_rank, "tile": p.tile,
                        "tiles_per_extranonce": tock::aipow_peak::PEAK_TICKETS_PER_EXTRANONCE,
                        "shape_work_factor": tock::aipow_peak::MAC_EQUIV_PER_PEAK_ATTEMPT,
                        "target_is_scaled_by_shape_work_factor": true,
                    }),
                    default_peak_target(),
                )
            }
        };
        // A requested tier overrides the instance's configured target, and is
        // recorded on the challenge so the wins are later verified against the
        // target the client was actually told to grind — not against whatever
        // the instance is configured for at submit time.
        let target = tier.map_or(instance_target, |(_, t)| t);
        // Dense-peak challenges record their statement: it sets their 3 h
        // window and binds the submission to dense-peak (see
        // `aipow::PendingChallenge::statement`). Others write the M5 line.
        let pinned = (statement == crate::aipow::Statement::DensePeak).then_some(statement);
        st.ai
            .lock()
            .await
            .record_challenge_for(nonce, crate::aipow::unix_ms(), tier.map(|(_, t)| t), pinned)
            .expect("ai store append failed");
        let mut body = json!({
            "nonce": nonce.to_string(),
            "challenge": tock::aipow::hex32(&challenge),
            "target": tock::aipow::hex32(&target),
            "k": st.ai_k,
            "statement": statement.as_str(),
            "params": params,
            "nonce_rule": statement.nonce_rule(),
        });
        // The GRANTED tier, echoed only when one was asked for — so a request
        // without `?attempts=` produces byte-for-byte the response it always
        // did, and a request with it can always see what the clamp/rounding
        // actually gave. Clients still read `target`; this is the human- and
        // board-legible form of the same thing.
        if let Some((granted, _)) = tier {
            body["attempts"] = json!(granted);
        }
        if pinned.is_some() {
            body["window_ms"] = json!(statement.window_ms());
        }
        (StatusCode::OK, Json(body))
    }
}

/// `POST /challenge?track=zk[&attempts=N]`: mint a ZK v5 challenge.
///
/// The tier (`attempts`, expected Tip5 hashes per win) is validated and its
/// target derived BEFORE the mint, so a malformed request consumes nothing.
/// The challenge, commitment and target are all derived from the kernel nonce
/// and the granted tier; the store records the target the client was told to
/// grind, which is the one its wins are verified and credited against.
async fn new_zk_challenge(
    st: AppState,
    q: &std::collections::HashMap<String, String>,
) -> (StatusCode, Json<serde_json::Value>) {
    use tock::zkv5;
    let requested = match attempts_from_query(q) {
        Ok(a) => a.unwrap_or(zkv5::ZK_ATTEMPTS_DEFAULT),
        Err(e) => return bad(e),
    };
    let granted = zkv5::grant_attempts(requested);
    let Some(target) = zkv5::target_for_attempts(granted) else {
        return bad(format!("no target for a tier of {granted} expected hashes per win"));
    };
    let nonce = st
        .kernel
        .lock()
        .await
        .mint_challenge()
        .await
        .expect("mint_challenge failed");
    st.zk
        .lock()
        .await
        .record_challenge(nonce, crate::aipow::unix_ms(), target, granted)
        .expect("zk store append failed");
    let challenge = zkv5::challenge32(nonce);
    (
        StatusCode::OK,
        Json(json!({
            "track": "zk",
            "nonce": nonce.to_string(),
            "challenge": tock::aipow::hex32(&challenge),
            "commitment": zkv5::belts_to_strings(&zkv5::commitment(&challenge)),
            "commitment_rule": zkv5::ZK_V5_COMMITMENT_RULE,
            "target": zkv5::belts_to_strings(&target),
            "attempts": granted,
            "k": st.k,
            "pow_len": zkv5::ZK_V5_POW_LEN,
            "proof_version": zkv5::ZK_V5_PROOF_VERSION,
            "nonce_rule": zkv5::ZK_V5_NONCE_RULE,
        })),
    )
}

fn bad(msg: String) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

async fn submit_run(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<serde_json::Value>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    match track_from_query(&q) {
        Err(e) => bad(e),
        Ok(Track::Untracked) => gone(),
        Ok(Track::Zk) => submit_zk_run(st, body).await,
        Ok(Track::Ai) => {
            let statement = match statement_from_query(&q) {
                Ok(s) => s,
                Err(e) => return bad(e),
            };
            submit_ai_run(st, statement, body).await
        }
    }
}

#[derive(serde::Deserialize)]
struct ZkRunSubmission {
    nonce: String,
    hardware: String,
    prover_version: String,
    /// Client-measured grind time: displayed, never ranked.
    grind_elapsed_ms: u64,
    /// k base64 proof jams, one per win, in strictly ascending nonce order.
    proofs: Vec<String>,
}

/// A process-random u64 for the verifier's Merkle-check ordering
/// ([`crate::zkv5::verify_win_native`]): soundness does not depend on it.
fn verifier_entropy() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(crate::aipow::unix_ms());
    h.finish()
}

/// `POST /run?track=zk`. Validate cheaply, check every win
/// ([`crate::zkv5::check_win`]: version, object 0, commitment, len, digest,
/// target, canonical polynomials), dedupe on the nonce, then verify every
/// STARK twice — upstream's native verifier and the consensus Hoon verifier —
/// BEFORE touching any state, then commit. See [`crate::zkv5`] for why each
/// check is there.
async fn submit_zk_run(st: AppState, body: serde_json::Value) -> (StatusCode, Json<serde_json::Value>) {
    use base64::Engine;
    use tock::zkv5;

    let sub: ZkRunSubmission = match serde_json::from_value(body) {
        Ok(sub) => sub,
        Err(e) => return bad(format!("bad submission body: {e}")),
    };
    let Ok(nonce) = sub.nonce.parse::<u64>() else {
        return bad("nonce must be a decimal u64".into());
    };
    // 0 would make the grind rate Infinity, which serde_json writes as null.
    if sub.grind_elapsed_ms == 0 {
        return bad("grind_elapsed_ms must be greater than zero".into());
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
    let mut jams: Vec<Vec<u8>> = Vec::with_capacity(sub.proofs.len());
    for (i, b64) in sub.proofs.iter().enumerate() {
        let Ok(jam) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            return bad(format!("proof {i}: invalid base64"));
        };
        jams.push(jam);
    }
    // Pending, unexpired, unused — checked now (before any verifier time) and
    // again at commit.
    let pending = match st.zk.lock().await.challenge_status(nonce, crate::aipow::unix_ms()) {
        Ok(p) => p,
        Err(reason) => return bad(reason),
    };
    // Re-derived from the stored nonce: nothing challenge-shaped is taken from
    // the client.
    let commitment = zkv5::commitment_for_nonce(nonce);
    let target = pending.target;

    // 1. Every cheap check, on every win, before any STARK work.
    let checked = {
        let jams = jams.clone();
        tokio::task::spawn_blocking(move || {
            jams.iter()
                .enumerate()
                .map(|(i, jam)| {
                    crate::zkv5::check_win(&commitment, &target, jam).map_err(|e| format!("proof {i}: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .await
    };
    let checked = match checked {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return bad(e),
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("win check task failed: {join_err}") })),
            )
        }
    };
    // 2. One win per (challenge, nonce), whatever the proof bytes: the suffix
    //    after object 0 is a witness envelope consensus does not identify.
    let nonces: Vec<zkv5::Belts5> = checked.iter().map(|c| c.nonce).collect();
    for (i, n) in nonces.iter().enumerate() {
        if let Some(j) = nonces[..i].iter().position(|m| m == n) {
            return bad(format!(
                "proof {i}: duplicate win — its nonce was already counted by proof {j} \
                 (one win per (challenge, nonce), whatever the proof bytes)"
            ));
        }
    }
    if !zkv5::nonces_strictly_ascending(&nonces) {
        return bad("proofs must be in strictly ascending nonce order".into());
    }
    // 3. Upstream's native STARK verifier, per win, on the blocking pool.
    for (i, win) in checked.into_iter().enumerate() {
        let eny = verifier_entropy();
        let res = tokio::task::spawn_blocking(move || {
            crate::zkv5::verify_win_native(win, &commitment, eny)
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return bad(format!("proof {i}: {e}")),
            Err(join_err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("proof {i}: verify task failed: {join_err}") })),
                )
            }
        }
    }
    // 4. The consensus Hoon verifier (`verify:nv`, via roswell), per win.
    //    `Verifier::verify`'s future is not `Send` (roswell holds a raw-pointer
    //    `NockStack` across an await), so it runs to completion on a blocking
    //    thread under a nested `block_on`.
    for (i, jam) in jams.into_iter().enumerate() {
        let verifier = st.verifier.clone();
        let res = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current()
                .block_on(async move { verifier.lock().await.verify(&jam).await })
        })
        .await;
        match res {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return bad(format!("proof {i}: consensus (Hoon) verifier rejected it")),
            Ok(Err(e)) => return bad(format!("proof {i}: verifier error: {e}")),
            Err(join_err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("proof {i}: verify task failed: {join_err}") })),
                )
            }
        }
    }
    match st.zk.lock().await.commit_run(
        nonce,
        &sub.hardware,
        &sub.prover_version,
        sub.grind_elapsed_ms,
        nonces,
        crate::aipow::unix_ms(),
    ) {
        Ok(Ok(run)) => (StatusCode::OK, Json(json!({ "run_id": run.id }))),
        Ok(Err(reason)) => bad(reason),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("zk store error: {e}") })),
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
    /// Which statement these wins are for. Absent means dense, so an M5
    /// client's body is still a valid dense submission. Takes precedence over
    /// `?statement=` when both are present — the body is what the client
    /// actually proved.
    #[serde(default)]
    statement: Option<String>,
}

/// `POST /run?track=ai` — same shape as the ZK path: validate cheaply,
/// verify every certificate BEFORE touching any state, then commit.
///
/// `statement` (body field, or `?statement=`, default dense) selects the verify
/// rules, the target and the blob format. It is chosen at SUBMIT time rather
/// than pinned at mint time on purpose: the challenge is nothing but 32 bytes
/// of server entropy, both statements bind it, and each is scored against its
/// own target over the same server-observed window — so a client that mints
/// once and proves either statement is doing full, honestly-measured work
/// either way.
async fn submit_ai_run(
    st: AppState,
    query_statement: crate::aipow::Statement,
    body: serde_json::Value,
) -> (StatusCode, Json<serde_json::Value>) {
    use base64::Engine;

    let sub: AiRunSubmission = match serde_json::from_value(body) {
        Ok(sub) => sub,
        Err(e) => return bad(format!("bad submission body: {e}")),
    };
    let statement = match &sub.statement {
        Some(s) => match crate::aipow::Statement::parse(s) {
            Ok(s) => s,
            Err(e) => return bad(e),
        },
        None => query_statement,
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
    // The canonical-MoE grind rule numbers attempts with a u32 ordinal; a
    // wider value cannot name an attempt and must not reach the verifier.
    if statement == crate::aipow::Statement::CanonicalMoe
        && extranonces.iter().any(|x| *x > u64::from(u32::MAX))
    {
        return bad("canonical-moe ordinals must fit in u32".into());
    }
    // Dense-peak indices are `extranonce · 2^19 + tile` with a u32 extranonce.
    if statement == crate::aipow::Statement::DensePeak
        && extranonces
            .iter()
            .any(|x| tock::aipow_peak::split_attempt_index(*x).is_none())
    {
        return bad("dense-peak attempt indices must have a u32 extranonce".into());
    }
    // Decode + size-cap every blob before any expensive work.
    let blob_max = match statement {
        crate::aipow::Statement::Dense => crate::aipow::AI_SUBMISSION_BLOB_MAX,
        crate::aipow::Statement::CanonicalMoe => crate::aipow_moe::AI_MOE_SUBMISSION_BLOB_MAX,
        crate::aipow::Statement::DensePeak => tock::aipow_peak::AI_PEAK_SUBMISSION_BLOB_MAX,
    };
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(sub.wins.len());
    for (i, win) in sub.wins.iter().enumerate() {
        let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(&win.cert_b64) else {
            return bad(format!("win {i}: invalid base64"));
        };
        if blob.len() > blob_max {
            return bad(format!(
                "win {i}: blob {} bytes exceeds {blob_max}",
                blob.len(),
            ));
        }
        blobs.push(blob);
    }
    // Challenge must be pending, unexpired, unused — checked cheaply now
    // (before burning verifier CPU) and re-checked at commit.
    let pending = match st
        .ai
        .lock()
        .await
        .challenge_status(nonce, crate::aipow::unix_ms())
    {
        Ok(p) => p,
        Err(reason) => return bad(reason),
    };
    // A dense-peak challenge takes only dense-peak wins, and dense-peak wins
    // need a dense-peak challenge: its 3 h window and 2^36 tier ceiling belong
    // to that statement alone.
    let peak = crate::aipow::Statement::DensePeak;
    if (pending.statement == Some(peak)) != (statement == peak) {
        return bad(format!(
            "challenge was minted for {}, submission is {}",
            pending.statement.map_or("dense or canonical-moe", |s| s.as_str()),
            statement.as_str()
        ));
    }
    // The target these wins are verified AND scored against is the one this
    // challenge was issued at, when the client requested a difficulty tier.
    // Otherwise the instance's configured target, which is what every
    // pre-B2a challenge (and every untiered one since) is measured at.
    //
    // Note the challenge does NOT pin the statement — that is chosen at submit
    // time by design (see this function's docs) — so a tier'd challenge minted
    // for one statement and submitted under the other is scored with the other
    // statement's reading of the same 32 bytes. That is not a way to be
    // credited for work not done: the credit is always `2^256/(Θ+1)` expected
    // attempts at the target the wins were actually verified against, so a
    // cross-statement reading changes how hard the grind was, and the credit
    // moves with it in lockstep. (In practice the dense→MoE direction is
    // fail-closed anyway: `T·F` overflows for the easier tiers.)
    let target = pending.target.unwrap_or(match statement {
        crate::aipow::Statement::Dense => st.ai_target,
        crate::aipow::Statement::CanonicalMoe => st.ai_moe_target,
        crate::aipow::Statement::DensePeak => default_peak_target(),
    });
    // Verifier context: built/loaded once per statement, lazily, on the
    // blocking pool (first submission of a statement pays the load from its
    // persisted blob or a build-by-proving; every later one reuses the
    // OnceCell). The two statements own separate contexts.
    enum AnyVerifier {
        Dense(Arc<crate::aipow::AiVerifier>),
        Moe(Arc<crate::aipow_moe::AiMoeVerifier>),
        Peak(Arc<crate::aipow_peak::AiPeakVerifier>),
    }
    let verifier = match statement {
        crate::aipow::Statement::Dense => {
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
                Ok(v) => AnyVerifier::Dense(v.clone()),
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": format!("ai verifier unavailable: {e}") })),
                    )
                }
            }
        }
        crate::aipow::Statement::CanonicalMoe => {
            let data_dir = st.data_dir.clone();
            match st
                .ai_moe_verifier
                .get_or_try_init(|| async move {
                    tokio::task::spawn_blocking(move || {
                        crate::aipow_moe::AiMoeVerifier::load_or_build(&data_dir).map(Arc::new)
                    })
                    .await
                    .map_err(|e| format!("moe verifier init task failed: {e}"))?
                })
                .await
            {
                Ok(v) => AnyVerifier::Moe(v.clone()),
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": format!("ai moe verifier unavailable: {e}") })),
                    )
                }
            }
        }
        crate::aipow::Statement::DensePeak => {
            let data_dir = st.data_dir.clone();
            match st
                .ai_peak_verifier
                .get_or_try_init(|| async move {
                    tokio::task::spawn_blocking(move || {
                        crate::aipow_peak::AiPeakVerifier::load_or_build(&data_dir).map(Arc::new)
                    })
                    .await
                    .map_err(|e| format!("dense-peak verifier init task failed: {e}"))?
                })
                .await
            {
                Ok(v) => AnyVerifier::Peak(v.clone()),
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": format!("dense-peak verifier unavailable: {e}") })),
                    )
                }
            }
        }
    };
    // Verify every win (CPU-bound) on the blocking pool. The challenge is
    // re-derived from the stored nonce — nothing challenge-shaped is taken
    // from the client.
    let challenge = crate::aipow::challenge32(nonce);
    for (i, (win, blob)) in sub.wins.iter().zip(blobs).enumerate() {
        let attempt = win.extranonce;
        let verify_result = match &verifier {
            AnyVerifier::Dense(v) => {
                let v = v.clone();
                tokio::task::spawn_blocking(move || v.verify(&challenge, &target, attempt, &blob))
                    .await
            }
            AnyVerifier::Moe(v) => {
                let v = v.clone();
                // Range-checked above, so the cast is exact.
                let ordinal = attempt as u32;
                tokio::task::spawn_blocking(move || v.verify(&challenge, &target, ordinal, &blob))
                    .await
            }
            AnyVerifier::Peak(v) => {
                let v = v.clone();
                tokio::task::spawn_blocking(move || v.verify(&challenge, &target, attempt, &blob))
                    .await
            }
        };
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
        statement,
        &sub.hardware,
        &sub.prover_version,
        &target,
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
    /// 1-based rank among rows verified under the current AI rules
    /// (`hardened`); `None` for earlier rows, which stay listed after them.
    pub rank: Option<u64>,
    /// Estimated NOCK/day at the observed AI difficulty, from the RANKED
    /// `verified_mac_per_sec_lb` — never the client-reported grind rate.
    /// ABSENT until an `ai-pow` tip has actually been observed: an AI
    /// difficulty cannot be derived from the ZK one, and a zero would read
    /// as "this card earns nothing" rather than "we have not seen a block".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_nock_per_day: Option<f64>,
    /// `est_nock_per_day × spot NOCK/USD`, absent on the same terms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_usd_per_day: Option<f64>,
}

fn to_ai_entry(
    run: crate::aipow::AiRunRecord,
    econ: Option<&crate::economics::EconParams>,
) -> AiLeaderboardEntry {
    let server_window_ms = run.submitted_at_ms.saturating_sub(run.issued_at_ms).max(1);
    // The per-run persisted target; an unparseable one (torn line) falls
    // back to the max target, which can only UNDERSTATE the rate.
    let target = tock::aipow::parse_hex32(&run.target).unwrap_or([0xff; 32]);
    let rates = crate::aipow::rates(
        run.statement,
        run.k,
        &target,
        server_window_ms,
        run.grind_elapsed_ms,
    );
    // The published, already-rounded rate goes in, so the estimate is one a
    // reader can reproduce from what the row itself prints.
    let est_nock_per_day = econ.and_then(|p| {
        crate::economics::ai_nock_per_day(rates.verified_mac_per_sec_lb, p)
            .map(crate::economics::signif4)
    });
    let est_usd_per_day = est_nock_per_day
        .zip(econ)
        .and_then(|(nock, p)| crate::economics::usd_per_day(nock, p));
    AiLeaderboardEntry {
        run,
        server_window_ms,
        rates,
        rank: None,
        est_nock_per_day,
        est_usd_per_day,
    }
}

/// The AI board, every row, ranked ([`ai_board`]).
async fn ai_board_rows(st: &AppState) -> Vec<AiLeaderboardEntry> {
    let econ = st.econ.read().await.clone();
    let entries: Vec<AiLeaderboardEntry> = st
        .ai
        .lock()
        .await
        .runs()
        .iter()
        .cloned()
        .map(|run| to_ai_entry(run, econ.as_ref()))
        .collect();
    ai_board(entries)
}

/// The v5 half of the ZK board, unranked (ranking happens in [`zk_board`]).
async fn zk_v5_rows(st: &AppState) -> Vec<ZkV5LeaderboardEntry> {
    let econ = st.econ.read().await.clone();
    st.zk
        .lock()
        .await
        .runs()
        .iter()
        .cloned()
        .map(|run| to_zk_entry(run, econ.as_ref()))
        .collect()
}

async fn leaderboard(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    match track_from_query(&q) {
        Err(e) => bad(e).into_response(),
        Ok(Track::Ai) => (StatusCode::OK, Json(ai_board_rows(&st).await)).into_response(),
        // `/leaderboard` with no track has always been the ZK board.
        Ok(Track::Zk | Track::Untracked) => {
            let v5 = zk_v5_rows(&st).await;
            match st.kernel.lock().await.leaderboard().await {
                Ok(runs) => {
                    let legacy = runs.into_iter().map(to_entry).collect();
                    (StatusCode::OK, Json(zk_board(v5, legacy))).into_response()
                }
                Err(_e) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<ZkBoardRow>::new())).into_response()
                }
            }
        }
    }
}

/// `GET /runs/:id`. The tracks number runs independently, so the track picks
/// the id space: `?track=ai`, `?track=zk` (v5), or none for the
/// pre-Anthropos kernel runs (the links pre-M7 tock printed). A ranked row
/// carries the rank it has on its board.
async fn run_by_id(
    State(st): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    match track_from_query(&q) {
        Err(e) => bad(e).into_response(),
        Ok(Track::Ai) => match ai_board_rows(&st).await.into_iter().find(|e| e.run.id == id) {
            Some(e) => (StatusCode::OK, Json(Some(e))).into_response(),
            None => (StatusCode::NOT_FOUND, Json(None::<AiLeaderboardEntry>)).into_response(),
        },
        Ok(Track::Zk) => {
            let found = zk_board(zk_v5_rows(&st).await, Vec::new())
                .into_iter()
                .find_map(|r| match r {
                    ZkBoardRow::V5(e) if e.run.id == id => Some(e),
                    _ => None,
                });
            match found {
                Some(e) => (StatusCode::OK, Json(Some(e))).into_response(),
                None => (StatusCode::NOT_FOUND, Json(None::<ZkV5LeaderboardEntry>)).into_response(),
            }
        }
        Ok(Track::Untracked) => match st.kernel.lock().await.leaderboard().await {
            Ok(runs) => match runs.into_iter().find(|r| r.id == id).map(to_entry) {
                Some(e) => (StatusCode::OK, Json(Some(e))).into_response(),
                None => (StatusCode::NOT_FOUND, Json(None::<LeaderboardEntry>)).into_response(),
            },
            Err(_e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(None::<LeaderboardEntry>)).into_response()
            }
        },
    }
}

/// Every input behind every published figure, so a reader can recompute any
/// of them without trusting us — the same standard the rest of the registry
/// holds itself to (a ranked rate is checkable because the certificates are
/// re-verifiable; an earnings estimate is checkable because the inputs and
/// the formulas are both printed).
///
/// Anything unobserved is OMITTED rather than defaulted: no AI block seen
/// means no `ai_*` keys at all, no price means no `nock_usd*` keys and no
/// `est_usd_per_day` anywhere on the board. Observation times are published
/// alongside, plus their age at request time, because the two difficulties
/// refresh at very different rates — ZK on nearly every 10-minute tick, AI
/// only when the ~30% of blocks that are AI put one at the tip.
///
/// `?pps=` and `?mac_per_sec=` run the two models for an arbitrary machine.
async fn economics(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    use crate::economics as econ;

    let Some(p) = st.econ.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "economics not configured on this instance" })),
        );
    };
    let mut out = json!({
        // --- ZK track inputs ---
        "difficulty": p.difficulty,
        "block_time_secs": econ::block_time_secs(),
        "est_network_zk_hashes_per_sec": econ::network_pps(&p),
        // --- shared inputs ---
        "block_reward_nock": p.block_reward_nock,
        // The consensus exchange rate between the two tracks' work units
        // (height-selected in tx-engine.hoon). Published so the AI board's
        // zk_attempt_equiv_per_sec is checkable too.
        "mac_equiv_per_zk_attempt": econ::MAC_EQUIV_PER_ZK_ATTEMPT,
        "mac_equiv_per_zk_attempt_since_height": 154_500,
        "model": {
            "zk": "est_nock_per_day = zk_hashes_per_sec * 86400 / difficulty * block_reward_nock",
            "ai": "est_nock_per_day = mac_per_sec * 86400 / ai_difficulty_mac_equiv_per_block * block_reward_nock",
            "ai_difficulty": "ai_difficulty_mac_equiv_per_block = 2^256 / (ai_target + 1)",
            "usd": "est_usd_per_day = est_nock_per_day * nock_usd",
        },
        "notes": {
            "difficulty": "expected attempts per ZK block; since height 147,500 (Anthropos, ZK proof %5) one attempt is one Tip5 hash of proof object 0, and a full proof is built only for a winning nonce",
            "zk_board": econ::ZK_PROOF_ROWS_UNPRICED,
            "ai_difficulty": "expected MAC-equivalents per AI block, independent of the tile shape a miner picked",
            "block_reward_nock": "already the 80% miner share; an AI block and a ZK block pay a miner identically",
            "ranked_input": "board estimates are computed from the ranked, server-measured rate (verified_zk_hashes_per_sec_lb, verified_mac_per_sec_lb), never a client-reported one",
            "block_times": "ZK 500 s and AI 214 s ideal intervals since Anthropos (~30% ZK / ~70% AI blocks, ~150 s combined)",
            "excludes": econ::EXCLUDED_FROM_ESTIMATES,
        },
    });
    // Absent while difficulty is still the operator-supplied seed: an
    // unconfirmed figure should not wear an observation time it never had.
    if let Some(t) = p.difficulty_observed_at {
        out["difficulty_observed_at"] = json!(t);
        out["difficulty_age_secs"] = json!(econ::age_secs(t));
    }
    if let Some(src) = &p.difficulty_source {
        out["difficulty_source"] = json!(src);
    }
    // Absent while the reward is still the env seed — the same rule.
    if let Some(t) = p.block_reward_observed_at {
        out["block_reward_observed_at"] = json!(t);
        out["block_reward_age_secs"] = json!(econ::age_secs(t));
    }
    // …and the age of the BLOCK it came from, which is the figure that
    // actually says whether the chain is moving. We re-read the tip every ten
    // minutes, so `difficulty_age_secs` stays small even against a chain (or
    // an explorer) that stopped days ago; only this one can tell you.
    if let Some(t) = p.difficulty_tip_timestamp {
        out["difficulty_tip_timestamp"] = json!(t);
        out["difficulty_tip_age_secs"] = json!(econ::age_secs(t));
    }
    if let Some(ai) = &p.ai {
        out["ai_difficulty_mac_equiv_per_block"] = json!(ai.mac_equiv_per_block);
        if let Some(t) = &ai.target {
            out["ai_target"] = json!(t);
        }
        out["ai_difficulty_source"] = json!(ai.source);
        out["ai_difficulty_observed_at"] = json!(ai.observed_at);
        out["ai_difficulty_age_secs"] = json!(econ::age_secs(ai.observed_at));
        if let Some(t) = ai.tip_timestamp {
            out["ai_difficulty_tip_timestamp"] = json!(t);
            out["ai_difficulty_tip_age_secs"] = json!(econ::age_secs(t));
        }
        out["ai_block_time_secs"] = json!(econ::ai_block_time_secs());
        out["est_ai_network_mac_per_sec"] = json!(econ::ai_network_mac_per_sec(&p));
    }
    if let Some(price) = &p.price {
        out["nock_usd"] = json!(price.usd);
        out["nock_usd_source"] = json!(price.source);
        out["nock_usd_observed_at"] = json!(price.observed_at);
        out["nock_usd_age_secs"] = json!(econ::age_secs(price.observed_at));
    }
    if let Some(hps) = q.get("zk_hashes_per_sec").and_then(|s| s.parse::<f64>().ok()) {
        let nock = econ::nock_per_day(hps, &p);
        out["zk_hashes_per_sec"] = json!(hps);
        out["est_nock_per_day"] = json!(nock);
        out["network_share"] = json!(hps / econ::network_pps(&p));
        if let Some(usd) = econ::usd_per_day(nock, &p) {
            out["est_usd_per_day"] = json!(usd);
        }
    }
    // The pre-M7 query. Answered with why, not with a number: a proofs/sec
    // figure divided by a difficulty that counts hashes is the ≈ 0 the board
    // printed for weeks.
    if q.contains_key("pps") {
        out["pps_note"] = json!(
            "?pps= is retired: since height 147,500 a ZK attempt is one Tip5 hash, not a proof. Use ?zk_hashes_per_sec="
        );
    }
    if let Some(mac) = q.get("mac_per_sec").and_then(|s| s.parse::<f64>().ok()) {
        out["mac_per_sec"] = json!(mac);
        // Still absent, deliberately, when no AI tip has been seen: asking the
        // question does not conjure an answer.
        if let Some(nock) = econ::ai_nock_per_day(mac, &p) {
            out["est_ai_nock_per_day"] = json!(nock);
            if let Some(usd) = econ::usd_per_day(nock, &p) {
                out["est_ai_usd_per_day"] = json!(usd);
            }
        }
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
