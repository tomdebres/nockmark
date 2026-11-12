//! M7 step 3: the ZK v5 (Anthropos) hash-rate track — the protocol both sides
//! share, and the client's grind and prove.
//!
//! ## What changed at height 147,500
//!
//! Anthropos (protocol 017, ZK proof `%5`) moved the ZK lottery off the proof.
//! A ZK attempt is now ONE Tip5 hash of proof object 0,
//! `[%puzzle block-commitment nonce len p]`, where `p` is the puzzle product
//! for that nonce over `len = 64` sponge belts:
//!
//! ```text
//! pow-v5 = Tip5([leaf+%zkpow-v5 hash+hash-proof([%5 [object-0] ~ 0])])
//! ```
//!
//! A miner hashes nonces until one meets the target and only then builds the
//! complete STARK, which is bound to that nonce. The mining unit is therefore
//! Tip5 hashes per second, and that is what this track ranks.
//!
//! ## The benchmark statement
//!
//! * Challenge `C` = `blake3("nockmark-zk-v5" ‖ LE8(nonce))` over the
//!   kernel-minted u64 nonce — the AI track's derivation with its own domain.
//! * The block commitment is `C` reduced to 5 based belts: `C` read as a
//!   little-endian 256-bit integer, written in base `p` (Goldilocks). The map
//!   is injective (`2^256 < p^5`), so the commitment carries all of `C`.
//! * The client grinds nonces against a benchmark target `T_b` with upstream's
//!   own digest ([`zk_pow_miner::v5::v5_pow_digest`], or `CudaSession::search`
//!   on a GPU) and upstream's own comparison ([`zk_pow_miner::v5::digest_le`],
//!   which is `check-target`'s `digest ≤ target` on base-p limbs).
//! * It proves `k` distinct winners through the miner kernel with
//!   `[%5 commitment nonce T_b 64]`. The kernel recomputes the digest, refuses
//!   a nonce that misses `T_b`, and returns a proof bound to the nonce.
//!
//! ## Units
//!
//! A hash wins with probability `(T_b + 1)/p^5`, so each verified win is
//! credited `p^5/(T_b + 1)` expected Tip5 hashes ([`expected_attempts_per_win`];
//! consensus prices a block's work as `(p^5 − 1)/(target + 1)`, the same figure
//! at every tier). `k` wins in a server-observed window `w` are
//! `k · p^5/(T_b + 1) / w` Tip5 hashes per second — the unit the network
//! difficulty counts, which is what makes the earnings estimate a straight
//! division.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ibig::UBig;
use nockapp::kernel::form::SerfThread;
use nockapp::save::SaveableCheckpoint;
use nockchain_math::tip5::hash::{absorb, hash_ten_cell, tip5_calc_digest};
use zk_pow_miner::v5::{digest_le, v5_pow_digest, V5Job};
use zkvm_jetpack::form::belt::PRIME;
use zkvm_jetpack::form::tog::hash_proof_data;
use zkvm_jetpack::form::ProofData;

use crate::aipow::AI_TIER_PROVE_RATIO;
use crate::miner::{self, ProveOutput};

/// Puzzle length every v5 win must use: mainnet's `pow-len`.
pub const ZK_V5_POW_LEN: u64 = 64;

/// The proof version a v5 win must carry.
pub const ZK_V5_PROOF_VERSION: u64 = 5;

/// Domain separator of the nonce→challenge derivation. Versioned: changing
/// the derivation, or [`commitment`], bumps the suffix and
/// [`ZK_V5_COMMITMENT_RULE`].
pub const ZK_V5_CHALLENGE_DOMAIN: &[u8] = b"nockmark-zk-v5";

/// Identifies the challenge→commitment rule on the wire; a client checks it
/// before grinding.
pub const ZK_V5_COMMITMENT_RULE: &str = "blake3-nockmark-zk-v5-base-p-v1";

