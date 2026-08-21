//! M5 AI-PoW track, server side (Task 3): challenge derivation, the AI run
//! store, certificate verification (lifted from the Task-1 spike's
//! `verify_submission`), and MAC-equivalent economics.
//!
//! ## Why the AI track lives in a server-side store, not the kernel
//!
//! The registry kernel (`hoon/registry.hoon`) is the ZK track's source of
//! truth and M5 deliberately ships NO hoon changes and no kernel jam
//! rebuild (design-doc scope). The AI track does not need kernel state for
//! its trust story: every ranked number is computed from certificates THIS
//! server verified, of a challenge THIS server derived from a
//! kernel-minted nonce, over a window THIS server observed with its own
//! clock. What must be remembered — (nonce, issued-at, wins, timestamps,
//! hardware, prover version) — goes into an append-only JSONL file beside
//! `econ-history.jsonl` on the persistent volume, replayed at boot. The
//! kernel's mint machinery is still reused untouched: it provides the
//! unique, entropy-backed 64-bit nonce the 32-byte AI challenge is derived
//! from.
//!
//! ## One track, two statements (M6 Phase B1)
//!
//! The AI track carries TWO statements — the M5 `dense` single-tile benchmark
//! and the `canonical-moe` block mainnet AI miners actually run — on ONE
//! leaderboard. They are not two boards because MAC-equivalents per second is
//! exactly the unit consensus uses to compare heterogeneous AI work: by
//! `ai_pow::difficulty`'s invariant D2, expected MAC-equivalents per block is
//! `2^256 / T` *whatever tile shape the miner picked*, which is what makes
//! `+compute-work-ai` a meaningful fork-choice weight. A dense row and a
//! canonical-MoE row are therefore directly comparable, and ranking them apart
//! would be inventing a distinction consensus does not make.
//!
//! What the two do NOT share is their arithmetic. The dense path compares the
//! jackpot against `T` raw; the canonical path compares it against `T · F`.
//! [`Statement`] is the discriminator that keeps every per-statement rule —
//! target, expected attempts, verifier, grind rule, blob format — on the right
//! side of that line, and it is persisted on every run so a re-calibration or a
//! new statement cannot rewrite how an old row was scored.
//!
//! One consequence, documented rather than closed: an AI-minted nonce is
//! also a valid pending ZK challenge in the kernel, so a client could
//! submit BOTH an AI run (here) and a ZK run (kernel) against one mint.
//! That is not a cheat vector — each board's number still reflects full,
//! independently verified work inside the same server-observed window.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use ai_pow::commit::matrix_commitment;
use ai_pow::fiat_shamir::{
    block_state, canonical_noise_seeds_from_matrix_commitments, commitment_key,
};
use ai_pow::prover::{params_tag, BlockContext};
use ai_pow::synth::synth_matrices;
use ai_pow::zk_bridge::{
    expected_layer0_rows_for_strip_schedule, prove_ai_pow_compact_recursive_certificate,
    verify_ai_pow_full_matmul_production_statement, zk_params_from_matmul, ZkPublicCommitments,
};
use ai_pow_zk::canonical::{canonical_program_for_strip_schedule, BlockPublic, StripIndexSchedule};
use ai_pow_zk::recursion::{
    canonical_l0_program_commitment_vals, compact_batch_verifier_key_digest_to_bytes,
    decode_compact_batch_recursive_certificate,
    verify_compact_batch_recursive_certificate_with_context, AiPowCompactBatchVerifierContext,
    AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES,
};
use ai_pow_zk::CircuitConfig;
use serde::{Deserialize, Serialize};
use tock::aipow::{nonce_extranonce, AiCertBlob, AI_PARAMS};

// ---------------------------------------------------------------------------
// Statement discriminator
// ---------------------------------------------------------------------------

/// Which AI-PoW statement a challenge, submission or stored run is for.
///
/// Serializes as the wire strings `"dense"` / `"canonical-moe"`, and DEFAULTS
/// to `Dense` on deserialize: every row written before M6 Phase B1 predates the
/// field and was a dense run, so replaying `aipow-track.jsonl` reproduces them
/// correctly without a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Statement {
    #[default]
    #[serde(rename = "dense")]
    Dense,
    #[serde(rename = "canonical-moe")]
    CanonicalMoe,
}

