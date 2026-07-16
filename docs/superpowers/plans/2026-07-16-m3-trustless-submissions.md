# M3 — Trustless Public Submissions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open the Nockmark registry to trustless public submissions: `tock bench --submit` end-to-end, server-window timing enforcement, abuse hardening, k=8, an economics endpoint, and announcement collateral.

**Architecture:** The kernel (`hoon/registry.hoon`) gains one rejection rule (claimed `elapsed-ms` may not exceed the server-observed mint→submit window). The driver (`registry/`) re-ranks the leaderboard by the server-window rate (the trustless number; client `elapsed_ms` becomes self-reported info), adds per-IP rate limiting + body/string limits, and an `/economics` endpoint. `tock` gains a `--submit` flag implementing fetch-challenge → prove-k → submit. The e2e test drives the whole flow through a real localhost HTTP server using the same client code miners run.

**Tech Stack:** Rust (axum 0.7, tokio, reqwest 0.12 new), Hoon (registry kernel, rebuilt via hoonc), Railway (deploy).

## Global Constraints

- Every `cargo`/`hoonc` invocation needs:
  `export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"` and `export RUST_MIN_STACK=8388608`. There is **no rustup binary** on this machine — the toolchain dir is on disk but not managed.
- Repo root: `/Users/openclaw/Obsidian/vault/tomdebres/projects/nockmark/m3-trustless-submissions` (branch `tomdebres/m3-trustless-submissions`). All paths below are relative to it unless absolute.
- The nockchain checkout at `../../nockchain` (i.e. `projects/nockchain`, pinned `31b8a015`, hoonc prebuilt at `target/release/hoonc`) is **READ-ONLY**. Never modify it.
- `tock/assets/` is gitignored; runtime/test jams live there locally. Tracked deploy copies live in `deploy/assets/` — whenever `registry.jam` is rebuilt, copy it to `deploy/assets/registry.jam` and commit that.
- Kernel/verifier boots abort if run concurrently in one test binary — every new test in `registry/tests/http.rs` must take the existing `test_lock()` guard first.
- Final constants: `K_DEFAULT = 8`, body limit 4 MiB, `hardware` ≤ 128 bytes, `prover_version` ≤ 64 bytes, rate limit 10 req/min/IP on `POST /challenge` and `POST /run`, challenge expiry unchanged (`~h1`).
- Production URL: `https://nockmark-registry-production.up.railway.app`.
- Timing tests use real sleeps with ≥3× margins (e.g. sleep 1200 ms, claim 1000 ms). Never claim an elapsed_ms within 2× of the slept window.
- Commit after every task; do not push or open a PR until Task 8 says so.