/// The rule for nonces. Any based 5-belt nonce may win; the registry only
/// needs the `k` winners of one submission distinct, and takes them in
/// strictly ascending base-p order so that "distinct" is checkable in one pass.
pub const ZK_V5_NONCE_RULE: &str = "zk-v5-any-based-nonce-ascending-v1";

/// Tier floor, in expected Tip5 hashes per win: 2^12 is milliseconds on any
/// CPU, useful for tests, and harmless because an easy tier only lowers the
/// score (less credit against the same proving cost).
pub const ZK_ATTEMPTS_MIN: u64 = 1 << 12;

/// Tier ceiling: 2^31 expected hashes per win.
///
/// The bound is the 1 h challenge expiry. Upstream measures an RTX 5090 at
/// ≈ 18 MH/s on this digest, where 2^31 is ~2 min of grinding per win, so the
/// registry's k = 16 wins plus 16 proofs (~15 s each on a fast CPU) is ~34 min.
/// 2^32 would be ~68 min. A tier the fastest card cannot finish would mint
/// challenges nobody can submit against. (A self-calibrated 5090 asks for
/// about 2^29: twice a proof's worth of grinding.)
pub const ZK_ATTEMPTS_MAX: u64 = 1 << 31;

/// The tier a challenge gets when the request names none: 2^20, about a second
/// of CPU grinding per win. Self-calibrating clients always name one.
pub const ZK_ATTEMPTS_DEFAULT: u64 = 1 << 20;

/// How long the client's grind-rate calibration runs before choosing a tier.
pub const ZK_CALIBRATION_SECS: f64 = 2.0;

/// Little-endian base-p digits (`[u64; 5]`, least significant first) — the
/// layout of a Tip5 digest, of [`V5Job::target`], and of every belt-5 value on
/// this track's wire.
pub type Belts5 = [u64; 5];

// ---------------------------------------------------------------------------
// Challenge → commitment
// ---------------------------------------------------------------------------

/// The 32-byte challenge for a kernel-minted nonce:
/// `blake3("nockmark-zk-v5" ‖ LE8(nonce))`.
pub fn challenge32(nonce: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(ZK_V5_CHALLENGE_DOMAIN);
    h.update(&nonce.to_le_bytes());
    *h.finalize().as_bytes()
}

/// `C` reduced to 5 based belts: `C` as a little-endian 256-bit integer,
/// written in base `p`. Every limb is `< p`, so the result is a valid
/// `noun-digest:tip5` block commitment.
///
/// Limb 4 is almost always 0: `p^4 ≈ 2^256 · (1 − 2^-30)`, so only a `C` in
/// the top ~2^-30 of the range reaches it. That costs nothing. The map is
/// injective, so the commitment carries all 256 bits of `C` and is as
/// unpredictable before the mint; and a V5 hash costs the same whatever the
/// commitment's values (fixed-width field arithmetic over 10 seed belts).
pub fn commitment(challenge: &[u8; 32]) -> Belts5 {
    to_base_p(&UBig::from_le_bytes(challenge))
        .expect("2^256 < p^5: a 32-byte challenge always fits")
}

/// The commitment for a kernel-minted nonce — what the registry re-derives at
/// verify time instead of trusting anything challenge-shaped from a client.
pub fn commitment_for_nonce(nonce: u64) -> Belts5 {
    commitment(&challenge32(nonce))
}

// ---------------------------------------------------------------------------
// Base-p integers, targets, tiers
// ---------------------------------------------------------------------------

/// `p^5 − 1`: `max-tip5-atom`, the largest digest (and the numerator of
/// consensus's ZK work formula).
pub fn max_tip5_atom() -> UBig {
    let p = UBig::from(PRIME);
    &p * &p * &p * &p * &p - UBig::from(1u8)
}

/// Write `v` in base `p`, least significant limb first. `None` if `v ≥ p^5`.
pub fn to_base_p(v: &UBig) -> Option<Belts5> {
    if v > &max_tip5_atom() {
        return None;
    }
    let p = UBig::from(PRIME);
    let mut rest = v.clone();
    let mut out = [0u64; 5];
    for limb in &mut out {
        *limb = u64::try_from(&(&rest % &p)).expect("a remainder mod p fits in u64");
        rest = &rest / &p;
    }
    Some(out)
}

