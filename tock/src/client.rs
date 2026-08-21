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

// ---------------------------------------------------------------------------
// AI track (M5; M6 Phase B1 adds the statement discriminator): fetch an AI
// challenge, submit verified wins
// ---------------------------------------------------------------------------

/// Which AI statement a challenge/submission is for. Both statements live in
/// the one AI track and rank together — MAC-equivalents per second is the unit
/// consensus itself uses to compare heterogeneous AI work — so this selects the
/// workload and its verify rules, not a separate board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiStatement {
    /// The M5 single-tile dense statement ([`crate::aipow`]).
    Dense,
    /// The canonical MoE block mainnet miners run ([`crate::aipow_moe`]).
    CanonicalMoe,
}

impl AiStatement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dense => crate::aipow_moe::STATEMENT_DENSE,
            Self::CanonicalMoe => crate::aipow_moe::STATEMENT_CANONICAL_MOE,
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            crate::aipow_moe::STATEMENT_DENSE => Ok(Self::Dense),
            crate::aipow_moe::STATEMENT_CANONICAL_MOE => Ok(Self::CanonicalMoe),
            other => Err(format!(
                "unknown AI statement {other:?} (expected \"dense\" or \"canonical-moe\")"
            )),
        }
    }

    /// The grind rule this statement's client must speak — checked against the
    /// registry's advertised rule so a version skew fails at the challenge, not
    /// after minutes of grinding.
    pub fn nonce_rule(self) -> &'static str {
        match self {
            Self::Dense => crate::aipow::AI_NONCE_RULE,
            Self::CanonicalMoe => crate::aipow_moe::AI_MOE_NONCE_RULE,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiRegistryChallenge {
    pub nonce: String,
    /// hex32; plays the block_commitment role AND seeds the matrices.
    pub challenge: String,
    /// hex32 jackpot target T_b.
    pub target: String,
    /// Wins required.
    pub k: u64,
    pub nonce_rule: String,
    /// Which statement this challenge is for. Absent on pre-M6 registries,
    /// which only ever issued dense challenges.
    #[serde(default)]
    pub statement: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AiWinSubmission {
    /// Dense: the u64 extranonce. Canonical MoE: the u32 grind ordinal, widened
    /// (the wire field is shared; the registry range-checks it per statement).
    pub extranonce: u64,
    /// base64 postcard of [`crate::aipow::AiCertBlob`] (dense) or
    /// [`crate::aipow_moe::AiMoeCertBlob`] (canonical MoE).
    pub cert_b64: String,
}

#[derive(Debug, Serialize)]
pub struct AiSubmission {
    pub nonce: String,
    pub hardware: String,
    pub prover_version: String,
    pub grind_elapsed_ms: u64,
    pub wins: Vec<AiWinSubmission>,
    /// Omitted for the dense statement, so a dense submission is byte-identical
    /// to the M5 wire format (the registry defaults an absent field to dense,
    /// which is also what makes stored M5 rows replay correctly).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement: Option<&'static str>,
}

pub async fn fetch_ai_challenge(
    base: &str,
    statement: AiStatement,
) -> Result<AiRegistryChallenge, String> {
    // Dense omits the parameter entirely: the M5 request byte-for-byte.
    let url = match statement {
        AiStatement::Dense => format!("{}/challenge?track=ai", base.trim_end_matches('/')),
        other => format!(
            "{}/challenge?track=ai&statement={}",
            base.trim_end_matches('/'),
            other.as_str()
        ),
    };
    let resp = http(Duration::from_secs(30))
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("POST {url}: HTTP {}", resp.status()));
    }
    let ch = resp
        .json::<AiRegistryChallenge>()
        .await
        .map_err(|e| format!("bad AI challenge JSON from {url}: {e}"))?;
    if ch.nonce_rule != statement.nonce_rule() {
        return Err(format!(
            "registry nonce rule {:?} != client {:?} for statement {:?} — update tock",
            ch.nonce_rule,
            statement.nonce_rule(),
            statement.as_str()
        ));
    }
    // A registry that answered with a different statement than we asked for is
    // serving a workload we did not calibrate against; fail before grinding.
    if let Some(got) = &ch.statement {
        if got != statement.as_str() {
            return Err(format!(
                "registry issued statement {got:?}, asked for {:?}",
                statement.as_str()
            ));
        }
    } else if statement != AiStatement::Dense {
        return Err(format!(
            "registry does not advertise a statement; it predates {:?}",
            statement.as_str()
        ));
    }
    Ok(ch)
}

