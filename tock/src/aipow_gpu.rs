//! AI-PoW track, canonical-MoE statement on **CUDA** (M6 Phase B2b): the GPU
//! grind loop.
//!
//! ## What moves to the GPU, and what emphatically does not
//!
//! Exactly one thing moves: **jackpot search**. Upstream's
//! [`GpuSearchBackend`] takes a [`PreparedCanonicalMoeTemplate`] and a
//! [`SearchBatch`] of contiguous ordinals and returns at most the LOWEST
//! ordinal in that batch whose jackpot clears the threshold. Everything that
//! decides whether a win is real stays where [`crate::aipow_moe`] put it:
//!
//! | stage | CPU path | GPU path |
//! |---|---|---|
//! | attempt evaluation | `template.evaluate` per ordinal | one CUDA launch per batch |
//! | jackpot ≤ Θ | `hash_le_target` in Rust | in-kernel, then re-checked in Rust |
//! | scalar recheck | [`evaluate_canonical_moe_jackpot`] | [`evaluate_canonical_moe_jackpot`], unchanged |
//! | certificate | [`prove_canonical_moe_block_at`] on CPU | [`prove_canonical_moe_block_at`] on CPU |
//!
//! So the trust story is byte-for-byte the one B1 shipped. The GPU is a
//! *filter*: it proposes ordinals, and a winner only reaches the prover after
//! the standalone scalar oracle — the same function the registry's verifier
//! re-derives from `(challenge, ordinal)` — agrees with it hash-for-hash.
//! There is in fact a second, independent recheck we get for free: upstream's
//! `GpuSearchBackend::search_canonical` already re-evaluates its own winner
//! through `template.evaluate` and refuses to return one that disagrees with
//! the device (`gpu.rs:391`). We do NOT lean on it — a backend cannot be its
//! own auditor — but it means a bad kernel has to fool two different scalar
//! implementations before it can even reach [`grind_moe_gpu`]'s assert.
//!
//! ## Window semantics, mirrored precisely
//!
//! Identical to [`crate::aipow_moe`], because the board compares these runs:
//!
//!   * **Calibration is outside everything.** [`calibrate_moe_attempts_per_sec`]
//!     runs before the challenge is minted, and — unlike the CPU peer, which
//!     has nothing to warm — it discards a warm-up dispatch first, because the
//!     first launch pays CUDA context creation and the one-off upload of the
//!     template's fixed inputs. Timing that would under-report the GPU by a
//!     wide margin and hand it a tier far below what it can grind.
//!   * **The grind window excludes proving.** It opens after the template, the
//!     backend AND the CUDA session exist (the CPU path hoists `template.
//!     scratch()` out of its loop for the same reason) and closes when the
//!     k-th win is found. Certificates are proved afterwards, on the CPU,
//!     through the shared [`prove_moe_wins`].
//!   * **Attempts are counted through the winner, not through the batch.** The
//!     device evaluates every ordinal in a launch, including ones past the
//!     winner; upstream's [`OrderedBatchScheduler`] — which this loop drives
//!     rather than reimplements — reports `attempts_tried` only up to the
//!     lowest returned winner, exactly as the CPU loop stops at the k-th win
//!     with `attempts = ordinal + 1`. That undercounts the final partial batch
//!     by construction, which is the direction we want: never claim faster
//!     than measured.
//!
//! ## Tiers are the point
//!
//! `ai-bench --gpu` self-calibrates through the same `resolve_tier` path as
//! every other run, just with [`calibrate_moe_attempts_per_sec`] supplying the
//! rate. That is the entire reason M6 shipped tiers before it shipped CUDA: at
//! the CPU-calibrated `T_b = 2^224` a GPU finishes the grind in milliseconds
//! and the run degenerates into a measurement of `prove_canonical_moe_block_at`,
//! which is not the thing the AI board ranks. Let the machine measure itself
//! and the tier moves with it.
//!
//! Observed on the 3090 (see [`MEASURED_3090_ATTEMPTS_PER_SEC`]), at `k = 4`:
//! the calibration measures ~926 k attempts/sec, `tier_for_rate` asks for
//! `2^24`, and the grind then takes ~35 s against ~42 s of CPU proving — a
//! window the throughput term actually shows up in. The same machine at the
//! CPU-calibrated `2^16` tier would have ground for 0.07 s.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_pow_miner::canonical::{evaluate_canonical_moe_jackpot, PreparedCanonicalMoeTemplate};
use ai_pow_miner::gpu::GpuSearchBackend;
use ai_pow_miner::search::{OrderedBatchScheduler, SearchBackend, SearchBatch};

