//! Nockmark registry crate (M2). `RegistryKernel` wraps the compiled
//! registry.jam kernel (M2 Task 2) with a mint/submit/leaderboard API.
//!
//! Additional mods (`http`) land in later M2 tasks; `kernel` (Task 3),
//! `binding` (Task 4), and `verifier` (Task 5) exist so far.
pub mod binding;
pub mod economics;
pub mod http;
pub mod kernel;
pub mod ratelimit;
pub mod verifier;
