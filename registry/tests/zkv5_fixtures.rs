//! Regenerates `tests/fixtures/zkv5/`: real `%5` (and one `%3`) proofs for the
//! ZK v5 verification tests in `tests/zkv5.rs`. Ignored by default — it needs
//! a compiled `tock/assets/miner.jam` at the pinned rev and ~3 min of proving.
//!
//! ```text
//! cd registry && cargo test --test zkv5_fixtures -- --ignored --nocapture
//! ```
//!
//! Every fixture is made through tock's own client path (`zkv5::grind_cpu`,
//! `zkv5::prove_win`, `miner::run_prove_at`), so regenerating them also
//! exercises the client against the kernel.

use std::path::{Path, PathBuf};

use nockapp::utils::NOCK_STACK_SIZE_TINY;
use tock::miner;
use tock::zkv5::{self, Belts5};
use zk_pow_miner::v5::{digest_le, v5_pow_digest};

/// The kernel nonces the fixtures' challenges are derived from. Tests seed
/// these into the store as pending challenges, which is how a fixture proof
/// can be submitted to a registry that never minted it.
pub const FIXTURE_NONCE_A: u64 = 4242;
pub const FIXTURE_NONCE_B: u64 = 4243;
/// Tier of the fixture target: the easiest granted tier.
pub const FIXTURE_ATTEMPTS: u64 = zkv5::ZK_ATTEMPTS_MIN;

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zkv5")
}

fn max_target() -> Belts5 {
    zkv5::to_base_p(&zkv5::max_tip5_atom()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn regenerate_zkv5_fixtures() {
    let jam = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tock/assets/miner.jam");
    let kernel = std::fs::read(&jam).unwrap_or_else(|e| panic!("{}: {e}", jam.display()));
    let serf = miner::boot_kernel(kernel, NOCK_STACK_SIZE_TINY).await;
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();

    let ca = zkv5::commitment_for_nonce(FIXTURE_NONCE_A);
    let cb = zkv5::commitment_for_nonce(FIXTURE_NONCE_B);
    let t = zkv5::target_for_attempts(FIXTURE_ATTEMPTS).unwrap();

    // Three winners of challenge A at the fixture tier.
    let g = zkv5::grind_cpu(ca, t, 3, 4);
    eprintln!(
        "grind A: {} hashes in {:?} for {:?}",
        g.hashes, g.elapsed, g.winners
    );
    let mut prove_secs = Vec::new();
    for (i, w) in g.winners.iter().enumerate() {
        let out = zkv5::prove_win(&serf, &ca, w, &t)
            .await
            .expect("winner proves");
        prove_secs.push(out.duration.as_secs_f64());
        std::fs::write(dir.join(format!("win-{i}.jam")), &out.proof_jam).unwrap();
    }

    // A %3 proof of winner 0: same object 0, version 3.
    let v3 = miner::run_prove_at(
        &serf,
        3,
        &ca,
        &g.winners[0],
        &zkv5::target_u32_limbs(&max_target()),
        64,
    )
    .await
    .expect("v3 at the max target");
    std::fs::write(dir.join("v3-win-0.jam"), &v3.proof_jam).unwrap();

    // A winner of challenge B: a valid win, of another challenge.
    let gb = zkv5::grind_cpu(cb, t, 1, 4);
    let other = zkv5::prove_win(&serf, &cb, &gb.winners[0], &t)
        .await
        .unwrap();
    std::fs::write(dir.join("other-challenge.jam"), &other.proof_jam).unwrap();

    // A len-32 proof of challenge A whose len-32 digest meets the target.
    let m = (0u64..)
        .map(zkv5::nonce_at)
        .find(|n| digest_le(&v5_pow_digest(ca, *n, 32), &t))
        .unwrap();
    let len32 = miner::run_prove_at(&serf, 5, &ca, &m, &zkv5::target_u32_limbs(&t), 32)
        .await
        .expect("len-32 winner proves");
    std::fs::write(dir.join("len32.jam"), &len32.proof_jam).unwrap();

    // A loser: a valid %5 proof of challenge A whose digest misses the target
    // (proved at the maximum target, which admits every nonce).
    let l = (0u64..)
        .map(zkv5::nonce_at)
        .find(|n| !digest_le(&v5_pow_digest(ca, *n, 64), &t))
        .unwrap();
    assert!(
        zkv5::prove_win(&serf, &ca, &l, &t).await.is_err(),
        "the kernel refuses to prove a loser at the real target"
    );
    let loser = zkv5::prove_win(&serf, &ca, &l, &max_target())
        .await
        .unwrap();
    std::fs::write(dir.join("loser.jam"), &loser.proof_jam).unwrap();

    let manifest = serde_json::json!({
        "pin": miner::NOCKCHAIN_PIN,
        "nonce_a": FIXTURE_NONCE_A,
        "nonce_b": FIXTURE_NONCE_B,
        "attempts": FIXTURE_ATTEMPTS,
        "target": zkv5::belts_to_strings(&t),
        "winners_a": g.winners.iter().map(zkv5::belts_to_strings).collect::<Vec<_>>(),
        "winner_b": zkv5::belts_to_strings(&gb.winners[0]),
        "len32_nonce": zkv5::belts_to_strings(&m),
        "loser_nonce": zkv5::belts_to_strings(&l),
        "prove_secs": prove_secs,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    eprintln!("{manifest:#}");
}
