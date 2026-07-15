use std::path::Path;

use nockmark_registry::kernel::RegistryKernel;

fn jam() -> &'static Path {
    let p = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tock/assets/registry.jam"
    ));
    assert!(
        p.exists(),
        "registry.jam missing — run scripts/build-registry-jam.sh first"
    );
    p
}

#[tokio::test]
async fn mint_and_submit_and_leaderboard() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = RegistryKernel::boot(jam(), dir.path()).await.unwrap();

    let nonce = k.mint_challenge().await.unwrap();
    assert!(nonce > 0);

    // happy path
    let id = k
        .submit_run(nonce, "test-hw", "31b8a015", 2, 42_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, 0);

    // replay of the same nonce must be rejected
    let rej = k
        .submit_run(nonce, "test-hw", "31b8a015", 2, 42_000)
        .await
        .unwrap();
    assert_eq!(rej.unwrap_err(), "nonce-used");

    // unknown nonce must be rejected
    let rej = k.submit_run(0xdead_beef, "x", "y", 1, 1).await.unwrap();
    assert_eq!(rej.unwrap_err(), "unknown-nonce");

    let board = k.leaderboard().await.unwrap();
    assert_eq!(board.len(), 1);
    assert_eq!(board[0].hardware, "test-hw");
    assert_eq!(board[0].k, 2);
}
