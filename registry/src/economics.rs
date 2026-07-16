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