use crate::aipow_moe::{
    dev_challenge, moe_threshold, prove_moe_wins, AiMoeBenchSummary, AiMoeChallenge,
    MoeGrindResult, AI_MOE_E, AI_MOE_HW, AI_MOE_PARAMS, AI_MOE_TOP_K,
};

// ---------------------------------------------------------------------------
// Device and launch geometry
// ---------------------------------------------------------------------------

/// The CUDA device this backend runs on. A constant rather than a flag because
/// upstream rejects everything else outright — `GpuSearchBackend::new` bails
/// with "the GPU backend currently supports CUDA device 0 only" (`gpu.rs:267`)
/// — so a `--gpu-device` flag could only ever produce an error message we would
/// have had to write ourselves.
pub const GPU_DEVICE_ORDINAL: usize = 0;

/// Default attempts per CUDA launch (`ai-bench --gpu-batch`).
///
/// `32_768` is what upstream's production miner image ships as
/// `GPU_BATCH_ATTEMPTS` (`docker/Dockerfile.ai-pow-miner-gpu`), so it is the
/// geometry the kernel was tuned against rather than a number we picked. It is
/// also 128× upstream's [`ai_pow_miner::search::DEFAULT_SEARCH_BATCH_ATTEMPTS`]
/// of 256, which exists to bound *cancellation latency* for a node miner that
/// must abandon a batch when the chain tip moves. A benchmark has no tip to
/// chase, so the only thing a small batch buys here is launch overhead per
/// attempt.
///
/// The session allocates for `batch_attempts` up front, so the ceiling is
/// device memory; the floor is the point where kernel launch latency dominates.
pub const DEFAULT_GPU_BATCH_ATTEMPTS: u64 = 32_768;

/// Measured canonical-MoE grind rate on an **NVIDIA RTX 3090** — Ampere,
/// `AI_POW_CUDA_ARCH=compute_86`, `AI_POW_CUDA_CODE=sm_86`, CUDA 12.8, at
/// [`DEFAULT_GPU_BATCH_ATTEMPTS`]: **929 000 attempts/sec**, i.e. 60.9
/// GMAC-equivalents/sec at `F = 2^16`.
///
/// The peer of [`crate::aipow_moe::MEASURED_ATTEMPTS_PER_SEC`] and directly
/// comparable to it, because both count the same attempt on the same
/// statement: that constant is **3 394 attempts/sec**, so this is a **274×**
/// ratio. Read it as "one 3090 versus one CPU core", not "versus one CPU" —
/// [`crate::aipow_moe::grind_moe`] is a single-threaded loop, deliberately, so
/// that the number it reports is a property of the statement rather than of
/// how many cores happened to be idle.
///
/// Recorded here rather than in a results file for the reason
/// [`crate::aipow_moe::calibrated_moe_target`] gives: a measurement that is
/// going to be re-run belongs next to the code that re-runs it. Unlike the CPU
/// constant, nothing is derived FROM this one — the GPU never grinds a
/// hardcoded target, it calibrates itself ([`calibrate_moe_attempts_per_sec`])
/// and lets `resolve_tier` pick. It is here to make a regression visible.
pub const MEASURED_3090_ATTEMPTS_PER_SEC: f64 = 929_000.0;

