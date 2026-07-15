use nockmark_registry::binding::{check_binding, BindingError};

const GOOD: &[u8] = include_bytes!("fixtures/proof-good.jam");

#[test]
fn good_proof_binds() {
    assert!(check_binding(GOOD, "fixture-challenge", 0, 64).is_ok());
}

#[test]
fn wrong_challenge_rejected() {
    assert!(matches!(
        check_binding(GOOD, "other-challenge", 0, 64),
        Err(BindingError::WrongNonce) | Err(BindingError::WrongHeader)
    ));
}

#[test]
fn wrong_index_rejected() {
    assert!(matches!(
        check_binding(GOOD, "fixture-challenge", 1, 64),
        Err(BindingError::WrongNonce)
    ));
}

#[test]
fn wrong_pow_len_rejected() {
    assert!(matches!(
        check_binding(GOOD, "fixture-challenge", 0, 32),
        Err(BindingError::WrongLen)
    ));
}

#[test]
fn garbage_rejected() {
    assert!(matches!(
        check_binding(b"not a jam", "x", 0, 64),
        Err(BindingError::Undecodable)
    ));
}
