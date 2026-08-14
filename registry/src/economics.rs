//! Proving-rate → estimated NOCK/day. zkPoW is an attempt lottery: with
//! `difficulty` = expected proof attempts per block, a miner producing
//! `pps` proofs/sec expects `pps × 86400 / difficulty` blocks/day, each
//! paying `block_reward_nock` (eon-based emission; 150 s block target).
//! Operator-supplied via env: NOCKMARK_DIFFICULTY,
//! NOCKMARK_BLOCK_REWARD_NOCK; NOCKMARK_ECON_URL optionally refreshes
//! difficulty. Two source shapes:
//!   - plain GET returning JSON with a top-level "difficulty" number
//!     (key matched case-insensitively);
//!   - with NOCKMARK_ECON_API_KEY set, a NockBlocks-style JSON-RPC
//!     endpoint: POST getTip with an x-api-key header, difficulty
//!     derived from the tip target (see below).
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

/// Largest tip5 digest as an atom: five base-p digits of p−1, i.e. p⁵−1,
/// where p = 2⁶⁴ − 2³² + 1 is the Goldilocks prime. This is nockchain's
/// max-target-atom (hoon/common/tx-engine-0.hoon); a block's work — the
/// expected proof attempts it took — is max-target/(target+1), Bitcoin's
/// GetBlockProof shape. f64 precision (~1e-16 relative) is far below the
/// noise floor of an earnings estimate.
const MAX_TIP5_ATOM: f64 = {
    let p = 18_446_744_069_414_584_321u128 as f64; // 2^64 − 2^32 + 1
    p * p * p * p * p
};

/// Ideal ZK block interval, overridable via NOCKMARK_BLOCK_TIME_SECS.
/// 150 s under aletheia; the Logos fork (height 126,000) re-anchors ZK
/// ASERT to 214 s, so this must be operator-adjustable without a
/// rebuild. Only used for the derived network-rate estimate, never for
/// ranking.
pub fn block_time_secs() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("NOCKMARK_BLOCK_TIME_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(150.0)
    })
}

pub fn nock_per_day(pps: f64, p: &EconParams) -> f64 {
    pps * 86_400.0 / p.difficulty * p.block_reward_nock
}

/// Estimated whole-network proving rate: difficulty is expected attempts
/// per block, and ZK blocks arrive every block_time_secs() on average.
pub fn network_pps(p: &EconParams) -> f64 {
    p.difficulty / block_time_secs()
}

pub fn from_env() -> Option<EconParams> {
    let difficulty: f64 = std::env::var("NOCKMARK_DIFFICULTY").ok()?.parse().ok()?;
    let block_reward_nock: f64 =
        std::env::var("NOCKMARK_BLOCK_REWARD_NOCK").ok()?.parse().ok()?;
    (difficulty > 0.0 && block_reward_nock > 0.0)
        .then_some(EconParams { difficulty, block_reward_nock })
}

/// Poll `url` every 10 minutes and update the shared params. Only
/// refreshes an already-configured cache (the reward has no online
/// source; it changes once per eon). Each successful observation is
/// appended to `history` (JSONL on the persistent volume) so the board
/// can show difficulty over time.
pub async fn refresh_loop(
    url: String,
    api_key: Option<String>,
    history: Option<std::path::PathBuf>,
    econ: Arc<RwLock<Option<EconParams>>>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("reqwest client");
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        let fetched = match &api_key {
            Some(key) => fetch_difficulty_rpc(&client, &url, key).await,
            None => fetch_difficulty_get(&client, &url).await,
        };
        match fetched {
            Ok(d) if d > 0.0 => {
                if let Some(p) = econ.write().await.as_mut() {
                    if p.difficulty != d {
                        eprintln!("econ refresh: difficulty {} -> {d}", p.difficulty);
                    }
                    p.difficulty = d;
                }
                if let Some(path) = &history {
                    if let Err(e) = append_history(path, unix_now(), d) {
                        eprintln!("econ refresh: history append failed: {e}");
                    }
                }
            }
            Ok(d) => eprintln!("econ refresh: ignoring non-positive difficulty {d}"),
            Err(e) => eprintln!("econ refresh failed (keeping cached value): {e}"),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One difficulty observation, `{"t":unix_secs,"difficulty":n}` per line.
/// At one line per 10-minute poll this grows ~7 KB/day — no rotation
/// needed on any horizon that matters.
fn append_history(path: &std::path::Path, t: u64, difficulty: f64) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::json!({ "t": t, "difficulty": difficulty }))
}

/// Read observations newer than `since` (unix secs), oldest first,
/// thinned to at most `cap` evenly-spaced points. Unparseable lines
/// (torn writes) are skipped.
pub fn read_history(
    path: &std::path::Path,
    since: u64,
    cap: usize,
) -> std::io::Result<Vec<(u64, f64)>> {
    let raw = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        r => r?,
    };
    let pts: Vec<(u64, f64)> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| Some((v["t"].as_u64()?, v["difficulty"].as_f64()?)))
        .filter(|(t, _)| *t >= since)
        .collect();
    if pts.len() <= cap || cap == 0 {
        return Ok(pts);
    }
    // Evenly-spaced thinning that always keeps the newest point.
    let last = *pts.last().unwrap();
    let step = pts.len() as f64 / (cap - 1) as f64;
    let mut out: Vec<(u64, f64)> =
        (0..cap - 1).map(|i| pts[(i as f64 * step) as usize]).collect();
    out.push(last);
    Ok(out)
}

