//! M7 step 3: the ZK v5 (Anthropos) hash-rate track, server side — the run
//! store, the per-win checks, and the rates.
//!
//! ## Trust story
//!
//! Mirrors the AI track ([`crate::aipow`]): every ranked number is computed
//! from proofs THIS server verified, of a commitment THIS server derived from
//! a kernel-minted nonce, against a target THIS server issued, over a window
//! THIS server observed on its own clock. What must be remembered goes into an
//! append-only JSONL file beside the AI store, replayed at boot; the registry
//! kernel only mints the nonce (no hoon change, no kernel-state migration).
//!
//! ## What one win must pass
//!
//! Upstream's verifier (`zkvm_jetpack::form::verify::verify`) checks that a
//! STARK is valid for the puzzle in its own object 0. It does NOT check the
//! target, that the commitment is ours, that `len` is 64, or that the version
//! is `%5` — a `%3` proof passes it. Consensus does those separately
//! (`+verify-page-pow` and `+check-target`, hoon/apps/dumbnet). So, per win,
//! cheapest first, in [`check_win`] and [`verify_win_native`]:
//!
//! 1. size cap, then cue and decode the proof;
//! 2. `version` is `%5` — consensus requires `%5` from height 147,500;
//! 3. object 0 is a `%puzzle` (`+check-pow-puzzle`);
//! 4. its commitment is the one derived from this challenge
//!    (`+check-pow-puzzle`: `block-commitment = commitment.puzzle`) — which is
//!    what stops a proof made for another challenge;
//! 5. `len` is 64 (`+check-pow-puzzle`: `pow-len = len.puzzle`);
//! 6. the nonce is based;
//! 7. `pow-v5` is re-derived from the SUBMITTED object 0 exactly as
//!    `+proof-to-pow` does ([`tock::zkv5::pow_digest_of_object0`]), and must
//!    equal upstream's own [`v5_pow_digest`] of the object's
//!    `(commitment, nonce, len)` — so object 0 is the canonical puzzle object
//!    and the digest is the one consensus would compute. Nothing digest-shaped
//!    is taken from the client;
//! 8. that digest is `≤ T_b` under upstream's comparison ([`digest_le`], i.e.
//!    `+check-target`);
//! 9. every `%poly` object is canonical (`+canonical-pow-proof`, in force
//!    since height 112,500);
//! 10. the STARK verifies natively (`verify_win_native`), and its reported
//!     commitment and nonce are the ones checked above;
//! 11. it verifies again under the consensus Hoon verifier (`verify:nv`, via
//!     the roswell kernel — [`crate::verifier::Verifier`], in `http.rs`).
//!
//! Across a submission the winners' nonces must be strictly ascending, so
//! each `(C, nonce)` is counted once. That is the dedupe, and it is on the
//! nonce, not on the proof: a proof's objects after object 0 are a witness
//! envelope that consensus deliberately does not identify (Anthropos #188 —
//! alternate valid suffixes name the same block), so two different proofs of
//! one nonce are one win. Each challenge is single-use, so `(C, nonce)` is
//! unique across submissions as well.
//!
//! A byte-for-byte `%3` proof relabelled `%5` IS a `%5` proof of the same
//! object 0 (the two versions share the AIR, tables and transcript), and is
//! counted exactly when consensus would count it: once per nonce, only if its
//! object-0 digest meets the target.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use nockapp::noun::slab::NounSlab;
use nockapp::NounAllocator;
use noun_serde::NounDecode;
use serde::{Deserialize, Serialize};
use tock::zkv5::{
    belts_serde, belts_vec_serde, expected_attempts_per_win, is_based, pow_digest_of_object0,
    Belts5, ZK_V5_POW_LEN,
};
use zk_pow_miner::v5::{digest_le, v5_pow_digest};
use zkvm_jetpack::form::belt::Belt;
use zkvm_jetpack::form::proof::{Proof, ProofData, ProofVersion};
use zkvm_jetpack::form::verify::{verify, VerifyArgs};