**Design decisions locked in (from design spec + M2 carry-forwards):**
1. The ranked leaderboard rate is `k / server_window` (`submitted_at − issued_at`). Rejecting `elapsed_ms > window` alone would NOT close the trust gap — a cheater under-reports `elapsed_ms`; ranking from the server window is what makes rates trustless (design spec §trust-model). Client `elapsed_ms` stays visible as `self_reported_pps`.
2. Timing rejection lives in the **kernel** (it owns both timestamps and `now`); rate display lives in the **driver**.
3. Rate limiting is in-app (hand-rolled fixed-window per IP) — Railway has no edge rate limiter, and a 60-line tested module beats an alpha-crate dependency.
4. Economics is **driver-side** (in-memory, env-configured, optional URL auto-refresh), not kernel state: difficulty needs no durability (deviation from design spec's kernel-cache noted deliberately — avoids kernel churn and constant checkpoint writes).
5. The server window includes proof verification time (~0.5 s/proof, stamped at kernel poke after verification). That bias is conservative (rates are lower bounds) and gets documented, not engineered away.

---

### Task 1: Kernel timing enforcement (`elapsed-ms` vs server window)

**Files:**
- Modify: `hoon/registry.hoon:66-70` (the `%submit-run` arm)
- Modify: `registry/tests/kernel.rs`
- Modify: `registry/tests/http.rs:132-176` (`leaderboard_sorts_by_proofs_per_sec_desc` — becomes timing-realistic)
- Modify (generated): `deploy/assets/registry.jam`
- Test: `registry/tests/kernel.rs`

**Interfaces:**
- Consumes: existing `RegistryKernel::{boot, mint_challenge, submit_run, leaderboard}` (`registry/src/kernel.rs`).
- Produces: kernel rejection reason cord `'elapsed-exceeds-window'` — Task 2 and Task 6 rely on this exact string; a rebuilt `tock/assets/registry.jam` + `deploy/assets/registry.jam` all later tasks boot.

- [ ] **Step 1: Populate local gitignored assets (one-time setup this worktree needs)**

```bash
cd /Users/openclaw/Obsidian/vault/tomdebres/projects/nockmark/m3-trustless-submissions
mkdir -p tock/assets
cp deploy/assets/registry.jam deploy/assets/roswell.jam tock/assets/
cp ../m0-prover-spike/tock/assets/miner.jam tock/assets/
ls -l tock/assets   # expect: miner.jam (~42 MB), registry.jam (~1.6 MB), roswell.jam (~26 MB)
```

- [ ] **Step 2: Baseline — existing kernel tests pass**

Run (from `registry/`, with the Global Constraints env exports):
`cargo test --test kernel`
Expected: `mint_and_submit_and_leaderboard ... ok` (first build takes ~10+ min; kernel boot needs the jams from Step 1).

- [ ] **Step 3: Write the failing test**

Append to `registry/tests/kernel.rs`:

```rust
#[tokio::test]
async fn elapsed_exceeding_server_window_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = RegistryKernel::boot(jam(), dir.path()).await.unwrap();
    let nonce = k.mint_challenge().await.unwrap();
    // Claim an hour of proving when the mint→submit window is milliseconds:
    // provably lying about elapsed time.
    let rej = k
        .submit_run(nonce, "cheat-hw", "v", 2, 3_600_000)
        .await
        .unwrap();
    assert_eq!(rej.unwrap_err(), "elapsed-exceeds-window");
}
```

Also update the existing happy path in `mint_and_submit_and_leaderboard` (a claim of 42 s zero milliseconds after minting will now be rejected). Replace the happy-path submit with:

```rust
    // Enforcement is elapsed-ms ≤ server window: wait so a small claim fits.
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    let id = k
        .submit_run(nonce, "test-hw", "31b8a015", 2, 1_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, 0);
```

and change the replay submit on the next lines to the same `(nonce, "test-hw", "31b8a015", 2, 1_000)` arguments (it must still fail with `"nonce-used"` — the used-check precedes the window check).

- [ ] **Step 4: Run the new test to verify it fails**

Run: `cargo test --test kernel elapsed_exceeding_server_window_is_rejected`
Expected: FAIL — `called `Result::unwrap_err()` on an `Ok` value` (the kernel currently accepts the lying claim).

- [ ] **Step 5: Make `leaderboard_sorts_by_proofs_per_sec_desc` timing-realistic (it fakes elapsed_ms and will break under enforcement)**

In `registry/tests/http.rs`, replace the two mint/submit blocks inside `leaderboard_sorts_by_proofs_per_sec_desc` with (keep the rest of the test):

```rust
    // Windows are real (enforced by the kernel now): each run's claimed
    // elapsed_ms must fit inside its own mint→submit window. slow-hw:
    // ~1200 ms window / 1000 ms claim; fast-hw: ~400 ms window / 300 ms
    // claim. fast-hw ranks first under both client-pps and server-window
    // ranking (Task 2 flips the ranked source; this scenario survives it).
    let nonce_slow = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    st.kernel
        .lock()
        .await
        .submit_run(nonce_slow, "slow-hw", "v", 2, 1_000)
        .await
        .unwrap()
        .unwrap();

    let nonce_fast = st.kernel.lock().await.mint_challenge().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    st.kernel
        .lock()
        .await
        .submit_run(nonce_fast, "fast-hw", "v", 2, 300)
        .await
        .unwrap()
        .unwrap();
```

- [ ] **Step 6: Implement the kernel check**

In `hoon/registry.hoon`, in the `%submit-run` arm, directly after the stale-nonce check (`?: (gth now (add issued-at.u.c ~h1)) ...`), insert:

```hoon
      =/  window-ms  (div (mul (sub now issued-at.u.c) 1.000) ~s1)
      ?:  (gth elapsed-ms.cau window-ms)
        [[%rejected 'elapsed-exceeds-window']~ k]
```

(`@dr` is 64.64 fixed-point seconds; `~s1` = `2^64`, so `diff × 1000 ÷ ~s1` is the window in ms. Atoms are bignums — no overflow. State shape is untouched, so the `%0` state version stays and the live Railway checkpoint still loads.)

- [ ] **Step 7: Rebuild registry.jam and sync the deploy copy**

```bash
cd /Users/openclaw/Obsidian/vault/tomdebres/projects/nockmark/m3-trustless-submissions
bash scripts/build-registry-jam.sh          # writes tock/assets/registry.jam
cp tock/assets/registry.jam deploy/assets/registry.jam
```

Expected: `wrote .../tock/assets/registry.jam`.

- [ ] **Step 8: Run kernel + http tests to verify they pass**

Run (from `registry/`): `cargo test --test kernel && cargo test --test http`
Expected: all PASS, including `elapsed_exceeding_server_window_is_rejected`.

- [ ] **Step 9: Commit**

```bash
git add hoon/registry.hoon deploy/assets/registry.jam registry/tests/kernel.rs registry/tests/http.rs
git commit -m "kernel: reject elapsed-ms claims exceeding the server-observed window"
```

---

### Task 2: Rank the leaderboard by the server-window rate

**Files:**
- Modify: `registry/src/kernel.rs` (add `da_diff_to_ms` + unit tests)
- Modify: `registry/src/http.rs` (`LeaderboardEntry`, `leaderboard`, `run_by_id`)
- Modify: `registry/static/index.html`
- Test: `registry/tests/http.rs`

**Interfaces:**
- Consumes: `RunRecord { issued_at: u128, submitted_at: u128, k, elapsed_ms, .. }` (`registry/src/kernel.rs`), the Task 1 test scenario (slow-hw window ≈ 1200 ms, fast-hw ≈ 400 ms).
- Produces: `pub fn da_diff_to_ms(issued_at: u128, submitted_at: u128) -> u64` in `registry/src/kernel.rs`; leaderboard JSON rows gain `server_window_ms: u64` and `self_reported_pps: f64`, and `proofs_per_sec` becomes server-window-based. Tasks 6–8 rely on these exact field names.

- [ ] **Step 1: Write the failing unit test for `da_diff_to_ms`**

Append to `registry/src/kernel.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::da_diff_to_ms;

    #[test]
    fn da_diff_to_ms_converts_fixed_point_seconds() {
        let one_second: u128 = 1 << 64; // @dr: 64.64 fixed-point seconds
        assert_eq!(da_diff_to_ms(0, one_second), 1_000);
        assert_eq!(da_diff_to_ms(one_second, one_second * 43), 42_000);
        assert_eq!(da_diff_to_ms(0, one_second / 2), 500);
        // clock nonsense (submitted before issued) must not panic
        assert_eq!(da_diff_to_ms(one_second, 0), 0);
    }
}
```

- [ ] **Step 2: Run it to verify it fails to compile**

Run: `cargo test --lib da_diff_to_ms`
Expected: FAIL — `cannot find function da_diff_to_ms`.

- [ ] **Step 3: Implement `da_diff_to_ms`**

Add to `registry/src/kernel.rs` (below `atom_to_u128`):

```rust
/// Difference of two `@da` atoms (64.64 fixed-point seconds) in milliseconds.
/// The absolute Urbit epoch cancels out — only the difference is meaningful.
pub fn da_diff_to_ms(issued_at: u128, submitted_at: u128) -> u64 {
    let diff = submitted_at.saturating_sub(issued_at);
    // diff < 2^76 for windows under ~1 h, so ×1000 stays well inside u128.
    (diff.saturating_mul(1_000) >> 64) as u64
}
```

Run: `cargo test --lib da_diff_to_ms` — Expected: PASS.

- [ ] **Step 4: Write the failing http test assertions**

In `registry/tests/http.rs`, `leaderboard_sorts_by_proofs_per_sec_desc`, replace everything from `let rows = v.as_array().unwrap();` to the end of the test with:

```rust
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
```

Run: `cargo test --test http leaderboard_sorts` — Expected: FAIL (no `server_window_ms` field).

- [ ] **Step 5: Implement server-window entries in `registry/src/http.rs`**

Replace the `LeaderboardEntry` struct and add a builder:

```rust
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
```

In `leaderboard()`, replace the `.map(|run| { ... })` closure with `.map(to_entry)` (the sort line stays — it now sorts by the server-window rate). In `run_by_id()`, replace its `.map(|run| { ... })` closure with `.map(to_entry)`.

- [ ] **Step 6: Update the static leaderboard page**

In `registry/static/index.html`, change the header row to:

```html
  <thead><tr><th>#</th><th>hardware</th><th>proofs/sec (verified)</th><th>self-reported</th><th>k</th><th>prover</th></tr></thead>
```

and the cell array in the script to:

```js
    [i + 1, r.hardware, r.proofs_per_sec, r.self_reported_pps, r.k, r.prover_version].forEach(v => {
```

Also change the footer paragraph's first sentence to:

```html
<p>Every row passed server-side STARK verification against a server-issued
challenge nonce; the ranked rate is computed from the server-observed
submission window, so it cannot be inflated by the submitter. <a
href="https://github.com/tomdebres/nockmark">How it works.</a></p>
```

- [ ] **Step 7: Run all registry tests**

Run (from `registry/`): `cargo test`
Expected: all PASS (`http`, `kernel`, `binding` unit tests, `da_diff_to_ms`).

- [ ] **Step 8: Commit**

```bash
git add registry/src/kernel.rs registry/src/http.rs registry/static/index.html registry/tests/http.rs
git commit -m "registry: rank leaderboard by server-window rate; client elapsed_ms is self-reported"
```

---

### Task 3: Abuse hardening — rate limit, body limit, string caps

**Files:**
- Create: `registry/src/ratelimit.rs`
- Modify: `registry/src/lib.rs` (add `pub mod ratelimit;`)
- Modify: `registry/src/http.rs` (AppState field, router wiring, submit_run caps)
- Test: `registry/src/ratelimit.rs` (unit), `registry/tests/http.rs`

**Interfaces:**
- Consumes: `AppState { kernel, verifier }` and `router()` from Task 2's `registry/src/http.rs`.
- Produces: `pub struct RateLimiter` with `pub fn new(max_per_window: u32, window: Duration) -> Self` and `pub fn check(&self, key: &str) -> bool`; `AppState` gains `pub limiter: Arc<RateLimiter>`. HTTP: 429 on limit, 413 over 4 MiB, 400 on over-long strings.

- [ ] **Step 1: Write the failing rate-limiter unit tests**

Create `registry/src/ratelimit.rs`:

```rust
//! Fixed-window per-key rate limiting for the two expensive POST routes.
//! In-app because Railway fronts us with no edge rate limiter (M2 review
//! carry-forward). Fixed-window (not sliding) is enough: the goal is to
//! bound verifier CPU (~0.5 s/proof), not to be fair at the margin.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    max_per_window: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self { max_per_window, window, hits: Mutex::new(HashMap::new()) }
    }

    /// Record a hit for `key`; false = over the limit for this window.
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        // Bound memory under key-spraying: drop expired windows once large.
        if hits.len() > 10_000 {
            hits.retain(|_, (t0, _)| now.duration_since(*t0) < self.window);
        }
        let entry = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max_per_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_blocks() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        assert!(rl.check("b"), "keys are independent");
    }

    #[test]
    fn window_expiry_resets_the_count() {
        let rl = RateLimiter::new(1, Duration::from_millis(50));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(rl.check("a"), "new window after expiry");
    }
}
```

Add `pub mod ratelimit;` to `registry/src/lib.rs`.

- [ ] **Step 2: Run the unit tests**

Run: `cargo test --lib ratelimit`
Expected: PASS (module is self-contained; TDD cycle here is compile-fail → implement in one file).

- [ ] **Step 3: Write the failing http tests**

Append to `registry/tests/http.rs`:

```rust
#[tokio::test]
async fn challenge_is_rate_limited_per_ip() {
    let _guard = test_lock().lock().await;
    let app = nockmark_registry::http::router(state().await);
    // Limit is 10/min/IP on POST /challenge and POST /run.
    for i in 0..10 {
        let res = app.clone()
            .oneshot(Request::post("/challenge")
                .header("x-forwarded-for", "203.0.113.7")
                .body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "request {i} should pass");
    }
    let res = app.clone()
        .oneshot(Request::post("/challenge")
            .header("x-forwarded-for", "203.0.113.7")
            .body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // A different IP is unaffected.
    let res = app
        .oneshot(Request::post("/challenge")
            .header("x-forwarded-for", "203.0.113.8")
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
```

Run: `cargo test --test http rate_limited` — Expected: FAIL (no limiter yet; status 200 on the 11th request).

- [ ] **Step 4: Wire the limiter, body limit, and caps into `registry/src/http.rs`**

Add imports:

```rust
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Request};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;

use crate::ratelimit::RateLimiter;
```

Extend `AppState` and `boot`:

```rust
#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Mutex<RegistryKernel>>,
    pub verifier: Arc<Mutex<Verifier>>,
    pub limiter: Arc<RateLimiter>,
}
```

In `AppState::boot`, add `limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60)))` to the struct literal.

Add the middleware and rewire `router()`:

```rust
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
```

In `submit_run`, after the `elapsed_ms == 0` check, add:

```rust
    if sub.hardware.len() > 128 {
        return bad("hardware string too long (max 128 bytes)".into());
    }
    if sub.prover_version.len() > 64 {
        return bad("prover_version string too long (max 64 bytes)".into());
    }
```

- [ ] **Step 5: Run the http tests**

Run: `cargo test --test http`
Expected: all PASS, including the three new tests. (The pre-existing tests each make ≤3 POSTs on a fresh `AppState`, so the shared `"direct"` key never trips the limit.)

- [ ] **Step 6: Commit**

```bash
git add registry/src/ratelimit.rs registry/src/lib.rs registry/src/http.rs registry/tests/http.rs
git commit -m "registry: per-IP rate limiting, explicit 4MiB body limit, string caps"
```

---

### Task 4: k = 8, configurable per instance

**Files:**
- Modify: `registry/src/http.rs` (`K_DEFAULT`, `AppState.k`, `boot_with_k`, handlers)
- Modify: `registry/src/main.rs` (`--k` flag)
- Modify: `registry/tests/http.rs` (`state()` helper boots k=2 for fixture tests; new default-k test)
- Test: `registry/tests/http.rs`

**Interfaces:**
- Consumes: Task 3's `AppState` (kernel, verifier, limiter).
- Produces: `K_DEFAULT: u64 = 8`; `AppState { .., pub k: u64 }`; `pub async fn boot_with_k(jam: &Path, data_dir: &Path, k: u64) -> Result<Self, NockAppError>` (and `boot` = `boot_with_k(.., K_DEFAULT)`). Tasks 5–6 rely on `/challenge` returning the instance's `k` and on `boot_with_k` for fast tests.

- [ ] **Step 1: Write the failing test**

Append to `registry/tests/http.rs`:

```rust
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
```

Run: `cargo test --test http default_k` — Expected: FAIL (`K_DEFAULT` is 2; `state()` has no k override).

- [ ] **Step 2: Implement configurable k**

In `registry/src/http.rs`:

```rust
/// Proofs per submission. 8 ≈ 3 minutes on an M1 Mac (21 s/proof) — the
/// design spec's "minutes of proving" target; the spike value was 2.
pub const K_DEFAULT: u64 = 8;
```

Add `pub k: u64` to `AppState`; split boot:

```rust
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
```

In `new_challenge`, return `"k": st.k` instead of `K_DEFAULT`. In `submit_run`, replace both `K_DEFAULT` uses with `st.k` (the proof-count check message and the kernel `submit_run(.., st.k, ..)` poke — capture `let k = st.k;` before the loop since `st` is moved into the spawn closure? It isn't — `st.verifier.clone()` is used; just use `st.k` directly, it's `Copy` through `&st`).

In `registry/src/main.rs`, add a flag and use it:

```rust
    /// Proofs required per submission (challenge responses advertise this).
    #[arg(long, default_value_t = nockmark_registry::http::K_DEFAULT)]
    k: u64,
```

and boot with `AppState::boot_with_k(&cli.kernel, &cli.data_dir, cli.k)`.

In `registry/tests/http.rs`, change `state()` to boot with k=2:

```rust
    let state = nockmark_registry::http::AppState::boot_with_k(jam, dir.path(), 2).await.unwrap();
```

In `registry/tests/e2e.rs`, change the boot line to `boot_with_k(reg_jam, dir.path(), K)` (K is already 2 there).

- [ ] **Step 3: Run the tests**

Run: `cargo test --test http`
Expected: all PASS (fixture tests still send 2 proofs against k=2 states; `default_k_is_eight_and_test_states_override_it` passes).

- [ ] **Step 4: Commit**

```bash
git add registry/src/http.rs registry/src/main.rs registry/tests/http.rs registry/tests/e2e.rs
git commit -m "registry: K_DEFAULT=8 (minutes-of-proving target), per-instance --k override"
```

---

### Task 5: `tock bench --submit`

**Files:**
- Create: `tock/src/client.rs`
- Modify: `tock/src/lib.rs` (add `pub mod client;`)
- Modify: `tock/Cargo.toml` (reqwest)
- Modify: `tock/src/main.rs` (flags + submit flow; bench collects proof jams)
- Test: `tock/src/client.rs` unit tests

**Interfaces:**
- Consumes: registry JSON API — `POST /challenge` → `{nonce, k, pow_len, nonce_rule}`; `POST /run` accepts `{nonce, hardware, prover_version, elapsed_ms, proofs: [b64…]}` → `{run_id}` or `{error}`. `tock::{hardware, miner, nonce}` as today.
- Produces: `tock::client::{Challenge, Submission, fetch_challenge, submit_run, hardware_summary}` with the exact signatures below — Task 6's e2e drives the registry through them.

```rust
pub struct Challenge { pub nonce: String, pub k: u64, pub pow_len: u64, pub nonce_rule: String }
pub struct Submission { pub nonce: String, pub hardware: String, pub prover_version: String, pub elapsed_ms: u64, pub proofs: Vec<String> }
pub async fn fetch_challenge(base: &str) -> Result<Challenge, String>
pub async fn submit_run(base: &str, sub: &Submission) -> Result<u64, String>
pub fn hardware_summary(hw: &crate::hardware::Hardware) -> String
```

- [ ] **Step 1: Add the dependency**

In `tock/Cargo.toml` `[dependencies]`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

(rustls so bench boxes need no system OpenSSL — `setup-bench.sh` targets bare cloud instances.)

- [ ] **Step 2: Write the failing unit tests**

Create `tock/src/client.rs` with ONLY the tests first:

```rust
//! Registry client for `tock bench --submit`: fetch a challenge, submit the
//! proof bundle. Mirrors the manual seeding flow in
//! docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_parses_registry_json() {
        let ch: Challenge = serde_json::from_str(
            r#"{"nonce":"12345","pow_len":64,"k":8,"nonce_rule":"fnv1a-splitmix64-v1"}"#,
        )
        .unwrap();
        assert_eq!(ch.nonce, "12345");
        assert_eq!(ch.k, 8);
        assert_eq!(ch.pow_len, 64);
        assert_eq!(ch.nonce_rule, "fnv1a-splitmix64-v1");
    }

    #[test]
    fn submission_serializes_the_registry_shape() {
        let sub = Submission {
            nonce: "12345".into(),
            hardware: "hw".into(),
            prover_version: "31b8a015".into(),
            elapsed_ms: 42,
            proofs: vec!["AA==".into()],
        };
        let v: serde_json::Value = serde_json::to_value(&sub).unwrap();
        assert_eq!(v["nonce"], "12345");
        assert_eq!(v["elapsed_ms"], 42);
        assert_eq!(v["proofs"][0], "AA==");
    }

    #[test]
    fn hardware_summary_is_compact_and_capped() {
        let hw = crate::hardware::Hardware {
            cpu_model: Some("Apple M1 Max".into()),
            logical_cores: Some(10),
            physical_cores: Some(10),
            perf_cores: Some(8),
            eff_cores: Some(2),
            mem_bytes: Some(64 * (1u64 << 30)),
            os: "macOS 15.5".into(),
            arch: "aarch64".into(),
        };
        let s = hardware_summary(&hw);
        assert!(s.contains("Apple M1 Max"));
        assert!(s.contains("10c"));
        assert!(s.contains("64GB"));
        assert!(s.len() <= 128);

        let hw_long = crate::hardware::Hardware {
            cpu_model: Some("X".repeat(300)),
            logical_cores: None,
            physical_cores: None,
            perf_cores: None,
            eff_cores: None,
            mem_bytes: None,
            os: "os".into(),
            arch: "arch".into(),
        };
        // The registry rejects >128 bytes — the client must never send it.
        assert!(hardware_summary(&hw_long).len() <= 128);
    }
}
```

Add `pub mod client;` to `tock/src/lib.rs`. Run (from `tock/`): `cargo test client` — Expected: FAIL to compile (`Challenge` not found).

- [ ] **Step 3: Implement the client**

Prepend to `tock/src/client.rs` (above the tests):

```rust
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    pub nonce: String,
    pub k: u64,
    pub pow_len: u64,
    pub nonce_rule: String,
}

#[derive(Debug, Serialize)]
pub struct Submission {
    pub nonce: String,
    pub hardware: String,
    pub prover_version: String,
    pub elapsed_ms: u64,
    pub proofs: Vec<String>,
}

fn http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("reqwest client")
}

pub async fn fetch_challenge(base: &str) -> Result<Challenge, String> {
    let url = format!("{}/challenge", base.trim_end_matches('/'));
    let resp = http(Duration::from_secs(30))
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("POST {url}: HTTP {}", resp.status()));
    }
    resp.json::<Challenge>()
        .await
        .map_err(|e| format!("bad challenge JSON from {url}: {e}"))
}

/// Returns the recorded run id. Timeout is generous: the registry verifies
/// every proof (~0.5 s each) before answering.
pub async fn submit_run(base: &str, sub: &Submission) -> Result<u64, String> {
    let url = format!("{}/run", base.trim_end_matches('/'));
    let resp = http(Duration::from_secs(180))
        .post(&url)
        .json(sub)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("bad response JSON from {url}: {e}"))?;
    if !status.is_success() {
        return Err(body["error"].as_str().unwrap_or("unknown error").to_string());
    }
    body["run_id"]
        .as_u64()
        .ok_or_else(|| "response missing run_id".to_string())
}

/// Compact self-reported hardware descriptor, capped to the registry's
/// 128-byte limit (truncated on a char boundary).
pub fn hardware_summary(hw: &crate::hardware::Hardware) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(hw.cpu_model.clone().unwrap_or_else(|| "unknown CPU".into()));
    if let Some(c) = hw.logical_cores {
        parts.push(format!("{c}c"));
    }
    if let Some(b) = hw.mem_bytes {
        parts.push(format!("{}GB", b >> 30));
    }
    parts.push(hw.os.clone());
    parts.push(hw.arch.clone());
    let mut s = parts.join(" / ");
    if s.len() > 128 {
        let mut cut = 128;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}
```

Run: `cargo test client` — Expected: 3 tests PASS.

- [ ] **Step 4: Wire `--submit` into `tock/src/main.rs`**

Add flags to the `Bench` variant:

```rust
        /// Registry base URL. When set, fetches a challenge (which supplies
        /// the seed, k, and pow-len — --seed/-k are ignored) and submits the
        /// proof bundle after proving. Without it, bench stays fully local.
        #[arg(long, value_name = "REGISTRY_URL")]
        submit: Option<String>,
        /// Prover version string recorded on submission (the nockchain
        /// commit the kernel jams were built from).
        #[arg(long, default_value = "31b8a015")]
        prover_version: String,
```

Pass both through the `Command::Bench` match arm into `bench(...)`.

Change `bench()` to collect proof jams and submit. The complete new flow (replacing the current body from the `let header_belts = ...` line onward; keep hardware detection, kernel read/hash, keep-proofs dir creation, and serf boot exactly as they are — booting **before** the challenge fetch keeps the ~3 s/serf boots out of the server window):

```rust
    // Resolve the workload: local seed, or a server challenge (fetched
    // AFTER kernel boot so boot time never counts against the window).
    let (challenge, seed, k, pow_len) = match &submit {
        Some(base) => {
            let ch = client::fetch_challenge(base)
                .await
                .unwrap_or_else(|e| panic!("could not fetch challenge: {e}"));
            assert_eq!(
                ch.nonce_rule,
                nonce::NONCE_RULE,
                "registry expects nonce rule {:?}; this tock speaks {:?} — upgrade tock",
                ch.nonce_rule,
                nonce::NONCE_RULE
            );
            eprintln!(
                "challenge {} from {base} (k={}, pow_len={}); the clock is running",
                ch.nonce, ch.k, ch.pow_len
            );
            let seed = ch.nonce.clone();
            let (k, pow_len) = (ch.k, ch.pow_len);
            (Some(ch), seed, k, pow_len)
        }
        None => (None, seed.to_string(), k, pow_len),
    };
    if threads > k {
        eprintln!("note: {threads} threads for {k} proofs — extra threads idle");
    }

    let header_belts = nonce::seed_to_belts(&seed, "header");

    let total_t0 = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for (tid, serf) in serfs.into_iter().enumerate() {
        let seed = seed.clone();
        let keep_proofs = keep_proofs.clone();
        tasks.spawn(async move {
            let mut results: Vec<(u64, u64, Vec<u8>)> = Vec::new(); // (i, ms, jam)
            let mut i = tid as u64;
            while i < k {
                let nonce_belts = nonce::seed_to_belts(&format!("{seed}/{i}"), "nonce");
                let out = miner::run_prove(&serf, &header_belts, &nonce_belts, pow_len).await;
                eprintln!(
                    "  proof {i}: {:.2?} ({} bytes, thread {tid})",
                    out.duration,
                    out.proof_jam.len()
                );
                if let Some(dir) = &keep_proofs {
                    std::fs::write(dir.join(format!("proof-{i}.jam")), &out.proof_jam)
                        .expect("could not write proof jam");
                }
                results.push((i, out.duration.as_millis() as u64, out.proof_jam));
                i += threads;
            }
            results
        });
    }
    let mut per_proof: Vec<(u64, u64, Vec<u8>)> = Vec::new();
    while let Some(res) = tasks.join_next().await {
        per_proof.extend(res.expect("proving task panicked"));
    }
    let total_s = total_t0.elapsed().as_secs_f64();
    per_proof.sort_by_key(|(i, _, _)| *i);

    let result = BenchResult {
        tool: "tock",
        tool_version: env!("CARGO_PKG_VERSION"),
        nonce_rule: nonce::NONCE_RULE,
        seed: seed.clone(),
        proof_version: miner::PROOF_VERSION,
        pow_len,
        k,
        threads,
        kernel_jam_sha256,
        kernel_boot_s,
        per_proof_ms: per_proof.iter().map(|(_, ms, _)| *ms).collect(),
        proof_bytes: per_proof.iter().map(|(_, _, jam)| jam.len() as u64).collect(),
        total_s,
        proofs_per_sec: k as f64 / total_s,
        hardware: hw,
        timestamp_epoch_s: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs(),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("could not serialize result")
        );
    } else {
        print_human(&result);
    }

    if let (Some(base), Some(ch)) = (&submit, challenge) {
        use base64::Engine;
        let proofs: Vec<String> = per_proof
            .iter() // i-ordered: proof i must sit at index i (binding check)
            .map(|(_, _, jam)| base64::engine::general_purpose::STANDARD.encode(jam))
            .collect();
        // Round UP: never claim faster than measured, so an honest claim
        // always fits inside the server window.
        let elapsed_ms = (total_s * 1000.0).ceil().max(1.0) as u64;
        let sub = client::Submission {
            nonce: ch.nonce,
            hardware: client::hardware_summary(&result.hardware),
            prover_version,
            elapsed_ms,
            proofs,
        };
        match client::submit_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: run {id}");
                println!("  {}/runs/{id}", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission REJECTED or failed: {e}");
                eprintln!("(local bench result above is still valid)");
                std::process::exit(1);
            }
        }
    }
```

Also: change `bench`'s signature to take `submit: Option<String>, prover_version: String`; delete the old `assert!(threads >= 1 && threads <= k, ...)` and keep only `assert!(threads >= 1, "threads must be at least 1");` (server k is unknown at parse time); add `use base64::Engine;` needs `base64 = "0.22"` in `tock/Cargo.toml` `[dependencies]`; add `use tock::client;` to the imports in `main.rs`; `result.hardware` replaces the moved `hw` binding (note `BenchResult` takes `hardware: hw` by value — keep that, and reference it back out via `&result.hardware` as shown).

- [ ] **Step 5: Verify offline behavior is unchanged and everything builds**

Run (from `tock/`):

```bash
cargo test && cargo build --release
./target/release/tock bench --seed m3-smoke --kernel assets/miner.jam -k 1 --json | head -20
```

Expected: tests PASS; the local bench runs one proof (~21 s on this Mac) and prints JSON with `"seed": "m3-smoke"` — no network touched.

- [ ] **Step 6: Commit**

```bash
git add tock/Cargo.toml tock/Cargo.lock tock/src/client.rs tock/src/lib.rs tock/src/main.rs
git commit -m "tock: bench --submit — challenge-response submission to a registry"
```

---

### Task 6: End-to-end test through the real HTTP client

**Files:**
- Modify: `registry/tests/e2e.rs` (rewrite: serve on a real port, drive via `tock::client`)
- Test: `registry/tests/e2e.rs`

**Interfaces:**
- Consumes: `tock::client::{fetch_challenge, submit_run, Submission}` (Task 5), `AppState::boot_with_k` (Task 4), kernel rejection `'elapsed-exceeds-window'` (Task 1), leaderboard fields `server_window_ms`/`proofs_per_sec`/`self_reported_pps` (Task 2).
- Produces: the M3 acceptance test — the exact code path an external miner exercises.

- [ ] **Step 1: Rewrite `registry/tests/e2e.rs`**

```rust
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
    let sub2 = tock::client::Submission { nonce: ch2.nonce, ..{
        tock::client::Submission {
            nonce: String::new(),
            hardware: "e2e-test".into(),
            prover_version: "31b8a015".into(),
            elapsed_ms: 3_600_000,
            proofs,
        }
    }};
    let err = tock::client::submit_run(&base, &sub2).await.unwrap_err();
    assert!(err.contains("nonce"), "binding must reject cross-nonce proofs: {err}");

    // 6. the board ranks by the server window: the elapsed_ms=1 lie shows
    //    up in self_reported_pps but the ranked rate is bounded by reality.
    let board: serde_json::Value = reqwest::get(format!("{base}/leaderboard"))
        .await.unwrap().json().await.unwrap();
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
    assert!(rows[0]["self_reported_pps"].as_f64().unwrap() > 100.0,
        "the lie is visible as self-reported");
    assert!(rows[0]["server_window_ms"].as_u64().unwrap() >= true_elapsed_ms);
}
```

Note: the `sub2` construction above uses struct-update on a fresh literal only to reuse `proofs`; write it as a plain literal with `nonce: ch2.nonce, proofs` if the update-syntax nesting reads badly — either compiles.

Add to `registry/Cargo.toml` `[dev-dependencies]`: `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }` (for the direct `/leaderboard` GET; the client fns come via the `tock` path dep).

- [ ] **Step 2: Run it**

Run (from `registry/`):
`RUST_MIN_STACK=8388608 cargo test --release --test e2e -- --ignored --nocapture`
Expected: PASS in ~1–2 min (release build + 2 proofs ≈ 45 s + kernel boots). If step 5's error is `expected 2 proofs, got 2` style instead of a nonce error, the k override didn't reach the state — recheck Task 4.

- [ ] **Step 3: Run the full registry + tock suites once (pre-commit gate)**

```bash
(cd tock && cargo test)
(cd registry && cargo test && RUST_MIN_STACK=8388608 cargo test --release --test e2e -- --ignored)
```

Expected: everything PASS.

- [ ] **Step 4: Commit**

```bash
git add registry/tests/e2e.rs registry/Cargo.toml registry/Cargo.lock
git commit -m "e2e: drive the registry through tock's real submit client; anti-cheat assertions"
```

---

### Task 7: Economics endpoint (difficulty → est. NOCK/day)

**Files:**
- Create: `registry/src/economics.rs`
- Modify: `registry/src/lib.rs` (add `pub mod economics;`)
- Modify: `registry/src/http.rs` (AppState field, `/economics` route, leaderboard `est_nock_per_day`)
- Modify: `registry/src/main.rs` (env init + optional refresh task)
- Modify: `registry/static/index.html` (NOCK/day column when present)
- Test: `registry/src/economics.rs` (unit), `registry/tests/http.rs`

**Interfaces:**
- Consumes: Task 2's `to_entry` builder; `AppState` from Task 4.
- Produces: `economics::{EconParams, nock_per_day, from_env, refresh_loop}`; `AppState.econ: Arc<tokio::sync::RwLock<Option<EconParams>>>`; `GET /economics[?pps=…]`; leaderboard rows gain optional `est_nock_per_day`.

**Model (document verbatim in the module):** zkPoW is attempt-lottery like Bitcoin: with `difficulty` = expected proof attempts per block, a miner producing `pps` proofs/sec expects `pps × 86400 / difficulty` blocks/day, each paying `block_reward_nock`. Values are operator-supplied via env (`NOCKMARK_DIFFICULTY`, `NOCKMARK_BLOCK_REWARD_NOCK`) because the Block Explorer API's difficulty exposure needs probing at deploy time (Task 8); `NOCKMARK_ECON_URL` optionally auto-refreshes difficulty from any JSON endpoint exposing a top-level `"difficulty"` number. Estimates are labelled estimates.

- [ ] **Step 1: Write the failing unit tests**

Create `registry/src/economics.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_network_pps_earns_the_whole_emission() {
        // 10-min blocks → 144/day. A miner who IS the entire network
        // (pps × 600 s = difficulty) earns exactly 144 × reward per day.
        let p = EconParams { difficulty: 6_000.0, block_reward_nock: 2_048.0 };
        let network_pps = p.difficulty / 600.0;
        let est = nock_per_day(network_pps, &p);
        assert!((est - 144.0 * p.block_reward_nock).abs() < 1e-6);
    }

    #[test]
    fn est_scales_linearly_with_pps() {
        let p = EconParams { difficulty: 1_000_000.0, block_reward_nock: 2_048.0 };
        assert!((nock_per_day(0.1, &p) * 2.0 - nock_per_day(0.2, &p)).abs() < 1e-9);
    }

    #[test]
    fn from_env_requires_both_vars() {
        // (Runs single-threaded within this test fn; env is process-global,
        // so restore it.)
        std::env::remove_var("NOCKMARK_DIFFICULTY");
        std::env::remove_var("NOCKMARK_BLOCK_REWARD_NOCK");
        assert!(from_env().is_none());
        std::env::set_var("NOCKMARK_DIFFICULTY", "5000000");
        assert!(from_env().is_none(), "difficulty alone is not enough");
        std::env::set_var("NOCKMARK_BLOCK_REWARD_NOCK", "2048");
        let p = from_env().unwrap();
        assert_eq!(p.difficulty, 5_000_000.0);
        assert_eq!(p.block_reward_nock, 2_048.0);
        std::env::remove_var("NOCKMARK_DIFFICULTY");
        std::env::remove_var("NOCKMARK_BLOCK_REWARD_NOCK");
    }
}
```

Run: `cargo test --lib economics` — Expected: FAIL to compile.

- [ ] **Step 2: Implement the module**

Prepend to `registry/src/economics.rs`:

```rust
//! Proving-rate → estimated NOCK/day. zkPoW is an attempt lottery: with
//! `difficulty` = expected proof attempts per block, a miner producing
//! `pps` proofs/sec expects `pps × 86400 / difficulty` blocks/day, each
//! paying `block_reward_nock` (eon-based emission; 10-min block target).
//! Operator-supplied via env: NOCKMARK_DIFFICULTY,
//! NOCKMARK_BLOCK_REWARD_NOCK; NOCKMARK_ECON_URL optionally refreshes
//! difficulty from a JSON endpoint with a top-level "difficulty" number.
//! Unset → the /economics endpoint reports itself unconfigured and the
//! leaderboard omits estimates. Estimates are estimates: hardware costs,
//! pool fees, and difficulty drift are out of scope.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct EconParams {
    /// Expected proof attempts per block at current network difficulty.
    pub difficulty: f64,
    /// Current eon's block reward in NOCK.
    pub block_reward_nock: f64,
}

pub fn nock_per_day(pps: f64, p: &EconParams) -> f64 {
    pps * 86_400.0 / p.difficulty * p.block_reward_nock
}

pub fn from_env() -> Option<EconParams> {
    let difficulty: f64 = std::env::var("NOCKMARK_DIFFICULTY").ok()?.parse().ok()?;
    let block_reward_nock: f64 =
        std::env::var("NOCKMARK_BLOCK_REWARD_NOCK").ok()?.parse().ok()?;
    (difficulty > 0.0 && block_reward_nock > 0.0)
        .then_some(EconParams { difficulty, block_reward_nock })
}

/// Poll `url` every 10 minutes for `{"difficulty": <number>, ...}` and
/// update the shared params. Only refreshes an already-configured cache
/// (the reward has no online source; it changes once per eon).
pub async fn refresh_loop(url: String, econ: Arc<RwLock<Option<EconParams>>>) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("reqwest client");
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        match fetch_difficulty(&client, &url).await {
            Ok(d) if d > 0.0 => {
                if let Some(p) = econ.write().await.as_mut() {
                    p.difficulty = d;
                }
            }
            Ok(d) => eprintln!("econ refresh: ignoring non-positive difficulty {d}"),
            Err(e) => eprintln!("econ refresh failed (keeping cached value): {e}"),
        }
    }
}

async fn fetch_difficulty(client: &reqwest::Client, url: &str) -> Result<f64, String> {
    let v: serde_json::Value = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("bad JSON from {url}: {e}"))?;
    v["difficulty"]
        .as_f64()
        .ok_or_else(|| format!("no numeric \"difficulty\" field at {url}"))
}
```

Add `pub mod economics;` to `registry/src/lib.rs`. Move `reqwest` in `registry/Cargo.toml` from `[dev-dependencies]` to `[dependencies]` (same line, keep it in dev too if both sections need it — they don't; a regular dependency is visible to tests).

Run: `cargo test --lib economics` — Expected: PASS.

- [ ] **Step 3: Write the failing http tests**

Append to `registry/tests/http.rs`:

```rust
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
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    st.kernel.lock().await
        .submit_run(nonce, "hw", "v", 2, 400).await.unwrap().unwrap();
    let res = app
        .oneshot(Request::get("/leaderboard").body(Body::empty()).unwrap())
        .await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v[0]["est_nock_per_day"].as_f64().unwrap() > 0.0);
}
```

Run: `cargo test --test http economics` — Expected: FAIL (no `econ` field / route).

- [ ] **Step 4: Wire it into `registry/src/http.rs` and `main.rs`**

`AppState` gains:

```rust
    pub econ: Arc<tokio::sync::RwLock<Option<crate::economics::EconParams>>>,
```

initialized in `boot_with_k` with `econ: Arc::new(tokio::sync::RwLock::new(crate::economics::from_env()))`.

`LeaderboardEntry` gains:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub est_nock_per_day: Option<f64>,
```

`to_entry` becomes `fn to_entry(run: RunRecord, econ: Option<crate::economics::EconParams>) -> LeaderboardEntry` and sets:

```rust
    let est_nock_per_day =
        econ.map(|p| round4(crate::economics::nock_per_day(proofs_per_sec, &p)));
```

`leaderboard` and `run_by_id` read the cache once (`let econ = *st.econ.read().await;`) and pass it to `to_entry` (`.map(|run| to_entry(run, econ))`).

New handler + route:

```rust
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
        "model": "est_nock_per_day = pps * 86400 / difficulty * block_reward_nock",
        "note": "difficulty = expected proof attempts per block; estimates only",
    });
    if let Some(pps) = q.get("pps").and_then(|s| s.parse::<f64>().ok()) {
        out["pps"] = json!(pps);
        out["est_nock_per_day"] = json!(crate::economics::nock_per_day(pps, &p));
    }
    (StatusCode::OK, Json(out))
}
```

Route: add `.route("/economics", get(economics))` next to `/leaderboard`.

`registry/src/main.rs`, after `AppState::boot_with_k(...)`:

```rust
    if let Ok(url) = std::env::var("NOCKMARK_ECON_URL") {
        tokio::spawn(nockmark_registry::economics::refresh_loop(
            url,
            state.econ.clone(),
        ));
    }
