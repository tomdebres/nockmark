//! Nockmark registry crate (M2). `RegistryKernel` wraps the compiled
//! registry.jam kernel (M2 Task 2) with a mint/submit/leaderboard API.
//!
//! Additional mods (`binding`, `verifier`, `http`) land in later M2 tasks;
//! only `kernel` exists so far.
pub mod kernel;