/// The canonical ordinal space. [`crate::aipow_moe::AI_MOE_NONCE_RULE`] numbers
/// attempts with a **u32** ordinal, so the search runs over `[0, 2^32)` and not
/// one ordinal further; upstream enforces the same bound at the ABI edge
/// (`SearchBackendError::CanonicalOrdinalOutOfRange`). Handing the scheduler
/// this end lets it, rather than an ad-hoc `checked_add`, be the thing that
/// reports exhaustion.
const CANONICAL_ORDINAL_SPACE: u64 = 1 << 32;

/// A threshold nothing can clear: a win requires `jackpot ≤ 0`, i.e. a 32-byte
/// zero hash, at probability `2^-256`.
///
/// Used for the warm-up and the calibration dispatches, and it is the exact
/// peer of the CPU calibration's decision to make **no jackpot comparison at
/// all**: a throwaway workload must not be able to manufacture a win that
/// reaches the prover. The device compares in-kernel whatever we pass, so
/// "compare against nothing" is not available here — "compare against a bound
/// nothing meets" is. (Upstream pins the same behaviour in
/// `canonical_search_obeys_targets_and_session_lifetime`, which asserts a
/// `[0; 32]` batch returns `None`.)
const UNREACHABLE_THRESHOLD: [u8; 32] = [0u8; 32];

// ---------------------------------------------------------------------------
// Backend construction
// ---------------------------------------------------------------------------

/// Build the CUDA backend and the (shared, immutable) canonical template, and
/// force the CUDA session into existence with one throwaway dispatch.
///
/// The warm-up matters for both callers and for different reasons. For
/// [`calibrate_moe_attempts_per_sec`] it keeps context creation out of the
/// measured rate. For [`grind_moe_gpu`] it keeps context creation out of the
/// **grind window** — the CPU path builds its template and hoists its scratch
/// buffer before `t0`, and upstream's backend defers session creation to the
/// first `search_canonical` call (`gpu.rs:366`), so without this the window
/// would open with a one-off setup cost charged to throughput.
///
/// The template is returned as the same `Arc` the caller must keep using:
/// the backend caches its session against template identity via `Arc::ptr_eq`,
/// so cloning the `Arc` reuses the session and rebuilding the template would
/// silently tear it down and re-upload every fixed input.
fn warm_backend(
    challenge: [u8; 32],
    batch_attempts: u64,
) -> (GpuSearchBackend, Arc<PreparedCanonicalMoeTemplate>) {
    let backend = GpuSearchBackend::new(GPU_DEVICE_ORDINAL, batch_attempts).unwrap_or_else(|e| {
        panic!("CUDA backend on device {GPU_DEVICE_ORDINAL} (batch {batch_attempts}): {e}")
    });
    let template = Arc::new(
        PreparedCanonicalMoeTemplate::new(
            &AI_MOE_PARAMS,
            AI_MOE_HW,
            AI_MOE_E,
            AI_MOE_TOP_K,
            challenge,
        )
        .expect("PreparedCanonicalMoeTemplate::new"),
    );
    // One ordinal at an unreachable threshold: allocates the session for the
    // full `batch_attempts` (upstream sizes it from the backend, not from this
    // batch) and cannot produce a winner.
    let warmup = SearchBatch::new(0, 1, UNREACHABLE_THRESHOLD).expect("one-attempt warm-up batch");
    let none = backend
        .search_canonical(Arc::clone(&template), warmup)
        .expect("CUDA session creation");
    assert!(none.is_none(), "the zero threshold must be unwinnable");
    (backend, template)
}

// ---------------------------------------------------------------------------
// Grind
// ---------------------------------------------------------------------------