```

`registry/static/index.html`: add `<th>est. NOCK/day</th>` after the prover column, and append to the cell array `r.est_nock_per_day ?? '—'`; add below the footer paragraph:

```html
<p>NOCK/day figures are estimates (attempt-lottery model at current
difficulty and emission), shown only when the instance has economics
configured.</p>
```

- [ ] **Step 5: Run all registry tests**

Run: `cargo test` (from `registry/`)
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add registry/src/economics.rs registry/src/lib.rs registry/src/http.rs registry/src/main.rs registry/Cargo.toml registry/Cargo.lock registry/static/index.html registry/tests/http.rs
git commit -m "registry: /economics endpoint + NOCK/day estimates (env-configured, optional auto-refresh)"
```

---

### Task 8: Docs, deploy, reseed, announce collateral

**Files:**
- Modify: `docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md` (Known limitations + submit section)
- Modify: `tock/README.md` (submitting section)
- Create: `docs/announce/2026-07-m3-announcement.md`
- Modify (live): Railway deployment + reseed (manual steps below)

**Interfaces:**
- Consumes: everything above, deployed; `tock bench --submit` as the seeding tool.
- Produces: updated public docs, live M3 registry, announcement drafts for Tom.

- [ ] **Step 1: Rewrite the runbook's "Known limitations" section**

