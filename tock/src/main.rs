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
use tock::{aipow, aipow_moe, client, hardware, miner, nonce};
use tock::miner::DEFAULT_POW_LEN;

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
        /// Registry base URL. When set, fetches a challenge (which supplies
        /// the seed, k, and pow-len — --seed/-k are ignored) and submits the
        /// proof bundle after proving. Without it, bench stays fully local.
        #[arg(long, value_name = "REGISTRY_URL")]
        submit: Option<String>,
        /// Prover version string recorded on submission (the nockchain
        /// commit the kernel jams were built from).
        #[arg(long, default_value = tock::miner::NOCKCHAIN_PIN)]
        prover_version: String,
    },
    /// Benchmark the Logos AI-PoW puzzle: grind attempts, prove a compact
    /// recursive certificate per jackpot win. `--statement` picks which AI-PoW
    /// statement to benchmark; both rank on the one AI board, in
    /// MAC-equivalents per second. `--gpu` moves the canonical-MoE grind (and
    /// nothing else) onto CUDA.
    AiBench {
        /// Which statement to benchmark: `dense` (M5, single-tile) or
        /// `canonical-moe` (the block mainnet AI miners actually run).
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
        /// --target, ai-bench measures this machine for ~2 s and picks a tier
        /// sized for ~60 s of grinding across all k wins — which is what lets
        /// fast hardware be measured on its grind rate instead of on the fixed
        /// certificate-proving cost.
        #[arg(long)]
        attempts: Option<u64>,
        /// Jackpot wins to find (each win costs a ~24 s certificate prove).
        /// Also what the self-calibrated tier is sized for; with --submit the
        /// registry's own k takes over for the run itself, so pass the
        /// registry's k here if you want the grind budget to land exactly.
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
        /// canonical-moe` only). Requires a build with the `gpu` feature:
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
            }
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
    if statement != client::AiStatement::CanonicalMoe {
        eprintln!(
            "--gpu is implemented for --statement {} only (asked for {:?}); \
             the dense statement has no validated CUDA path",
            aipow_moe::STATEMENT_CANONICAL_MOE,
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
/// the ~2 s of calibration is charged to nothing. `--attempts` skips the
/// measurement entirely.
///
/// The heuristic is one line — measure, then size the grind for
/// [`aipow::AI_TIER_GRIND_BUDGET_SECS`] across all k wins — and the reason it
/// is safe to let the client run it is that the tier is not a claim. The
/// registry re-derives the target from the tier, verifies every win against it,
/// and credits exactly the attempts that target implies. A machine that
/// overestimates itself simply grinds longer; one that underestimates itself
/// reports a looser lower bound than it deserved.
fn resolve_tier(
    statement: client::AiStatement,
    backend: Backend,
    attempts: Option<u64>,
    explicit_target: bool,
    k: u64,
) -> Option<u64> {
    if let Some(requested) = attempts {
        let granted = aipow::grant_attempts(requested);
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
    };
    let tier = aipow::tier_for_rate(rate, k);
    eprintln!(
        "calibration: {rate:.0} attempts/sec measured over {:.0}s on {} \
         -> tier 2^{} ({tier} expected attempts/win, ~{:.0}s of grinding for k={k})",
        aipow::AI_CALIBRATION_SECS,
        backend.label(),
        tier.ilog2(),
        tier as f64 * k as f64 / rate.max(1e-9),
    );
    Some(tier)
}

/// Warn when the registry's k differs from the one the tier was sized for.
///
/// The tier has to be chosen before the challenge exists (it is a parameter OF
/// the challenge request), so with `--submit` it is sized for the local `-k`.
/// If the registry wants a different k the grind simply scales — the run is
/// still correct, just longer or shorter than the budget — so this is a note,
/// not an error.
fn note_k_mismatch(registry_ch: Option<&client::AiRegistryChallenge>, tier: Option<u64>, k: u64) {
    let Some(rc) = registry_ch else { return };
    if rc.k == k || tier.is_none() {
        return;
    }
    eprintln!(
        "note: the tier was sized for k={k} but the registry wants k={}; \
         expect the grind to take {:.1}x the ~{:.0}s budget",
        rc.k,
        rc.k as f64 / k as f64,
        aipow::AI_TIER_GRIND_BUDGET_SECS,
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

    println!("tock ai-bench — Logos AI-PoW, canonical single-tile shape (CPU reference)");
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
