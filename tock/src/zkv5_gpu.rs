//! CUDA grind backend for the ZK v5 track (`zk-bench --gpu`, feature
//! `zk-gpu`): upstream's open-source V5 search (`zk_pow_miner::cuda`, #192).
//!
//! Only the SEARCH moves to the device. Every winner it reports is re-checked
//! against the native CPU oracle ([`v5_pow_digest`] + [`digest_le`]) before it
//! is kept, and is proved on the CPU by the miner kernel like any other win.
//! The registry then checks each one from scratch, so a GPU row is held to
//! exactly the standard of a CPU row.
//!
//! The search walks the same nonces as the CPU grinder: `add_nonce([0; 5], i)`
//! is [`crate::zkv5::nonce_at`]`(i)` for every `i < p`.

use std::time::{Duration, Instant};

use zk_pow_miner::cuda::CudaSession;
use zk_pow_miner::v5::{add_nonce, digest_le, v5_pow_digest, V5Job};

use crate::zkv5::{commitment_for_nonce, nonce_at, Belts5, GrindResult, ZK_V5_POW_LEN};

/// The device every search runs on.
pub const ZK_GPU_DEVICE: u32 = 0;

/// Attempts per launch: upstream's measured RTX 4090/5090 default
/// (`--cuda-batch-size 262144`, zk-pow-miner README).
pub const DEFAULT_ZK_GPU_BATCH: u64 = 262_144;

/// The device's descriptor for a GPU row.
pub fn device_descriptor() -> Option<String> {
    crate::hardware::nvidia_smi_device(ZK_GPU_DEVICE)
}

fn session() -> CudaSession {
    CudaSession::new(ZK_GPU_DEVICE)
        .unwrap_or_else(|e| panic!("CUDA session on device {ZK_GPU_DEVICE}: {e}"))
}

/// The device's digest must equal the CPU oracle's before any of its output is
/// trusted — upstream's own bench does the same before timing.
fn assert_device_matches_oracle(s: &mut CudaSession, job: &V5Job) {
    for i in [0u64, 1, 2, 1_000_003] {
        let n = nonce_at(i);
        let gpu = s.digest(job, n).expect("device digest");
        assert_eq!(
            gpu,
            v5_pow_digest(job.commitment, n, ZK_V5_POW_LEN),
            "CUDA V5 digest differs from the CPU oracle at nonce {i}"
        );
    }
}

/// Measure the device's hash rate for `budget` at `batch` attempts per launch,
/// before any challenge is minted. Target zero: nothing wins, all hashing.
pub fn calibrate_hashes_per_sec(budget: Duration, batch: u64) -> f64 {
    let mut s = session();
    let job = V5Job::new(commitment_for_nonce(0), [0; 5], ZK_V5_POW_LEN).expect("job");
    assert_device_matches_oracle(&mut s, &job);
    let mut start = [0u64; 5];
    let mut hashes = 0u64;
    let t0 = Instant::now();
    while t0.elapsed() < budget {
        let r = s.search(&job, start, batch).expect("CUDA search");
        hashes += r.attempts;
        start = r.next_nonce;
    }
    hashes as f64 / t0.elapsed().as_secs_f64().max(1e-9)
}

/// Grind on the device until `k` nonces meet `target`.
///
/// A launch evaluates its whole batch and reports the LOWEST winning index,
/// so the next launch starts one past that winner: no winner is skipped, and
/// the batch tail after a win is evaluated again (counted once below).
pub fn grind(commitment: Belts5, target: Belts5, k: usize, batch: u64) -> GrindResult {
    let mut s = session();
    let job = V5Job::new(commitment, target, ZK_V5_POW_LEN).expect("job");
    assert_device_matches_oracle(&mut s, &job);
    let mut start = [0u64; 5];
    let mut winners = Vec::with_capacity(k);
    let mut hashes = 0u64;
    let t0 = Instant::now();
    while winners.len() < k {
        let r = s.search(&job, start, batch).expect("CUDA search");
        if r.found {
            let w = r.next_nonce;
            assert!(
                digest_le(&v5_pow_digest(commitment, w, ZK_V5_POW_LEN), &target),
                "the device reported nonce {w:?} as a winner but the CPU oracle disagrees"
            );
            // Nonces stay in limb 0 (i < p), so the attempts this launch spent
            // up to and including the winner are a limb-0 difference.
            hashes += w[0] - start[0] + 1;
            winners.push(w);
            start = add_nonce(w, 1);
        } else {
            hashes += r.attempts;
            start = r.next_nonce;
        }
    }
    GrindResult {
        winners,
        hashes,
        elapsed: t0.elapsed(),
    }
}
