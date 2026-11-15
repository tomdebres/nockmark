//! ZK v5 device-vs-oracle probe (M7 step 5, the RTX 4090 anomaly).
//!
//! Live run zk 8 (RTX 4090, tier 2^29) needed 1.81× the expected hashes for
//! its 16 wins. This probe separates bad luck from a device that misses
//! winners, using upstream's own primitives only: the CUDA V5 search
//! (`CudaSession::search`, feature `zk-gpu`) and the CPU oracle
//! (`v5_pow_digest` + `digest_le`).
//!
//! Every subcommand takes the kernel-minted challenge nonce (`--challenge`),
//! so a scan can replay a live run's exact job. Nonces are `[i, 0, 0, 0, 0]`,
//! the order both zk-bench grinders walk. Output is plain text, one record per
//! line, so two scans compare with `diff`.
//!
//! ```text
//! cpu-scan     every winner in each range, by the CPU oracle (many threads)
//! device-scan  every winner in each range, by the device: search, and restart
//!              one past each reported winner (zk-bench's own loop)
//! launches     single device launches at given starts (exact-launch replays)
//! digests      device digest vs CPU digest, nonce by nonce
//! ```
//!
//! Ranges files hold `start len` lines; launches files `start batch` lines;
//! digests files one nonce per line. `#` starts a comment.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use clap::{Parser, Subcommand};
use tock::zkv5::{commitment_for_nonce, nonce_at, target_for_attempts, Belts5, ZK_V5_POW_LEN};
use zk_pow_miner::v5::{digest_le, v5_pow_digest};

#[derive(Parser)]
struct Cli {
    /// The kernel-minted challenge nonce (run zk 8: 2664258700595450701).
    #[arg(long)]
    challenge: u64,
    /// Benchmark tier: a win is a digest ≤ the target for 2^a expected hashes.
    #[arg(long, default_value_t = 20)]
    tier_log2: u32,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Every winner in each range, by the CPU oracle.
    CpuScan {
        #[arg(long)]
        ranges: PathBuf,
        #[arg(long)]
        threads: Option<usize>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Every winner in each range, by the device (search; restart one past
    /// each reported winner). Each winner is re-checked by the CPU oracle.
    DeviceScan {
        #[arg(long)]
        ranges: PathBuf,
        #[arg(long, default_value_t = 262_144)]
        batch: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// One device launch per `start batch` line, reported as found/next.
    Launches {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Device digest vs CPU digest for each nonce.
    Digests {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn read_lines(path: &Path) -> Vec<Vec<u64>> {
    let f = File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    BufReader::new(f)
        .lines()
        .map(|l| l.expect("read"))
        .map(|l| l.split('#').next().unwrap_or("").trim().to_owned())
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.split_whitespace()
                .map(|w| w.parse::<u64>().unwrap_or_else(|e| panic!("{w}: {e}")))
                .collect()
        })
        .collect()
}

fn read_ranges(path: &Path) -> Vec<(u64, u64)> {
    read_lines(path)
        .into_iter()
        .map(|v| {
            assert_eq!(v.len(), 2, "ranges lines are `start len`");
            (v[0], v[1])
        })
        .collect()
}

fn fmt_digest(d: &Belts5) -> String {
    d.iter().map(u64::to_string).collect::<Vec<_>>().join(" ")
}

fn main() {
    let cli = Cli::parse();
    let commitment = commitment_for_nonce(cli.challenge);
    let target = target_for_attempts(1u64 << cli.tier_log2).expect("a granted tier");
    eprintln!(
        "challenge {} commitment [{}] tier 2^{} target [{}]",
        cli.challenge,
        fmt_digest(&commitment),
        cli.tier_log2,
        fmt_digest(&target)
    );
    match cli.cmd {
        Cmd::CpuScan { ranges, threads, out } => {
            cpu_scan(commitment, target, &read_ranges(&ranges), threads, &out)
        }
        Cmd::DeviceScan { ranges, batch, out } => {
            device::scan(commitment, target, &read_ranges(&ranges), batch, &out)
        }
        Cmd::Launches { file, out } => device::launches(commitment, target, &read_lines(&file), &out),
        Cmd::Digests { file, out } => device::digests(commitment, &read_lines(&file), &out),
    }
}

/// Every nonce in each range whose CPU digest meets `target`, written as
/// `range_index nonce` lines in ascending order.
fn cpu_scan(commitment: Belts5, target: Belts5, ranges: &[(u64, u64)], threads: Option<usize>, out: &Path) {
    const CHUNK: u64 = 1 << 16;
    let threads = threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let mut work = Vec::new();
    for (ri, &(start, len)) in ranges.iter().enumerate() {
        let mut s = start;
        while s < start + len {
            let e = (s + CHUNK).min(start + len);
            work.push((ri, s, e));
            s = e;
        }
    }
    let total: u64 = ranges.iter().map(|r| r.1).sum();
    eprintln!("cpu-scan: {} ranges, {total} nonces, {threads} threads", ranges.len());
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::<(usize, u64)>::new());
    let t0 = Instant::now();
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(ri, s, e)) = work.get(i) else { break };
                let mut mine = Vec::new();
                for n in s..e {
                    if digest_le(&v5_pow_digest(commitment, nonce_at(n), ZK_V5_POW_LEN), &target) {
                        mine.push((ri, n));
                    }
                }
                found.lock().unwrap().extend(mine);
                if i % 512 == 0 {
                    eprintln!("  cpu-scan {i}/{} chunks, {:.0} s", work.len(), t0.elapsed().as_secs_f64());
                }
            });
        }
    });
    let mut found = found.into_inner().unwrap();
    found.sort_unstable();
    let secs = t0.elapsed().as_secs_f64();
    let mut w = BufWriter::new(File::create(out).expect("out"));
    writeln!(w, "# cpu-scan nonces {total} threads {threads} secs {secs:.1} rate {:.0}", total as f64 / secs).unwrap();
    for (ri, n) in &found {
        writeln!(w, "{ri} {n}").unwrap();
    }
    eprintln!("cpu-scan: {} winners in {secs:.1} s ({:.0} H/s)", found.len(), total as f64 / secs);
}

