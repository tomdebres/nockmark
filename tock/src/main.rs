//! tock — the Nockmark bench harness (M1: local benchmarking).
//!
//! Runs Nockchain's real STARK prover standalone (see docs/superpowers/specs/
//! 2026-07-15-m0-spike-findings.md for how) and reports proofs/sec for this
//! machine. `bench` is the headline command; `prove`/`verify` are the M0
//! spike primitives, kept because the registry driver will need them.

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

#[cfg(feature = "gpu")]
use tock::aipow_gpu;
#[cfg(feature = "zk-gpu")]
use tock::zkv5_gpu;
use tock::{aipow, aipow_moe, aipow_peak, client, hardware, miner, nonce, zkv5};
use tock::miner::DEFAULT_POW_LEN;

#[derive(Parser)]
#[command(version, about = "tock — Nockmark bench harness (real Nockchain STARK prover)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Benchmark complete `%3` proving: k proofs, per-proof timings,
    /// proofs/sec. A proving benchmark only: since Anthropos (height 147,500)
    /// a ZK mining attempt is one Tip5 hash and mainnet takes `%5` proofs —
    /// `zk-bench` measures that, and is what the registry ranks.
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
        /// Retired: the registry no longer takes `%3` proofs/sec runs (it
        /// answers 410 Gone). Use `zk-bench --submit`.
        #[arg(long, value_name = "REGISTRY_URL", hide = true)]
        submit: Option<String>,
        /// Prover version string recorded on submission (the nockchain
        /// commit the kernel jams were built from).
        #[arg(long, default_value = tock::miner::NOCKCHAIN_PIN)]
        prover_version: String,
    },
    /// Benchmark ZK mining as mainnet does it since Anthropos (height
    /// 147,500): grind Tip5 hashes of proof object 0 for nonces that meet a
    /// target, then prove each winner (`%5`) through the miner kernel. The
    /// registry ranks verified Tip5 hashes per second.
    ZkBench {
        /// Path to the compiled miner kernel jam (proves the winners).
        #[arg(long)]
        kernel: PathBuf,
        /// Difficulty tier: expected Tip5 hashes per win, rounded to the
        /// nearest power of two and clamped to [2^12, 2^31]. Skips the
        /// self-calibration. Without it, zk-bench first measures this machine
        /// (~2 s of grinding plus one throwaway proof, ~21 s on an M1 Max) and
        /// sizes each win's grind at 2x that proof, so the registry's window
        /// recovers 2/3 of the true hash rate on every host.
        #[arg(long)]
        attempts: Option<u64>,
        /// Wins to find and prove. With --submit the registry's k applies.
        #[arg(short, long, default_value_t = 1)]
        k: u64,
        /// CPU grind threads (default: every core). Proving is one kernel,
        /// one proof at a time, whatever this says.
        #[arg(short, long)]
        threads: Option<usize>,
        /// Grind on CUDA device 0 instead of the CPU. Needs a build with
        /// `--features zk-gpu` (nvcc; arch from ZK_POW_CUDA_ARCH). Only the
        /// search moves to the device: every winner is re-checked on the CPU
        /// and proved there.
        #[arg(long)]
        gpu: bool,
        /// Attempts per CUDA launch with --gpu.
        #[arg(long, value_name = "N")]
        gpu_batch: Option<u64>,
        /// Registry base URL: mint a challenge there at the chosen tier, and
        /// submit the proofs to it. Without it, zk-bench stays local.
        #[arg(long, value_name = "REGISTRY_URL")]
        submit: Option<String>,
        /// Prover version string recorded on submission.
        #[arg(long, default_value = tock::miner::NOCKCHAIN_PIN)]
        prover_version: String,
        /// Directory to write the winners' proof jams into.
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    /// Benchmark the Logos AI-PoW puzzle: grind attempts, prove a compact
    /// recursive certificate per jackpot win. `--statement` picks which AI-PoW
    /// statement to benchmark; both rank on the one AI board, in
    /// MAC-equivalents per second. `--gpu` moves the canonical-MoE grind (and
    /// nothing else) onto CUDA.
    AiBench {
        /// Which statement to benchmark: `dense` (M5, single-tile),
        /// `canonical-moe` (the reference MoE block) or `dense-peak`
        /// (mainnet's production dense shape, m=4096 k=8192 n=32768, 16x16
        /// tiles; ~75 s and ~12-15 GB per certificate on an M1 Max).
        #[arg(long, default_value = "dense")]
        statement: String,
        /// 32-byte challenge, hex (the registry challenge lands here later).
        /// Defaults to a fixed dev constant for fully-local runs.
        #[arg(long)]
        challenge: Option<String>,
        /// 32-byte jackpot target, hex. Pinning it here is itself a difficulty
        /// choice, so it SKIPS the self-calibration below. NOTE the two
        /// statements price targets differently: dense compares the jackpot
        /// against the target raw, canonical-MoE against target × 2^16.
        #[arg(long)]
        target: Option<String>,
        /// Difficulty tier: expected grind attempts per win. Skips the
        /// self-calibration and asks for exactly this (rounded to the nearest
        /// power of two and clamped to [2^12, 2^30]). Without it, and without
        /// --target, ai-bench first measures this machine — ~2 s of grinding
        /// AND one throwaway certificate prove (~7-26 s, the same cost a real
        /// win pays) — and sizes each win's grind at 2x that measured prove
        /// time, which is what lets fast hardware be ranked on its grind rate
        /// instead of on whichever CPU happened to do the proving. Pass
        /// --attempts (or --target) to skip that startup cost entirely.
        #[arg(long)]
        attempts: Option<u64>,
        /// Jackpot wins to find (each win costs a ~24 s certificate prove).
        /// The self-calibrated tier does NOT depend on k — it is sized per win
        /// — so with --submit a differing registry k only changes how long the
        /// run takes, not how hard it is.
        #[arg(short, long, default_value_t = 1)]
        k: u64,
        /// Directory to write win certificates into (cert-<extranonce>.bin).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Registry base URL for the AI track. When set, fetches the AI
        /// challenge (its challenge/target/k override the local flags)
        /// and submits the wins after proving.
        #[arg(long, value_name = "REGISTRY_URL")]
        submit: Option<String>,
        /// Grind on the CUDA backend instead of the CPU (`--statement
        /// canonical-moe` or `dense-peak`; dense-peak uses upstream's #159
        /// peak kernel). Requires a build with the `gpu` feature:
        /// `cargo build --release --features gpu`. Only the jackpot SEARCH
        /// moves to the device — every win is still re-checked against the
        /// scalar oracle and certified by the CPU prover.
        #[arg(long)]
        gpu: bool,
        /// Attempts per CUDA launch when `--gpu` is set. Bigger amortizes
        /// launch overhead; the session allocates for it up front, so device
        /// memory is the ceiling.
        #[arg(long, value_name = "N")]
        gpu_batch: Option<u64>,
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
        /// Seed for header-belt derivation (the registry challenge lands
        /// here later). Defaults to the fixed M0-spike seed for backwards
        /// compatibility with existing fixtures/benches.
        #[arg(long, default_value = "nockmark-m0-spike")]
        header_seed: String,
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
            submit,
            prover_version,
        } => {
            bench(
                &seed,
                &kernel,
                k,
                threads,
                pow_len,
                json,
                keep_proofs,
                submit,
                prover_version,
            )
            .await
        }
        Command::AiBench {
            statement,
            challenge,
            target,
            attempts,
            k,
            out_dir,
            submit,
            gpu,
            gpu_batch,
        } => {
            let statement = client::AiStatement::parse(&statement)
                .unwrap_or_else(|e| panic!("bad --statement: {e}"));
            // Which backend grinds — settled before the calibration, because
            // the calibration must measure the backend that will do the work.
            let backend = resolve_backend(statement, gpu, gpu_batch);
            if statement == client::AiStatement::DensePeak {
                // Dense-peak calibrates on the same matrices and backend it
                // then grinds with, so it resolves its own tier.
                return ai_bench_peak(challenge, target, attempts, k, out_dir, submit, backend)
                    .await;
            }
            // Resolve the difficulty tier FIRST, before anything that starts a
            // clock: the calibration grind must not land inside the reported
            // grind window, nor (with --submit) inside the server window, which
            // opens the moment the challenge is minted.
            let tier = resolve_tier(statement, backend, attempts, target.is_some(), k);
            match statement {
                client::AiStatement::Dense => {
                    ai_bench(challenge, target, tier, k, out_dir, submit).await
                }
                client::AiStatement::CanonicalMoe => {
                    ai_bench_moe(challenge, target, tier, k, out_dir, submit, backend).await
                }
                client::AiStatement::DensePeak => unreachable!("dispatched above"),
            }
        }
        Command::ZkBench {
            kernel,
            attempts,
            k,
            threads,
            gpu,
            gpu_batch,
            submit,
            prover_version,
            out_dir,
        } => {
            let backend = resolve_zk_backend(gpu, gpu_batch, threads);
            zk_bench(kernel, attempts, k, backend, submit, prover_version, out_dir).await
        }
        Command::Prove {
            nonce,
            kernel,
            out,
            pow_len,
            header_seed,
        } => prove(&nonce, &kernel, &out, pow_len, &header_seed).await,
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
    submit: Option<String>,
    prover_version: String,
) {
    assert!(k >= 1, "k must be at least 1");
    assert!(threads >= 1, "threads must be at least 1");
    if submit.is_some() {
        eprintln!(
            "bench --submit is retired: since height 147,500 (Anthropos) the registry ranks \
             Tip5 hashes/s from %5 proofs, not complete %3 proofs/s. Run \
             `tock zk-bench --kernel {} --submit <REGISTRY_URL>` instead.",
            kernel.display()
        );
        std::process::exit(2);
    }
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
    eprintln!("booted {threads} kernel(s) in {kernel_boot_s:.2}s");

    // Resolve the workload: local seed, or a server challenge (fetched
    // AFTER kernel boot so boot time never counts against the window).
    let (challenge, seed, k, pow_len) = match &submit {
        Some(base) => {
            let ch = client::fetch_challenge(base)
                .await
                .unwrap_or_else(|e| panic!("could not fetch challenge: {e}"));
            assert_eq!(
                ch.nonce_rule,
                nonce::NONCE_RULE,
                "registry expects nonce rule {:?}; this tock speaks {:?} — upgrade tock",
                ch.nonce_rule,
                nonce::NONCE_RULE
            );
            eprintln!(
                "challenge {} from {base} (k={}, pow_len={}); the clock is running",
                ch.nonce, ch.k, ch.pow_len
            );
            let seed = ch.nonce.clone();
            let (k, pow_len) = (ch.k, ch.pow_len);
            (Some(ch), seed, k, pow_len)
        }
        None => {
            eprintln!("proving {k} proofs…");
            (None, seed.to_string(), k, pow_len)
        }
    };
    if threads > k {
        eprintln!("note: {threads} threads for {k} proofs — extra threads idle");
    }

    let header_belts = nonce::seed_to_belts(&seed, "header");

    let total_t0 = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for (tid, serf) in serfs.into_iter().enumerate() {
        let seed = seed.clone();
        let keep_proofs = keep_proofs.clone();
        tasks.spawn(async move {
            let mut results: Vec<(u64, u64, Vec<u8>)> = Vec::new(); // (i, ms, jam)
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
                results.push((i, out.duration.as_millis() as u64, out.proof_jam));
                i += threads;
            }
            results
        });
    }
    let mut per_proof: Vec<(u64, u64, Vec<u8>)> = Vec::new();
    while let Some(res) = tasks.join_next().await {
        per_proof.extend(res.expect("proving task panicked"));
    }
    let total_s = total_t0.elapsed().as_secs_f64();
    per_proof.sort_by_key(|(i, _, _)| *i);

    let result = BenchResult {
        tool: "tock",
        tool_version: env!("CARGO_PKG_VERSION"),
        nonce_rule: nonce::NONCE_RULE,
        seed: seed.clone(),
        proof_version: miner::PROOF_VERSION,
        pow_len,
        k,
        threads,
        kernel_jam_sha256,
        kernel_boot_s,
        per_proof_ms: per_proof.iter().map(|(_, ms, _)| *ms).collect(),
        proof_bytes: per_proof.iter().map(|(_, _, jam)| jam.len() as u64).collect(),
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

    if let (Some(base), Some(ch)) = (&submit, challenge) {
        use base64::Engine;
        let proofs: Vec<String> = per_proof
            .iter() // i-ordered: proof i must sit at index i (binding check)
            .map(|(_, _, jam)| base64::engine::general_purpose::STANDARD.encode(jam))
            .collect();
        // Round UP: never claim faster than measured, so an honest claim
        // always fits inside the server window.
        let elapsed_ms = (total_s * 1000.0).ceil().max(1.0) as u64;
        let sub = client::Submission {
            nonce: ch.nonce,
            hardware: client::hardware_summary(&result.hardware),
            prover_version,
            elapsed_ms,
            proofs,
        };
        match client::submit_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: run {id}");
                println!("  {}/runs/{id}", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission REJECTED or failed: {e}");
                eprintln!("(local bench result above is still valid)");
                std::process::exit(1);
            }
        }
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

/// Which backend grinds this `ai-bench` run.
///
/// Carries the CUDA launch size rather than leaving it in a separate variable
/// so the calibration and the run cannot end up dispatching differently sized
/// batches — the tier is derived from the measured rate, and the measured rate
/// is a function of the launch geometry.
///
/// The `Gpu` variant does not exist at all without the `gpu` feature. That is
/// deliberate: it makes "this build cannot grind on a GPU" a fact the type
/// system knows, so the only place the missing feature has to be reported is
/// [`resolve_backend`], and no downstream `match` can silently fall through to
/// a CPU grind the user did not ask for.
#[derive(Clone, Copy)]
enum Backend {
    Cpu,
    #[cfg(feature = "gpu")]
    Gpu {
        batch_attempts: u64,
    },
}

impl Backend {
    /// How the run reports itself on the summary — the board compares CPU and
    /// GPU rows in the same MAC-equivalent unit, so which one produced a row
    /// has to be legible.
    fn label(self) -> String {
        match self {
            Self::Cpu => "CPU (scalar canonical template)".to_string(),
            #[cfg(feature = "gpu")]
            Self::Gpu { batch_attempts } => format!(
                "CUDA device {} ({batch_attempts} attempts/launch, jackpot search only)",
                aipow_gpu::GPU_DEVICE_ORDINAL
            ),
        }
    }

    /// The descriptor submitted with the run. A GPU row has to name the
    /// GPU: the board ranks both backends in one unit, so a row carrying
    /// only its host CPU reads as that CPU having done the work.
    fn hardware_descriptor(self, hw: &hardware::Hardware) -> String {
        match self {
            Self::Cpu => client::hardware_summary(hw),
            #[cfg(feature = "gpu")]
            Self::Gpu { .. } => client::hardware_summary_with_device(
                aipow_gpu::device_descriptor().as_deref(),
                hw,
            ),
        }
    }
}

/// Resolve `--gpu` / `--gpu-batch` into a [`Backend`], failing loudly rather
/// than silently downgrading.
///
/// Two ways this exits instead of returning, both with a message that names the
/// fix:
///
///   * `--gpu` on a build without the `gpu` feature. A panic here would be
///     read as a bug in tock; the actual situation is that the binary was built
///     without CUDA, and the remedy is a build flag.
///   * `--gpu --statement dense`. Upstream's `GpuSearchBackend` does implement
///     `search_dense`, but through the generic session — a different kernel,
///     with attempt strips prepared on the CPU first — so it is a separate
///     measurement that has not been validated here. Grinding the dense
///     statement on the CPU while the user asked for a GPU would put a
///     mislabelled row on the board.
fn resolve_backend(statement: client::AiStatement, gpu: bool, gpu_batch: Option<u64>) -> Backend {
    if !gpu {
        if gpu_batch.is_some() {
            eprintln!("note: --gpu-batch has no effect without --gpu");
        }
        return Backend::Cpu;
    }
    if statement == client::AiStatement::Dense {
        eprintln!(
            "--gpu is implemented for --statement {} and {} only (asked for {:?}); \
             the dense statement has no validated CUDA path",
            aipow_moe::STATEMENT_CANONICAL_MOE,
            aipow_peak::STATEMENT_DENSE_PEAK,
            statement.as_str()
        );
        std::process::exit(2);
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = gpu_batch;
        eprintln!(
            "--gpu requires a CUDA build of tock, which this binary is not: \
             rebuild with `cargo build --release --features gpu` on a machine \
             with nvcc and a CUDA driver"
        );
        std::process::exit(2)
    }
    #[cfg(feature = "gpu")]
    {
        Backend::Gpu {
            batch_attempts: gpu_batch.unwrap_or(aipow_gpu::DEFAULT_GPU_BATCH_ATTEMPTS),
        }
    }
}

/// Decide which difficulty tier this `ai-bench` run grinds at — expected grind
/// attempts per win — and say so on stderr. `None` means "the caller pinned the
/// difficulty with `--target`; leave it alone".
///
/// Runs BEFORE the challenge is minted and before either grind window opens, so
/// the whole calibration — the ~2 s rate measurement AND the one throwaway
/// certificate — is charged to nothing. `--attempts` (and `--target`) skip both
/// measurements entirely, which is the way to avoid paying for the prove.
///
/// The heuristic is one line — measure this machine's grind rate and its
/// per-certificate prove time, then ask for
/// [`aipow::AI_TIER_PROVE_RATIO`] × one certificate's worth of grinding per win
/// — and the reason it is safe to let the client run it is that the tier is not
/// a claim. The registry re-derives the target from the tier, verifies every
/// win against it, and credits exactly the attempts that target implies. A
/// machine that overestimates itself simply grinds longer; one that
/// underestimates itself reports a looser lower bound than it deserved.
fn resolve_tier(
    statement: client::AiStatement,
    backend: Backend,
    attempts: Option<u64>,
    explicit_target: bool,
    k: u64,
) -> Option<u64> {
    if let Some(requested) = attempts {
        let granted = statement.grant_attempts(requested);
        if granted != requested {
            eprintln!(
                "--attempts {requested} rounded/clamped to the nearest tier: \
                 2^{} ({granted})",
                granted.ilog2()
            );
        }
        return Some(granted);
    }
    if explicit_target {
        return None;
    }
    let budget = std::time::Duration::from_secs_f64(aipow::AI_CALIBRATION_SECS);
    // The calibration MUST run on the backend that will grind: a CPU-measured
    // rate would hand a GPU a tier some three orders of magnitude too easy, and
    // the run would degenerate into a measurement of certificate proving —
    // which is exactly what tiers exist to prevent.
    let rate = match (statement, backend) {
        (client::AiStatement::Dense, _) => aipow::calibrate_attempts_per_sec(budget),
        #[cfg(feature = "gpu")]
        (client::AiStatement::CanonicalMoe, Backend::Gpu { batch_attempts }) => {
            aipow_gpu::calibrate_moe_attempts_per_sec(budget, batch_attempts)
        }
        (client::AiStatement::CanonicalMoe, _) => aipow_moe::calibrate_moe_attempts_per_sec(budget),
        (client::AiStatement::DensePeak, _) => unreachable!("dense-peak calibrates in ai_bench_peak"),
    };
    // The prove half is chosen by STATEMENT and not by backend, because
    // certificates are proved on the CPU on every path — the GPU only grinds.
    // This is the expensive part of the calibration (~7-26 s, one certificate).
    let prove_secs = match statement {
        client::AiStatement::Dense => aipow::calibrate_prove_secs(),
        client::AiStatement::CanonicalMoe => aipow_moe::calibrate_moe_prove_secs(),
        client::AiStatement::DensePeak => unreachable!("dense-peak calibrates in ai_bench_peak"),
    };
    // No k: the tier is sized per win against one certificate, and both halves
    // scale with k, so the grind/prove mix the board sees is the same at any k.
    let tier = aipow::tier_for_prove_ratio(rate, prove_secs);
    eprintln!(
        "calibration: {rate:.0} attempts/sec measured over {:.0}s on {}, \
         {prove_secs:.1}s per certificate -> tier 2^{} ({tier} expected \
         attempts/win, ~{:.0}s of grinding vs ~{:.0}s of proving for k={k})",
        aipow::AI_CALIBRATION_SECS,
        backend.label(),
        tier.ilog2(),
        tier as f64 * k as f64 / rate.max(1e-9),
        prove_secs * k as f64,
    );
    Some(tier)
}

/// Note when the registry's k differs from the one the run was quoted for.
///
/// The tier has to be chosen before the challenge exists (it is a parameter OF
/// the challenge request), so with `--submit` the wall clock printed by
/// [`resolve_tier`] is for the local `-k`. The TIER itself is k-independent
/// under [`aipow::AI_TIER_PROVE_RATIO`] — grinding and proving both scale with
/// the number of wins, so the mix the board measures is unchanged — and only
/// the run length moves. Hence a note, not an error.
fn note_k_mismatch(registry_ch: Option<&client::AiRegistryChallenge>, tier: Option<u64>, k: u64) {
    let Some(rc) = registry_ch else { return };
    if rc.k == k || tier.is_none() {
        return;
    }
    eprintln!(
        "note: the run was quoted for k={k} but the registry wants k={}; \
         the tier stands (it is per-win) — expect {:.1}x the wall clock",
        rc.k,
        rc.k as f64 / k as f64,
    );
}

/// Report the tier a run ended up at, after the registry has had its say.
/// Printed for both statements, from the target actually being ground.
fn print_tier(
    statement: client::AiStatement,
    target: &[u8; 32],
    tier: Option<u64>,
    registry_ch: Option<&client::AiRegistryChallenge>,
) {
    let expected = statement.expected_attempts_per_win(target);
    println!("  tier:         {expected:.0} expected attempts/win");
    match (registry_ch.and_then(|rc| rc.attempts), tier) {
        (Some(granted), _) => println!("  granted:      2^{} by the registry", granted.ilog2()),
        // An older registry ignores `?attempts=` and echoes nothing; we grind
        // whatever target it sent, exactly as a pre-tier client would have.
        (None, Some(_)) if registry_ch.is_some() => {
            println!("  granted:      registry does not support tiers — using its target")
        }
        _ => {}
    }
}

async fn ai_bench(
    challenge: Option<String>,
    target: Option<String>,
    tier: Option<u64>,
    k: u64,
    out_dir: Option<PathBuf>,
    submit: Option<String>,
) {
    assert!(k >= 1, "k must be at least 1");
    // With --submit the registry supplies the whole challenge (its
    // challenge/target/k override the local flags, like `bench`); the tier is
    // the one thing we ask it for.
    let registry_ch = match &submit {
        Some(base) => Some(
            client::fetch_ai_challenge(base, client::AiStatement::Dense, tier)
                .await
                .unwrap_or_else(|e| panic!("fetch AI challenge: {e}")),
        ),
        None => None,
    };
    note_k_mismatch(registry_ch.as_ref(), tier, k);
    let (challenge, target, k) = match &registry_ch {
        Some(rc) => (
            aipow::parse_hex32(&rc.challenge)
                .unwrap_or_else(|e| panic!("registry challenge: {e}")),
            aipow::parse_hex32(&rc.target).unwrap_or_else(|e| panic!("registry target: {e}")),
            rc.k,
        ),
        None => (
            match &challenge {
                Some(hex) => {
                    aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --challenge: {e}"))
                }
                None => aipow::dev_challenge(),
            },
            match &target {
                Some(hex) => aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --target: {e}")),
                // A local run grinds the tier's own target, derived exactly as
                // the registry would derive it — so a local benchmark and a
                // submitted one measure the same workload. Without a tier
                // (i.e. --target was given and parsed above) this is
                // unreachable; the max target stays the fallback.
                None => tier
                    .and_then(|t| client::AiStatement::Dense.target_for_attempts(t))
                    .unwrap_or([0xff; 32]), // max target: every attempt wins
            },
            k,
        ),
    };
    let hw = hardware::detect();
    let ch = aipow::AiChallenge {
        challenge,
        target,
        k,
    };

    let summary = aipow::run(&ch);

    println!("tock ai-bench — Logos AI-PoW, canonical single-tile shape (CPU; --gpu is canonical-moe only)");
    println!("  challenge:    {}", aipow::hex32(&ch.challenge));
    println!("  target:       {}", aipow::hex32(&ch.target));
    print_tier(
        client::AiStatement::Dense,
        &ch.target,
        tier,
        registry_ch.as_ref(),
    );
    println!("  nonce rule:   {}", aipow::AI_NONCE_RULE);
    println!(
        "  hardware:     {} ({} cores)",
        hw.cpu_model.as_deref().unwrap_or("unknown CPU"),
        hw.logical_cores
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    );
    println!(
        "  grind:        {} win(s) in {:.2}s, {} attempts",
        summary.wins.len(),
        summary.grind_elapsed_ms as f64 / 1000.0,
        summary.total_attempts
    );
    println!(
        "  grind rate:   {:.1} attempts/sec ({:.1} MMAC-equiv/s at F=2^16)",
        summary.attempts_per_sec,
        summary.attempts_per_sec * aipow::MAC_EQUIV_PER_ATTEMPT / 1e6
    );
    let prove_ms: Vec<u64> = summary.wins.iter().map(|w| w.prove_ms).collect();
    let min = prove_ms.iter().min().copied().unwrap_or(0);
    let max = prove_ms.iter().max().copied().unwrap_or(0);
    let mean = prove_ms.iter().sum::<u64>() as f64 / prove_ms.len().max(1) as f64;
    println!("  cert prove:   min {min} ms / mean {mean:.0} ms / max {max} ms (outside window)");
    for w in &summary.wins {
        println!(
            "  win:          extranonce {} — cert {} bytes, proved in {} ms",
            w.extranonce,
            w.cert_bytes.len(),
            w.prove_ms
        );
    }

    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("could not create --out-dir");
        for w in &summary.wins {
            let path = dir.join(format!("cert-{}.bin", w.extranonce));
            std::fs::write(&path, &w.cert_bytes).expect("could not write certificate");
        }
        println!(
            "  certs:        {} written to {}",
            summary.wins.len(),
            dir.display()
        );
    }

    if let (Some(base), Some(rc)) = (&submit, &registry_ch) {
        use base64::Engine;
        let sub = client::AiSubmission {
            nonce: rc.nonce.clone(),
            hardware: client::hardware_summary(&hw),
            prover_version: miner::NOCKCHAIN_PIN.into(),
            grind_elapsed_ms: summary.grind_elapsed_ms,
            wins: summary
                .wins
                .iter()
                .map(|w| client::AiWinSubmission {
                    extranonce: w.extranonce,
                    cert_b64: base64::engine::general_purpose::STANDARD
                        .encode(&w.submission_bytes),
                })
                .collect(),
            // Omitted on the wire: an M5-identical dense submission.
            statement: None,
        };
        match client::submit_ai_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: ai run {id}");
                println!("  {}/runs/{id}?track=ai", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// `tock ai-bench --statement canonical-moe`: the same shape of run as
/// [`ai_bench`], against the canonical MoE block mainnet AI miners submit.
///
/// The two differ only in the workload and the target semantics (see
/// `tock::aipow_moe`): the ordinal grind rule replaces the LE8 extranonce rule,
/// and the jackpot is compared against `target · F`, not `target`. Everything
/// downstream — the server window, the MAC-equivalent arithmetic, the board —
/// is shared, which is the point: both statements rank together.
///
/// `backend` chooses where the GRIND runs (CPU template loop or CUDA search).
/// Nothing else moves with it: the target semantics, the scalar recheck, the
/// certificate prover, the window accounting and the submission blob are the
/// same code either way, so a GPU row and a CPU row on the board differ only in
/// the rate they achieved.
async fn ai_bench_moe(
    challenge: Option<String>,
    target: Option<String>,
    tier: Option<u64>,
    k: u64,
    out_dir: Option<PathBuf>,
    submit: Option<String>,
    backend: Backend,
) {
    assert!(k >= 1, "k must be at least 1");
    let registry_ch = match &submit {
        Some(base) => Some(
            client::fetch_ai_challenge(base, client::AiStatement::CanonicalMoe, tier)
                .await
                .unwrap_or_else(|e| panic!("fetch AI challenge: {e}")),
        ),
        None => None,
    };
    note_k_mismatch(registry_ch.as_ref(), tier, k);
    let (challenge, target, k) = match &registry_ch {
        Some(rc) => (
            aipow::parse_hex32(&rc.challenge)
                .unwrap_or_else(|e| panic!("registry challenge: {e}")),
            aipow::parse_hex32(&rc.target).unwrap_or_else(|e| panic!("registry target: {e}")),
            rc.k,
        ),
        None => (
            match &challenge {
                Some(hex) => {
                    aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --challenge: {e}"))
                }
                None => aipow_moe::dev_challenge(),
            },
            match &target {
                Some(hex) => aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --target: {e}")),
                // The tier's own target, derived under THIS statement's scaled
                // semantics (2^(240−a), not the dense 2^(256−a)). Falling back
                // to the loosest target this shape can scale — NOT all-FF,
                // which is fail-closed here (see aipow_moe::max_moe_target).
                None => tier
                    .and_then(|t| client::AiStatement::CanonicalMoe.target_for_attempts(t))
                    .unwrap_or_else(aipow_moe::max_moe_target),
            },
            k,
        ),
    };
    let hw = hardware::detect();
    let ch = aipow_moe::AiMoeChallenge {
        challenge,
        target,
        k,
    };
    // Fail before grinding on a target this shape cannot scale (`T·F` is
    // computed fail-closed upstream). The value itself is reported by
    // `print_tier` below.
    aipow_moe::expected_attempts_per_moe_win(&ch.target)
        .unwrap_or_else(|e| panic!("target is outside the canonical shape's domain: {e}"));

    let summary = match backend {
        #[cfg(feature = "gpu")]
        Backend::Gpu { batch_attempts } => aipow_gpu::run(&ch, batch_attempts),
        Backend::Cpu => aipow_moe::run(&ch),
    };

    println!("tock ai-bench — Logos AI-PoW, canonical MoE block");
    println!("  statement:    {}", aipow_moe::STATEMENT_CANONICAL_MOE);
    println!("  backend:      {}", backend.label());
    println!("  challenge:    {}", aipow::hex32(&ch.challenge));
    // The attempt count is printed by `print_tier` below, under this
    // statement's own scaled reading of the target.
    println!(
        "  target:       {} (x F = 2^16)",
        aipow::hex32(&ch.target)
    );
    print_tier(
        client::AiStatement::CanonicalMoe,
        &ch.target,
        tier,
        registry_ch.as_ref(),
    );
    println!("  nonce rule:   {}", aipow_moe::AI_MOE_NONCE_RULE);
    println!(
        "  hardware:     {} ({} cores)",
        hw.cpu_model.as_deref().unwrap_or("unknown CPU"),
        hw.logical_cores
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    );
    println!(
        "  grind:        {} win(s) in {:.2}s, {} attempts",
        summary.wins.len(),
        summary.grind_elapsed_ms as f64 / 1000.0,
        summary.total_attempts
    );
    println!(
        "  grind rate:   {:.1} attempts/sec ({:.1} MMAC-equiv/s at F=2^16)",
        summary.attempts_per_sec,
        summary.attempts_per_sec * aipow_moe::MAC_EQUIV_PER_MOE_ATTEMPT / 1e6
    );
    let prove_ms: Vec<u64> = summary.wins.iter().map(|w| w.prove_ms).collect();
    let min = prove_ms.iter().min().copied().unwrap_or(0);
    let max = prove_ms.iter().max().copied().unwrap_or(0);
    let mean = prove_ms.iter().sum::<u64>() as f64 / prove_ms.len().max(1) as f64;
    println!("  cert prove:   min {min} ms / mean {mean:.0} ms / max {max} ms (outside window)");
    for w in &summary.wins {
        println!(
            "  win:          ordinal {} — cert {} bytes, proved in {} ms",
            w.ordinal,
            w.cert_bytes.len(),
            w.prove_ms
        );
    }

    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("could not create --out-dir");
        for w in &summary.wins {
            let path = dir.join(format!("moe-cert-{}.bin", w.ordinal));
            std::fs::write(&path, &w.cert_bytes).expect("could not write certificate");
        }
        println!(
            "  certs:        {} written to {}",
            summary.wins.len(),
            dir.display()
        );
    }

    if let (Some(base), Some(rc)) = (&submit, &registry_ch) {
        use base64::Engine;
        let sub = client::AiSubmission {
            nonce: rc.nonce.clone(),
            hardware: backend.hardware_descriptor(&hw),
            prover_version: miner::NOCKCHAIN_PIN.into(),
            grind_elapsed_ms: summary.grind_elapsed_ms,
            wins: summary
                .wins
                .iter()
                .map(|w| client::AiWinSubmission {
                    extranonce: w.ordinal as u64,
                    cert_b64: base64::engine::general_purpose::STANDARD
                        .encode(&w.submission_bytes),
                })
                .collect(),
            statement: Some(aipow_moe::STATEMENT_CANONICAL_MOE),
        };
        match client::submit_ai_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: ai run {id}");
                println!("  {}/runs/{id}?track=ai", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// The dense-peak backend as printed: [`Backend::label`] names the canonical-MoE
/// paths.
fn peak_backend_label(backend: Backend) -> String {
    match backend {
        Backend::Cpu => format!(
            "CPU, {} workers (upstream CpuSearchBackend::search_dense)",
            ai_pow_miner::search::CpuSearchBackend::default_worker_count()
        ),
        #[cfg(feature = "gpu")]
        Backend::Gpu { .. } => format!(
            "CUDA device {} (upstream #159 peak kernel, search only)",
            aipow_gpu::GPU_DEVICE_ORDINAL
        ),
    }
}

/// `tock ai-bench --statement dense-peak`: mainnet's production dense shape
/// (see `tock::aipow_peak`). The same shape of run as [`ai_bench_moe`] —
/// calibrate, mint at a tier, grind to k wins, prove each, submit — with three
/// differences: the calibration needs the 288 MB production matrices and the
/// grind backend, so it happens here rather than in [`resolve_tier`]; the
/// tier range is dense-peak's own (`[2^12, 2^36]`); and its challenges live
/// 3 h, because one certificate costs ~60-76 s on an M1 Max.
async fn ai_bench_peak(
    challenge: Option<String>,
    target: Option<String>,
    attempts: Option<u64>,
    k: u64,
    out_dir: Option<PathBuf>,
    submit: Option<String>,
    backend: Backend,
) {
    assert!(k >= 1, "k must be at least 1");
    let statement = client::AiStatement::DensePeak;
    let matrices = aipow_peak::PeakMatrices::synth();
    let cpu = ai_pow_miner::search::CpuSearchBackend::default();
    #[cfg(feature = "gpu")]
    let gpu = match backend {
        Backend::Gpu { .. } => Some(aipow_peak::gpu::backend(&matrices)),
        Backend::Cpu => None,
    };

    // Tier: --attempts, else (unless --target) calibrate before anything is
    // minted, exactly as resolve_tier does for the other statements.
    let tier = match (attempts, target.is_some()) {
        (Some(requested), _) => {
            let granted = statement.grant_attempts(requested);
            if granted != requested {
                eprintln!(
                    "--attempts {requested} rounded/clamped to the nearest tier: 2^{} ({granted})",
                    granted.ilog2()
                );
            }
            Some(granted)
        }
        (None, true) => None,
        (None, false) => {
            let budget = std::time::Duration::from_secs_f64(aipow::AI_CALIBRATION_SECS);
            let rate = match &backend {
                #[cfg(feature = "gpu")]
                Backend::Gpu { .. } => aipow_peak::gpu::calibrate_attempts_per_sec(
                    &matrices,
                    gpu.as_ref().expect("gpu backend"),
                    budget,
                ),
                Backend::Cpu => {
                    aipow_peak::calibrate_peak_cpu_attempts_per_sec(&matrices, &cpu, budget)
                }
            };
            eprintln!("calibration: {rate:.0} attempts/sec on {}; proving one throwaway certificate…", peak_backend_label(backend));
            let prove_secs = aipow_peak::calibrate_peak_prove_secs(&matrices, &cpu);
            let tier = aipow::tier_for_prove_ratio_with(rate, prove_secs, aipow_peak::grant_peak_attempts);
            let est = tier as f64 * k as f64 / rate.max(1e-9) + prove_secs * k as f64;
            eprintln!(
                "calibration: {prove_secs:.1}s per certificate -> tier 2^{} ({tier} expected \
                 attempts/win), ~{:.0} min for k={k} against a {} h window",
                tier.ilog2(),
                est / 60.0,
                aipow_peak::AI_PEAK_WINDOW_MS / 3_600_000,
            );
            if est * 1000.0 > aipow_peak::AI_PEAK_WINDOW_MS as f64 * 0.8 {
                eprintln!("warning: that is over 80% of the challenge window; Poisson luck may expire it");
            }
            Some(tier)
        }
    };

    let registry_ch = match &submit {
        Some(base) => Some(
            client::fetch_ai_challenge(base, statement, tier)
                .await
                .unwrap_or_else(|e| panic!("fetch AI challenge: {e}")),
        ),
        None => None,
    };
    note_k_mismatch(registry_ch.as_ref(), tier, k);
    let (challenge, target, k) = match &registry_ch {
        Some(rc) => (
            aipow::parse_hex32(&rc.challenge).unwrap_or_else(|e| panic!("registry challenge: {e}")),
            aipow::parse_hex32(&rc.target).unwrap_or_else(|e| panic!("registry target: {e}")),
            rc.k,
        ),
        None => (
            match &challenge {
                Some(hex) => aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --challenge: {e}")),
                None => aipow_peak::dev_challenge(),
            },
            match &target {
                Some(hex) => aipow::parse_hex32(hex).unwrap_or_else(|e| panic!("bad --target: {e}")),
                None => tier
                    .and_then(aipow_peak::peak_target_for_attempts)
                    .unwrap_or_else(aipow_peak::max_peak_target),
            },
            k,
        ),
    };
    aipow_peak::expected_attempts_per_peak_win(&target)
        .unwrap_or_else(|e| panic!("target is outside the dense-peak domain: {e}"));
    let hw = hardware::detect();
    let ch = aipow_peak::AiPeakChallenge { challenge, target, k };
    let summary = match &backend {
        #[cfg(feature = "gpu")]
        Backend::Gpu { .. } => aipow_peak::gpu::run(&ch, &matrices, gpu.as_ref().expect("gpu backend")),
        Backend::Cpu => aipow_peak::run_cpu(&ch, &matrices, &cpu),
    };

    let p = &aipow_peak::AI_PEAK_PARAMS;
    println!("tock ai-bench — Logos AI-PoW, dense-peak (mainnet production shape)");
    println!("  statement:    {}", aipow_peak::STATEMENT_DENSE_PEAK);
    println!(
        "  shape:        m={} k={} n={} r={} tile={} ({} tiles per extranonce)",
        p.m, p.k, p.n, p.noise_rank, p.tile, aipow_peak::PEAK_TICKETS_PER_EXTRANONCE
    );
    println!("  backend:      {}", peak_backend_label(backend));
    println!("  challenge:    {}", aipow::hex32(&ch.challenge));
    println!("  target:       {} (x F = 2^21)", aipow::hex32(&ch.target));
    print_tier(statement, &ch.target, tier, registry_ch.as_ref());
    println!("  nonce rule:   {}", aipow_peak::AI_PEAK_NONCE_RULE);
    println!(
        "  hardware:     {} ({} cores)",
        hw.cpu_model.as_deref().unwrap_or("unknown CPU"),
        hw.logical_cores.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
    );
    println!(
        "  grind:        {} win(s) in {:.2}s, {} attempts",
        summary.wins.len(),
        summary.grind_elapsed_ms as f64 / 1000.0,
        summary.total_attempts
    );
    println!(
        "  grind rate:   {:.1} attempts/sec ({:.3e} MAC-equiv/s at F=2^21)",
        summary.attempts_per_sec,
        summary.attempts_per_sec * aipow_peak::MAC_EQUIV_PER_PEAK_ATTEMPT
    );
    let prove_ms: Vec<u64> = summary.wins.iter().map(|w| w.prove_ms).collect();
    let min = prove_ms.iter().min().copied().unwrap_or(0);
    let max = prove_ms.iter().max().copied().unwrap_or(0);
    let mean = prove_ms.iter().sum::<u64>() as f64 / prove_ms.len().max(1) as f64;
    println!("  cert prove:   min {min} ms / mean {mean:.0} ms / max {max} ms (outside the grind window)");
    for w in &summary.wins {
        println!(
            "  win:          attempt {} — artifact {} bytes, proved in {} ms",
            w.index,
            w.artifact_jam.len(),
            w.prove_ms
        );
    }
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("could not create --out-dir");
        for w in &summary.wins {
            let path = dir.join(format!("peak-artifact-{}.jam", w.index));
            std::fs::write(&path, &w.artifact_jam).expect("could not write artifact");
        }
        println!("  artifacts:    {} written to {}", summary.wins.len(), dir.display());
    }

    if let (Some(base), Some(rc)) = (&submit, &registry_ch) {
        use base64::Engine;
        let sub = client::AiSubmission {
            nonce: rc.nonce.clone(),
            hardware: backend.hardware_descriptor(&hw),
            prover_version: miner::NOCKCHAIN_PIN.into(),
            grind_elapsed_ms: summary.grind_elapsed_ms,
            wins: summary
                .wins
                .iter()
                .map(|w| client::AiWinSubmission {
                    extranonce: w.index,
                    cert_b64: base64::engine::general_purpose::STANDARD.encode(&w.artifact_jam),
                })
                .collect(),
            statement: Some(aipow_peak::STATEMENT_DENSE_PEAK),
        };
        match client::submit_ai_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: ai run {id}");
                println!("  {}/runs/{id}?track=ai", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Which backend grinds a `zk-bench` run. As with [`Backend`], the `Gpu`
/// variant only exists in a `zk-gpu` build, so "this binary cannot grind on a
/// GPU" is reported once, in [`resolve_zk_backend`].
#[derive(Clone, Copy)]
enum ZkBackend {
    Cpu {
        threads: usize,
    },
    #[cfg(feature = "zk-gpu")]
    Gpu {
        batch: u64,
    },
}

impl ZkBackend {
    fn label(self) -> String {
        match self {
            Self::Cpu { threads } => format!("CPU, {threads} thread(s), upstream v5_pow_digest"),
            #[cfg(feature = "zk-gpu")]
            Self::Gpu { batch } => format!(
                "CUDA device {} ({batch} attempts/launch, search only)",
                zkv5_gpu::ZK_GPU_DEVICE
            ),
        }
    }

    fn hardware_descriptor(self, hw: &hardware::Hardware) -> String {
        match self {
            Self::Cpu { .. } => client::hardware_summary(hw),
            #[cfg(feature = "zk-gpu")]
            Self::Gpu { .. } => {
                client::hardware_summary_with_device(zkv5_gpu::device_descriptor().as_deref(), hw)
            }
        }
    }

    fn calibrate(self) -> f64 {
        let budget = std::time::Duration::from_secs_f64(zkv5::ZK_CALIBRATION_SECS);
        match self {
            Self::Cpu { threads } => zkv5::calibrate_cpu_hashes_per_sec(budget, threads),
            #[cfg(feature = "zk-gpu")]
            Self::Gpu { batch } => zkv5_gpu::calibrate_hashes_per_sec(budget, batch),
        }
    }

    fn grind(self, commitment: zkv5::Belts5, target: zkv5::Belts5, k: usize) -> zkv5::GrindResult {
        match self {
            Self::Cpu { threads } => zkv5::grind_cpu(commitment, target, k, threads),
            #[cfg(feature = "zk-gpu")]
            Self::Gpu { batch } => zkv5_gpu::grind(commitment, target, k, batch),
        }
    }
}

fn resolve_zk_backend(gpu: bool, gpu_batch: Option<u64>, threads: Option<usize>) -> ZkBackend {
    if !gpu {
        if gpu_batch.is_some() {
            eprintln!("note: --gpu-batch has no effect without --gpu");
        }
        let threads = threads.unwrap_or_else(zkv5::default_threads).max(1);
        return ZkBackend::Cpu { threads };
    }
    if threads.is_some() {
        eprintln!("note: --threads has no effect with --gpu");
    }
    #[cfg(not(feature = "zk-gpu"))]
    {
        let _ = gpu_batch;
        eprintln!(
            "--gpu requires a CUDA build of tock, which this binary is not: rebuild with \
             `cargo build --release --features zk-gpu` on a machine with nvcc and a CUDA driver"
        );
        std::process::exit(2)
    }
    #[cfg(feature = "zk-gpu")]
    {
        ZkBackend::Gpu {
            batch: gpu_batch.unwrap_or(zkv5_gpu::DEFAULT_ZK_GPU_BATCH),
        }
    }
}

/// `tock zk-bench`: calibrate (before any challenge exists), mint, grind,
/// prove each winner, submit.
async fn zk_bench(
    kernel: PathBuf,
    attempts: Option<u64>,
    k: u64,
    backend: ZkBackend,
    submit: Option<String>,
    prover_version: String,
    out_dir: Option<PathBuf>,
) {
    use base64::Engine;
    assert!(k >= 1, "k must be at least 1");
    let hw = hardware::detect();
    let kernel_bytes = std::fs::read(&kernel)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", kernel.display()));
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("could not create --out-dir");
    }
    // Boot before anything that starts a clock.
    let serf = miner::boot_kernel(kernel_bytes, NOCK_STACK_SIZE_TINY).await;

    // The tier, BEFORE the challenge exists: it is a parameter of the request,
    // and a calibration after the mint would be charged to the server window.
    let tier = match attempts {
        Some(requested) => {
            let granted = zkv5::grant_attempts(requested);
            if granted != requested {
                eprintln!("--attempts {requested} rounded/clamped to the nearest tier: 2^{}", granted.ilog2());
            }
            granted
        }
        None => {
            let rate = backend.calibrate();
            let prove_secs = zkv5::calibrate_prove_secs(&serf).await;
            let tier = zkv5::tier_for_prove_ratio(rate, prove_secs);
            eprintln!(
                "calibration: {rate:.0} hashes/s on {}, {prove_secs:.1}s per proof -> tier 2^{} \
                 (~{:.0}s of grinding vs ~{:.0}s of proving per win)",
                backend.label(),
                tier.ilog2(),
                tier as f64 / rate.max(1e-9),
                prove_secs,
            );
            tier
        }
    };

    let (registry_ch, commitment, target, k) = match &submit {
        Some(base) => {
            let ch = client::fetch_zk_challenge(base, Some(tier))
                .await
                .unwrap_or_else(|e| panic!("could not fetch a ZK challenge: {e}"));
            if ch.attempts != tier {
                eprintln!("note: asked for tier 2^{}, the registry granted 2^{}", tier.ilog2(), ch.attempts.ilog2());
            }
            if ch.k != k {
                eprintln!("note: the registry wants k={} (asked for {k})", ch.k);
            }
            eprintln!(
                "challenge {} from {base} (k={}, tier 2^{}); the clock is running",
                ch.nonce,
                ch.k,
                ch.attempts.ilog2()
            );
            let (c, t, kk) = (ch.commitment, ch.target, ch.k);
            (Some(ch), c, t, kk)
        }
        None => {
            let t = zkv5::target_for_attempts(tier).expect("a granted tier has a target");
            (None, zkv5::commitment_for_nonce(0), t, k)
        }
    };

    eprintln!("grinding {k} win(s) on {}…", backend.label());
    let g = backend.grind(commitment, target, k as usize);
    let grind_secs = g.elapsed.as_secs_f64();
    eprintln!(
        "  {} winner(s) in {:.1}s ({} hashes, {:.0} hashes/s)",
        g.winners.len(),
        grind_secs,
        g.hashes,
        g.hashes as f64 / grind_secs.max(1e-9)
    );

    let mut proofs = Vec::with_capacity(g.winners.len());
    let mut prove_ms = Vec::with_capacity(g.winners.len());
    for (i, w) in g.winners.iter().enumerate() {
        let out = zkv5::prove_win(&serf, &commitment, w, &target)
            .await
            .unwrap_or_else(|e| panic!("winner {i} ({w:?}) did not prove: {e}"));
        eprintln!("  proof {i}: {:.2?} ({} bytes)", out.duration, out.proof_jam.len());
        if let Some(dir) = &out_dir {
            std::fs::write(dir.join(format!("proof-{i}.jam")), &out.proof_jam)
                .expect("could not write proof jam");
        }
        prove_ms.push(out.duration.as_millis() as u64);
        proofs.push(base64::engine::general_purpose::STANDARD.encode(&out.proof_jam));
    }

    let credited = k as f64 * zkv5::expected_attempts_per_win(&target);
    let prove_total = prove_ms.iter().sum::<u64>() as f64 / 1000.0;
    println!("tock zk-bench — ZK v5 (Anthropos): Tip5 hashes of proof object 0, %5 proofs");
    println!("  backend:      {}", backend.label());
    println!("  tier:         2^{} expected hashes/win", zkv5::expected_attempts_per_win(&target).log2().round());
    println!("  wins:         {k}");
    println!("  grind:        {grind_secs:.1}s, {:.0} credited hashes/s", credited / grind_secs.max(1e-9));
    println!("  proving:      {prove_total:.1}s ({:.1}s per win)", prove_total / k as f64);
    println!(
        "  grind+prove:  {:.0} credited hashes/s (the registry's window adds network time)",
        credited / (grind_secs + prove_total).max(1e-9)
    );

    if let (Some(base), Some(ch)) = (&submit, registry_ch) {
        let sub = client::ZkSubmission {
            nonce: ch.nonce,
            hardware: backend.hardware_descriptor(&hw),
            prover_version,
            // Round up: never claim a faster grind than measured.
            grind_elapsed_ms: (grind_secs * 1000.0).ceil().max(1.0) as u64,
            proofs,
        };
        match client::submit_zk_run(base, &sub).await {
            Ok(id) => {
                println!("submitted: run {id}");
                println!("  {}/runs/{id}?track=zk", base.trim_end_matches('/'));
            }
            Err(e) => {
                eprintln!("submission REJECTED or failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn prove(nonce_seed: &str, kernel: &PathBuf, out: &PathBuf, pow_len: u64, header_seed: &str) {
    let kernel_bytes = std::fs::read(kernel)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", kernel.display()));
    let boot_t0 = Instant::now();
    let serf = miner::boot_kernel(kernel_bytes, NOCK_STACK_SIZE_TINY).await;
    eprintln!("kernel boot: {:.2?}", boot_t0.elapsed());

    let header_belts = nonce::seed_to_belts(header_seed, "header");
    let nonce_belts = nonce::seed_to_belts(nonce_seed, "nonce");
    eprintln!("nonce belts: {nonce_belts:?}");

    let result = miner::run_prove(&serf, &header_belts, &nonce_belts, pow_len).await;
    std::fs::write(out, &result.proof_jam).expect("could not write proof jam");

    println!("prove: OK");
    println!("  nonce seed:   {nonce_seed:?}");
    println!("  header seed:  {header_seed:?}");
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