Replace the whole `## Known limitations (before public submissions / M3)` section in `docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md` with:

```markdown
## M3 status of the M2 known limitations

All three M2 carry-forwards are closed as of M3:

- **Timing is enforced and the ranked rate is server-side.** The kernel
  rejects submissions whose claimed `elapsed_ms` exceeds the
  server-observed window (`submitted_at − issued_at`), and the leaderboard
  ranks by `k / server_window` — client `elapsed_ms` is displayed as
  `self_reported_pps` only. Note the window includes proof verification
  (~0.5 s/proof), so published rates are slightly conservative lower
  bounds — by design.
- **Rate limiting is in-app.** `POST /challenge` and `POST /run` are
  limited to 10/min per IP (first `X-Forwarded-For` entry; Railway sets
  it). The Caddy `rate_limit` suggestion below remains optional
  belt-and-braces for VPS deploys.
- **Request bodies are explicitly capped at 4 MiB**, and `hardware` /
  `prover_version` strings at 128/64 bytes.

Remaining (acceptable for M3, revisit post-v1): the challenge map grows
monotonically (bounded to ≤14.4k mints/day by the rate limit; expired
entries are rejected but not purged), and hardware descriptors remain
self-reported by design.
```

Also update the seeding section header note: add one line at the top of `## Seeding the Registry`:

