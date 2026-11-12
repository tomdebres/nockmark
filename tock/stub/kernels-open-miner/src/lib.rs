//! Empty stand-in for upstream's `kernels-open-miner`.
//!
//! Upstream's crate is one line, `include_bytes!(env!("KERNEL_JAM_PATH"))`,
//! and its build script panics unless a compiled `miner.jam` exists at build
//! time. Nockmark depends on `zk-pow-miner` only for its V5 digest, target
//! comparison and CUDA search (`zk_pow_miner::v5`, `zk_pow_miner::cuda`), none
//! of which touch the kernel. tock boots `miner.jam` itself, from a path given
//! at run time (`miner::boot_kernel`), as it always has.
//!
//! Without this stub every tock and registry build would need a miner jam on
//! disk, and the registry build would need two jams under one env var
//! (`KERNEL_JAM_PATH` already names roswell.jam there). Empty, so anything
//! that did try to boot upstream's embedded miner (`zk_pow_miner::SerfWorker`)
//! would fail at boot instead of running the wrong kernel.
pub static KERNEL: &[u8] = &[];
