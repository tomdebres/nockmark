pub mod aipow;
/// CUDA grind backend for the canonical-MoE statement. Behind the `gpu`
/// feature because building it requires `nvcc` and links `cudart`.
#[cfg(feature = "gpu")]
pub mod aipow_gpu;
pub mod aipow_moe;
pub mod client;
pub mod hardware;
pub mod miner;
pub mod nonce;
/// M7 step 3: the ZK v5 (Anthropos) hash-rate track — protocol, CPU grind,
/// `%5` proving.
pub mod zkv5;
/// CUDA grind backend for the ZK v5 track. Behind the `zk-gpu` feature
/// because building it requires `nvcc` and links `cudart`.
#[cfg(feature = "zk-gpu")]
pub mod zkv5_gpu;
