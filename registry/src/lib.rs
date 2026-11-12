//! Nockmark registry driver (M2/M3): the compiled registry.jam kernel
//! wrapper (`kernel`), the consensus Hoon STARK `verifier` (roswell), the HTTP
//! API (`http`), per-IP `ratelimit`ing, and `economics` estimates.
//! M5 adds the AI-PoW track (`aipow`): challenge derivation, certificate
//! verification, run store, and MAC-equivalent economics. M7 adds the ZK v5
//! hash-rate track (`zkv5`), which replaced complete-proof ZK submissions.
pub mod aipow;
pub mod aipow_moe;
/// M7 step 4: dense-peak verification (mainnet's production AI shape).
pub mod aipow_peak;
pub mod economics;
pub mod http;
/// Append-only JSONL for the run stores, safe against torn writes.
pub mod jsonl;
pub mod kernel;
pub mod ratelimit;
pub mod verifier;
/// M7 step 3: the ZK v5 hash-rate track's store, per-win checks and rates.
pub mod zkv5;