/// Returns the recorded AI run id. First-submission latency can include the
/// registry's one-time verifier-context build (~25 s), hence the generous
/// timeout on top of k × ~80 ms verifies.
pub async fn submit_ai_run(base: &str, sub: &AiSubmission) -> Result<u64, String> {
    let url = format!("{}/run?track=ai", base.trim_end_matches('/'));
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
            prover_version: crate::miner::NOCKCHAIN_PIN.into(),
            elapsed_ms: 42,
            proofs: vec!["AA==".into()],
        };
        let v: serde_json::Value = serde_json::to_value(&sub).unwrap();
        assert_eq!(v["nonce"], "12345");
        assert_eq!(v["elapsed_ms"], 42);
        assert_eq!(v["proofs"][0], "AA==");
    }

    /// The statement crosses the wire as a string, and the DENSE submission
    /// must stay byte-identical to M5's: no `statement` key at all. (The
    /// registry defaults an absent field to dense, so adding one would be
    /// harmless — but "we did not touch the existing wire format" is a
    /// property worth pinning rather than asserting in a comment.)
    #[test]
    fn ai_statement_wire_values_and_dense_body_is_unchanged() {
        assert_eq!(AiStatement::Dense.as_str(), "dense");
        assert_eq!(AiStatement::CanonicalMoe.as_str(), "canonical-moe");
        for st in [AiStatement::Dense, AiStatement::CanonicalMoe] {
            assert_eq!(AiStatement::parse(st.as_str()).unwrap(), st);
        }
        assert!(AiStatement::parse("sparse").is_err());
        assert_ne!(
            AiStatement::Dense.nonce_rule(),
            AiStatement::CanonicalMoe.nonce_rule()
        );
        assert_eq!(
            AiStatement::CanonicalMoe.nonce_rule(),
            "canonical-ordinal-v3"
        );

        let win = AiWinSubmission {
            extranonce: 7,
            cert_b64: "AA==".into(),
        };
        let dense = AiSubmission {
            nonce: "12345".into(),
            hardware: "hw".into(),
            prover_version: crate::miner::NOCKCHAIN_PIN.into(),
            grind_elapsed_ms: 42,
            wins: vec![win],
            statement: None,
        };
        let v: serde_json::Value = serde_json::to_value(&dense).unwrap();
        assert!(
            v.get("statement").is_none(),
            "a dense submission must carry no statement field"
        );
        assert_eq!(v["wins"][0]["extranonce"], 7);

        let moe = AiSubmission {
            statement: Some(AiStatement::CanonicalMoe.as_str()),
            ..dense
        };
        assert_eq!(
            serde_json::to_value(&moe).unwrap()["statement"],
            "canonical-moe"
        );
    }

    /// A registry challenge parses with or without the statement field: absent
    /// means a pre-M6 registry, which only ever issued dense challenges.
    #[test]
    fn ai_challenge_statement_is_optional() {
        let legacy: AiRegistryChallenge = serde_json::from_str(
            r#"{"nonce":"1","challenge":"00","target":"ff","k":4,
                "nonce_rule":"extranonce-le8-v1"}"#,
        )
        .unwrap();
        assert_eq!(legacy.statement, None);
        let moe: AiRegistryChallenge = serde_json::from_str(
            r#"{"nonce":"1","challenge":"00","target":"ff","k":4,
                "nonce_rule":"canonical-ordinal-v3","statement":"canonical-moe"}"#,
        )
        .unwrap();
        assert_eq!(moe.statement.as_deref(), Some("canonical-moe"));
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