impl Statement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dense => tock::aipow_moe::STATEMENT_DENSE,
            Self::CanonicalMoe => tock::aipow_moe::STATEMENT_CANONICAL_MOE,
        }
    }

    /// Parse a wire value. Unknown statements are rejected rather than silently
    /// treated as dense — a client asking for a statement this registry does
    /// not implement must be told so, not scored under different rules.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            x if x == tock::aipow_moe::STATEMENT_DENSE => Ok(Self::Dense),
            x if x == tock::aipow_moe::STATEMENT_CANONICAL_MOE => Ok(Self::CanonicalMoe),
            other => Err(format!(
                "unknown statement {other:?} (expected \"dense\" or \"canonical-moe\")"
            )),
        }
    }

    /// The grind rule advertised on this statement's challenge.
    pub fn nonce_rule(self) -> &'static str {
        match self {
            Self::Dense => tock::aipow::AI_NONCE_RULE,
            Self::CanonicalMoe => tock::aipow_moe::AI_MOE_NONCE_RULE,
        }
    }

    /// MAC-equivalents one grind attempt costs for this statement's shape.
    /// Both shapes open an 8×8 tile over `k = 1024`, so both are `2^16` — but
    /// they are derived independently (see
    /// [`tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT`]) and a re-pin could move
    /// one without the other.
    pub fn mac_equiv_per_attempt(self) -> f64 {
        match self {
            Self::Dense => tock::aipow::MAC_EQUIV_PER_ATTEMPT,
            Self::CanonicalMoe => tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT,
        }
    }

    /// Expected grind attempts per win at this statement's target semantics:
    /// `2^256/(T+1)` for dense (raw comparison), `2^256/(T·F+1)` for canonical
    /// MoE (scaled comparison). Conflating the two is a factor-`F` error in
    /// every rate the board prints.
    pub fn expected_attempts_per_win(self, target: &[u8; 32]) -> f64 {
        match self {
            Self::Dense => expected_attempts_per_win(target),
            // A target the canonical shape cannot scale is impossible on a
            // stored run (the submission that wrote it was verified against
            // it), so the unreachable branch degrades to "1 attempt", which can
            // only UNDERSTATE a rate — never inflate one.
            Self::CanonicalMoe => {
                tock::aipow_moe::expected_attempts_per_moe_win(target).unwrap_or(1.0)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Challenge derivation & track configuration
// ---------------------------------------------------------------------------

/// Domain separator of the nonce→challenge derivation. Versioned: changing
/// the derivation (or [`tock::aipow::AI_NONCE_RULE`]) bumps the suffix.
pub const AI_CHALLENGE_DOMAIN: &[u8] = b"nockmark-ai-v1";

/// AI challenges expire like ZK ones: the kernel's `~h1` stale-nonce rule
/// (`hoon/registry.hoon`), mirrored here in server-clock milliseconds.
pub const AI_WINDOW_MS: u64 = 3_600_000;

/// Wins required per AI submission (env `NOCKMARK_AI_K` overrides).
/// 4 ≈ 100 s of certificate proving on an M1 Max (~24 s each, outside the
/// grind window) — same "minutes of client work" envelope as the ZK k=8.
pub const AI_K_DEFAULT: u64 = 4;

/// Consensus exchange rate: MAC-equivalents per ZK proof attempt
/// (`tx-engine.hoon`, Logos).
pub const MAC_EQUIV_PER_ZK_ATTEMPT: f64 = 25.75e9;

/// Submission blob cap: the certificate's 150 KB consensus cap plus
/// headroom for the statement metadata (nonce + PIs + trace height,
/// well under 4 KB). Enforced before any decode work.
pub const AI_SUBMISSION_BLOB_MAX: usize = 154 * 1024;

/// The 32-byte AI challenge, DERIVED from the kernel-minted u64 nonce:
/// `blake3("nockmark-ai-v1" || nonce_le8)`. Uses the `blake3` crate
/// directly (already in the dep tree via ai-pow, same 1.x line) rather
/// than ai-pow's keyed helpers, so the derivation is the plain, documented
/// hash. The challenge plays the `block_commitment` role AND the matrix
/// seed; the registry recomputes it from the stored nonce at verify time,
/// so nothing challenge-shaped needs to be persisted or trusted.
pub fn challenge32(nonce: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(AI_CHALLENGE_DOMAIN);
    h.update(&nonce.to_le_bytes());
    *h.finalize().as_bytes()
}

/// Default benchmark jackpot target `T_b`.
///
/// Semantics (matched to upstream `hash_le_target`, the comparison BOTH
/// `mine_with_context_at_target` and the verify path use): the 32 bytes
/// are a **little-endian** 256-bit integer, and a win is `jackpot ≤ T_b`
/// with NO shape-factor scaling — our `T_b` is the effective threshold
/// directly (unlike consensus targets, which are scaled by `Θ = T·F` in
/// `attempt_wins`). Getting this wrong is not hypothetical: the first
/// deploy shipped a big-endian-intended 2^243 that reads as 2^11 in LE —
/// expected attempts 2^229 per win, a grind that never ends.
///
/// Default: 2^248 (`bytes[31] = 1`), an expected 2^256/(T+1) = 256
/// attempts per win — generous, for tests and dev loops. Production
/// overrides via `NOCKMARK_AI_TARGET` (64 hex chars, LE); the deployed
/// value is 2^243 (`…0800` — `bytes[30] = 8`) ≈ 8191 attempts per win.
pub fn default_target() -> [u8; 32] {
    let mut t = [0u8; 32];
    t[31] = 0x01;
    t
}

/// The instance's jackpot target: `NOCKMARK_AI_TARGET` if set and valid
/// hex32, else [`default_target`].
pub fn target_from_env() -> [u8; 32] {
    std::env::var("NOCKMARK_AI_TARGET")
        .ok()
        .and_then(|s| tock::aipow::parse_hex32(&s).ok())
        .unwrap_or_else(default_target)
}

/// The instance's wins-per-submission: `NOCKMARK_AI_K` if set and ≥ 1,
/// else [`AI_K_DEFAULT`].
pub fn k_from_env() -> u64 {
    std::env::var("NOCKMARK_AI_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|k| *k >= 1)
        .unwrap_or(AI_K_DEFAULT)
}

/// The grind rule's server-side check: winning extranonces must be
/// strictly ascending u64s (clients grind 0, 1, 2, … — see
/// `tock::aipow::grind`), so duplicates and reorderings are malformed.
pub fn extranonces_strictly_ascending(xs: &[u64]) -> bool {
    xs.windows(2).all(|w| w[0] < w[1])
}

/// Server clock as unix milliseconds. The AI window is server-observed
/// (issue → submit) on this clock; only differences are meaningful.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Economics: attempts → MAC-equivalents → rates
// ---------------------------------------------------------------------------

/// Expected grind attempts per jackpot win at **little-endian** target
/// `T`: `2^256 / (T+1)` (the jackpot hash is uniform over 2^256 and a win
/// is `hash ≤ T`, compared LE — see [`default_target`]). f64 precision
/// (~1e-16 relative) is far below the ±1σ Poisson noise floor of a k-win
/// sample.
pub fn expected_attempts_per_win(target: &[u8; 32]) -> f64 {
    let mut t = 0.0f64;
    for &b in target.iter().rev() {
        t = t * 256.0 + b as f64;
    }
    2f64.powi(256) / (t + 1.0)
}

/// Round to 4 significant digits — rate magnitudes here span ~1e-6
/// (zk-attempt equivalents) to ~1e9 (MAC/s), so fixed decimal places
/// would be either lossy or noisy.
fn signif4(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let scale = 10f64.powf(3.0 - x.abs().log10().floor());
    (x * scale).round() / scale
}

/// The computed board figures for one AI run.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AiRates {
    /// k · attempts-per-win · F over the SERVER window (issue → submit,
    /// certificate proving included) — a hard lower bound; the ranked
    /// number, and the figure both statements are ranked by. F = 2^16
    /// MAC-equivalents per attempt for both shapes; attempts-per-win is
    /// statement-specific (see [`Statement::expected_attempts_per_win`]).
    pub verified_mac_per_sec_lb: f64,
    /// Same numerator over the client-reported grind window (proving
    /// excluded) — display-only, like the ZK track's self_reported_pps.
    pub grind_mac_per_sec: f64,
    /// verified_mac_per_sec_lb ÷ 25.75e9 (consensus MAC↔ZK-attempt
    /// exchange rate).
    pub zk_attempt_equiv_per_sec: f64,
    /// ±1σ as a fraction of the rate: k wins is Poisson, so 1/√k.
    pub sigma_frac: f64,
}

/// Rates for a run of `k` wins of `statement` at `target`, over the
/// server-observed and client-reported windows (both in ms, both ≥ 1 by
/// construction).
///
/// `statement` enters ONLY through the MAC-equivalent numerator — the windows,
/// the exchange rate and the Poisson error bar are statement-independent. That
/// is the whole reason both statements can share a board.
pub fn rates(
    statement: Statement,
    k: u64,
    target: &[u8; 32],
    server_window_ms: u64,
    grind_elapsed_ms: u64,
) -> AiRates {
    let mac_total = k as f64
        * statement.expected_attempts_per_win(target)
        * statement.mac_equiv_per_attempt();
    let verified = mac_total / (server_window_ms.max(1) as f64 / 1000.0);
    let grind = mac_total / (grind_elapsed_ms.max(1) as f64 / 1000.0);
    AiRates {
        verified_mac_per_sec_lb: signif4(verified),
        grind_mac_per_sec: signif4(grind),
        zk_attempt_equiv_per_sec: signif4(verified / MAC_EQUIV_PER_ZK_ATTEMPT),
        sigma_frac: signif4(1.0 / (k as f64).sqrt()),
    }
}

// ---------------------------------------------------------------------------
// AI store: append-only JSONL, replayed at boot (like econ-history.jsonl)
// ---------------------------------------------------------------------------

/// One verified AI run, exactly as persisted (and served, with computed
/// rates alongside).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiRunRecord {
    pub id: u64,
    /// The kernel-minted nonce; the challenge is re-derivable via
    /// [`challenge32`].
    pub nonce: u64,
    pub hardware: String,
    pub prover_version: String,
    /// Which statement's rules these wins were verified under. Absent in
    /// rows written before M6 Phase B1, which were all dense.
    #[serde(default)]
    pub statement: Statement,
    /// Wins verified (k at submission time).
    pub k: u64,
    /// hex32 of the jackpot target the wins were verified against —
    /// persisted per-run so a later re-calibration cannot rewrite history.
    pub target: String,
    /// Client-reported grind window (display-only).
    pub grind_elapsed_ms: u64,
    /// The winning extranonces, strictly ascending.
    pub win_extranonces: Vec<u64>,
    /// Server-observed unix ms.
    pub issued_at_ms: u64,
    pub submitted_at_ms: u64,
}

/// Pending/used challenges + verified runs, one JSON event per line:
/// `{"ev":"challenge",…}` on mint, `{"ev":"run",…}` on a verified
/// submission. Unparseable lines (torn writes) are skipped at load, same
/// policy as `economics::read_history`.
pub struct AiStore {
    path: PathBuf,
    /// nonce → issued_at_ms, for challenges minted but not yet consumed.
    pending: HashMap<u64, u64>,
    used: HashSet<u64>,
    runs: Vec<AiRunRecord>,
    next_id: u64,
}

impl AiStore {
    /// The store lives beside the kernel state and econ history on the
    /// persistent volume.
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join("aipow-track.jsonl")
    }

    /// Replay the event log (a missing file is an empty store).
    pub fn load(path: PathBuf) -> std::io::Result<Self> {
        let mut store = Self {
            path,
            pending: HashMap::new(),
            used: HashSet::new(),
            runs: Vec::new(),
            next_id: 1,
        };
        let raw = match std::fs::read_to_string(&store.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            r => r?,
        };
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match v["ev"].as_str() {
                Some("challenge") => {
                    if let (Some(nonce), Some(t)) = (v["nonce"].as_u64(), v["issued_at_ms"].as_u64())
                    {
                        store.pending.insert(nonce, t);
                    }
                }
                Some("run") => {
                    if let Ok(run) = serde_json::from_value::<AiRunRecord>(v) {
                        store.pending.remove(&run.nonce);
                        store.used.insert(run.nonce);
                        store.next_id = store.next_id.max(run.id + 1);
                        store.runs.push(run);
                    }
                }
                _ => {}
            }
        }
        Ok(store)
    }

    fn append(&self, v: &serde_json::Value) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{v}")
    }

    /// Record a freshly minted challenge (server-observed issue time).
    pub fn record_challenge(&mut self, nonce: u64, issued_at_ms: u64) -> std::io::Result<()> {
        self.append(&serde_json::json!({
            "ev": "challenge", "nonce": nonce, "issued_at_ms": issued_at_ms,
        }))?;
        self.pending.insert(nonce, issued_at_ms);
        Ok(())
    }

    /// Is `nonce` pending, unexpired and unused at `now_ms`? Returns its
    /// issue time; error strings mirror the kernel's ZK reject reasons.
    pub fn challenge_status(&self, nonce: u64, now_ms: u64) -> Result<u64, String> {
        if self.used.contains(&nonce) {
            return Err("nonce-used".into());
        }
        let issued_at_ms = *self.pending.get(&nonce).ok_or("unknown-nonce")?;
        if now_ms.saturating_sub(issued_at_ms) > AI_WINDOW_MS {
            return Err("stale-nonce".into());
        }
        Ok(issued_at_ms)
    }

    /// Atomically (under the caller's lock) re-check the challenge, mark
    /// the nonce used and append the run. `Ok(Err(reason))` = rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_run(
        &mut self,
        nonce: u64,
        statement: Statement,
        hardware: &str,
        prover_version: &str,
        target: &[u8; 32],
        grind_elapsed_ms: u64,
        win_extranonces: Vec<u64>,
        now_ms: u64,
    ) -> std::io::Result<Result<AiRunRecord, String>> {
        let issued_at_ms = match self.challenge_status(nonce, now_ms) {
            Ok(t) => t,
            Err(reason) => return Ok(Err(reason)),
        };
        let run = AiRunRecord {
            id: self.next_id,
            nonce,
            hardware: hardware.to_string(),
            prover_version: prover_version.to_string(),
            statement,
            k: win_extranonces.len() as u64,
            target: tock::aipow::hex32(target),
            grind_elapsed_ms,
            win_extranonces,
            issued_at_ms,
            submitted_at_ms: now_ms,
        };
        let mut v = serde_json::to_value(&run).expect("run record serializes");
        v["ev"] = serde_json::json!("run");
        self.append(&v)?;
        self.pending.remove(&nonce);
        self.used.insert(nonce);
        self.next_id += 1;
        self.runs.push(run.clone());
        Ok(Ok(run))
    }

    pub fn runs(&self) -> &[AiRunRecord] {
        &self.runs
    }
}

