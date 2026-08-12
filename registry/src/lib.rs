//! Nockmark registry driver (M2/M3): the compiled registry.jam kernel
//! wrapper (`kernel`), STARK proof `verifier`, nonce/proof `binding`,
//! the HTTP API (`http`), per-IP `ratelimit`ing, and `economics` estimates.
//! M5 adds the AI-PoW track (`aipow`): challenge derivation, certificate
//! verification, run store, and MAC-equivalent economics.
pub mod aipow;
pub mod binding;
pub mod economics;
pub mod http;
pub mod kernel;
pub mod ratelimit;
pub mod verifier;