/// The consensus rules a v5 row was verified under, stamped by the server.
/// The ZK peer of the AI track's `hardened`; the board shows `Anthropos`.
pub const ZK_V5_RULES: &str = "anthropos";

/// Challenges go stale an hour after mint, like every other track's.
pub const ZK_V5_WINDOW_MS: u64 = 3_600_000;

/// Per-proof cap, enforced before any decode. A pow-len-64 proof jams to
/// ~116 KB; 512 KiB is headroom without letting one request allocate much.
pub const ZK_PROOF_JAM_MAX: usize = 512 * 1024;

/// A minted, not-yet-consumed v5 challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingZkChallenge {
    pub issued_at_ms: u64,
    /// The benchmark target T_b this challenge was issued at (base-p limbs).
    pub target: Belts5,
    /// The granted tier it realises, in expected Tip5 hashes per win.
    pub attempts: u64,
}

/// One verified v5 run, exactly as persisted (and served, with rates).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZkV5RunRecord {
    pub id: u64,
    /// The kernel-minted nonce; the commitment is re-derivable from it
    /// ([`tock::zkv5::commitment_for_nonce`]).
    pub nonce: u64,
    pub hardware: String,
    pub prover_version: String,
    /// Server-attested ([`ZK_V5_RULES`]), never taken from the client.
    pub rules: String,
    /// Wins verified.
    pub k: u64,
    /// T_b the wins were verified against, as 5 decimal-string belts.
    #[serde(with = "belts_serde")]
    pub target: Belts5,
    /// The tier that target realises (expected Tip5 hashes per win).
    pub attempts: u64,
    /// Client-reported grind window (display only).
    pub grind_elapsed_ms: u64,
    /// The winning nonces, strictly ascending.
    #[serde(with = "belts_vec_serde")]
    pub win_nonces: Vec<Belts5>,
    /// Server-observed unix ms.
    pub issued_at_ms: u64,
    pub submitted_at_ms: u64,
}

/// Pending/used challenges plus verified runs, one JSON event per line
/// (`{"ev":"challenge",…}` on mint, `{"ev":"run",…}` on a verified
/// submission), replayed at boot. Unparseable lines are skipped, the AI
/// store's policy.
pub struct ZkV5Store {
    path: PathBuf,
    pending: HashMap<u64, PendingZkChallenge>,
    used: HashSet<u64>,
    runs: Vec<ZkV5RunRecord>,
    next_id: u64,
}