// ---------------------------------------------------------------------------
// Verifier: compact batch-STARK context + full submission verify
// ---------------------------------------------------------------------------

/// The verifier-owned compact setup (~1 GB resident) plus the pinned
/// 40-byte verifier-key digest derived from it.
///
/// The digest is re-derived from the context's own metadata/FRI binding via
/// `validate_setup_binding` on every load — the context file is
/// server-owned (written by this process or provisioned by the operator),
/// so this pins "the setup this server booted with", never anything a
/// prover supplied. Task 5 may additionally pin the digest as a config
/// constant per NOCKCHAIN_PIN.
pub struct AiVerifier {
    context: AiPowCompactBatchVerifierContext,
    pinned_digest: [u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
}

impl AiVerifier {
    /// Where the serialized context lives on the persistent volume.
    /// Pin-scoped: the context encodes the AIR at the pinned nockchain
    /// commit, so a re-pin must never load an old blob (the pre-Pearl-V3
    /// one is for a different constraint system). A new pin simply misses
    /// this path and rebuilds by proving (~25 s) on the first submission —
    /// rotation is automatic, old blobs are inert leftovers — which
    /// [`sweep_stale_contexts`] deletes at boot, because they are ~1 GB
    /// each and two accumulate per re-pin.
    pub fn context_path(data_dir: &Path) -> PathBuf {
        data_dir.join(format!(
            "aipow-verifier-context-{}.bin",
            tock::miner::NOCKCHAIN_PIN
        ))
    }

