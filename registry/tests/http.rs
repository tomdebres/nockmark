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

/// `?attempts=` on the AI challenge: the granted tier is echoed, the target it
/// derives is the statement's own, clamping is visible, and — the property that
/// matters most for an already-deployed API — omitting the parameter reproduces
/// the previous response exactly.
#[tokio::test]
async fn ai_challenge_difficulty_tiers() {
    use nockmark_registry::aipow::Statement;
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);

    let mint = |path: String| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, v)
        }
    };

    // --- Backward compatibility: no `attempts` means no change, at all. ---
    let (status, plain) = mint("/challenge?track=ai".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        plain["target"].as_str().unwrap(),
        tock::aipow::hex32(&nockmark_registry::aipow::default_target()),
        "an untiered challenge still carries the instance's configured target"
    );
    let mut keys: Vec<&String> = plain.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "challenge",
            "k",
            "nonce",
            "nonce_rule",
            "params",
            "statement",
            "target"
        ],
        "an untiered response must carry no new keys — not even an echoed tier"
    );

    // --- A requested tier: echoed, and the target realizes it exactly. ---
    let (status, tiered) = mint("/challenge?track=ai&attempts=1048576".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tiered["attempts"], 1_048_576u64);
    let target = tock::aipow::parse_hex32(tiered["target"].as_str().unwrap()).unwrap();
    assert_eq!(
        Statement::Dense.expected_attempts_per_win(&target),
        1_048_576.0
    );
    assert_eq!(target, Statement::Dense.target_for_attempts(1 << 20).unwrap());
    assert_ne!(
        tiered["target"], plain["target"],
        "a tier must actually move the target"
    );

    // --- Clamping at both ends is granted, not refused, and is visible. ---
    let (status, floor) = mint("/challenge?track=ai&attempts=1".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(floor["attempts"], tock::aipow::AI_ATTEMPTS_MIN);
    let (status, ceiling) = mint("/challenge?track=ai&attempts=99999999999999".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ceiling["attempts"], tock::aipow::AI_ATTEMPTS_MAX);
    // …as is the rounding to the nearest power of two.
    let (_, rounded) = mint("/challenge?track=ai&attempts=5000".into()).await;
    assert_eq!(rounded["attempts"], 4096u64, "5000 rounds down to 2^12");

    // --- The same tier is a DIFFERENT target on the canonical-MoE statement,
    //     because that statement compares the jackpot against target · F. ---
    let (status, moe) = mint("/challenge?track=ai&statement=canonical-moe&attempts=1048576".into())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moe["attempts"], 1_048_576u64);
    let moe_target = tock::aipow::parse_hex32(moe["target"].as_str().unwrap()).unwrap();
    assert_eq!(
        Statement::CanonicalMoe.expected_attempts_per_win(&moe_target),
        1_048_576.0
    );
    assert_ne!(
        moe_target, target,
        "one tier, two statements, two targets — off by exactly F"
    );

    // --- A malformed tier is rejected outright, before a nonce is minted. ---
    let (status, err) = mint("/challenge?track=ai&attempts=lots".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("attempts"), "{err}");

    // --- The ZK track ignores the parameter exactly as it ignores ?statement=.
    let (status, zk) = mint("/challenge?attempts=1048576".into()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(zk.get("attempts").is_none());
    assert!(zk.get("target").is_none());
    assert_eq!(zk["nonce_rule"], "fnv1a-splitmix64-v1");
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

/// The Logos AI ASERT anchor, `T_ai = 2^192` — `2^64` MAC-equivalents per
/// block. Used as the stand-in AI difficulty here because mainnet has not
/// yet produced an AI-PoW tip to read one from.
const AI_ANCHOR_MAC_EQUIV_PER_BLOCK: f64 = 18_446_744_073_709_551_616.0; // 2^64

fn econ_seed() -> nockmark_registry::economics::EconParams {
    nockmark_registry::economics::EconParams::seed(6_000.0, 1_228.8)
}

async fn get_json(app: axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Put one verified AI run on the board without proving anything: the store
/// is the board's only source, and its write path is what a real submission
/// reaches after verification anyway.
async fn seed_ai_run(st: &nockmark_registry::http::AppState) {
    let mut ai = st.ai.lock().await;
    ai.record_challenge(4242, 1_000_000, None).unwrap();
    ai.commit_run(
        4242,
        nockmark_registry::aipow::Statement::Dense,
        "test-gpu",
        "v",
        &nockmark_registry::aipow::default_target(),
        2_000,
        vec![0, 1],
        1_004_000, // a 4 s server window
    )
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn economics_estimates_and_annotates_the_board() {
    let _guard = test_lock().lock().await;
    let st = state().await;
    *st.econ.write().await = Some(econ_seed());
    let app = nockmark_registry::http::router(st.clone());

    // /economics?zk_hashes_per_sec= computes the ZK estimate: since
    // Anthropos a ZK attempt is a Tip5 hash, and difficulty counts hashes.
    let (status, v) = get_json(app.clone(), "/economics?zk_hashes_per_sec=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["difficulty"], 6_000.0);
    let est = v["est_nock_per_day"].as_f64().unwrap();
    assert!((est - 10.0 * 86_400.0 / 6_000.0 * 1_228.8).abs() < 1e-6);
    // The retired ?pps= is answered with why, never with a number.
    let (_, v) = get_json(app.clone(), "/economics?pps=0.2191").await;
    assert!(v["pps_note"].as_str().unwrap().contains("Tip5 hash"));
    assert!(v.get("est_nock_per_day").is_none(), "{v}");

    // ZK board rows carry NO estimate, even with economics configured: they
    // are complete proofs/sec, and difficulty counts hashes.
    let nonce = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    st.kernel.lock().await
        .submit_run(nonce, "hw", "v", 2, 400).await.unwrap().unwrap();
    let (_, v) = get_json(app.clone(), "/leaderboard").await;

    // Pinned as an exact key set, so an earnings field cannot drift back onto
    // the proofs/sec path by accident.
    let mut keys: Vec<&str> = v[0].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "elapsed_ms",
            "hardware",
            "id",
            "issued_at",
            "k",
            "nonce",
            "proofs_per_sec",
            "prover_version",
            "self_reported_pps",
            "server_window_ms",
            "submitted_at",
        ]
    );
    // …and /economics says why, in words.
    let (_, e) = get_json(app, "/economics").await;
    assert!(e["notes"]["zk_board"].as_str().unwrap().contains("147,500"));
}

/// USD appears on the AI board exactly when a price has been observed, and is
/// always the published NOCK/day times the published price — one
/// multiplication a reader can check against the two numbers beside it.
#[tokio::test]
async fn usd_is_omitted_without_a_price_and_derived_from_nock_with_one() {
    use nockmark_registry::economics::{signif4, AiDifficulty, NockPrice};

    let _guard = test_lock().lock().await;
    let st = state().await;
    // A fully-observed instance: ZK difficulty, AI difficulty, and a price.
    {
        let mut econ = st.econ.write().await;
        let p = econ.insert(econ_seed());
        p.ai = Some(AiDifficulty {
            mac_equiv_per_block: AI_ANCHOR_MAC_EQUIV_PER_BLOCK,
            target: Some("6277101735386680763835789423207666416102355444464034512896".into()),
            source: "test".into(),
            observed_at: 1_700_000_000,
            tip_timestamp: None,
        });
        p.price = Some(NockPrice {
            usd: 0.02,
            source: "https://example.test/price".into(),
            observed_at: 1_700_000_100,
        });
    }
    seed_ai_run(&st).await;
    let nonce = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    st.kernel.lock().await
        .submit_run(nonce, "hw", "v", 2, 400).await.unwrap().unwrap();
    let app = nockmark_registry::http::router(st.clone());

    // The ZK board stays unpriced whatever the price feed says.
    let (_, zk) = get_json(app.clone(), "/leaderboard").await;
    assert!(zk[0].get("est_nock_per_day").is_none(), "{}", zk[0]);
    assert!(zk[0].get("est_usd_per_day").is_none(), "{}", zk[0]);

    let (_, ai) = get_json(app.clone(), "/leaderboard?track=ai").await;
    let mac = ai[0]["verified_mac_per_sec_lb"].as_f64().unwrap();
    let ai_nock = ai[0]["est_nock_per_day"].as_f64().unwrap();
    let expect = signif4(mac * 86_400.0 / AI_ANCHOR_MAC_EQUIV_PER_BLOCK * 1_228.8);
    assert_eq!(ai_nock, expect, "AI row: {mac} MAC/s");
    assert_eq!(ai[0]["est_usd_per_day"].as_f64().unwrap(), signif4(ai_nock * 0.02));

    // Drop the price: every USD figure vanishes from the board and from
    // /economics, while every NOCK figure stays exactly where it was.
    st.econ.write().await.as_mut().unwrap().price = None;
    let (_, ai) = get_json(app.clone(), "/leaderboard?track=ai").await;
    assert_eq!(ai[0]["est_nock_per_day"].as_f64().unwrap(), ai_nock);
    assert!(ai[0].get("est_usd_per_day").is_none(), "{}", ai[0]);
    let (_, e) = get_json(app, "/economics?zk_hashes_per_sec=1e9&mac_per_sec=1e11").await;
    assert!(e.get("nock_usd").is_none());
    assert!(e.get("est_usd_per_day").is_none());
    assert!(e.get("est_ai_usd_per_day").is_none());
}

/// Before an `ai-pow` tip has ever been seen there is no AI earnings figure
/// at all — not a zero, and not one quietly derived from ZK difficulty.
#[tokio::test]
async fn ai_earnings_are_omitted_until_an_ai_tip_is_observed() {
    let _guard = test_lock().lock().await;
    let st = state().await;
    *st.econ.write().await = Some(econ_seed()); // ZK only, as every instance boots
    seed_ai_run(&st).await;
    let app = nockmark_registry::http::router(st.clone());

    let (_, ai) = get_json(app.clone(), "/leaderboard?track=ai").await;
    // The measured rate is still published — the measurement never depended
    // on the chain. Only the money does.
    assert!(ai[0]["verified_mac_per_sec_lb"].as_f64().unwrap() > 0.0);
    assert!(ai[0].get("est_nock_per_day").is_none(), "{}", ai[0]);
    assert!(ai[0].get("est_usd_per_day").is_none(), "{}", ai[0]);

    let (_, e) = get_json(app.clone(), "/economics?mac_per_sec=1e11").await;
    assert_eq!(e["mac_per_sec"], 1e11, "the question is echoed…");
    assert!(e.get("est_ai_nock_per_day").is_none(), "…but not answered");
    assert!(e.get("ai_difficulty_mac_equiv_per_block").is_none());
    assert!(e.get("ai_target").is_none());
    // The ZK side is entirely unaffected by the AI side being unknown.
    let (_, e) = get_json(app, "/economics?zk_hashes_per_sec=1.8e7").await;
    assert!(e["est_nock_per_day"].as_f64().unwrap() > 0.0);
}

/// `/economics` publishes every input behind every figure, each with the time
/// it was observed, so a reader can recompute the lot without trusting us.
#[tokio::test]
async fn economics_publishes_every_input_needed_to_recompute() {
    use nockmark_registry::economics::{signif4, AiDifficulty, NockPrice};

    let _guard = test_lock().lock().await;
    let st = state().await;
    let now = nockmark_registry::economics::unix_now();
    {
        let mut econ = st.econ.write().await;
        let p = econ.insert(econ_seed());
        p.difficulty_observed_at = Some(now - 60);
        p.ai = Some(AiDifficulty {
            mac_equiv_per_block: AI_ANCHOR_MAC_EQUIV_PER_BLOCK,
            target: Some("6277101735386680763835789423207666416102355444464034512896".into()),
            source: "https://nockblocks.example/rpc/v1".into(),
            observed_at: now - 86_400, // an AI tip a day old, as an RPC-only source can leave it
            tip_timestamp: Some(now - 86_500),
        });
        p.price = Some(NockPrice {
            usd: 0.02189269,
            source: nockmark_registry::economics::DEFAULT_PRICE_URL.into(),
            observed_at: now - 30,
        });
    }
    let app = nockmark_registry::http::router(st.clone());
    let (status, v) = get_json(app, "/economics?zk_hashes_per_sec=1.8e7&mac_per_sec=1.439e11").await;
    assert_eq!(status, StatusCode::OK);

    // Both difficulties, each with its own observation time and age — the
    // AI one visibly a day behind the ZK one.
    assert_eq!(v["difficulty"], 6_000.0);
    assert_eq!(v["difficulty_observed_at"], now - 60);
    assert!((59..=120).contains(&v["difficulty_age_secs"].as_u64().unwrap()));
    assert_eq!(v["ai_difficulty_mac_equiv_per_block"], AI_ANCHOR_MAC_EQUIV_PER_BLOCK);
    assert_eq!(
        v["ai_target"],
        "6277101735386680763835789423207666416102355444464034512896"
    );
    assert!(v["ai_difficulty_age_secs"].as_u64().unwrap() >= 86_400);
    assert_eq!(v["ai_difficulty_source"], "https://nockblocks.example/rpc/v1");
    // Still the env seed: no source and no reward observation to claim.
    assert!(v.get("difficulty_source").is_none());
    assert!(v.get("block_reward_observed_at").is_none());

    // The raw AI target recomputes the AI difficulty: 2^256/(2^192+1) = 2^64.
    let t: f64 = v["ai_target"].as_str().unwrap().parse().unwrap();
    let recomputed = 2f64.powi(256) / (t + 1.0);
    assert_eq!(recomputed, AI_ANCHOR_MAC_EQUIV_PER_BLOCK);

    // Price with its source and freshness.
    assert_eq!(v["nock_usd"], 0.02189269);
    assert_eq!(v["nock_usd_source"], nockmark_registry::economics::DEFAULT_PRICE_URL);
    assert_eq!(v["nock_usd_observed_at"], now - 30);
    assert!(v["nock_usd_age_secs"].as_u64().unwrap() >= 30);

    // The rest of the recompute kit.
    assert_eq!(v["block_reward_nock"], 1_228.8);
    assert_eq!(v["block_time_secs"], 500.0);
    assert_eq!(v["ai_block_time_secs"], 214.0);
    assert_eq!(v["mac_equiv_per_zk_attempt"], 14_285_714.0);
    assert_eq!(v["mac_equiv_per_zk_attempt_since_height"], 154_500);

    // …and both models actually reproduce from those inputs alone.
    let zk = v["est_nock_per_day"].as_f64().unwrap();
    assert!((zk - 1.8e7 * 86_400.0 / 6_000.0 * 1_228.8).abs() < 1e-3);
    assert_eq!(v["est_usd_per_day"].as_f64().unwrap(), signif4(zk * 0.02189269));
    let ai = v["est_ai_nock_per_day"].as_f64().unwrap();
    assert!(
        (ai - 1.439e11 * 86_400.0 / AI_ANCHOR_MAC_EQUIV_PER_BLOCK * 1_228.8).abs() < 1e-9,
        "{ai}"
    );
    assert_eq!(v["est_ai_usd_per_day"].as_f64().unwrap(), signif4(ai * 0.02189269));

    // And it says out loud what it does not account for.
    let excludes = v["notes"]["excludes"].as_array().unwrap();
    assert_eq!(excludes.len(), 5);
    let joined = excludes.iter().map(|s| s.as_str().unwrap()).collect::<Vec<_>>().join(" ");
    for word in ["electricity", "capital", "pool fees", "difficulty drift", "price drift"] {
        assert!(joined.contains(word), "notes must mention {word}: {joined}");
    }
}