```markdown
> As of M3 the supported path is `tock bench --submit <registry-url>` —
> the curl flow below is the manual equivalent, kept for debugging.
```

- [ ] **Step 2: Add a "Submitting to the registry" section to `tock/README.md`**

Append:

```markdown
## Submitting to the public registry (M3)

One command — it fetches a challenge, proves against it, and submits:

    export PATH="$HOME/.rustup/toolchains/<your-nightly-2026-04-03>/bin:$PATH"
    export RUST_MIN_STACK=8388608
    ./target/release/tock bench \
      --kernel assets/miner.jam \
      --submit https://nockmark-registry-production.up.railway.app

The registry supplies the nonce, k (currently 8), and pow-len; your
`--seed`/`-k` flags are ignored in submit mode. `--threads N` is honored.
The published rate is computed **server-side** from the challenge-issue →
submission window, so it is a cryptographically verified lower bound on
your machine's proving rate: nothing you report can inflate it. Hardware
strings are self-reported and labelled as such.

Fully offline benching (`tock bench` without `--submit`) is unchanged.
```

- [ ] **Step 3: Write the announcement drafts**

Create `docs/announce/2026-07-m3-announcement.md`:

```markdown
# M3 announcement drafts (Tom posts these manually)

Pre-flight checklist (human):
- [ ] Read through docs/writeups/2026-07-15-first-public-nockchain-proving-benchmarks.md (pending since M1) and publish it (repo link is fine)
- [ ] Verify the leaderboard shows the reseeded M3 runs
- [ ] Set NOCKMARK_DIFFICULTY / NOCKMARK_BLOCK_REWARD_NOCK on Railway (current values from https://nockblocks.com) so /economics is live
- [ ] Post Discord + Telegram drafts below; ask a mod about pinning

## Discord (Nockchain server, #mining or #ecosystem)

**Nockmark is open: a proving benchmark registry that can't lie.**

"What hardware proves fastest?" now has a trustless answer. Nockmark is a
public registry of Nockchain STARK proving benchmarks where the rates are
cryptographically verified — not self-reported:

- Your machine proves k=8 real mining workloads against a server-issued
  challenge nonce (no precomputing).
- The registry verifies every proof and computes your rate from the
  server-observed clock — the published number is a lower bound nobody
  can inflate, including you.

One command to get on the board:
`tock bench --submit https://nockmark-registry-production.up.railway.app`
(setup: https://github.com/tomdebres/nockmark — bare machine to leaderboard
in ~15 min, ~3 min of proving)