    /// Load the compact verifier context from `data_dir` (the ~505 ms
    /// path; Task 5 ships the pre-serialized blob on the deploy volume),
    /// else build it by proving one throwaway certificate (~25 s, ~4 GB
    /// peak — the acceptable v1 fallback) and persist it so subsequent
    /// boots take the fast path. Blocking and CPU/RAM heavy: call from
    /// `spawn_blocking`, once, lazily (first AI submission).
    pub fn load_or_build(data_dir: &Path) -> Result<Self, String> {
        let path = Self::context_path(data_dir);
        let context = match std::fs::read(&path) {
            Ok(bytes) => {
                let (context, _): (AiPowCompactBatchVerifierContext, usize) =
                    bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                        .map_err(|e| format!("decode {}: {e}", path.display()))?;
                context
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => build_context_by_proving(&path)?,
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        // Defense in depth, loaded or built: check the metadata/FRI/
        // common-data binding and re-derive the pinned digest from it.
        let digest = context
            .validate_setup_binding()
            .map_err(|e| format!("verifier context failed setup binding: {e:?}"))?;
        Ok(Self {
            pinned_digest: compact_batch_verifier_key_digest_to_bytes(&digest),
            context,
        })
    }

    /// Verify one win end-to-end (blocking, ~79 ms): see
    /// [`verify_submission`].
    pub fn verify(
        &self,
        challenge: &[u8; 32],
        target: &[u8; 32],
        extranonce: u64,
        blob_bytes: &[u8],
    ) -> Result<(), String> {
        verify_submission(
            challenge,
            target,
            &self.context,
            &self.pinned_digest,
            extranonce,
            blob_bytes,
        )
    }
}

/// Delete verifier-context blobs left behind by earlier pins.
///
/// Contexts are pin-scoped (~1 GB each, two per pin once the MoE
/// statement is in play), so without a sweep every re-pin adds ~2 GB to
/// a 5 GB volume. Stale blobs are inert: nothing re-verifies a stored
/// run — verification happens once, at submission — and a context for
/// any pin can be rebuilt by proving. Deleting is therefore safe and
/// the only thing standing between us and a full disk.
///
/// Conservative by construction: only files matching the two known
/// prefixes are considered, only when their pin segment differs from
/// the current one, and a failed removal logs rather than aborting boot.
pub fn sweep_stale_contexts(data_dir: &Path) {
    const PREFIXES: [&str; 2] = ["aipow-verifier-context-", "aipow-moe-verifier-context-"];
    let pin = tock::miner::NOCKCHAIN_PIN;
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(prefix) = PREFIXES.iter().find(|p| name.starts_with(**p)) else {
            continue;
        };
        if !name.ends_with(".bin") {
            continue;
        }
        let this_pin = &name[prefix.len()..name.len() - ".bin".len()];
        if this_pin == pin {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(entry.path()) {
            Ok(()) => eprintln!(
                "aipow: swept stale verifier context {name} ({:.0} MB, pin {this_pin})",
                bytes as f64 / (1024.0 * 1024.0)
            ),
            Err(e) => eprintln!("aipow: could not sweep {name}: {e}"),
        }
    }
}

/// Build the compact verifier context by proving one throwaway certificate
/// for the canonical shape (statement-independent: the context depends
/// only on the params/trace-height bucket, not the statement — Task 1
/// finding), then persist it for the next boot.
fn build_context_by_proving(path: &Path) -> Result<AiPowCompactBatchVerifierContext, String> {
    eprintln!(
        "aipow: no verifier context at {} — building by proving (~25 s, ~4 GB peak)…",
        path.display()
    );
    let challenge = *blake3::hash(b"nockmark-ai-v1 verifier-context-build").as_bytes();
    let nonce = 0u64.to_le_bytes();
    let target = [0xffu8; 32]; // max target: the throwaway attempt always wins
    let (a, b) = synth_matrices(&challenge, &AI_PARAMS);
    let ctx = BlockContext::build(&challenge, &nonce, &a, &b, &AI_PARAMS)
        .map_err(|e| format!("context build: {e}"))?;
    let run = prove_ai_pow_compact_recursive_certificate(&ctx, &AI_PARAMS, &nonce, &target, 0)
        .map_err(|e| format!("context prove: {e:?}"))?;
    let bytes = bincode::serde::encode_to_vec(run.verifier_context(), bincode::config::standard())
        .map_err(|e| format!("context serialize: {e}"))?;
    // Write-then-rename so a crash mid-write can't leave a torn blob for
    // the next boot's fast path.
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))?;
    eprintln!(
        "aipow: verifier context built and persisted ({:.1} MB)",
        bytes.len() as f64 / (1024.0 * 1024.0)
    );
    let (context, _): (AiPowCompactBatchVerifierContext, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map_err(|e| format!("round-trip decode: {e}"))?;
    Ok(context)
}

/// Verify one untrusted win end-to-end against a registry challenge —
/// the Task-1 spike's `verify_submission` (aipow-spike/src/verifier.rs),
/// lifted with two registry additions up front: the blob size cap and the
/// binding of the blob's nonce to the win's claimed extranonce.
///
/// Node-side accept recipe (the `derive_ai_pow_statement` path):
/// 1. Decode the submission blob (postcard) and the compact certificate
///    bytes (canonical-form-checked, 150 KB consensus cap).
/// 2. Check the certificate's verifier-key digest against the pinned
///    40-byte digest (never trust a prover-supplied setup).
/// 3. Re-derive the statement from trusted data only: re-synthesize the
///    matrices from the challenge, re-derive κ / matrix commitments /
///    noise seeds / pow_key, and bind every public input via
///    `verify_ai_pow_full_matmul_production_statement` (rejects multi-tile
///    params, wrong found_idx, wrong trace height, any PI mismatch, and
///    HASH_JACKPOT > target).
/// 4. Re-derive the canonical Layer-0 program commitment from the opened
///    schedule (never the prover's program) and run the compact
///    cryptographic verify against the verifier-owned context.
///
/// Nothing carried in the submission is trusted except as a claim to
/// check.
pub fn verify_submission(
    challenge: &[u8; 32],
    target: &[u8; 32],
    context: &AiPowCompactBatchVerifierContext,
    pinned_digest: &[u8; AI_POW_COMPACT_BATCH_VERIFIER_KEY_DIGEST_BYTES],
    extranonce: u64,
    blob_bytes: &[u8],
) -> Result<(), String> {
    if blob_bytes.len() > AI_SUBMISSION_BLOB_MAX {
        return Err(format!(
            "submission blob {} bytes exceeds {AI_SUBMISSION_BLOB_MAX}",
            blob_bytes.len()
        ));
    }
    let params = &AI_PARAMS;

    // (1) Decode the blob and the certificate bytes; bind the blob to the
    //     claimed extranonce (the AI_NONCE_RULE encoding, LE8).
    let sub: AiCertBlob =
        postcard::from_bytes(blob_bytes).map_err(|e| format!("submission decode: {e}"))?;
    if nonce_extranonce(&sub.nonce) != Some(extranonce) {
        return Err("blob nonce does not match the win's extranonce".to_string());
    }
    let cert = decode_compact_batch_recursive_certificate(&sub.cert_bytes)
        .map_err(|e| format!("certificate decode: {e}"))?;

    // (2) Pin check: the certificate must declare the registry-pinned setup.
    let digest_bytes = compact_batch_verifier_key_digest_to_bytes(cert.verifier_key_digest());
    if &digest_bytes != pinned_digest {
        return Err("certificate verifier-key digest does not match pinned digest".to_string());
    }

    // (3) Re-derive the statement from trusted data only.
    let (a, b) = synth_matrices(challenge, params);
    let kappa = commitment_key(&block_state(challenge, &sub.nonce), &params_tag(params));
    let a_bytes: Vec<u8> = a.iter().map(|&v| v as u8).collect();
    let b_bytes: Vec<u8> = b.iter().map(|&v| v as u8).collect();
    let commitments = ZkPublicCommitments {
        h_a_chunk: matrix_commitment(&a_bytes, &kappa),
        h_b_chunk: matrix_commitment(&b_bytes, &kappa),
    };
    verify_ai_pow_full_matmul_production_statement(
        challenge,
        &sub.nonce,
        params,
        target,
        sub.found_idx,
        &commitments,
        &sub.pis,
        sub.trace_height,
    )
    .map_err(|e| format!("statement re-derivation: {e:?}"))?;

    // (4) Canonical L0 program commitment from the opened schedule, then
    //     the compact cryptographic verify against the verifier-owned
    //     context.
    let zk_params = zk_params_from_matmul(params);
    let (tile_i, tile_j) = params.tile_coords(sub.found_idx as u64);
    let schedule = StripIndexSchedule::from_tile(&zk_params, tile_i, tile_j)
        .map_err(|e| format!("strip schedule: {e}"))?;
    // Defense-in-depth: the statement check above already pinned
    // trace_height to the schedule-derived value; recompute so this
    // function stands alone.
    let trace_height = expected_layer0_rows_for_strip_schedule(params, &schedule)
        .map_err(|e| format!("trace height: {e:?}"))?
        .required_trace_len();
    if trace_height != sub.trace_height {
        return Err("trace height does not match opened schedule".to_string());
    }
    let (s_a, s_b) = canonical_noise_seeds_from_matrix_commitments(
        &kappa,
        &commitments.h_a_chunk,
        &commitments.h_b_chunk,
        AI_PARAMS.m,
        AI_PARAMS.n,
    );
    let block_public = BlockPublic {
        tile_i,
        tile_j,
        kappa,
        s_a,
        s_b,
    };
    let program =
        canonical_program_for_strip_schedule(&zk_params, &schedule, &block_public, trace_height)
            .map_err(|e| format!("canonical program: {e}"))?;
    let profile = CircuitConfig::for_layer0_trace(trace_height);
    let l0_commitment = canonical_l0_program_commitment_vals(&zk_params, &profile, &program);

    verify_compact_batch_recursive_certificate_with_context(context, cert, &sub.pis, &l0_commitment)
        .map_err(|e| format!("compact certificate verify: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the nonce→challenge derivation. If either assertion breaks,
    /// AI_CHALLENGE_DOMAIN must be version-bumped — silently changing the
    /// derivation would orphan every pending challenge.
    #[test]
    fn challenge_derivation_is_stable_and_le8() {
        // Explicit-preimage form: the domain then the nonce as LE8.
        let mut h = blake3::Hasher::new();
        h.update(b"nockmark-ai-v1");
        h.update(&[0x67, 0x45, 0x23, 0x01, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(challenge32(0x0123_4567), *h.finalize().as_bytes());
        // Snapshot: pins the derivation across refactors.
        assert_eq!(
            tock::aipow::hex32(&challenge32(0)),
            "79abfc7c59a366198280b0ff994bed114d16d616cf0b94825fe9d1f52d57ad65",
        );
        assert_ne!(challenge32(0), challenge32(1));
    }

    #[test]
    fn extranonce_ordering_rule() {
        assert!(extranonces_strictly_ascending(&[0, 1, 5]));
        assert!(extranonces_strictly_ascending(&[7]));
        assert!(extranonces_strictly_ascending(&[]));
        assert!(!extranonces_strictly_ascending(&[1, 0]), "descending");
        assert!(!extranonces_strictly_ascending(&[0, 0]), "duplicate");
        assert!(!extranonces_strictly_ascending(&[0, 2, 2]), "late duplicate");
    }

    /// Worked example with round numbers: T+1 = 2^255 ⇒ 2 attempts/win;
    /// k=4 wins in a 4 s server window and a 2 s grind window ⇒
    /// 4·2·65536 = 524288 MAC-equivalents ⇒ 131072 MAC/s verified,
    /// 262144 MAC/s grind, ±1σ = 1/√4 = 0.5.
    #[test]
    fn economics_worked_example() {
        let mut target = [0xffu8; 32];
        target[31] = 0x7f; // LE: T = 2^255 − 1, so T+1 = 2^255 exactly
        assert_eq!(expected_attempts_per_win(&target), 2.0);
        let r = rates(Statement::Dense, 4, &target, 4_000, 2_000);
        // signif4 rounds to 4 significant digits: 131072 → 131100 etc.
        assert_eq!(r.verified_mac_per_sec_lb, 131_100.0);
        assert_eq!(r.grind_mac_per_sec, 262_100.0);
        assert_eq!(r.zk_attempt_equiv_per_sec, signif4(131_072.0 / 25.75e9));
        assert_eq!(r.sigma_frac, 0.5);
    }

    #[test]
    fn default_target_is_2_pow_248() {
        assert_eq!(expected_attempts_per_win(&default_target()), 256.0);
        let max = [0xffu8; 32]; // T+1 rounds to 2^256 in f64: 1 attempt/win
        assert_eq!(expected_attempts_per_win(&max), 1.0);
        // The deployed production target (LE hex "…0800", bytes[30]=8) is
        // 2^243 ⇒ ~8191 expected attempts per win. Locks the LE parse: the
        // same hex read big-endian would be 2^11 ⇒ 2^245 attempts — the
        // grind-that-never-ends the first deploy shipped.
        let prod = tock::aipow::parse_hex32(
            "0000000000000000000000000000000000000000000000000000000000000800",
        )
        .unwrap();
        // f64 can't see the +1 at 2^243 magnitude: exactly 2^13.
        let attempts = expected_attempts_per_win(&prod);
        assert_eq!(attempts, 8192.0, "LE parse of the production target");
    }

    #[test]
    fn signif4_rounds_sanely() {
        assert_eq!(signif4(123_456.0), 123_500.0);
        assert_eq!(signif4(0.000123456), 0.0001235);
        assert_eq!(signif4(0.0), 0.0);
        assert_eq!(signif4(1.0), 1.0);
    }

    #[test]
    /// The sweep must remove exactly the other-pin context blobs and
    /// nothing else — it runs at boot against the live volume, where the
    /// neighbours are kernel state and the leaderboard's own JSONL.
    #[test]
    fn sweep_removes_only_other_pins_contexts() {
        let pin = tock::miner::NOCKCHAIN_PIN;
        // Thread id as well as pid: the harness can run this binary's
        // tests on several threads, and a pid-only name would have two
        // runs sweeping each other's fixture directory.
        let dir = std::env::temp_dir().join(format!(
            "nockmark-sweep-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();

        let current = format!("aipow-verifier-context-{pin}.bin");
        let current_moe = format!("aipow-moe-verifier-context-{pin}.bin");
        write(&current);
        write(&current_moe);
        write("aipow-verifier-context-1372f270.bin");
        write("aipow-moe-verifier-context-1372f270.bin");
        // Neighbours that must survive: the store, econ history, and an
        // unrelated file that merely shares a word with the prefixes.
        write("aipow-track.jsonl");
        write("econ-history.jsonl");
        write("aipow-verifier-context-notes.txt");

        sweep_stale_contexts(&dir);

        let left: std::collections::BTreeSet<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.contains(&current), "current dense context kept");
        assert!(left.contains(&current_moe), "current MoE context kept");
        assert!(left.contains("aipow-track.jsonl"), "run store untouched");
        assert!(left.contains("econ-history.jsonl"), "econ history untouched");
        assert!(
            left.contains("aipow-verifier-context-notes.txt"),
            "non-.bin lookalike untouched"
        );
        assert!(!left.contains("aipow-verifier-context-1372f270.bin"), "stale dense swept");
        assert!(!left.contains("aipow-moe-verifier-context-1372f270.bin"), "stale MoE swept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_roundtrip_and_challenge_lifecycle() {
        let dir = std::env::temp_dir().join(format!("nockmark-aipow-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = AiStore::path_in(&dir);
        let _ = std::fs::remove_file(&path);

        let mut store = AiStore::load(path.clone()).unwrap();
        assert!(store.runs().is_empty(), "missing file is an empty store");
        assert_eq!(store.challenge_status(7, 0), Err("unknown-nonce".into()));

        store.record_challenge(7, 1_000).unwrap();
        assert_eq!(store.challenge_status(7, 2_000), Ok(1_000));
        assert_eq!(
            store.challenge_status(7, 1_000 + AI_WINDOW_MS + 1),
            Err("stale-nonce".into()),
            "1 h window, like the kernel's ZK rule"
        );

        let run = store
            .commit_run(
                7,
                Statement::Dense,
                "hw",
                "pin",
                &default_target(),
                500,
                vec![0, 3],
                61_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(run.id, 1);
        assert_eq!(run.k, 2);
        assert_eq!(run.issued_at_ms, 1_000);
        assert_eq!(run.submitted_at_ms, 61_000);
        assert_eq!(
            store.challenge_status(7, 62_000),
            Err("nonce-used".into()),
            "replay is rejected"
        );
        assert_eq!(
            store
                .commit_run(
                    7,
                    Statement::Dense,
                    "hw",
                    "pin",
                    &default_target(),
                    500,
                    vec![0],
                    62_000,
                )
                .unwrap(),
            Err("nonce-used".into())
        );

        // A second pending challenge and a torn trailing line survive the
        // reload; state is identical after replay.
        store.record_challenge(9, 70_000).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"ev\": \"run\", \"id\"\n"))
            .unwrap();
        let reloaded = AiStore::load(path.clone()).unwrap();
        assert_eq!(reloaded.runs(), store.runs());
        assert_eq!(reloaded.next_id, 2);
        assert_eq!(reloaded.challenge_status(9, 71_000), Ok(70_000));
        assert_eq!(reloaded.challenge_status(7, 71_000), Err("nonce-used".into()));

        std::fs::remove_file(&path).unwrap();
    }


    /// The statement round-trips through JSON and through the store, and an
    /// ABSENT field replays as dense — the property that makes every M5 row in
    /// `aipow-track.jsonl` reproduce its original score.
    #[test]
    fn statement_round_trips_and_defaults_to_dense() {
        assert_eq!(
            serde_json::to_value(Statement::CanonicalMoe).unwrap(),
            serde_json::json!("canonical-moe")
        );
        assert_eq!(
            serde_json::to_value(Statement::Dense).unwrap(),
            serde_json::json!("dense")
        );
        assert_eq!(Statement::parse("dense").unwrap(), Statement::Dense);
        assert_eq!(
            Statement::parse("canonical-moe").unwrap(),
            Statement::CanonicalMoe
        );
        assert!(Statement::parse("sparse").is_err(), "unknown is rejected");
        assert!(Statement::parse("Dense").is_err(), "wire values are exact");
        assert_eq!(Statement::default(), Statement::Dense);

        // A pre-M6 stored row (no `statement` key) replays as dense.
        let legacy = serde_json::json!({
            "id": 3, "nonce": 9, "hardware": "hw", "prover_version": "pin",
            "k": 4, "target": "00".repeat(32), "grind_elapsed_ms": 10,
            "win_extranonces": [0, 1, 2, 3],
            "issued_at_ms": 1, "submitted_at_ms": 2,
        });
        let run: AiRunRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(run.statement, Statement::Dense);
        // …and a round-trip through the record now carries it explicitly.
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["statement"], "dense");
        assert_eq!(serde_json::from_value::<AiRunRecord>(v).unwrap(), run);

        // The grind rules the two statements advertise are distinct.
        assert_ne!(
            Statement::Dense.nonce_rule(),
            Statement::CanonicalMoe.nonce_rule()
        );
    }

    /// The store keeps the statement per row, and a mixed board holds both.
    #[test]
    fn store_records_the_statement_per_run() {
        let dir = std::env::temp_dir().join(format!(
            "nockmark-aipow-statement-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mixed.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut store = AiStore::load(path.clone()).unwrap();
        store.record_challenge(1, 0).unwrap();
        store.record_challenge(2, 0).unwrap();
        store
            .commit_run(1, Statement::Dense, "hw", "pin", &default_target(), 10, vec![0], 100)
            .unwrap()
            .unwrap();
        store
            .commit_run(
                2,
                Statement::CanonicalMoe,
                "hw",
                "pin",
                &tock::aipow_moe::calibrated_moe_target(),
                10,
                vec![7],
                100,
            )
            .unwrap()
            .unwrap();

        let reloaded = AiStore::load(path.clone()).unwrap();
        assert_eq!(reloaded.runs(), store.runs());
        let statements: Vec<Statement> = reloaded.runs().iter().map(|r| r.statement).collect();
        assert_eq!(statements, vec![Statement::Dense, Statement::CanonicalMoe]);
        std::fs::remove_file(&path).unwrap();
    }

    /// **The two statements price the same 32 bytes differently, and the board
    /// must not conflate them.**
    ///
    /// At `T = 2^224` the dense rule expects `2^32` attempts per win and the
    /// canonical rule `2^16` — a factor of exactly `F`. Scoring a canonical run
    /// with the dense formula would inflate its rate 65 536×, which is the one
    /// way a shared leaderboard could actually lie.
    #[test]
    fn statements_score_the_same_target_differently_by_exactly_f() {
        let t = tock::aipow_moe::calibrated_moe_target(); // 2^224
        let dense = Statement::Dense.expected_attempts_per_win(&t);
        let moe = Statement::CanonicalMoe.expected_attempts_per_win(&t);
        assert_eq!(dense, 2f64.powi(32));
        assert_eq!(moe, 65_536.0);
        assert_eq!(dense / moe, tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT);

        // Same k, same windows, same target: the rates differ by F too.
        let d = rates(Statement::Dense, 4, &t, 4_000, 2_000);
        let m = rates(Statement::CanonicalMoe, 4, &t, 4_000, 2_000);
        // signif4 rounds both figures, so compare within its 4-digit slack.
        let ratio = d.verified_mac_per_sec_lb / m.verified_mac_per_sec_lb;
        let f = tock::aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT;
        assert!((ratio - f).abs() <= 0.001 * f, "ratio {ratio} vs F {f}");
        // Everything statement-independent is identical — that is what makes
        // one board legitimate.
        assert_eq!(d.sigma_frac, m.sigma_frac);

        // Worked canonical example: k=2 wins at the shipped target over a 4 s
        // server window = 2 · 65536 · 65536 MAC-equiv / 4 s.
        let r = rates(Statement::CanonicalMoe, 2, &t, 4_000, 4_000);
        assert_eq!(
            r.verified_mac_per_sec_lb,
            signif4(2.0 * 65_536.0 * 65_536.0 / 4.0)
        );
    }

    #[test]
    fn k_and_target_env_defaults() {
        // (Env is process-global; these vars are only read here and in
        // boot, and this test restores them.)
        std::env::remove_var("NOCKMARK_AI_K");
        std::env::remove_var("NOCKMARK_AI_TARGET");
        assert_eq!(k_from_env(), AI_K_DEFAULT);
        assert_eq!(target_from_env(), default_target());
        std::env::set_var("NOCKMARK_AI_K", "0");
        assert_eq!(k_from_env(), AI_K_DEFAULT, "k=0 is not a valid override");
        std::env::set_var("NOCKMARK_AI_K", "2");
        assert_eq!(k_from_env(), 2);
        std::env::set_var("NOCKMARK_AI_TARGET", &"f".repeat(64));
        assert_eq!(target_from_env(), [0xff; 32]);
        std::env::remove_var("NOCKMARK_AI_K");
        std::env::remove_var("NOCKMARK_AI_TARGET");
    }
}
