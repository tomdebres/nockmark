//! tock — the Nockmark bench harness (M1: local benchmarking).
//!
//! Runs Nockchain's real STARK prover standalone (see docs/superpowers/specs/
//! 2026-07-15-m0-spike-findings.md for how) and reports proofs/sec for this
//! machine. `bench` is the headline command; `prove`/`verify` are the M0
//! spike primitives, kept because the registry driver will need them.

mod hardware;
mod miner;
mod nonce;

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use nockapp::noun::slab::NounSlab;
use nockapp::noun::AtomExt;
use nockapp::utils::{NOCK_STACK_SIZE_HUGE, NOCK_STACK_SIZE_TINY};
use nockapp::wire::{SystemWire, Wire};
use nockapp::NounAllocator;
use nockvm::noun::{Atom, D, T};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::miner::DEFAULT_POW_LEN;

#[derive(Parser)]
#[command(version, about = "tock — Nockmark bench harness (real Nockchain STARK prover)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Benchmark this machine: k proofs, per-proof timings, proofs/sec.
    Bench {
        /// Seed for nonce derivation; per-proof nonces are "<seed>/<i>".
        /// (Later, the registry challenge lands here.)
        #[arg(long, default_value = "tock-local")]
        seed: String,
        /// Path to the compiled miner kernel jam.
        #[arg(long)]
        kernel: PathBuf,
        /// Number of proofs to run.
        #[arg(short, long, default_value_t = 6)]
        k: u64,
        /// Concurrent proving threads (like a miner's mining threads).
        #[arg(short, long, default_value_t = 1)]
        threads: u64,
        #[arg(long, default_value_t = DEFAULT_POW_LEN)]
        pow_len: u64,
        /// Emit the result as JSON on stdout instead of the human summary.
        #[arg(long)]
        json: bool,
        /// Directory to write the proof jams into (kept only if given).
        #[arg(long)]
        keep_proofs: Option<PathBuf>,
    },
    /// Produce one STARK proof whose input incorporates an arbitrary nonce.
    Prove {
        #[arg(long)]
        nonce: String,
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long, default_value = "proof.jam")]
        out: PathBuf,
        #[arg(long, default_value_t = DEFAULT_POW_LEN)]
        pow_len: u64,
    },
    /// Verify a proof jam; prints ACCEPT or REJECT (exit code 1 on reject).
    Verify {
        #[arg(long)]
        proof: PathBuf,
        /// Path to the compiled roswell kernel jam.
        #[arg(long)]
        kernel: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Bench {
            seed,
            kernel,
            k,
            threads,
            pow_len,
            json,
            keep_proofs,
        } => bench(&seed, &kernel, k, threads, pow_len, json, keep_proofs).await,
        Command::Prove {
            nonce,
            kernel,
            out,
            pow_len,
        } => prove(&nonce, &kernel, &out, pow_len).await,
        Command::Verify { proof, kernel } => verify(&proof, &kernel).await,
    }
}

#[derive(Serialize)]
struct BenchResult {
    tool: &'static str,
    tool_version: &'static str,
    nonce_rule: &'static str,
    seed: String,
    proof_version: u64,
    pow_len: u64,
    k: u64,
    threads: u64,
    /// SHA-256 of the miner kernel jam — identifies the exact workload.
    kernel_jam_sha256: String,
    kernel_boot_s: f64,
    per_proof_ms: Vec<u64>,
    proof_bytes: Vec<u64>,
    total_s: f64,
    proofs_per_sec: f64,
    hardware: hardware::Hardware,
    timestamp_epoch_s: u64,
}