/// Plain-JSON source: GET `url`, read a top-level "difficulty" number
/// (case-insensitive key — nock.dwd.com spells it "Difficulty").
async fn fetch_difficulty_get(client: &reqwest::Client, url: &str) -> Result<f64, String> {
    let v: serde_json::Value = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("bad JSON from {url}: {e}"))?;
    difficulty_field(&v).ok_or_else(|| format!("no numeric \"difficulty\" field at {url}"))
}

fn difficulty_field(v: &serde_json::Value) -> Option<f64> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("difficulty"))?
        .1
        .as_f64()
}

/// NockBlocks-style source: POST a getTip JSON-RPC call with the API key
/// and derive difficulty from the tip's target.
async fn fetch_difficulty_rpc(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<f64, String> {
    let v: serde_json::Value = client
        .post(url)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "method": "getTip", "params": [], "id": "nockmark-econ",
        }))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("bad JSON from {url}: {e}"))?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(format!("RPC error from {url}: {err}"));
    }
    // Post-Logos (dual-pow) the tip can be an AI-PoW block, whose target
    // is on a different scale entirely — deriving ZK difficulty from it
    // would poison the estimates. Only accept ZK tips; a skipped tick
    // keeps the cached value (tips alternate, so the next ZK tip is
    // minutes away). Pre-Logos responses have no powType field: accept.
    match v["result"]["powType"].as_str() {
        None | Some("zk-pow") => {}
        Some(other) => {
            return Err(format!(
                "tip is a {other} block — keeping cached ZK difficulty"
            ))
        }
    }
    let target = v["result"]["target"]
        .as_str()
        .ok_or_else(|| format!("no string \"result.target\" field at {url}"))?;
    difficulty_from_target(target)
}

fn difficulty_from_target(target: &str) -> Result<f64, String> {
    let t: f64 = target
        .parse()
        .map_err(|e| format!("unparseable target {target:?}: {e}"))?;
    if !(t > 0.0 && t < MAX_TIP5_ATOM) {
        return Err(format!("target {target} outside (0, p^5)"));
    }
    Ok(MAX_TIP5_ATOM / (t + 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_network_pps_earns_the_whole_emission() {
        // 150 s blocks → 576/day. A miner who IS the entire network
        // (pps × 150 s = difficulty) earns exactly 576 × reward per day.
        let p = EconParams { difficulty: 6_000.0, block_reward_nock: 2_048.0 };
        let network_pps = p.difficulty / 150.0;
        let est = nock_per_day(network_pps, &p);
        assert!((est - 576.0 * p.block_reward_nock).abs() < 1e-6);
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

    #[test]
    fn difficulty_field_is_case_insensitive() {
        let lower: serde_json::Value = serde_json::json!({"difficulty": 12.5});
        let upper: serde_json::Value = serde_json::json!({"Difficulty": 1073741824.0, "x": 1});
        assert_eq!(difficulty_field(&lower), Some(12.5));
        assert_eq!(difficulty_field(&upper), Some(1_073_741_824.0));
        assert_eq!(difficulty_field(&serde_json::json!({"diff": 1})), None);
        assert_eq!(difficulty_field(&serde_json::json!({"difficulty": "n/a"})), None);
    }

    #[test]
    fn difficulty_from_real_mainnet_target() {
        // Block 124570 (2026-08-11): nockblocks getTip returned this target,
        // and the chain's own accumulatedWork delta over the parent block —
        // i.e. this block's work, computed by consensus in exact bignum
        // arithmetic — was 2_491_784_163.
        let target = "857211898323310214279691199033669776696186506919492829732100799763647239070693679512270";
        let d = difficulty_from_target(target).unwrap();
        assert!((d - 2_491_784_163.0).abs() / 2_491_784_163.0 < 1e-9, "got {d}");
    }

    #[test]
    fn network_pps_is_difficulty_per_block_time() {
        // block_time_secs() is 150 unless NOCKMARK_BLOCK_TIME_SECS is set,
        // which the test env never does.
        let p = EconParams { difficulty: 1_500.0, block_reward_nock: 2_048.0 };
        assert!((network_pps(&p) - 10.0).abs() < 1e-12);
    }

    #[test]
    fn history_roundtrip_filters_and_thins() {
        let path = std::env::temp_dir()
            .join(format!("nockmark-econ-history-test-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_history(&path, 0, 100).unwrap(), vec![], "missing file is empty");
        for i in 0..10u64 {
            append_history(&path, 1000 + i, i as f64).unwrap();
        }
        // Torn/garbage line is skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"t\": 99\n"))
            .unwrap();
        let all = read_history(&path, 0, 100).unwrap();
        assert_eq!(all.len(), 10);
        assert_eq!(all[0], (1000, 0.0));
        let since = read_history(&path, 1005, 100).unwrap();
        assert_eq!(since.len(), 5, "since-filter keeps t >= since");
        let thinned = read_history(&path, 0, 4).unwrap();
        assert_eq!(thinned.len(), 4);
        assert_eq!(thinned[0], (1000, 0.0), "thinning keeps the oldest point");
        assert_eq!(*thinned.last().unwrap(), (1009, 9.0), "and the newest");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn difficulty_from_target_rejects_garbage() {
        assert!(difficulty_from_target("not-a-number").is_err());
        assert!(difficulty_from_target("0").is_err());
        assert!(difficulty_from_target("-5").is_err());
        // A target at/above p^5 would mean difficulty < 1 — malformed input.
        assert!(difficulty_from_target(&format!("{:.0}", MAX_TIP5_ATOM * 2.0)).is_err());
    }
}