/// The integer a base-p limb array denotes. Limbs are taken as given; callers
/// that accept untrusted limbs check [`is_based`] first.
pub fn from_base_p(limbs: &Belts5) -> UBig {
    let p = UBig::from(PRIME);
    limbs
        .iter()
        .rev()
        .fold(UBig::from(0u8), |acc, &l| acc * &p + UBig::from(l))
}

/// Every limb a Goldilocks field element (`< p`).
pub fn is_based(limbs: &Belts5) -> bool {
    limbs.iter().all(|&l| l < PRIME)
}

/// Expected Tip5 hashes per win at target `T`: `p^5 / (T + 1)`, floored, as
/// f64. A digest is uniform over the `p^5` values `0..=p^5 − 1` and wins iff
/// `digest ≤ T`, so a hash wins with probability `(T + 1)/p^5`.
///
/// Consensus's ZK work formula is `(p^5 − 1)/(T + 1)`, one less in the
/// numerator. At every granted tier the two agree exactly (both floor to
/// `2^a`); they part only near the maximum target, where consensus's floors to
/// 0 and the true expectation is 1.
///
/// Floored in integers before the conversion on purpose: for a granted tier
/// `2^a`, [`target_for_attempts`] sets `T + 1 = ⌊(p^5 − 1)/2^a⌋`, so
/// `p^5 = 2^a·(T + 1) + s` with `s ≤ 2^a`, far below `T + 1`, and the floor is
/// exactly `2^a`. Tier → target → credit therefore round-trips exactly, as on
/// the AI track, instead of drifting in the last bits of an f64 division.
pub fn expected_attempts_per_win(target: &Belts5) -> f64 {
    let t1 = from_base_p(target) + UBig::from(1u8);
    ((max_tip5_atom() + UBig::from(1u8)) / t1).to_f64()
}

/// The tier granted for a requested expected-hashes-per-win: clamped to
/// [`ZK_ATTEMPTS_MIN`]..=[`ZK_ATTEMPTS_MAX`], rounded to the nearest power of
/// two in log space — the AI track's rule ([`crate::aipow::grant_pow2_tier`])
/// with this track's bounds.
pub fn grant_attempts(requested: u64) -> u64 {
    crate::aipow::grant_pow2_tier(requested, ZK_ATTEMPTS_MIN, ZK_ATTEMPTS_MAX)
}

/// The target realising a granted tier: `T = ⌊(p^5 − 1)/attempts⌋ − 1`, so that
/// [`expected_attempts_per_win`] gives back exactly `attempts`. `None` unless
/// `attempts` is a granted tier.
pub fn target_for_attempts(attempts: u64) -> Option<Belts5> {
    if attempts != grant_attempts(attempts) {
        return None;
    }
    let t = max_tip5_atom() / UBig::from(attempts) - UBig::from(1u8);
    to_base_p(&t)
}

/// The integer target as LSB-first u32 limbs: the `[%bn ...]` bignum the miner
/// kernel's `check-target` merges.
pub fn target_u32_limbs(target: &Belts5) -> Vec<u32> {
    let bytes = from_base_p(target).to_le_bytes();
    let mut limbs: Vec<u32> = bytes
        .chunks(4)
        .map(|c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(b)
        })
        .collect();
    if limbs.is_empty() {
        limbs.push(0);
    }
    limbs
}

/// Pick a tier for a machine that hashes at `hashes_per_sec` and proves one
/// winner in `prove_secs`: [`AI_TIER_PROVE_RATIO`] × one proof's worth of
/// grinding per win, so a run's server window recovers `R/(R+1)` of the true
/// hash rate on every host, at every k (the recovery identity documented on
/// [`AI_TIER_PROVE_RATIO`], unchanged here).
pub fn tier_for_prove_ratio(hashes_per_sec: f64, prove_secs: f64) -> u64 {
    let per_win = hashes_per_sec * AI_TIER_PROVE_RATIO * prove_secs;
    grant_attempts(if per_win.is_finite() && per_win > 0.0 {
        per_win as u64
    } else {
        0
    })
}