Leaderboard: https://nockmark-registry-production.up.railway.app
Also: first public cross-hardware Nockchain proving benchmarks write-up
(M1/M2/EC2/Graviton numbers) in the repo. Feedback and PRs welcome —
especially runs from hardware we haven't seen.

## Telegram (mining groups — shorter)

Nockmark is live: verified Nockchain proving benchmarks. Your rate is
computed from a server-side challenge→submit window over STARK-verified
proofs, so the leaderboard can't be gamed by self-reporting. One command:
`tock bench --submit https://nockmark-registry-production.up.railway.app`
— repo: https://github.com/tomdebres/nockmark. Post your rig's rate.
```

- [ ] **Step 4: Commit the docs**

```bash
git add docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md tock/README.md docs/announce/2026-07-m3-announcement.md
git commit -m "docs: M3 limitations closed in runbook, submit instructions, announcement drafts"
```

- [ ] **Step 5: Probe the Block Explorer API for a difficulty source (best-effort)**

```bash
curl -s --max-time 10 https://nockblocks.com/api/stats | head -c 2000; echo
curl -s --max-time 10 https://nockblocks.com/api/v1/stats | head -c 2000; echo
curl -s --max-time 10 https://api.nockblocks.com/stats | head -c 2000; echo
```

If any returns JSON with a top-level numeric `difficulty`, note the URL for `NOCKMARK_ECON_URL` in the deploy step. If not (likely — the official API is gRPC-first), skip `NOCKMARK_ECON_URL`; env-set difficulty rules. Record the outcome in the progress ledger — do not block on this.

- [ ] **Step 6: Deploy to Railway and wipe the M2 seed**

The M2 seed run was submitted manually over a multi-minute curl window — under server-window ranking its rate is garbage. Wipe and reseed:

```bash
railway status                 # confirm project nockmark-registry is linked
railway up --detach            # rebuilds the Docker image with M3 code + new registry.jam
# wait for healthcheck (GET /leaderboard) to go green, then wipe old state:
railway ssh "rm -rf /data/*"
railway service restart 2>/dev/null || railway redeploy   # whichever the CLI version offers
curl -s https://nockmark-registry-production.up.railway.app/leaderboard   # expect []
# economics env (values read off nockblocks.com at deploy time — human fills):
railway variables --set "NOCKMARK_DIFFICULTY=<current>" --set "NOCKMARK_BLOCK_REWARD_NOCK=<current eon reward>"
```

If `railway ssh` is unavailable on this plan, detach + delete + re-add the volume in the Railway dashboard instead (same effect: empty `/data`).

- [ ] **Step 7: Reseed by dogfooding the real flow (M3 success criterion)**

From this Mac (the M1-Max bench machine):

```bash
cd /Users/openclaw/Obsidian/vault/tomdebres/projects/nockmark/m3-trustless-submissions/tock
export PATH="$HOME/.rustup/toolchains/nightly-2026-04-03-aarch64-apple-darwin/bin:$PATH"
export RUST_MIN_STACK=8388608
./target/release/tock bench --kernel assets/miner.jam \
  --submit https://nockmark-registry-production.up.railway.app