async fn bench(
    seed: &str,
    kernel: &PathBuf,
    k: u64,
    threads: u64,
    pow_len: u64,
    json: bool,
    keep_proofs: Option<PathBuf>,
) {
    assert!(k >= 1, "k must be at least 1");
    assert!(
        threads >= 1 && threads <= k,
        "threads must be between 1 and k"
    );
    let hw = hardware::detect();
    let kernel_bytes = std::fs::read(kernel)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", kernel.display()));
    let kernel_jam_sha256 = format!("{:x}", Sha256::digest(&kernel_bytes));

    if let Some(dir) = &keep_proofs {
        std::fs::create_dir_all(dir).expect("could not create --keep-proofs dir");
    }

    // One serf per thread, like the mining driver's per-thread kernels.
    let boot_t0 = Instant::now();
    let mut serfs = Vec::new();
    for _ in 0..threads {
        serfs.push(miner::boot_kernel(kernel_bytes.clone(), NOCK_STACK_SIZE_TINY).await);
    }
    let kernel_boot_s = boot_t0.elapsed().as_secs_f64();
    eprintln!("booted {threads} kernel(s) in {kernel_boot_s:.2}s; proving {k} proofs…");

    let header_belts = nonce::seed_to_belts(seed, "header");

    let total_t0 = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for (tid, serf) in serfs.into_iter().enumerate() {
        let seed = seed.to_string();
        let keep_proofs = keep_proofs.clone();
        tasks.spawn(async move {
            let mut results: Vec<(u64, u64, u64)> = Vec::new(); // (i, ms, bytes)
            let mut i = tid as u64;
            while i < k {
                let nonce_belts = nonce::seed_to_belts(&format!("{seed}/{i}"), "nonce");
                let out = miner::run_prove(&serf, &header_belts, &nonce_belts, pow_len).await;
                eprintln!(
                    "  proof {i}: {:.2?} ({} bytes, thread {tid})",
                    out.duration,
                    out.proof_jam.len()
                );
                if let Some(dir) = &keep_proofs {
                    std::fs::write(dir.join(format!("proof-{i}.jam")), &out.proof_jam)
                        .expect("could not write proof jam");
                }
                results.push((
                    i,
                    out.duration.as_millis() as u64,
                    out.proof_jam.len() as u64,
                ));
                i += threads;
            }
            results
        });
    }
    let mut per_proof: Vec<(u64, u64, u64)> = Vec::new();
    while let Some(res) = tasks.join_next().await {
        per_proof.extend(res.expect("proving task panicked"));
    }
    let total_s = total_t0.elapsed().as_secs_f64();
    per_proof.sort_unstable();

    let result = BenchResult {
        tool: "tock",
        tool_version: env!("CARGO_PKG_VERSION"),
        nonce_rule: nonce::NONCE_RULE,
        seed: seed.to_string(),
        proof_version: miner::PROOF_VERSION,
        pow_len,
        k,
        threads,
        kernel_jam_sha256,
        kernel_boot_s,
        per_proof_ms: per_proof.iter().map(|(_, ms, _)| *ms).collect(),
        proof_bytes: per_proof.iter().map(|(_, _, b)| *b).collect(),
        total_s,
        proofs_per_sec: k as f64 / total_s,
        hardware: hw,
        timestamp_epoch_s: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs(),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("could not serialize result")
        );
    } else {
        print_human(&result);
    }
}

fn print_human(r: &BenchResult) {
    println!(
        "tock bench — Nockchain STARK prover, workload v{} (pow-len {})",
        r.proof_version, r.pow_len
    );
    println!("  kernel:      sha256:{}…", &r.kernel_jam_sha256[..16]);
    println!(
        "  hardware:    {} ({} cores)",
        r.hardware.cpu_model.as_deref().unwrap_or("unknown CPU"),
        r.hardware
            .logical_cores
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    );
    let min = r.per_proof_ms.iter().min().copied().unwrap_or(0);
    let max = r.per_proof_ms.iter().max().copied().unwrap_or(0);
    let mean = r.per_proof_ms.iter().sum::<u64>() as f64 / r.per_proof_ms.len().max(1) as f64;
    println!(
        "  proofs:      {} in {:.1}s on {} thread(s)",
        r.k, r.total_s, r.threads
    );
    println!("  per proof:   min {min} ms / mean {mean:.0} ms / max {max} ms");
    println!("  proofs/sec:  {:.4}", r.proofs_per_sec);
    println!(
        "  (× 86400 = proofs/day: {:.0})",
        r.proofs_per_sec * 86400.0
    );
}