/// `a < b` as base-p integers (most significant limb first) — the order the
/// registry requires winners in. Defined through upstream's own
/// [`digest_le`]: `a < b ⇔ ¬(b ≤ a)`.
pub fn nonce_lt(a: &Belts5, b: &Belts5) -> bool {
    !digest_le(b, a)
}

/// Winners strictly ascending, hence pairwise distinct.
pub fn nonces_strictly_ascending(nonces: &[Belts5]) -> bool {
    nonces.windows(2).all(|w| nonce_lt(&w[0], &w[1]))
}

/// The nonce of the `i`-th attempt from zero: `i` in limb 0. This is what both
/// grinders walk (the GPU's `add_nonce` from `[0; 5]` gives the same values),
/// so a CPU and a GPU run of one challenge find the same winners.
pub fn nonce_at(i: u64) -> Belts5 {
    [i, 0, 0, 0, 0]
}

// ---------------------------------------------------------------------------
// The digest of a submitted object 0
// ---------------------------------------------------------------------------

/// `pow-v5` of a SUBMITTED object 0, computed the way consensus computes it
/// (`+proof-to-pow` for `%5`, hoon/common/ztd/four.hoon): hash the raw object
/// as `hash-proof([%5 [object-0] ~ 0])`, then domain-hash it under
/// `%zkpow-v5`.
///
/// This is deliberately NOT [`v5_pow_digest`], which recomputes the puzzle
/// product from `(commitment, nonce, len)` rather than reading the submitted
/// one. The registry computes both and requires them equal: that pins the
/// submitted object 0 to the canonical puzzle object for its own fields, so
/// the digest it ranks is the digest consensus would compute from the proof.
/// The steps after the object hash are [`v5_pow_digest`]'s own; the
/// `%zkpow-v5` tag hash is read from [`V5Job::pow_tag_hash`] rather than
/// restated.
pub fn pow_digest_of_object0(object0: &ProofData) -> Belts5 {
    let object_hash = hash_proof_data(object0);
    let mut sponge = [0u64; 16];
    absorb(&mut sponge, &object_hash);
    let pow_digest = tip5_calc_digest(&sponge);
    let tag = V5Job::new([0; 5], [0; 5], ZK_V5_POW_LEN)
        .expect("64 is a valid puzzle length")
        .pow_tag_hash;
    let mut pair = [0u64; 10];
    pair[..5].copy_from_slice(&tag);
    pair[5..].copy_from_slice(&pow_digest);
    hash_ten_cell(pair)
}

// ---------------------------------------------------------------------------
// Client: calibrate, grind, prove
// ---------------------------------------------------------------------------

/// What a grind found and what it cost.
#[derive(Debug, Clone)]
pub struct GrindResult {
    /// The winning nonces, strictly ascending; exactly `k` of them.
    pub winners: Vec<Belts5>,
    /// Digests evaluated (client-side count, display only).
    pub hashes: u64,
    pub elapsed: Duration,
}