impl ZkV5Store {
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join("zkv5-track.jsonl")
    }

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
        let (values, skipped) = crate::jsonl::parse_lines(&raw);
        if skipped > 0 {
            eprintln!(
                "zk v5 store: skipped {skipped} unparseable line(s) in {} (torn writes)",
                store.path.display()
            );
        }
        for v in values {
            match v["ev"].as_str() {
                Some("challenge") => {
                    #[derive(Deserialize)]
                    struct Ev {
                        nonce: u64,
                        issued_at_ms: u64,
                        #[serde(with = "belts_serde")]
                        target: Belts5,
                        attempts: u64,
                    }
                    if let Ok(ev) = serde_json::from_value::<Ev>(v) {
                        store.pending.insert(
                            ev.nonce,
                            PendingZkChallenge {
                                issued_at_ms: ev.issued_at_ms,
                                target: ev.target,
                                attempts: ev.attempts,
                            },
                        );
                    }
                }
                Some("run") => {
                    if let Ok(run) = serde_json::from_value::<ZkV5RunRecord>(v) {
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

    /// One record per line, crash-safe for the records already written
    /// ([`crate::jsonl::append_line`]).
    fn append(&self, v: &serde_json::Value) -> std::io::Result<()> {
        crate::jsonl::append_line(&self.path, v)
    }

    /// Record a freshly minted challenge with the target it was issued at.
    pub fn record_challenge(
        &mut self,
        nonce: u64,
        issued_at_ms: u64,
        target: Belts5,
        attempts: u64,
    ) -> std::io::Result<()> {
        self.append(&serde_json::json!({
            "ev": "challenge",
            "nonce": nonce,
            "issued_at_ms": issued_at_ms,
            "target": tock::zkv5::belts_to_strings(&target),
            "attempts": attempts,
        }))?;
        self.pending.insert(
            nonce,
            PendingZkChallenge {
                issued_at_ms,
                target,
                attempts,
            },
        );
        Ok(())
    }

    /// Pending, unexpired and unused at `now_ms`? Reasons mirror the kernel's.
    pub fn challenge_status(&self, nonce: u64, now_ms: u64) -> Result<PendingZkChallenge, String> {
        if self.used.contains(&nonce) {
            return Err("nonce-used".into());
        }
        let pending = *self.pending.get(&nonce).ok_or("unknown-nonce")?;
        if now_ms.saturating_sub(pending.issued_at_ms) > ZK_V5_WINDOW_MS {
            return Err("stale-nonce".into());
        }
        Ok(pending)
    }

    /// Under the caller's lock: re-check the challenge, mark it used, append
    /// the run. `Ok(Err(reason))` = rejected.
    pub fn commit_run(
        &mut self,
        nonce: u64,
        hardware: &str,
        prover_version: &str,
        grind_elapsed_ms: u64,
        win_nonces: Vec<Belts5>,
        now_ms: u64,
    ) -> std::io::Result<Result<ZkV5RunRecord, String>> {
        let pending = match self.challenge_status(nonce, now_ms) {
            Ok(p) => p,
            Err(reason) => return Ok(Err(reason)),
        };
        let run = ZkV5RunRecord {
            id: self.next_id,
            nonce,
            hardware: hardware.to_string(),
            prover_version: prover_version.to_string(),
            rules: ZK_V5_RULES.to_string(),
            k: win_nonces.len() as u64,
            target: pending.target,
            attempts: pending.attempts,
            grind_elapsed_ms,
            win_nonces,
            issued_at_ms: pending.issued_at_ms,
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

    pub fn runs(&self) -> &[ZkV5RunRecord] {
        &self.runs
    }
}

// ---------------------------------------------------------------------------
// Per-win checks
// ---------------------------------------------------------------------------

/// A win that passed every structural and lottery check ([`check_win`]) and
/// still awaits STARK verification.
pub struct CheckedWin {
    /// The nonce read from the proof's own object 0.
    pub nonce: Belts5,
    /// The decoded proof, for [`verify_win_native`].
    pub proof: Proof,
    /// The re-derived `pow-v5` (display/debug only; already checked).
    pub digest: Belts5,
}

/// `+bcan`'s fixed points (hoon/common/ztd/one.hoon): high zero coefficients
/// stripped, with the zero polynomial written `[0]`. So `[0]`, or non-empty
/// with a non-zero top coefficient.
fn bpoly_is_canonical(coeffs: &[Belt]) -> bool {
    match coeffs.last() {
        None => false,
        Some(&Belt(0)) => coeffs.len() == 1,
        Some(_) => true,
    }
}

/// Steps 1–9 of the module docs: everything short of STARK verification, for
/// one proof jam against this challenge's `commitment` and `target`. Cheap
/// (one decode, two digests), so a submission is checked in full before any
/// verifier time is spent on it.
pub fn check_win(commitment: &Belts5, target: &Belts5, jam: &[u8]) -> Result<CheckedWin, String> {
    if jam.len() > ZK_PROOF_JAM_MAX {
        return Err(format!(
            "proof jam {} bytes exceeds {ZK_PROOF_JAM_MAX}",
            jam.len()
        ));
    }
    let mut slab: NounSlab = NounSlab::new();
    let noun = slab
        .cue_into(jam.to_vec().into())
        .map_err(|_| "proof jam does not cue".to_string())?;
    let space = slab.noun_space();
    let proof =
        Proof::from_noun(&noun, &space).map_err(|e| format!("proof does not decode: {e:?}"))?;
    if proof.version != ProofVersion::V5 {
        return Err(format!(
            "proof version {:?}, expected V5: mainnet has taken only %5 ZK proofs since height 147,500",
            proof.version
        ));
    }
    let Some(ProofData::Puzzle {
        com, nonce, len, ..
    }) = proof.objects.first()
    else {
        return Err("object 0 is not a %puzzle".into());
    };
    if com != commitment {
        return Err(
            "object 0 commitment is not this challenge's (a proof made for another challenge?)"
                .into(),
        );
    }
    if *len != ZK_V5_POW_LEN {
        return Err(format!("object 0 len {len}, expected {ZK_V5_POW_LEN}"));
    }
    if !is_based(nonce) {
        return Err("object 0 nonce is not based".into());
    }
    let digest = pow_digest_of_object0(&proof.objects[0]);
    if digest != v5_pow_digest(*com, *nonce, *len) {
        return Err("object 0 is not the canonical puzzle object for its nonce".into());
    }
    if !digest_le(&digest, target) {
        return Err("object 0 digest does not meet the challenge target".into());
    }
    for (i, obj) in proof.objects.iter().enumerate() {
        if let ProofData::Poly(p) = obj {
            if !bpoly_is_canonical(&p.0) {
                return Err(format!("proof object {i} is a non-canonical polynomial"));
            }
        }
    }
    let nonce = *nonce;
    Ok(CheckedWin {
        nonce,
        proof,
        digest,
    })
}

/// Step 10: upstream's native STARK verifier, then a cross-check that what it
/// verified is the commitment and nonce [`check_win`] checked. Blocking and
/// CPU-bound; call from `spawn_blocking`.
///
/// `eny` only orders the Merkle-path checks (every one is checked whatever
/// the order), as consensus's own `verify:nv` call takes block entropy.
pub fn verify_win_native(win: CheckedWin, commitment: &Belts5, eny: u64) -> Result<(), String> {
    let nonce = win.nonce;
    let res = verify(VerifyArgs {
        proof: win.proof,
        table_override: None,
        verifier_eny: eny,
    })
    .map_err(|e| format!("STARK verification failed: {e:?}"))?;
    if res.commitment != *commitment || res.nonce != nonce {
        return Err("verified statement differs from the checked object 0".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rates
// ---------------------------------------------------------------------------

/// The computed board figures for one v5 run.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ZkV5Rates {
    /// k · expected-hashes-per-win over the SERVER window (mint → submit,
    /// proving included): the ranked lower bound, in Tip5 hashes/s.
    pub verified_zk_hashes_per_sec_lb: f64,
    /// The same numerator over the client-reported grind window (display).
    pub grind_zk_hashes_per_sec: f64,
    /// ±1σ as a fraction of the rate: k wins is Poisson, so 1/√k.
    pub sigma_frac: f64,
    /// Expected Tip5 hashes per win at the run's target: its tier.
    pub expected_attempts_per_win: f64,
}

pub fn rates(k: u64, target: &Belts5, server_window_ms: u64, grind_elapsed_ms: u64) -> ZkV5Rates {
    use crate::economics::signif4;
    let per_win = expected_attempts_per_win(target);
    let total = k as f64 * per_win;
    ZkV5Rates {
        verified_zk_hashes_per_sec_lb: signif4(total / (server_window_ms.max(1) as f64 / 1000.0)),
        grind_zk_hashes_per_sec: signif4(total / (grind_elapsed_ms.max(1) as f64 / 1000.0)),
        sigma_frac: signif4(1.0 / (k.max(1) as f64).sqrt()),
        // Not rounded: a granted tier is an exact power of two, shown as 2^N.
        expected_attempts_per_win: per_win,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn canonical_polynomials_are_bcan_fixed_points() {
        assert!(bpoly_is_canonical(&[Belt(0)]), "the zero polynomial is [0]");
        assert!(bpoly_is_canonical(&[Belt(3), Belt(1)]));
        assert!(!bpoly_is_canonical(&[]), "empty canonicalises to [0]");
        assert!(!bpoly_is_canonical(&[Belt(3), Belt(0)]), "high zero");
        assert!(!bpoly_is_canonical(&[Belt(0), Belt(0)]));
    }

    /// Worked example: tier 2^12, k = 2, 4 s server window, 2 s grind:
    /// 8192 hashes credited, 2048 H/s ranked, 4096 H/s grind, ±1/√2.
    #[test]
    fn rates_worked_example() {
        let t = tock::zkv5::target_for_attempts(1 << 12).unwrap();
        let r = rates(2, &t, 4_000, 2_000);
        assert_eq!(r.expected_attempts_per_win, 4096.0);
        assert_eq!(r.verified_zk_hashes_per_sec_lb, 2048.0);
        assert_eq!(r.grind_zk_hashes_per_sec, 4096.0);
        assert_eq!(r.sigma_frac, signif(1.0 / 2f64.sqrt()));
    }

    fn signif(x: f64) -> f64 {
        crate::economics::signif4(x)
    }

    #[test]
    fn store_round_trips_and_enforces_the_challenge_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = ZkV5Store::path_in(dir.path());
        let t = tock::zkv5::target_for_attempts(1 << 16).unwrap();
        let mut s = ZkV5Store::load(path.clone()).unwrap();
        assert_eq!(s.challenge_status(5, 0), Err("unknown-nonce".into()));
        s.record_challenge(5, 1_000, t, 1 << 16).unwrap();
        assert_eq!(s.challenge_status(5, 2_000).unwrap().target, t);
        assert_eq!(
            s.challenge_status(5, 1_000 + ZK_V5_WINDOW_MS + 1),
            Err("stale-nonce".into())
        );
        let run = s
            .commit_run(
                5,
                "hw",
                "pin",
                900,
                vec![[1, 0, 0, 0, 0], [9, 0, 0, 0, 0]],
                4_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!((run.id, run.k, run.attempts), (1, 2, 1 << 16));
        assert_eq!(run.rules, "anthropos");
        assert_eq!(s.challenge_status(5, 5_000), Err("nonce-used".into()));
        assert_eq!(
            s.commit_run(5, "hw", "pin", 900, vec![[1, 0, 0, 0, 0]], 5_000)
                .unwrap(),
            Err("nonce-used".into()),
            "a replayed submission is refused"
        );
        // Belts survive as strings on disk, and a torn line is skipped.
        s.record_challenge(6, 6_000, t, 1 << 16).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(b"{\"ev\": \"run\", \"id\"\n"))
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&format!("\"{}\"", t[0])),
            "belts are decimal strings"
        );
        let r = ZkV5Store::load(path.clone()).unwrap();
        assert_eq!(r.runs(), s.runs());
        assert_eq!(r.next_id, 2);
        assert_eq!(r.challenge_status(6, 7_000).unwrap().attempts, 1 << 16);
        assert_eq!(r.challenge_status(5, 7_000), Err("nonce-used".into()));
        // A crash mid-append leaves a fragment with NO newline. The next record
        // must still land on a line of its own and survive the replay
        // (adversarial review: it used to be glued to the fragment and lost).
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(b"{\"ev\": \"challenge\", \"nonce\""))
            .unwrap();
        let mut r = ZkV5Store::load(path.clone()).unwrap();
        r.record_challenge(8, 8_000, t, 1 << 16).unwrap();
        let r = ZkV5Store::load(path).unwrap();
        assert_eq!(r.challenge_status(8, 9_000).unwrap().issued_at_ms, 8_000);
        assert_eq!(r.runs().len(), 1);
    }
}
