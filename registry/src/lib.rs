//! Nockmark registry driver (M2/M3): the compiled registry.jam kernel
//! wrapper (`kernel`), STARK proof `verifier`, nonce/proof `binding`,
//! the HTTP API (`http`), per-IP `ratelimit`ing, and `economics` estimates.
pub mod binding;
pub mod economics;
pub mod http;
pub mod kernel;
pub mod ratelimit;
pub mod verifier;