/// CPU grind threads to use when the caller does not say: every core.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Grind on `threads` CPU threads until `k` nonces meet `target`.
///
/// Thread `t` walks attempts `t, t + threads, t + 2·threads, …` through
/// [`nonce_at`], so together they cover `0, 1, 2, …` once each. When several
/// threads win at once more than `k` winners can be found; the smallest `k` are
/// kept (any `k` distinct winners earn the same credit).
pub fn grind_cpu(commitment: Belts5, target: Belts5, k: usize, threads: usize) -> GrindResult {
    assert!(k >= 1 && threads >= 1);
    let done = Arc::new(AtomicBool::new(false));
    let hashes = Arc::new(AtomicU64::new(0));
    let winners = Arc::new(Mutex::new(Vec::<Belts5>::new()));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for tid in 0..threads as u64 {
            let (done, hashes, winners) = (done.clone(), hashes.clone(), winners.clone());
            s.spawn(move || {
                let mut i = tid;
                let mut local = 0u64;
                while !done.load(Ordering::Relaxed) {
                    let nonce = nonce_at(i);
                    let digest = v5_pow_digest(commitment, nonce, ZK_V5_POW_LEN);
                    local += 1;
                    if digest_le(&digest, &target) {
                        let mut w = winners.lock().expect("winners lock");
                        w.push(nonce);
                        if w.len() >= k {
                            done.store(true, Ordering::Relaxed);
                        }
                    }
                    i += threads as u64;
                }
                hashes.fetch_add(local, Ordering::Relaxed);
            });
        }
    });
    let elapsed = t0.elapsed();
    let mut winners = Arc::try_unwrap(winners)
        .expect("grind threads joined")
        .into_inner()
        .expect("winners lock");
    winners.sort_by(|a, b| {
        if nonce_lt(a, b) {
            std::cmp::Ordering::Less
        } else if nonce_lt(b, a) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    winners.truncate(k);
    GrindResult {
        winners,
        hashes: hashes.load(Ordering::Relaxed),
        elapsed,
    }
}

/// Measure this machine's CPU hash rate on `threads` threads for `budget`.
///
/// Run BEFORE a challenge is minted: the server window opens at mint, so a
/// calibration inside it would be charged to the rate it measures. The target
/// is zero, which only a zero digest meets (probability 1/p^5), so the loop is
/// pure hashing. The commitment is the one for nonce 0; hashing cost does not
/// depend on which commitment it is.
pub fn calibrate_cpu_hashes_per_sec(budget: Duration, threads: usize) -> f64 {
    let commitment = commitment_for_nonce(0);
    let hashes = Arc::new(AtomicU64::new(0));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for tid in 0..threads as u64 {
            let hashes = hashes.clone();
            s.spawn(move || {
                let mut i = tid;
                let mut local = 0u64;
                while t0.elapsed() < budget {
                    // Batches of 64 between clock reads: the clock is not free.
                    for _ in 0..64 {
                        let d = v5_pow_digest(commitment, nonce_at(i), ZK_V5_POW_LEN);
                        std::hint::black_box(digest_le(&d, &[0; 5]));
                        i += threads as u64;
                        local += 1;
                    }
                }
                hashes.fetch_add(local, Ordering::Relaxed);
            });
        }
    });
    hashes.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64().max(1e-9)
}

/// Prove one winner: poke the miner kernel with `[%5 commitment nonce target 64]`.
///
/// The kernel recomputes the object-0 digest and proves only if it meets
/// `target`, so an `Ok` here is a winner by the kernel's own reckoning too.
pub async fn prove_win(
    serf: &SerfThread<SaveableCheckpoint>,
    commitment: &Belts5,
    nonce: &Belts5,
    target: &Belts5,
) -> Result<ProveOutput, String> {
    miner::run_prove_at(
        serf,
        ZK_V5_PROOF_VERSION,
        commitment,
        nonce,
        &target_u32_limbs(target),
        ZK_V5_POW_LEN,
    )
    .await
}

/// Measure what ONE winner's proof costs this machine: prove a throwaway nonce
/// at the maximum target (every digest meets it) and time it. Before the mint,
/// like the grind calibration — it is the denominator of the tier.
pub async fn calibrate_prove_secs(serf: &SerfThread<SaveableCheckpoint>) -> f64 {
    let max = to_base_p(&max_tip5_atom()).expect("p^5 - 1 fits");
    let out = prove_win(serf, &commitment_for_nonce(0), &nonce_at(0), &max)
        .await
        .expect("the maximum target admits every nonce");
    out.duration.as_secs_f64()
}

// ---------------------------------------------------------------------------
// Wire helpers: belts travel as decimal strings
// ---------------------------------------------------------------------------

