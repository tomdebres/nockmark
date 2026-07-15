//! Nockmark registry crate (M2). `RegistryKernel` wraps the compiled
//! registry.jam kernel (M2 Task 2) with a mint/submit/leaderboard API.
//!
//! Additional mods (`verifier`, `http`) land in later M2 tasks; `kernel`
//! (Task 3) and `binding` (Task 4) exist so far.
pub mod binding;
pub mod kernel;