async fn prove(nonce_seed: &str, kernel: &PathBuf, out: &PathBuf, pow_len: u64) {
    let kernel_bytes = std::fs::read(kernel)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", kernel.display()));
    let boot_t0 = Instant::now();
    let serf = miner::boot_kernel(kernel_bytes, NOCK_STACK_SIZE_TINY).await;
    eprintln!("kernel boot: {:.2?}", boot_t0.elapsed());

    let header_belts = nonce::seed_to_belts("nockmark-m0-spike", "header");
    let nonce_belts = nonce::seed_to_belts(nonce_seed, "nonce");
    eprintln!("nonce belts: {nonce_belts:?}");

    let result = miner::run_prove(&serf, &header_belts, &nonce_belts, pow_len).await;
    std::fs::write(out, &result.proof_jam).expect("could not write proof jam");

    println!("prove: OK");
    println!("  nonce seed:   {nonce_seed:?}");
    println!("  pow-len:      {pow_len}");
    println!("  prove time:   {:.2?}", result.duration);
    println!("  proof size:   {} bytes (jammed)", result.proof_jam.len());
    println!("  proof digest: 0x{}", result.dig_hex);
    println!("  written to:   {}", out.display());
}

async fn verify(proof_path: &PathBuf, kernel: &PathBuf) {
    let kernel_bytes = std::fs::read(kernel)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", kernel.display()));
    let boot_t0 = Instant::now();
    let serf = miner::boot_kernel(kernel_bytes, NOCK_STACK_SIZE_HUGE).await;
    eprintln!("kernel boot: {:.2?}", boot_t0.elapsed());

    let jammed = std::fs::read(proof_path)
        .unwrap_or_else(|e| panic!("could not read proof jam {}: {e}", proof_path.display()));
    let proof_bytes_len = jammed.len();

    let mut slab: NounSlab = NounSlab::new();
    let proof = slab
        .cue_into(jammed.into())
        .expect("could not cue proof jam");

    // Cause: [%verify-proof ~ ~ proof] — p is a (unit (unit proof)).
    let tag = Atom::from_value(&mut slab, "verify-proof")
        .expect("tag atom")
        .as_noun();
    let inner_some = T(&mut slab, &[D(0), proof]);
    let outer_some = T(&mut slab, &[D(0), inner_some]);
    let cause = T(&mut slab, &[tag, outer_some]);
    slab.set_root(cause);

    let wire = SystemWire.to_wire();
    let t0 = Instant::now();
    let result = serf.poke(wire, slab).await;
    let verify_time = t0.elapsed();

    // Effects: list of [%exit code]; code 0 = proof verified.
    let verdict = match &result {
        Err(e) => {
            eprintln!("verify poke errored: {e}");
            false
        }
        Ok(result) => {
            let space = result.noun_space();
            let mut effects = unsafe { *result.root() }.in_space(&space);
            let mut ok = false;
            while let Ok(cell) = effects.as_cell() {
                let effect = cell.head();
                effects = cell.tail();
                let Ok(effect_cell) = effect.as_cell() else {
                    continue;
                };
                if effect_cell.head().eq_bytes("exit") {
                    let code = effect_cell
                        .tail()
                        .as_atom()
                        .expect("exit code should be an atom")
                        .as_u64()
                        .expect("exit code should be small");
                    ok = code == 0;
                    break;
                }
            }
            ok
        }
    };

    println!("verify: {}", if verdict { "ACCEPT" } else { "REJECT" });
    println!("  proof size:  {proof_bytes_len} bytes (jammed)");
    println!("  verify time: {verify_time:.2?}");
    if !verdict {
        std::process::exit(1);
    }
}
