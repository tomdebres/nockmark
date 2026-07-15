//! Deterministic mapping from an arbitrary seed string to the 5 field
//! elements of a noun-digest:tip5 — the nonce shape the miner kernel takes.
//!
//! This rule is part of the (future) challenge format: the registry hands a
//! miner a seed, and both sides must derive the same belts from it. It is
//! therefore versioned; changing the rule means bumping NONCE_RULE and the
//! workload version in the registry.

use zkvm_jetpack::form::belt::PRIME;

/// Identifier for the seed→belts rule, recorded in every bench result.
pub const NONCE_RULE: &str = "fnv1a-splitmix64-v1";

/// Map an arbitrary string to 5 belts (< Goldilocks PRIME): FNV-1a over
/// `domain ++ seed` seeds a splitmix64 stream, one output per belt.
/// Not cryptographic — collision resistance is not needed here; the proof
/// itself binds the actual belts, and the registry stores them.
pub fn seed_to_belts(seed: &str, domain: &str) -> [u64; 5] {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in domain.as_bytes().iter().chain(seed.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let mut out = [0u64; 5];
    for slot in out.iter_mut() {
        h = h.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = h;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        *slot = (z ^ (z >> 31)) % PRIME;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn belts_are_in_field() {
        for seed in ["", "a", "nockmark-m0-test-1", "🦀 unicode"] {
            for belt in seed_to_belts(seed, "nonce") {
                assert!(belt < PRIME, "belt {belt} out of field for seed {seed:?}");
            }
        }
    }

    #[test]
    fn deterministic_and_domain_separated() {
        assert_eq!(
            seed_to_belts("x", "nonce"),
            seed_to_belts("x", "nonce"),
            "same seed+domain must be stable"
        );
        assert_ne!(
            seed_to_belts("x", "nonce"),
            seed_to_belts("x", "header"),
            "domains must separate"
        );
        assert_ne!(
            seed_to_belts("x", "nonce"),
            seed_to_belts("y", "nonce"),
            "seeds must differ"
        );
    }

    /// Golden vector: locks the v1 rule. If this test breaks, the rule
    /// changed and NONCE_RULE must be bumped (plus registry workload version).
    #[test]
    fn golden_vector_v1() {
        assert_eq!(
            seed_to_belts("nockmark-m0-test-1", "nonce"),
            [
                7605750820624564413,
                9130906209974091515,
                666954970149679518,
                9484078685814613428,
                11683996358245670181
            ]
        );
    }
}