#[cfg(feature = "zk-gpu")]
mod device {
    use super::*;
    use zk_pow_miner::cuda::CudaSession;
    use zk_pow_miner::v5::{add_nonce, V5Job};

    fn session() -> CudaSession {
        let s = CudaSession::new(tock::zkv5_gpu::ZK_GPU_DEVICE).expect("CUDA session");
        eprintln!("device {}: {}", s.device(), s.name());
        s
    }

    /// zk-bench's grind loop, restricted to each range: search `batch` (never
    /// past the range end); on a win, record it and restart one past it.
    /// Writes `range_index nonce cpu_ok cpu_digest[0..5]` lines as it goes, so
    /// a long scan's partial output survives.
    pub fn scan(commitment: Belts5, target: Belts5, ranges: &[(u64, u64)], batch: u64, out: &Path) {
        let mut s = session();
        let job = V5Job::new(commitment, target, ZK_V5_POW_LEN).expect("job");
        let mut w = BufWriter::new(File::create(out).expect("out"));
        writeln!(w, "# device-scan batch {batch} device {}", s.name()).unwrap();
        let t0 = Instant::now();
        let (mut launches, mut wins, mut false_pos, mut evaluated) = (0u64, 0u64, 0u64, 0u64);
        let mut last_log = Instant::now();
        for (ri, &(start, len)) in ranges.iter().enumerate() {
            let end = start + len;
            let mut at = start;
            while at < end {
                let n = batch.min(end - at);
                let r = s.search(&job, nonce_at(at), n).expect("CUDA search");
                launches += 1;
                evaluated += r.attempts;
                if r.found {
                    let win = r.next_nonce;
                    assert!(win[1..].iter().all(|&l| l == 0) && win[0] >= at && win[0] < at + n,
                        "winner {win:?} outside the launch [{at}, {})", at + n);
                    let cpu = v5_pow_digest(commitment, win, ZK_V5_POW_LEN);
                    let ok = digest_le(&cpu, &target);
                    if ok { wins += 1 } else { false_pos += 1 }
                    writeln!(w, "{ri} {} {} {}", win[0], u8::from(ok), fmt_digest(&cpu)).unwrap();
                    at = add_nonce(win, 1)[0];
                } else {
                    at += n;
                }
                if last_log.elapsed().as_secs() >= 30 {
                    w.flush().unwrap();
                    last_log = Instant::now();
                    let secs = t0.elapsed().as_secs_f64();
                    eprintln!("  device-scan range {ri} at {at}: {wins} wins, {false_pos} false, \
                               {launches} launches, {secs:.0} s");
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let total: u64 = ranges.iter().map(|r| r.1).sum();
        writeln!(w, "# done wins {wins} false {false_pos} launches {launches} nonces {total} \
                     evaluated {evaluated} secs {secs:.1}").unwrap();
        eprintln!("device-scan: {wins} winners, {false_pos} false positives, {launches} launches, \
                   {total} nonces in {secs:.1} s ({:.0} nonces/s)", total as f64 / secs);
    }

    /// One launch per `start batch` line: `start batch found next attempts`.
    pub fn launches(commitment: Belts5, target: Belts5, lines: &[Vec<u64>], out: &Path) {
        let mut s = session();
        let job = V5Job::new(commitment, target, ZK_V5_POW_LEN).expect("job");
        let mut w = BufWriter::new(File::create(out).expect("out"));
        for v in lines {
            assert_eq!(v.len(), 2, "launch lines are `start batch`");
            let r = s.search(&job, nonce_at(v[0]), v[1]).expect("CUDA search");
            writeln!(w, "{} {} {} {} {}", v[0], v[1], u8::from(r.found), fmt_digest(&r.next_nonce), r.attempts).unwrap();
        }
        eprintln!("launches: {} done", lines.len());
    }

    /// `nonce equal device-digest` per line; a summary line at the end.
    pub fn digests(commitment: Belts5, lines: &[Vec<u64>], out: &Path) {
        let mut s = session();
        let job = V5Job::new(commitment, [0; 5], ZK_V5_POW_LEN).expect("job");
        let mut w = BufWriter::new(File::create(out).expect("out"));
        let mut bad = 0;
        for v in lines {
            let n = nonce_at(v[0]);
            let gpu = s.digest(&job, n).expect("device digest");
            let cpu = v5_pow_digest(commitment, n, ZK_V5_POW_LEN);
            bad += usize::from(gpu != cpu);
            writeln!(w, "{} {} {}", v[0], u8::from(gpu == cpu), fmt_digest(&gpu)).unwrap();
        }
        writeln!(w, "# done nonces {} mismatches {bad}", lines.len()).unwrap();
        eprintln!("digests: {} nonces, {bad} mismatches", lines.len());
    }
}

#[cfg(not(feature = "zk-gpu"))]
mod device {
    use super::*;

    const NEED: &str = "device subcommands need `--features zk-gpu`";

    pub fn scan(_: Belts5, _: Belts5, _: &[(u64, u64)], _: u64, _: &Path) {
        panic!("{NEED}")
    }
    pub fn launches(_: Belts5, _: Belts5, _: &[Vec<u64>], _: &Path) {
        panic!("{NEED}")
    }
    pub fn digests(_: Belts5, _: &[Vec<u64>], _: &Path) {
        panic!("{NEED}")
    }
}