/// Grind ordinals 0, 1, 2, … on the GPU until `k` jackpot wins, in batches of
/// `batch_attempts`.
///
/// The loop is upstream's [`OrderedBatchScheduler`] driven directly rather than
/// re-expressed: it emits the next `SearchBatch`, and `record_miss` /
/// `record_winner` advance it — to the batch end on a miss, to **winner + 1**
/// on a hit, so the ordinals past a winner inside its batch are re-searched
/// rather than skipped. That is what keeps the k-th win here the same win the
/// CPU loop would have found: both walk the ordinal space in strictly
/// ascending order and neither can hop over a winning ordinal.
///
/// Every winner is re-checked against [`evaluate_canonical_moe_jackpot`] — the
/// standalone scalar oracle, byte-identical to the CPU path's guard — before it
/// is allowed to reach the prover. A GPU that reports a jackpot the CPU
/// disagrees with aborts the run; it does not submit.
pub fn grind_moe_gpu(ch: &AiMoeChallenge, batch_attempts: u64) -> MoeGrindResult {
    assert!(ch.k >= 1, "k must be at least 1");
    let threshold = moe_threshold(&ch.target).expect("canonical grind threshold");
    let (backend, template) = warm_backend(ch.challenge, batch_attempts);
    let mut scheduler =
        OrderedBatchScheduler::new(0, CANONICAL_ORDINAL_SPACE, None, batch_attempts)
            .expect("nonzero batch over the canonical ordinal space");

    let t0 = Instant::now();
    let mut win_ordinals: Vec<u32> = Vec::with_capacity(ch.k as usize);
    while (win_ordinals.len() as u64) < ch.k {
        let batch = scheduler.next_batch(threshold).unwrap_or_else(|end| {
            panic!(
                "canonical ordinal space (u32) exhausted after {} attempts ({end:?}): \
                 the target is too hard for this shape",
                scheduler.attempts_tried()
            )
        });
        match backend
            .search_canonical(Arc::clone(&template), batch)
            .expect("CUDA canonical search")
        {
            Some(winner) => {
                scheduler
                    .record_winner(batch, winner)
                    .expect("backend winner lies inside the batch it was asked for");
                let ordinal = u32::try_from(winner.ordinal)
                    .expect("canonical ordinals are u32 by the scheduler's end bound");
                // Scalar-oracle recheck: the device's transcript must agree
                // with the standalone evaluator the prover will certify.
                let oracle = evaluate_canonical_moe_jackpot(
                    &AI_MOE_PARAMS,
                    AI_MOE_HW,
                    AI_MOE_E,
                    AI_MOE_TOP_K,
                    ch.challenge,
                    ordinal,
                )
                .expect("canonical scalar jackpot");
                assert_eq!(
                    oracle, winner.jackpot_hash,
                    "GPU jackpot disagrees with the canonical scalar oracle at ordinal {ordinal}"
                );
                win_ordinals.push(ordinal);
                eprintln!(
                    "  win {}/{} at ordinal {ordinal} ({:.1}s)",
                    win_ordinals.len(),
                    ch.k,
                    t0.elapsed().as_secs_f64()
                );
            }
            None => scheduler
                .record_miss(batch)
                .expect("a missed batch is the one the scheduler emitted"),
        }
    }
    let elapsed_s = t0.elapsed().as_secs_f64();
    // Through the k-th winner, not through its batch — see the module docs.
    let attempts = scheduler.attempts_tried();
    MoeGrindResult {
        win_ordinals,
        // Round UP like the CPU path: never claim faster than measured.
        elapsed_ms: (elapsed_s * 1000.0).ceil().max(1.0) as u64,
        total_attempts: attempts,
        attempts_per_sec: attempts as f64 / elapsed_s.max(1e-9),
    }
}