/// Belts as decimal strings. Every belt is a u64 up to ~2^64, which a JSON
/// number in JavaScript (and other f64 readers) would round, so the wire
/// carries them as strings, like the registry's u64 `nonce`.
pub fn belts_to_strings(b: &Belts5) -> Vec<String> {
    b.iter().map(u64::to_string).collect()
}

/// Inverse of [`belts_to_strings`]; checks there are exactly 5 decimal u64s.
/// Basedness is the caller's check ([`is_based`]).
pub fn belts_from_strings(v: &[String]) -> Result<Belts5, String> {
    if v.len() != 5 {
        return Err(format!("expected 5 belts, got {}", v.len()));
    }
    let mut out = [0u64; 5];
    for (slot, s) in out.iter_mut().zip(v) {
        *slot = s
            .parse::<u64>()
            .map_err(|e| format!("belt {s:?} is not a decimal u64: {e}"))?;
    }
    Ok(out)
}

/// serde for a `Belts5` field as 5 decimal strings (see [`belts_to_strings`]).
pub mod belts_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &super::Belts5, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(super::belts_to_strings(b))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<super::Belts5, D::Error> {
        let v = Vec::<String>::deserialize(d)?;
        super::belts_from_strings(&v).map_err(serde::de::Error::custom)
    }
}

/// serde for a `Vec<Belts5>` field, each as 5 decimal strings.
pub mod belts_vec_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[super::Belts5], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(super::belts_to_strings))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<super::Belts5>, D::Error> {
        let v = Vec::<Vec<String>>::deserialize(d)?;
        v.iter()
            .map(|b| super::belts_from_strings(b))
            .collect::<Result<_, _>>()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the challenge derivation. If this breaks, bump
    /// [`ZK_V5_CHALLENGE_DOMAIN`] and [`ZK_V5_COMMITMENT_RULE`]: a silent change
    /// would orphan every pending challenge.
    #[test]
    fn challenge_and_commitment_are_stable() {
        let mut h = blake3::Hasher::new();
        h.update(b"nockmark-zk-v5");
        h.update(&42u64.to_le_bytes());
        assert_eq!(challenge32(42), *h.finalize().as_bytes());
        assert_ne!(challenge32(0), challenge32(1));
        assert_ne!(challenge32(0), crate::aipow::dev_challenge(), "own domain");
        let c = commitment_for_nonce(42);
        assert!(is_based(&c));
        // Injective and exact: the base-p digits give back C itself.
        assert_eq!(from_base_p(&c).to_le_bytes(), {
            let mut b = challenge32(42).to_vec();
            while b.last() == Some(&0) {
                b.pop();
            }
            b
        });
        // Snapshot, so a refactor cannot move it unnoticed.
        assert_eq!(
            crate::aipow::hex32(&challenge32(0)),
            "7cf01a27552b8fdc68fef06a8c42921c16e2c428986e3200623a55d9749a6995"
        );
        assert_eq!(
            commitment_for_nonce(0),
            [
                12966553289958054017,
                7349195883257254719,
                5507600095582468858,
                10766306218379184578,
                0
            ]
        );
    }

    #[test]
    fn base_p_round_trips_and_rejects_overflow() {
        let max = max_tip5_atom();
        assert_eq!(to_base_p(&max), Some([PRIME - 1; 5]));
        assert_eq!(to_base_p(&(max.clone() + UBig::from(1u8))), None);
        assert_eq!(to_base_p(&UBig::from(0u8)), Some([0; 5]));
        let v = UBig::from(PRIME) * UBig::from(7u8) + UBig::from(3u8);
        assert_eq!(to_base_p(&v), Some([3, 7, 0, 0, 0]));
        assert_eq!(from_base_p(&[3, 7, 0, 0, 0]), v);
    }

