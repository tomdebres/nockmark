//! Best-effort hardware detection. Everything here is self-reported and
//! labelled as such in the registry; the load-bearing number (proofs/sec)
//! never depends on it.

use std::process::Command;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Hardware {
    pub cpu_model: Option<String>,
    pub logical_cores: Option<u64>,
    pub physical_cores: Option<u64>,
    /// Apple Silicon: performance/efficiency core split, if available.
    pub perf_cores: Option<u64>,
    pub eff_cores: Option<u64>,
    pub mem_bytes: Option<u64>,
    pub os: String,
    pub arch: String,
}

pub fn detect() -> Hardware {
    if cfg!(target_os = "macos") {
        detect_macos()
    } else {
        detect_linux()
    }
}

fn detect_macos() -> Hardware {
    Hardware {
        cpu_model: sysctl("machdep.cpu.brand_string"),
        logical_cores: sysctl("hw.logicalcpu").and_then(|s| s.parse().ok()),
        physical_cores: sysctl("hw.physicalcpu").and_then(|s| s.parse().ok()),
        perf_cores: sysctl("hw.perflevel0.physicalcpu").and_then(|s| s.parse().ok()),
        eff_cores: sysctl("hw.perflevel1.physicalcpu").and_then(|s| s.parse().ok()),
        mem_bytes: sysctl("hw.memsize").and_then(|s| s.parse().ok()),
        os: format!(
            "macOS {}",
            run("sw_vers", &["-productVersion"]).unwrap_or_default()
        ),
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn detect_linux() -> Hardware {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu_model = cpuinfo
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_string());
    let logical_cores = cpuinfo
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count() as u64;
    let mem_bytes = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
                .map(|kb| kb * 1024)
        });
    Hardware {
        cpu_model,
        logical_cores: (logical_cores > 0).then_some(logical_cores),
        physical_cores: None,
        perf_cores: None,
        eff_cores: None,
        mem_bytes,
        os: std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
            })
            .unwrap_or_else(|| std::env::consts::OS.to_string()),
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn sysctl(name: &str) -> Option<String> {
    run("sysctl", &["-n", name])
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}
