use nockmark_registry::verifier::Verifier;

const GOOD: &[u8] = include_bytes!("fixtures/proof-good.jam");

#[tokio::test]
async fn accepts_valid_rejects_corrupt() {
    let mut v = Verifier::boot().await.unwrap();
    assert!(v.verify(GOOD).await.unwrap());

    let mut bad = GOOD.to_vec();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x01;
    assert!(!v.verify(&bad).await.unwrap());
}