    /// A granted tier round-trips EXACTLY through its target, at every tier.
    #[test]
    fn tiers_round_trip_exactly() {
        let mut a = ZK_ATTEMPTS_MIN;
        while a <= ZK_ATTEMPTS_MAX {
            let t = target_for_attempts(a).expect("granted tier");
            assert_eq!(
                expected_attempts_per_win(&t),
                a as f64,
                "tier 2^{}",
                a.ilog2()
            );
            a <<= 1;
        }
        assert_eq!(target_for_attempts(3 << 12), None, "not a granted tier");
        assert_eq!(grant_attempts(1), ZK_ATTEMPTS_MIN);
        assert_eq!(grant_attempts(u64::MAX), ZK_ATTEMPTS_MAX);
        assert_eq!(
            grant_attempts(3 << 20),
            1 << 22,
            "nearest in log space rounds 1.5x up"
        );
        // The maximum target admits every digest: one expected hash per win.
        assert_eq!(expected_attempts_per_win(&[PRIME - 1; 5]), 1.0);
    }

    /// The bignum the kernel gets is the same integer as the base-p target.
    #[test]
    fn target_limbs_are_the_same_integer() {
        let t = target_for_attempts(1 << 20).unwrap();
        let limbs = target_u32_limbs(&t);
        let mut bytes = Vec::new();
        for l in &limbs {
            bytes.extend_from_slice(&l.to_le_bytes());
        }
        assert_eq!(UBig::from_le_bytes(&bytes), from_base_p(&t));
        assert_eq!(target_u32_limbs(&[0; 5]), vec![0]);
    }

    #[test]
    fn tier_sizing_is_rate_times_ratio_times_prove() {
        // 1e6 H/s, 20 s per proof, R = 2: 4e7 per win, nearest power of two 2^25.
        assert_eq!(tier_for_prove_ratio(1e6, 20.0), 1 << 25);
        assert_eq!(tier_for_prove_ratio(f64::NAN, 20.0), ZK_ATTEMPTS_MIN);
        assert_eq!(tier_for_prove_ratio(1e12, 1e3), ZK_ATTEMPTS_MAX);
    }

    #[test]
    fn nonce_order_is_base_p_most_significant_first() {
        assert!(nonce_lt(&[5, 0, 0, 0, 0], &[6, 0, 0, 0, 0]));
        assert!(nonce_lt(&[PRIME - 1, 0, 0, 0, 0], &[0, 1, 0, 0, 0]));
        assert!(!nonce_lt(&[1, 0, 0, 0, 0], &[1, 0, 0, 0, 0]));
        assert!(nonces_strictly_ascending(&[
            [1, 0, 0, 0, 0],
            [2, 0, 0, 0, 0]
        ]));
        assert!(
            !nonces_strictly_ascending(&[[2, 0, 0, 0, 0], [2, 0, 0, 0, 0]]),
            "duplicate"
        );
        assert!(
            !nonces_strictly_ascending(&[[3, 0, 0, 0, 0], [2, 0, 0, 0, 0]]),
            "descending"
        );
    }

    /// The CPU grinder finds exactly k ascending winners, each of which meets
    /// the target under upstream's own digest and comparison.
    #[test]
    fn grind_finds_k_ascending_winners() {
        let c = commitment_for_nonce(7);
        let t = target_for_attempts(ZK_ATTEMPTS_MIN).unwrap();
        let g = grind_cpu(c, t, 3, 4);
        assert_eq!(g.winners.len(), 3);
        assert!(nonces_strictly_ascending(&g.winners));
        for w in &g.winners {
            assert!(digest_le(&v5_pow_digest(c, *w, ZK_V5_POW_LEN), &t));
        }
        assert!(g.hashes >= 3);
    }

    #[test]
    fn belts_wire_format_is_decimal_strings() {
        let b = [PRIME - 1, 0, 1, 2, 3];
        let s = belts_to_strings(&b);
        assert_eq!(s[0], (PRIME - 1).to_string());
        assert_eq!(belts_from_strings(&s), Ok(b));
        assert!(belts_from_strings(&s[..4]).is_err());
        assert!(
            belts_from_strings(&["x".into(), "0".into(), "0".into(), "0".into(), "0".into()])
                .is_err()
        );
    }
}