/// Measure this GPU's canonical-MoE attempt rate: dispatch full batches at an
/// unreachable threshold for `budget` and count the attempts the device
/// actually evaluated.
///
/// The peer of [`crate::aipow_moe::calibrate_moe_attempts_per_sec`], subject to
/// the same rule — run it BEFORE minting a challenge, because the server window
/// opens at mint — with two differences that follow from the hardware:
///
///   * the CUDA context and the template upload are warmed away first
///     ([`warm_backend`]), so the rate is steady-state throughput and not an
///     average dragged down by a one-off setup;
///   * every attempt in a batch is counted, because at
///     [`UNREACHABLE_THRESHOLD`] there is no winner to truncate a batch at.
///     This is the one place a full-batch count is the honest one, and it is
///     why the tier this feeds is not systematically low.
pub fn calibrate_moe_attempts_per_sec(budget: Duration, batch_attempts: u64) -> f64 {
    let (backend, template) = warm_backend(dev_challenge(), batch_attempts);
    let t0 = Instant::now();
    let mut attempts: u64 = 0;
    let mut start: u64 = 0;
    while t0.elapsed() < budget {
        // Restart at 0 rather than run off the end of the u32 ordinal space: a
        // calibration that wrapped would simply measure the same attempts
        // again, which is still an honest rate (the CPU peer wraps too).
        if start + batch_attempts > CANONICAL_ORDINAL_SPACE {
            start = 0;
        }
        let batch = SearchBatch::new(start, batch_attempts, UNREACHABLE_THRESHOLD)
            .expect("calibration batch");
        let none = backend
            .search_canonical(Arc::clone(&template), batch)
            .expect("CUDA canonical search");
        assert!(none.is_none(), "the zero threshold must be unwinnable");
        attempts += batch_attempts;
        start += batch_attempts;
    }
    attempts as f64 / t0.elapsed().as_secs_f64().max(1e-9)
}

