//! Proof-to-challenge binding check (M2 Task 4).
//!
//! A submitted proof jam is only valid for a given registry challenge if its
//! embedded puzzle nonce/header/pow-len match what the registry expects for
//! that challenge. This module decodes the jam via `zkvm_jetpack::form::Proof`
//! and compares the `ProofData::Puzzle { com, nonce, len, .. }` fields against
//! belts derived from the challenge string via `tock::nonce::seed_to_belts`.

use nockapp::noun::slab::NounSlab;
use nockapp::NounAllocator;
use noun_serde::NounDecode;
use tock::nonce::seed_to_belts;
use zkvm_jetpack::form::proof::{Proof, ProofData};

#[derive(Debug, PartialEq, Eq)]
pub enum BindingError {
    Undecodable,
    NoPuzzle,
    WrongNonce,
    WrongHeader,
    WrongLen,
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BindingError::Undecodable => "proof jam undecodable",
            BindingError::NoPuzzle => "proof has no puzzle object",
            BindingError::WrongNonce => "proof nonce does not match challenge",
            BindingError::WrongHeader => "proof header does not match challenge",
            BindingError::WrongLen => "proof pow-len mismatch",
        };
        f.write_str(s)
    }
}

impl std::error::Error for BindingError {}

/// Verify that `proof_jam` was produced for `challenge`/`index` at `pow_len`.
///
/// Expected nonce belts are `seed_to_belts("{challenge}/{index}", "nonce")`
/// and expected header belts are `seed_to_belts(challenge, "header")` — the
/// same derivation `tock prove --header-seed` uses. The nonce is checked
/// before the header so `wrong_index_rejected`-style mismatches (same
/// challenge, different index) surface as `WrongNonce`.
pub fn check_binding(
    proof_jam: &[u8],
    challenge: &str,
    index: u64,
    pow_len: u64,
) -> Result<(), BindingError> {
    let mut slab: NounSlab = NounSlab::new();
    let noun = slab
        .cue_into(proof_jam.to_vec().into())
        .map_err(|_| BindingError::Undecodable)?;
    let space = slab.noun_space();
    let proof = Proof::from_noun(&noun, &space).map_err(|_| BindingError::Undecodable)?;

    let expected_nonce = seed_to_belts(&format!("{challenge}/{index}"), "nonce");
    let expected_header = seed_to_belts(challenge, "header");

    for obj in &proof.objects {
        if let ProofData::Puzzle { com, nonce, len, .. } = obj {
            if *len != pow_len {
                return Err(BindingError::WrongLen);
            }
            if *nonce != expected_nonce {
                return Err(BindingError::WrongNonce);
            }
            if *com != expected_header {
                return Err(BindingError::WrongHeader);
            }
            return Ok(());
        }
    }
    Err(BindingError::NoPuzzle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_undecodable() {
        assert_eq!(
            check_binding(b"not a jam", "x", 0, 64),
            Err(BindingError::Undecodable)
        );
    }
}