```

Expected: ~8 × 21 s of proving, then `submitted: run 0` and the run at
https://nockmark-registry-production.up.railway.app/runs/0 with a
`proofs_per_sec` slightly below the locally printed rate (server window
includes upload + verification). THAT bench output is the acceptance
evidence — paste it into the progress ledger.

- [ ] **Step 8: Final verification + hand off**

```bash
curl -s https://nockmark-registry-production.up.railway.app/leaderboard | jq .
curl -s "https://nockmark-registry-production.up.railway.app/economics?pps=0.05" | jq .
```

Expected: one seeded run with `server_window_ms`; economics JSON (or a
clean 503 if Tom hasn't set the env values yet — acceptable, listed on his
checklist). Then use superpowers:finishing-a-development-branch — merge/PR
`tomdebres/m3-trustless-submissions` → `main`, push, and leave the
announcement posting + write-up read-through to Tom per the checklist in
`docs/announce/2026-07-m3-announcement.md`.
```

---

## Self-review notes (kept for the executor)

- **Spec coverage:** kickoff items 1→Task 5/6, 2→Tasks 1/2, 3→Task 3, 4→Task 4, 5→Task 8, 6→Task 7. M3 success criterion (external miner, zero hand-holding) → Task 8 steps 2/7.
- **Cross-task type consistency:** `da_diff_to_ms(u128, u128) -> u64` (Tasks 2,6); `boot_with_k(&Path, &Path, u64)` (Tasks 4,6); rejection cord `'elapsed-exceeds-window'` (Tasks 1,6); JSON fields `server_window_ms`/`proofs_per_sec`/`self_reported_pps`/`est_nock_per_day` (Tasks 2,6,7,8).
- **Known accepted biases:** server window includes verification + upload (conservative); economics values are operator-supplied; challenge map growth bounded by rate limit only.
- **Test timing:** all sleep-based tests keep ≥3× margin between slept window and claimed elapsed; kernel/verifier boots stay serialized behind `test_lock()`.
