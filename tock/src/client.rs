//! Registry client for `tock bench --submit`: fetch a challenge, submit the
//! proof bundle. Mirrors the manual seeding flow in
//! docs/superpowers/specs/2026-07-15-m2-deploy-runbook.md.

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
    let text = resp
        .text()
        .await
        .map_err(|e| format!("POST {url}: reading response body: {e}"))?;
    if !status.is_success() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(msg) = v["error"].as_str() {
                return Err(msg.to_string());
            }
        }
        let snippet: String = text.chars().take(200).collect();
        return Err(format!("POST {url}: HTTP {status}: {snippet}"));
    }
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("POST {url}: bad response JSON: {e}"))?;
    body["run_id"]
        .as_u64()
        .ok_or_else(|| format!("POST {url}: response missing run_id"))
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