/// The full canonical-MoE workload with the GPU grinding: grind to k wins on
/// the device, then certify each on the CPU.
///
/// The peer of [`crate::aipow_moe::run`], and deliberately sharing its second
/// half verbatim — [`prove_moe_wins`] is called unchanged, so a GPU submission
/// and a CPU submission carry byte-identical certificates for the same
/// `(challenge, ordinal)`. The GPU never touches proving.
pub fn run(ch: &AiMoeChallenge, batch_attempts: u64) -> AiMoeBenchSummary {
    eprintln!(
        "grinding to {} canonical-MoE win(s) on CUDA device {GPU_DEVICE_ORDINAL} \
         ({batch_attempts} attempts/launch)…",
        ch.k
    );
    let grind = grind_moe_gpu(ch, batch_attempts);
    eprintln!(
        "grind window closed: {} attempts in {:.1}s",
        grind.total_attempts,
        grind.elapsed_ms as f64 / 1000.0
    );
    eprintln!(
        "proving {} certificate(s) on the CPU…",
        grind.win_ordinals.len()
    );
    let wins = prove_moe_wins(ch, &grind.win_ordinals);
    AiMoeBenchSummary {
        wins,
        grind_elapsed_ms: grind.elapsed_ms,
        total_attempts: grind.total_attempts,
        attempts_per_sec: grind.attempts_per_sec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::aipow::{grant_attempts, target_for_attempts, AI_ATTEMPTS_MAX};
    use crate::aipow_moe::{expected_attempts_per_moe_win, max_moe_target};

    /// **The scheduler must not be able to skip a winning ordinal.**
    ///
    /// Pure scheduler arithmetic — no device — because this is the property
    /// that makes a GPU run and a CPU run the *same* benchmark: batching is an
    /// implementation detail of how ordinals are visited, never of which ones
    /// are. A miss advances to the batch end; a win advances to winner + 1, so
    /// the ordinals between a winner and its batch end are re-emitted rather
    /// than skipped.
    #[test]
    fn batches_cover_the_ordinal_space_without_gaps() {
        use ai_pow_miner::search::SearchWinner;

        let mut scheduler = OrderedBatchScheduler::new(0, CANONICAL_ORDINAL_SPACE, None, 8)
            .expect("scheduler");
        let first = scheduler.next_batch([0x11; 32]).expect("first batch");
        assert_eq!((first.start, first.len), (0, 8));
        scheduler.record_miss(first).expect("miss");
        assert_eq!(scheduler.attempts_tried(), 8);

        let second = scheduler.next_batch([0x11; 32]).expect("second batch");
        assert_eq!(second.start, 8, "a miss advances to the batch end");
        // A winner at 10 leaves 11..16 unvisited — the next batch must start
        // at 11, not at 16.
        let attempts = scheduler
            .record_winner(
                second,
                SearchWinner {
                    ordinal: 10,
                    jackpot_hash: [0; 32],
                },
            )
            .expect("winner inside its batch");
        assert_eq!(attempts, 11, "attempts count through the winner, not the batch");
        assert_eq!(
            scheduler.next_batch([0x11; 32]).expect("third batch").start,
            11
        );
    }

    /// The grind never runs off the end of the u32 ordinal space, at any tier
    /// the registry can grant. `AI_ATTEMPTS_MAX = 2^30` expected attempts per
    /// win times the largest k a submission carries must stay inside
    /// [`CANONICAL_ORDINAL_SPACE`] — the second, statement-specific bound the
    /// tier ceiling's doc comment calls out. Pure arithmetic; no device.
    #[test]
    fn the_hardest_tier_still_fits_the_u32_ordinal_space() {
        assert_eq!(CANONICAL_ORDINAL_SPACE, 1u64 << 32);
        assert_eq!(grant_attempts(AI_ATTEMPTS_MAX), AI_ATTEMPTS_MAX);
        // k = 4 wins at the ceiling is exactly the space; anything less fits.
        assert!(AI_ATTEMPTS_MAX * 4 <= CANONICAL_ORDINAL_SPACE);
        let hardest = target_for_attempts(AI_ATTEMPTS_MAX, |t| {
            expected_attempts_per_moe_win(t).unwrap_or(1.0)
        })
        .expect("the ceiling tier has a canonical-MoE target");
        assert_eq!(
            expected_attempts_per_moe_win(&hardest).unwrap(),
            AI_ATTEMPTS_MAX as f64
        );
        // …and the threshold it scales to is representable, i.e. the grind
        // would actually start rather than fail closed.
        assert!(moe_threshold(&hardest).is_ok());
    }

    /// A generous target wins on the first ordinals of the first launch, the
    /// winners survive the scalar-oracle recheck, and the ordinals match what
    /// the CPU path reports for the same challenge. Requires a CUDA device (as
    /// does compiling this module at all). No proving.
    #[test]
    fn grind_at_max_target_wins_immediately() {
        let ch = AiMoeChallenge {
            challenge: dev_challenge(),
            target: max_moe_target(),
            k: 2,
        };
        let out = grind_moe_gpu(&ch, 64);
        assert_eq!(out.win_ordinals, vec![0, 1]);
        assert_eq!(out.total_attempts, 2);
        assert!(out.attempts_per_sec > 0.0);
    }

    /// The GPU and CPU grind loops must agree on which ordinals win at a
    /// non-trivial target — the whole benchmark rests on the two backends
    /// searching the same statement. ~2^12 attempts, so seconds on the CPU.
    #[test]
    fn gpu_and_cpu_find_the_same_winning_ordinals() {
        let target = target_for_attempts(1 << 12, |t| {
            expected_attempts_per_moe_win(t).unwrap_or(1.0)
        })
        .expect("floor tier target");
        let ch = AiMoeChallenge {
            challenge: dev_challenge(),
            target,
            k: 2,
        };
        let gpu = grind_moe_gpu(&ch, 512);
        let cpu = crate::aipow_moe::grind_moe(&ch);
        assert_eq!(gpu.win_ordinals, cpu.win_ordinals);
        assert_eq!(gpu.total_attempts, cpu.total_attempts);
    }

    /// The calibration reports a positive rate and, being run at the
    /// unreachable threshold, cannot produce a win. Short budget — this is a
    /// unit test, not the measurement.
    #[test]
    fn calibration_measures_a_positive_rate() {
        let rate = calibrate_moe_attempts_per_sec(Duration::from_millis(500), 1024);
        assert!(rate > 0.0 && rate.is_finite(), "rate {rate}");
    }
}
