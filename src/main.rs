use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use fs2::FileExt;
use print as dr_print;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
#[cfg(test)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::sleep;

use dracon_system_lib::analyze_workspace_storage;

// Re-export policy items (types and utility fns that live in policy.rs)
// Note: GuardRuntimeState, ProcSample, AutoCleanupResult are in main.rs,
// so tests use crate::* to access them without explicit re-exports.
mod doctor;
pub(crate) use doctor::*;
mod events;
pub(crate) use events::*;
mod links;
pub(crate) use links::*;
mod policy;
pub(crate) use policy::*;
mod safety;
pub(crate) use safety::*;
mod zram;
pub(crate) use zram::*;

#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod links_tests;

#[cfg(test)]
const TEST_PROTECTED: &[&str] = &[
    "/", "/home", "/etc", "/usr", "/var", "/boot", "/nix", "/run", "/sys", "/dev", "/proc",
];

#[cfg(test)]
pub(crate) fn check_path_str(path: &str, user_protected: &[&str]) -> bool {
    let normalized = if path.ends_with('/') && path != "/" {
        path.trim_end_matches('/')
    } else {
        path
    };
    for prot in TEST_PROTECTED {
        if is_protected_ancestor(normalized, prot) {
            return false;
        }
    }
    for prot in user_protected {
        if is_protected_ancestor(normalized, prot) {
            return false;
        }
    }
    true
}

static VERBOSITY: AtomicU8 = AtomicU8::new(0);

#[macro_export]
macro_rules! veprintln {
    ($lvl:expr, $($arg:tt)*) => {
        if $lvl <= VERBOSITY.load(Ordering::SeqCst) {
            eprintln!($($arg)*);
        }
    };
}

pub(crate) fn acquire_daemon_lock(name: &str) -> Result<File> {
    let lock_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".dracon")
        .join("locks");

    std::fs::create_dir_all(&lock_dir)?;
    let lock_file = lock_dir.join(format!("{}.lock", name));

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&lock_file)?;

    if file.lock_exclusive().is_err() {
        return Err(anyhow::anyhow!("lock file is held by another process"));
    }

    file.set_len(0)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(file)
}

#[derive(Parser, Debug)]
#[command(name = "dracon-system")]
#[command(about = "Disk/process guard, storage analyzer, and system diagnostics")]
#[command(version)]
struct Cli {
    /// Increase output verbosity. Can be repeated up to 2 times (-v, -vv).
    #[arg(global = true, short, long, action = ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show core path and service status.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run deterministic diagnostics for canonical dracon setup.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Fail non-zero on any warning (normally warnings are non-fatal).
        #[arg(long)]
        strict: bool,
    },
    /// Show recent events from the shared event stream.
    Events {
        /// Number of recent events to show.
        #[arg(short, long, default_value = "50")]
        tail: usize,
        /// Filter by source domain (e.g. system, warden, sync).
        #[arg(long)]
        source: Option<String>,
        /// Filter by severity (info, warn, error, critical).
        #[arg(short, long)]
        severity: Option<String>,
        /// Deduplicate consecutive identical events.
        #[arg(long)]
        dedup: bool,
        /// Output as JSON (raw JSONL, one per line).
        #[arg(long)]
        json: bool,
    },
    /// Analyze storage hotspots and optionally clean safe build/cache dirs.
    Storage {
        /// Optional root path to analyze. Defaults to policy or ~/Dev.
        root: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// List cleanup targets without modifying anything.
        #[arg(long)]
        cleanup: bool,
        /// Execute cleanup (delete files, empty trash).
        #[arg(long)]
        apply: bool,
        /// Also remove directories tracked by git (target/, node_modules/).
        #[arg(long)]
        allow_tracked: bool,
        /// Minimum file size to consider (MiB). [default: 50]
        #[arg(long)]
        min_size_mb: Option<u64>,
        /// Comma-separated kinds to clean (targets, trash, nix, caches, node_modules, docker).
        #[arg(long)]
        kinds: Option<String>,
    },
    /// Manage deterministic symlink ownership for system setup.
    Link {
        #[command(subcommand)]
        cmd: LinkCommands,
    },
    /// Scan filesystem for broken symlinks (report-only).
    Symlinks {
        /// Optional root paths to scan. Defaults to ~/Dev, ~/.dracon, ~/.local/bin, ~/.config.
        roots: Vec<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Maximum depth to descend (default: 4).
        #[arg(long, default_value_t = 4)]
        max_depth: usize,
    },
    /// Zram management: show stats and generate NixOS config for tuning.
    Zram {
        /// Show current zram statistics.
        #[arg(long, default_value = "false")]
        status: bool,
        /// Generate NixOS configuration for larger zram swap.
        #[arg(long)]
        gen_config: bool,
        /// Target memory percent for zram (e.g., 200 for 2x RAM).
        #[arg(long)]
        memory_percent: Option<u32>,
        /// Compression algorithm (lzo, lz4, lz4hc, zstd).
        #[arg(long)]
        algorithm: Option<String>,
    },
    /// Guard runtime: monitor disk/process pressure and notify/mitigate.
    Guard {
        #[command(subcommand)]
        cmd: GuardCommands,
    },
}

#[derive(Subcommand, Debug)]
enum LinkCommands {
    /// Show link reconciliation status from policy.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose link drift and invalid targets.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Apply link policy by creating/fixing symlinks.
    Apply {
        #[arg(long)]
        json: bool,
        /// Replace non-symlink paths at link locations (backs up existing content first).
        #[arg(long)]
        force_replace: bool,
    },
}

#[derive(Subcommand, Debug)]
enum GuardCommands {
    /// Run one guard evaluation pass.
    Once {
        #[arg(long)]
        json: bool,
    },
    /// Run continuous guard loop.
    Daemon,
    /// Prune system caches and Docker resources.
    Prune {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        docker: bool,
        #[arg(long)]
        docker_volumes: bool,
        #[arg(long)]
        package_caches: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Clean all reclaimable space (targets, trash, nix, caches, node_modules).
    Clean {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        rust: bool,
        #[arg(long)]
        trash: bool,
        #[arg(long)]
        nix: bool,
        #[arg(long)]
        caches: bool,
        #[arg(long)]
        node_modules: bool,
        #[arg(long)]
        docker: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        min_size_mb: Option<u64>,
    },
}

#[derive(Debug, Serialize)]
struct StatusReport {
    system_root: String,
    nixos_root: String,
    sync_policy: String,
    system_policy: String,
    system_policy_exists: bool,
    sync_service_active: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct DoctorReport {
    pub(crate) system_root_exists: bool,
    pub(crate) nixos_root_exists: bool,
    pub(crate) canonical_libs_exists: bool,
    pub(crate) canonical_utils_exists: bool,
    pub(crate) sync_policy_exists: bool,
    pub(crate) legacy_config_dracon_exists: bool,
    pub(crate) sync_service_active: bool,
}

impl DoctorReport {
    fn all_ok(&self) -> bool {
        self.system_root_exists
            && self.nixos_root_exists
            && self.canonical_libs_exists
            && self.canonical_utils_exists
            && self.sync_policy_exists
            && !self.legacy_config_dracon_exists
            && self.sync_service_active
    }
}

#[derive(Debug, Clone)]
struct CleanupConfig {
    apply: bool,
    allow_tracked: bool,
    min_size_mb: u64,
    kinds: HashSet<String>,
}

#[derive(Debug, Serialize)]
struct GuardProcessAlert {
    pid: i32,
    ppid: i32,
    command: String,
    args: String,
    cpu_percent: f32,
    rss_mb: u64,
    sustained_secs: u64,
    action: String,
    nice_value: i32,
}

#[derive(Debug, Serialize)]
pub(crate) struct GuardReport {
    enabled: bool,
    disk_use_percent: u8,
    disk_state: String,
    sync_frozen: bool,
    alerts: Vec<GuardProcessAlert>,
    /// ADDED 2026-08-10 (v0.112.35): memory/swap pressure snapshot.
    memory: Option<MemoryReport>,
    /// ADDED 2026-08-10 (v0.112.35): zombie detail (pid/ppid/age/parent).
    zombies: Vec<ZombieInfo>,
    /// ADDED 2026-08-10 (v0.112.35): sustained disk fill rate (GiB/hour).
    disk_fill_gbph: Option<f64>,
}

/// Memory/swap pressure snapshot for the guard report (JSON + table).
#[derive(Debug, Serialize)]
pub(crate) struct MemoryReport {
    pub(crate) mem_available_percent: u8,
    pub(crate) swap_used_percent: u8,
    /// PSI `full avg10` — the share of the last 10s that the system
    /// was completely stalled on memory (swap thrash detector).
    pub(crate) psi_full_avg10: Option<f64>,
    /// Swap-in rate (pages/s) — fallback thrash signal when PSI is off.
    pub(crate) pswpin_rate: Option<f64>,
    /// "ok" | "warn" | "critical"
    pub(crate) pressure: String,
    /// Top RSS offenders for diagnostics and optional pressure mitigation.
    pub(crate) top_rss: Vec<ProcSample>,
    /// ADDED 2026-08-10 (v0.112.36): actions taken this pass, e.g.
    /// "renice svelte-check=10", "oom-bias node=250".
    pub(crate) limited: Vec<String>,
}

/// A zombie (defunct) process with context useful for diagnosis.
#[derive(Debug, Serialize)]
pub(crate) struct ZombieInfo {
    pub(crate) pid: i32,
    pub(crate) ppid: i32,
    pub(crate) comm: String,
    pub(crate) parent_comm: String,
    /// Seconds since first seen in Z state by this guard process.
    pub(crate) age_secs: u64,
    /// Whether the parent is still alive (dead parent => init reaps).
    pub(crate) parent_alive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    /// `/proc/<pid>/comm`, retained for diagnostics and the ps-row check.
    pub(crate) comm: String,
    /// `/proc/<pid>/stat` field 22; stable for the lifetime of a process.
    pub(crate) starttime: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcSample {
    pub(crate) pid: i32,
    pub(crate) ppid: i32,
    pub(crate) cpu_percent: f32,
    pub(crate) rss_mb: u64,
    /// Current Unix nice value from `ps ni`, captured before any limiter
    /// changes it so memory-pressure release can restore the original value.
    pub(crate) nice: i32,
    pub(crate) command: String,
    pub(crate) args: String,
    pub(crate) starttime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryReniceState {
    pub(crate) original_nice: i32,
    pub(crate) applied_nice: i32,
    pub(crate) identity: ProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OomPendingDescendant {
    pub(crate) root_pid: i32,
    pub(crate) original_adj: i32,
    pub(crate) identity: ProcessIdentity,
}

#[derive(Default, Debug)]
pub(crate) struct GuardRuntimeState {
    /// CHANGED 2026-07-21 (v0.112.33, audit M34/F4.8): value is now
    /// `(first_seen, proc_starttime)` — the /proc/<pid>/stat
    /// starttime is recorded at first sight and re-verified before
    /// any renice, closing the PID-reuse window (a PID recycled
    /// during the sustain window gets a DIFFERENT starttime and is
    /// skipped).
    pub(crate) heavy_since: HashMap<i32, (Instant, u64)>,
    pub(crate) notify_cooldowns: HashMap<String, Instant>,
    pub(crate) last_disk_state: String,
    pub(crate) disk_history: Vec<(Instant, u8)>,
    /// ADDED 2026-08-10 (v0.112.35): byte-precise df history for
    /// rapid-fill rate detection (percent deltas are too coarse on
    /// large disks: 1% of 907 GiB is ~9 GiB).
    pub(crate) disk_bytes_history: Vec<(Instant, u64)>,
    /// ADDED 2026-08-10 (v0.112.35): first-sight timestamps for
    /// zombie processes so alerts can report zombie age.
    pub(crate) zombies_since: HashMap<i32, Instant>,
    /// ADDED 2026-08-10 (v0.112.35): last pswpin/pswpout counters
    /// for the swap-thrash fallback when PSI is unavailable.
    pub(crate) prev_swap_counters: Option<(Instant, u64, u64)>,
    /// ADDED 2026-08-10 (v0.112.36): pids reniced by the memory-
    /// pressure limiter (original/applied nice, stable process identity),
    /// and their release timers.
    pub(crate) memory_reniced_pids: HashMap<i32, MemoryReniceState>,
    pub(crate) memory_cooled_since: HashMap<i32, Instant>,
    /// ADDED 2026-08-10 (v0.112.36): pids whose oom_score_adj was
    /// raised under critical pressure (original adjustment, stable
    /// process identity), and their restore timers.
    pub(crate) oom_biased_pids: HashMap<i32, (i32, ProcessIdentity)>,
    /// ADDED 2026-08-11 (audit LOW): process incarnations that already
    /// descended from each biased pid when the bias was applied. Children
    /// created afterwards inherit the target adjustment and are swept back
    /// to the root's original value instead of remaining stranded at 250.
    pub(crate) oom_known_descendants: HashMap<i32, HashSet<(i32, u64)>>,
    /// Descendant restorations that could not yet be verified or written.
    /// Root bias entries stay alive while any child incarnation is pending.
    pub(crate) oom_pending_descendants: HashMap<(i32, u64), OomPendingDescendant>,
    pub(crate) oom_cooled_since: HashMap<i32, Instant>,
    /// ADDED 2026-08-10 (v0.112.36): pids inside a transient
    /// CPUQuota scope (scope_name, original cgroup, stable identity),
    /// and their release timers.
    pub(crate) capped_pids: HashMap<i32, (String, String, ProcessIdentity)>,
    pub(crate) cap_cooled_since: HashMap<i32, Instant>,
    pub(crate) active_build_pids: HashSet<i32>,
    pub(crate) reniced_pids: HashMap<i32, (i32, ProcessIdentity)>,
    pub(crate) cooled_since: HashMap<i32, Instant>,
    pub(crate) guard_cycle: u64,
    pub(crate) last_proactive_cleanup: Option<Instant>,
}

/// Information about a Rust target directory for cleanup consideration
#[derive(Debug, Clone)]
struct TargetDirInfo {
    path: PathBuf,
    bytes: u64,
    mtime_secs_ago: u64,
}

/// Result of automatic cleanup operation
#[derive(Debug, Serialize, Default)]
struct AutoCleanupResult {
    pub(crate) cleaned_count: usize,
    pub(crate) reclaimed_bytes: u64,
    pub(crate) cleaned_paths: Vec<String>,
    pub(crate) protected_paths: Vec<String>,
}

pub(crate) fn parse_df_use_percent(output: &str) -> Option<u8> {
    output
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
        .and_then(|v| v.trim_end_matches('%').parse::<u8>().ok())
}

/// Parsed disk usage details from `df -P` output.
pub(crate) struct DiskDetails {
    pub(crate) total_bytes: u64,
    pub(crate) used_bytes: u64,
    pub(crate) avail_bytes: u64,
    pub(crate) use_percent: u8,
    pub(crate) mount: String,
}

pub(crate) fn parse_df_details(output: &str) -> Option<DiskDetails> {
    let line = output.lines().nth(1)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    let total_bytes = parts[1].parse::<u64>().ok()? * 1024;
    let used_bytes = parts[2].parse::<u64>().ok()? * 1024;
    let avail_bytes = parts[3].parse::<u64>().ok()? * 1024;
    let use_percent = parts[4].trim_end_matches('%').parse::<u8>().ok()?;
    let mount = parts[5].to_string();
    Some(DiskDetails {
        total_bytes,
        used_bytes,
        avail_bytes,
        use_percent,
        mount,
    })
}

async fn disk_details_for(path: &str) -> Result<DiskDetails> {
    let out = Command::new("df").args(["-P", path]).output().await?;
    if !out.status.success() {
        return Err(anyhow::anyhow!("df command failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_df_details(&text).ok_or_else(|| anyhow::anyhow!("failed parsing df output"))
}

pub(crate) fn parse_ps_output(output: &str) -> Vec<ProcSample> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Format: pid ppid pcpu rss nice comm args...
            let mut parts = trimmed.split_whitespace();
            let pid = parts.next()?.parse::<i32>().ok()?;
            let ppid = parts.next()?.parse::<i32>().ok()?;
            let cpu_percent = parts.next()?.parse::<f32>().ok()?;
            let rss_kb = parts.next()?.parse::<u64>().ok()?;
            let nice = parts.next()?.parse::<i32>().ok()?;
            let command = parts.next()?.to_string();
            let args = parts.collect::<Vec<_>>().join(" ");
            Some(ProcSample {
                pid,
                ppid,
                cpu_percent,
                rss_mb: rss_kb / 1024,
                nice,
                command,
                args,
                starttime: 0,
            })
        })
        .collect()
}

/// Parsed /proc/meminfo values (all in kB).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemorySample {
    pub(crate) mem_total_kb: u64,
    pub(crate) mem_available_kb: u64,
    pub(crate) swap_total_kb: u64,
    pub(crate) swap_free_kb: u64,
}

impl MemorySample {
    pub(crate) fn mem_available_percent(&self) -> u8 {
        if self.mem_total_kb == 0 {
            return 0;
        }
        (self.mem_available_kb.saturating_mul(100) / self.mem_total_kb).min(100) as u8
    }

    pub(crate) fn swap_used_percent(&self) -> u8 {
        if self.swap_total_kb == 0 {
            return 0;
        }
        (self
            .swap_total_kb
            .saturating_sub(self.swap_free_kb)
            .saturating_mul(100)
            / self.swap_total_kb)
            .min(100) as u8
    }
}

/// Parse /proc/meminfo into a MemorySample.
pub(crate) fn parse_meminfo(output: &str) -> Option<MemorySample> {
    let mut mem_total_kb = 0u64;
    let mut mem_available_kb = 0u64;
    let mut swap_total_kb = 0u64;
    let mut swap_free_kb = 0u64;
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next()?;
        let value: u64 = parts.next()?.parse().ok()?;
        match key {
            "MemTotal:" => mem_total_kb = value,
            "MemAvailable:" => mem_available_kb = value,
            "SwapTotal:" => swap_total_kb = value,
            "SwapFree:" => swap_free_kb = value,
            _ => {}
        }
    }
    if mem_total_kb == 0 {
        return None;
    }
    Some(MemorySample {
        mem_total_kb,
        mem_available_kb,
        swap_total_kb,
        swap_free_kb,
    })
}

async fn memory_sample() -> Option<MemorySample> {
    let content = tokio::fs::read_to_string("/proc/meminfo").await.ok()?;
    parse_meminfo(&content)
}

/// Parse /proc/pressure/memory. Returns Some((full_avg10, some_avg10)).
pub(crate) fn parse_pressure_memory(output: &str) -> Option<(f64, f64)> {
    let mut full = None;
    let mut some = None;
    for line in output.lines() {
        if line.starts_with("full") && full.is_none() {
            full = line
                .split_whitespace()
                .find_map(|t| t.strip_prefix("avg10="))
                .and_then(|v| v.parse::<f64>().ok());
        } else if line.starts_with("some") && some.is_none() {
            some = line
                .split_whitespace()
                .find_map(|t| t.strip_prefix("avg10="))
                .and_then(|v| v.parse::<f64>().ok());
        }
    }
    Some((full?, some?))
}

/// PSI `full avg10` — 0..=100 (percent of the last 10s fully stalled).
async fn psi_full_avg10() -> Option<f64> {
    let content = tokio::fs::read_to_string("/proc/pressure/memory")
        .await
        .ok()?;
    parse_pressure_memory(&content).map(|(full, _)| full)
}

/// Read pswpin/pswpout counters from /proc/vmstat.
pub(crate) fn parse_vmstat_swap(output: &str) -> Option<(u64, u64)> {
    let mut pin = None;
    let mut pout = None;
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next()?;
        let value: u64 = parts.next()?.parse().ok()?;
        match key {
            "pswpin" => pin = Some(value),
            "pswpout" => pout = Some(value),
            _ => {}
        }
    }
    Some((pin?, pout?))
}

async fn vmstat_swap_counters() -> Option<(u64, u64)> {
    let content = tokio::fs::read_to_string("/proc/vmstat").await.ok()?;
    parse_vmstat_swap(&content)
}

/// Parse one /proc/<pid>/stat line for zombie detection.
/// Returns (pid, comm, ppid, starttime) when state == 'Z'.
pub(crate) fn parse_proc_stat_zombie(line: &str) -> Option<(i32, String, i32, u64)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }
    let pid = line[..open].trim().parse::<i32>().ok()?;
    let comm = line[open + 1..close].to_string();
    let rest: Vec<&str> = line[close + 2..].split_whitespace().collect();
    if *rest.first()? != "Z" {
        return None;
    }
    let ppid = rest.get(1)?.parse::<i32>().ok()?;
    // starttime is field 22 overall; after stripping pid+comm the
    // remaining list starts at field 3 (state), so index 19.
    let starttime = rest.get(19)?.parse::<u64>().ok()?;
    Some((pid, comm, ppid, starttime))
}

/// Credential-signal filename check for the trash guard (and any
/// future bulk-delete protection). Mirrors the pattern list in
/// docs/design/disk-full-credentials-2026-08-10.md section 5.
pub(crate) fn looks_credential_like(name: &str) -> bool {
    let lower = name.to_lowercase();
    const SUBSTR: &[&str] = &[
        "chrome",
        "chromium",
        "credential",
        "password",
        "secret",
        "token",
        "keyring",
        "login data",
        "hosts.yml",
        ".git-credentials",
        ".npmrc",
        ".netrc",
        "cookie",
    ];
    const SUFFIX: &[&str] = &[".env", ".pem", ".key", ".age", ".p12", ".pfx"];
    SUBSTR.iter().any(|s| lower.contains(s)) || SUFFIX.iter().any(|s| lower.ends_with(s))
}

/// Sustained disk fill rate in GiB/hour from a byte-precise df
/// history. Returns None until at least 3 samples spanning 60s of
/// wall time exist, or when the disk is not filling.
pub(crate) fn disk_fill_rate_gbph(history: &[(Instant, u64)]) -> Option<f64> {
    let n = history.len().min(30);
    if n < 3 {
        return None;
    }
    let recent = &history[history.len() - n..];
    let t0 = recent[0].0;
    let b0 = recent[0].1 as f64;
    let span = recent.last()?.0.duration_since(t0).as_secs_f64();
    if span < 60.0 {
        return None;
    }
    let mut s_xy = 0.0;
    let mut s_xx = 0.0;
    for &(t, b) in recent {
        let x = t.duration_since(t0).as_secs_f64();
        let y = b as f64 - b0;
        s_xy += x * y;
        s_xx += x * x;
    }
    if s_xx <= 0.0 {
        return None;
    }
    let slope_bps = s_xy / s_xx;
    if slope_bps <= 0.0 {
        return None;
    }
    Some(slope_bps * 3600.0 / (1024.0 * 1024.0 * 1024.0))
}

async fn disk_use_percent_for(path: &str) -> Result<u8> {
    let out = Command::new("df").args(["-P", path]).output().await?;
    if !out.status.success() {
        return Err(anyhow::anyhow!("df command failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_df_use_percent(&text).ok_or_else(|| anyhow::anyhow!("failed parsing df output"))
}

async fn process_samples() -> Result<Vec<ProcSample>> {
    let out = Command::new("ps")
        .args(["-eo", "pid,ppid,pcpu,rss,ni,comm,args", "--no-headers"])
        .output()
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "ps spawn failed: {} (is /run/current-system/sw/bin on PATH?)",
                e
            )
        })?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "ps command failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let parsed = parse_ps_output(&String::from_utf8_lossy(&out.stdout));
    // ADDED 2026-07-21 (v0.112.33, audit M34/F4.8): verify each
    // sample's pid/comm pair out-of-band via /proc/<pid>/comm. A
    // process can embed `\n` in its argv, and `ps -eo ... args`
    // prints argv raw — `parse_ps_output` then treats the injected
    // text as additional rows, letting a local process FABRICATE a
    // heavy-process sample for an arbitrary victim PID (which the
    // guard would then renice). The injected row's pid/comm pair
    // doesn't match a real process, so this filter kills it. Rows
    // for just-exited PIDs are dropped the same way. Preserve the
    // verified starttime in the sample so any later adjustment is
    // tied to this exact PID incarnation.
    Ok(parsed
        .into_iter()
        .filter_map(
            |mut p| match read_proc_identity(Path::new("/proc"), p.pid) {
                Ok(identity) if identity.comm == p.command => {
                    p.starttime = identity.starttime;
                    Some(p)
                }
                _ => None,
            },
        )
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessIdentityStatus {
    Match,
    Gone,
    Mismatch,
    Unavailable,
}

/// Read a process's identity out-of-band: `/proc/<pid>/comm` (name,
/// 15-char-truncated like ps's comm column) and `/proc/<pid>/stat` field 22
/// (starttime, for the PID-reuse check).
fn read_proc_identity(root: &Path, pid: i32) -> std::io::Result<ProcessIdentity> {
    let comm = std::fs::read_to_string(root.join(pid.to_string()).join("comm"))?
        .trim()
        .to_string();
    let stat = std::fs::read_to_string(root.join(pid.to_string()).join("stat"))?;
    // /proc/<pid>/stat field 22 is starttime. The comm field (2) can
    // contain spaces/parens, so split after the LAST ')'.
    let after_comm = stat
        .rsplit_once(')')
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed /proc stat")
        })?
        .1;
    let starttime = after_comm
        .split_whitespace()
        .nth(19) // field 22 - field 3 (0-indexed after comm)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing starttime"))?
        .parse::<u64>()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid starttime"))?;
    Ok(ProcessIdentity { comm, starttime })
}

/// `/proc/self` is a mount-availability marker: unlike a tracked PID,
/// it exists whenever the proc filesystem is mounted and readable. A missing
/// root or marker is therefore indeterminate, not evidence that the tracked
/// process exited.
fn proc_root_is_available(root: &Path) -> bool {
    matches!(
        std::fs::metadata(root),
        Ok(meta) if meta.is_dir()
    ) && std::fs::metadata(root.join("self")).is_ok()
}

fn process_identity_status(
    root: &Path,
    pid: i32,
    expected: &ProcessIdentity,
) -> ProcessIdentityStatus {
    if !proc_root_is_available(root) {
        return ProcessIdentityStatus::Unavailable;
    }
    match read_proc_identity(root, pid) {
        Ok(actual) if actual.starttime == expected.starttime => ProcessIdentityStatus::Match,
        Ok(_) => ProcessIdentityStatus::Mismatch,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing comm/stat file while the PID directory remains is a
            // partial/unavailable proc read. Only a missing PID directory in
            // an available proc tree proves that the process is gone.
            match std::fs::metadata(root.join(pid.to_string())) {
                Err(dir_error) if dir_error.kind() == std::io::ErrorKind::NotFound => {
                    ProcessIdentityStatus::Gone
                }
                _ => ProcessIdentityStatus::Unavailable,
            }
        }
        Err(_) => ProcessIdentityStatus::Unavailable,
    }
}

fn process_sample_identity(sample: &ProcSample) -> ProcessIdentity {
    ProcessIdentity {
        comm: sample.command.clone(),
        starttime: sample.starttime,
    }
}

fn process_sample_is_current(sample: &ProcSample) -> bool {
    matches!(
        process_identity_status(
            Path::new("/proc"),
            sample.pid,
            &process_sample_identity(sample)
        ),
        ProcessIdentityStatus::Match
    )
}

/// Return whether `pid` is a descendant of `root_pid` in a point-in-time
/// process table. The bounded walk tolerates a process disappearing between
/// `ps` and this check and cannot loop forever on malformed/cyclic fixtures.
fn process_is_descendant_of(pid: i32, root_pid: i32, parent_by_pid: &HashMap<i32, i32>) -> bool {
    if pid == root_pid {
        return false;
    }
    let mut current = pid;
    let mut seen = HashSet::new();
    for _ in 0..1024 {
        let Some(parent) = parent_by_pid.get(&current).copied() else {
            return false;
        };
        if parent == root_pid {
            return true;
        }
        if parent <= 0 || !seen.insert(current) {
            return false;
        }
        current = parent;
    }
    false
}

fn process_descendant_samples(samples: &[ProcSample], root_pid: i32) -> Vec<ProcSample> {
    let parent_by_pid: HashMap<i32, i32> = samples
        .iter()
        .map(|sample| (sample.pid, sample.ppid))
        .collect();
    samples
        .iter()
        .filter(|sample| process_is_descendant_of(sample.pid, root_pid, &parent_by_pid))
        .cloned()
        .collect()
}

fn nearest_biased_ancestor(
    pid: i32,
    parent_by_pid: &HashMap<i32, i32>,
    biased_roots: &HashSet<i32>,
) -> Option<i32> {
    let mut current = pid;
    let mut seen = HashSet::new();
    for _ in 0..1024 {
        let parent = parent_by_pid.get(&current).copied()?;
        if biased_roots.contains(&parent) {
            return Some(parent);
        }
        if parent <= 0 || !seen.insert(current) {
            return None;
        }
        current = parent;
    }
    None
}

#[cfg(test)]
fn oom_descendant_candidates(
    samples: &[ProcSample],
    root_pid: i32,
    known_descendants: &HashSet<(i32, u64)>,
    tracked_pids: &HashSet<i32>,
    exempt_names: &HashSet<String>,
) -> Vec<ProcSample> {
    process_descendant_samples(samples, root_pid)
        .into_iter()
        .filter(|sample| {
            !known_descendants.contains(&(sample.pid, sample.starttime))
                && !tracked_pids.contains(&sample.pid)
                && !exempt_names.contains(&sample.command)
                && !is_kernel_process(&sample.command)
        })
        .collect()
}

pub(crate) fn disk_state(used: u8, guard: &GuardPolicy) -> &'static str {
    if used >= guard.disk_critical_percent {
        "critical"
    } else if used >= guard.disk_action_percent {
        "action"
    } else if used >= guard.disk_warn_percent {
        "warn"
    } else {
        "ok"
    }
}

async fn send_notification(guard: &GuardPolicy, title: &str, body: &str) {
    if !guard.notify || guard.notify_command.trim().is_empty() {
        return;
    }
    let cmd = guard.notify_command.trim();
    if !cmd.starts_with('/') {
        eprintln!("⚠️ notify_command must be an absolute path, got: {}", cmd);
        return;
    }
    if let Err(e) = Command::new(cmd).arg(title).arg(body).output().await {
        eprintln!("⚠️ notification failed: {}", e);
    }
}

fn log_guard_event(guard: &GuardPolicy, event: &str, details: &str) {
    if guard.guard_log_file.is_empty() {
        return;
    }
    let path = PathBuf::from(&guard.guard_log_file);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("⚠️ failed to create log dir: {}", e);
            return;
        }
    }
    let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
    if max_bytes > 0 {
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > max_bytes {
                if let Err(e) = fs::remove_file(&path) {
                    eprintln!("⚠️ failed to rotate guard log: {}", e);
                }
            }
        }
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = serde_json::json!({
        "ts": ts,
        "event": event,
        "details": details
    })
    .to_string();
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{}", line))
    {
        eprintln!("⚠️ failed to write guard log: {}", e);
    }
}

pub(crate) fn should_notify(state: &mut GuardRuntimeState, key: &str, cooldown_secs: u64) -> bool {
    let now = Instant::now();
    if let Some(until) = state.notify_cooldowns.get(key).copied() {
        if now < until {
            return false;
        }
    }
    state.notify_cooldowns.insert(
        key.to_string(),
        now + Duration::from_secs(cooldown_secs.max(1)),
    );
    true
}

fn sync_freeze_marker_path(guard: &GuardPolicy) -> PathBuf {
    PathBuf::from(guard.sync_freeze_marker.clone())
}

/// Graduated auto-renice: higher CPU/memory usage = higher nice value (lower priority).
/// The process still gets full CPU when nothing else needs it — it just yields to the DE
/// and other interactive processes.
///
/// Process mitigation is limited to reversible renice, optional `oom_score_adj`
/// biasing, and optional CPUQuota capping. The guard never invokes `kill`:
/// OOM biasing only influences the kernel's last-resort choice if an OOM occurs,
/// while CPUQuota throttles the process without killing it.
pub(crate) fn graduated_nice_value(cpu_percent: f32, rss_mb: u64, base_nice: i32) -> i32 {
    let cpu_tiers: &[(f32, i32)] = &[(500.0, 15), (300.0, 10), (180.0, 5)];
    let mem_tiers: &[(u64, i32)] = &[(8192, 10), (4096, 5)];
    let cpu_nice = cpu_tiers
        .iter()
        .find(|(threshold, _)| cpu_percent >= *threshold)
        .map(|(_, nice)| *nice)
        .unwrap_or(base_nice);
    let mem_nice = mem_tiers
        .iter()
        .find(|(threshold, _)| rss_mb >= *threshold)
        .map(|(_, nice)| *nice)
        .unwrap_or(0);
    cpu_nice.max(mem_nice).clamp(0, 19)
}

async fn renice_process_with_bin(bin: &Path, pid: i32, value: i32) -> Result<()> {
    let output = Command::new(bin)
        .args(["-n", &value.to_string(), "-p", &pid.to_string()])
        .output()
        .await
        .with_context(|| format!("failed to invoke {}", bin.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        anyhow::bail!("renice exited with status {}", output.status);
    }
    anyhow::bail!("renice exited with status {}: {}", output.status, stderr);
}

async fn renice_process(pid: i32, value: i32) -> Result<()> {
    renice_process_with_bin(Path::new("renice"), pid, value).await
}

/// OOM-killer steering target (v0.112.36). Higher oom_score_adj =
/// more likely to be picked when the kernel's last-resort OOM killer
/// fires. Writing it NEVER triggers a kill — it only steers the
/// victim choice IF the kernel kills anyway. Returns the target to
/// write, or None when the process should be left alone (already at/
/// above the bias, or deliberately protected with adj <= -500).
pub(crate) const OOM_BIAS_TARGET: i32 = 250;
pub(crate) const OOM_PROTECTED_ADJ: i32 = -500;

pub(crate) fn oom_bias_target(current: i32) -> Option<i32> {
    if current >= OOM_BIAS_TARGET || current <= OOM_PROTECTED_ADJ {
        None
    } else {
        Some(OOM_BIAS_TARGET)
    }
}

const KERNEL_PROCESS_PREFIXES: &[&str] = &[
    "kworker",
    "ksoftirqd",
    "kthreadd",
    "kswapd",
    "kcompactd",
    "rcu_",
    "kdevtmpfs",
    "kblockd",
    "khugepaged",
    "ksmd",
    "kernfs",
    "kauditd",
    "kstrp",
    "mm_percpu",
    "oom_reaper",
    "kvm",
    "ktrain",
    "kthrotld",
    "scsi_",
    "nvme",
    "irq/",
    "watchdog",
];

fn is_kernel_process(command: &str) -> bool {
    KERNEL_PROCESS_PREFIXES
        .iter()
        .any(|prefix| command.starts_with(prefix))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct OomSweepResult {
    restored: Vec<String>,
    deferred: usize,
}

fn remember_oom_descendant_identity(
    state: &mut GuardRuntimeState,
    root_pid: i32,
    pid: i32,
    identity: &ProcessIdentity,
) {
    state
        .oom_known_descendants
        .entry(root_pid)
        .or_default()
        .insert((pid, identity.starttime));
}

/// Retry descendant writes that were previously unreadable or failed. A
/// pending child keeps its root bias entry alive until restoration succeeds,
/// the child is proven gone, or its PID incarnation changes.
fn restore_pending_oom_descendants(
    proc_root: &Path,
    state: &mut GuardRuntimeState,
    exempt_names: &HashSet<String>,
) -> OomSweepResult {
    let pending_keys: Vec<(i32, u64)> = state.oom_pending_descendants.keys().copied().collect();
    let mut result = OomSweepResult::default();
    for key in pending_keys {
        let Some(pending) = state.oom_pending_descendants.get(&key).cloned() else {
            continue;
        };
        if exempt_names.contains(&pending.identity.comm)
            || is_kernel_process(&pending.identity.comm)
        {
            state.oom_pending_descendants.remove(&key);
            remember_oom_descendant_identity(state, pending.root_pid, key.0, &pending.identity);
            continue;
        }
        match process_identity_status(proc_root, key.0, &pending.identity) {
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                state.oom_pending_descendants.remove(&key);
                continue;
            }
            ProcessIdentityStatus::Unavailable => {
                result.deferred += 1;
                continue;
            }
            ProcessIdentityStatus::Match => {}
        }
        let adj_path = proc_root.join(key.0.to_string()).join("oom_score_adj");
        let current = match fs::read_to_string(&adj_path)
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok())
        {
            Some(current) => current,
            None => {
                result.deferred += 1;
                continue;
            }
        };
        if current != OOM_BIAS_TARGET {
            state.oom_pending_descendants.remove(&key);
            remember_oom_descendant_identity(state, pending.root_pid, key.0, &pending.identity);
            continue;
        }
        if let Err(error) = fs::write(&adj_path, format!("{}\n", pending.original_adj)) {
            eprintln!(
                "⚠️ oom-descendant-restore failed for pid={} parent={} : {}",
                key.0, pending.root_pid, error
            );
            result.deferred += 1;
            continue;
        }
        eprintln!(
            "🛡️ oom-descendant-restore pid={} parent={} adj -> {}",
            key.0, pending.root_pid, pending.original_adj
        );
        result.restored.push(format!(
            "oom-restore-descendant {}={}",
            pending.identity.comm, pending.original_adj
        ));
        state.oom_pending_descendants.remove(&key);
        remember_oom_descendant_identity(state, pending.root_pid, key.0, &pending.identity);
    }
    result
}

/// Restore oom_score_adj inherited by descendants created after a tracked
/// process was biased. Existing descendants are recorded at bias time so a
/// pre-existing operator adjustment is not overwritten. Nested biased roots
/// assign each new descendant to the nearest root, avoiding arbitrary
/// HashMap iteration order.
fn sweep_stranded_oom_descendants(
    proc_root: &Path,
    samples: &[ProcSample],
    state: &mut GuardRuntimeState,
    exempt_names: &HashSet<String>,
) -> OomSweepResult {
    let tracked_pids: HashSet<i32> = state.oom_biased_pids.keys().copied().collect();
    let mut root_values = HashMap::new();
    for (&pid, &(original_adj, ref identity)) in &state.oom_biased_pids {
        match process_identity_status(proc_root, pid, identity) {
            ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => {
                root_values.insert(pid, (original_adj, identity.clone()));
            }
            ProcessIdentityStatus::Mismatch | ProcessIdentityStatus::Unavailable => {}
        }
    }
    let biased_roots: HashSet<i32> = root_values.keys().copied().collect();
    let parent_by_pid: HashMap<i32, i32> = samples
        .iter()
        .map(|sample| (sample.pid, sample.ppid))
        .collect();
    for child in samples {
        if tracked_pids.contains(&child.pid)
            || exempt_names.contains(&child.command)
            || is_kernel_process(&child.command)
        {
            continue;
        }
        let Some(root_pid) = nearest_biased_ancestor(child.pid, &parent_by_pid, &biased_roots)
        else {
            continue;
        };
        let key = (child.pid, child.starttime);
        if state.oom_pending_descendants.contains_key(&key)
            || state
                .oom_known_descendants
                .get(&root_pid)
                .is_some_and(|known| known.contains(&key))
        {
            continue;
        }
        let Some((original_adj, _)) = root_values.get(&root_pid) else {
            continue;
        };
        state.oom_pending_descendants.insert(
            key,
            OomPendingDescendant {
                root_pid,
                original_adj: *original_adj,
                identity: process_sample_identity(child),
            },
        );
    }
    restore_pending_oom_descendants(proc_root, state, exempt_names)
}

/// Cap a process's CPU to `percent`% via a transient user systemd
/// unit (CPUQuota). Returns (unit_name, orig_cgroup_rel_path). The
/// process is MOVED into the unit's cgroup — moving between cgroups
/// never kills. On release, `uncap_cpu_process` moves it back and
/// stops the now-empty unit. Fails cleanly (Err) when systemd-run
/// is unavailable or the move is denied. The placeholder sleep lives
/// 3600s, bounding a guard crash: the pid stays capped at most an
/// hour, then the unit dies with no members.
///
/// WHY a transient SERVICE, not --scope (2026-08-10, verified live):
/// `systemd-run --scope` runs the command in the foreground and
/// blocks until it exits; `--scope --no-block` creates the scope but
/// it is torn down the moment systemd-run exits. A plain transient
/// service with `--no-block` returns instantly and persists under
/// the manager — the cgroup survives for the pid move.
async fn cap_cpu_process(pid: i32, percent: u32) -> Result<(String, String), String> {
    if percent == 0 || percent > 100 {
        return Err(format!("invalid CPUQuota percent {percent}"));
    }
    // Current cgroup BEFORE moving (the path we restore to later).
    let orig = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|e| format!("read /proc/{pid}/cgroup: {e}"))?;
    let orig_rel = parse_cgroup_rel_path(&orig)
        .ok_or_else(|| format!("unparseable cgroup line: {}", orig.trim()))?
        .to_string();

    let out = Command::new("systemd-run")
        .args([
            "--user",
            "--no-block",
            "-p",
            &format!("CPUQuota={percent}%"),
            "--",
            "sleep",
            "3600",
        ])
        .output()
        .await
        .map_err(|e| format!("systemd-run: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!("systemd-run: {} {}", out.status, stderr.trim()));
    }
    // systemd-run prints "Running as unit: X; invocation ID: ..."
    // on STDERR — take the unit name up to the ';'.
    let unit = stdout
        .lines()
        .chain(stderr.lines())
        .find_map(|l| l.strip_prefix("Running as unit: "))
        .map(|s| s.trim().split(';').next().unwrap_or("").trim().to_string())
        .ok_or_else(|| format!("could not parse unit name from: {stdout} {stderr}"))?;

    // --no-block returns before the unit's cgroup exists: poll for it.
    let mut cg = String::new();
    for _ in 0..10 {
        let out = Command::new("systemctl")
            .args(["--user", "show", &unit, "-p", "ControlGroup", "--value"])
            .output()
            .await
            .map_err(|e| format!("systemctl show: {e}"))?;
        cg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !cg.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if cg.is_empty() {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .status()
            .await;
        return Err(format!("unit {unit} has no control group"));
    }
    let procs_file = format!("/sys/fs/cgroup/{cg}/cgroup.procs");
    if let Err(e) = std::fs::write(&procs_file, format!("{pid}\n")) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .status()
            .await;
        return Err(format!("move pid {pid} into {procs_file}: {e}"));
    }
    Ok((unit, orig_rel))
}

fn parse_cgroup_rel_path(cg_line: &str) -> Option<&str> {
    let path = cg_line
        .lines()
        .next()?
        .split_once("::")
        .map(|(_, path)| path.trim_start_matches('/'))?;
    (!path.is_empty()).then_some(path)
}

async fn systemctl_user_action(
    systemctl_bin: &Path,
    action: &str,
    scope: &str,
) -> Result<(), String> {
    let output = Command::new(systemctl_bin)
        .args(["--user", action, scope])
        .output()
        .await
        .map_err(|e| format!("systemctl {action} {scope}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "systemctl {action} {scope} exited with {}: {}",
        output.status,
        detail.trim()
    ))
}

/// Lift a CPUQuota cap: move the pid back to its original cgroup and
/// stop the now-empty transient scope. Every read, move, and systemd
/// operation is checked so callers retain the cap entry when restoration
/// cannot be verified.
async fn uncap_cpu_process_with_bin(
    systemctl_bin: &Path,
    proc_root: &Path,
    pid: i32,
    scope: &str,
    orig_cgroup: &str,
    allow_pid_move: bool,
) -> Result<(), String> {
    let cgroup_path = proc_root.join(pid.to_string()).join("cgroup");
    let process_was_read = match std::fs::read_to_string(&cgroup_path) {
        Ok(cg_line) => {
            let rel = parse_cgroup_rel_path(&cg_line)
                .ok_or_else(|| format!("unparseable {}/cgroup", proc_root.display()))?;
            if rel.contains(scope) {
                if !allow_pid_move {
                    return Err(format!(
                        "pid {pid} remains in {scope} but its identity changed"
                    ));
                }
                let procs_file = format!("/sys/fs/cgroup/{orig_cgroup}/cgroup.procs");
                std::fs::write(&procs_file, format!("{pid}\n"))
                    .map_err(|e| format!("move pid {pid} back to {procs_file}: {e}"))?;
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing cgroup file means the process is gone only when its
            // PID directory is also gone from an available proc tree. If the
            // proc/cgroup source itself is unavailable, retain the entry and
            // do not stop the transient service: doing so could remove the
            // cap from, or kill, a process whose membership we cannot inspect.
            let pid_dir = proc_root.join(pid.to_string());
            let pid_gone = matches!(
                std::fs::metadata(&pid_dir),
                Err(dir_error) if dir_error.kind() == std::io::ErrorKind::NotFound
            );
            if !proc_root_is_available(proc_root) || !pid_gone {
                return Err(format!(
                    "cgroup source unavailable for pid {} under {}",
                    pid,
                    proc_root.display()
                ));
            }
            false
        }
        Err(e) => return Err(format!("read {}/cgroup: {e}", proc_root.display())),
    };

    let mut errors = Vec::new();
    if let Err(e) = systemctl_user_action(systemctl_bin, "stop", scope).await {
        errors.push(e);
    }
    if let Err(e) = systemctl_user_action(systemctl_bin, "reset-failed", scope).await {
        errors.push(e);
    }

    // If the process was readable before cleanup, prove it is no longer in
    // the transient scope. An unreadable live process is indeterminate and
    // must remain tracked for a later retry.
    if process_was_read {
        match std::fs::read_to_string(&cgroup_path) {
            Ok(cg_line) => match parse_cgroup_rel_path(&cg_line) {
                Some(rel) if rel.contains(scope) => {
                    errors.push(format!("pid {pid} remains in CPUQuota scope {scope}"));
                }
                Some(_) => {}
                None => errors.push(format!(
                    "verify {}/cgroup: unparseable cgroup line",
                    proc_root.display()
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let pid_dir = proc_root.join(pid.to_string());
                let pid_gone = matches!(
                    std::fs::metadata(&pid_dir),
                    Err(dir_error) if dir_error.kind() == std::io::ErrorKind::NotFound
                );
                if !proc_root_is_available(proc_root) || !pid_gone {
                    errors.push(format!(
                        "verify {}/cgroup: source unavailable",
                        proc_root.display()
                    ));
                }
            }
            Err(e) => errors.push(format!("verify {}/cgroup: {e}", proc_root.display())),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeAdjustment {
    LegacyRenice {
        pid: i32,
        identity: ProcessIdentity,
    },
    MemoryRenice {
        pid: i32,
        original_nice: i32,
        identity: ProcessIdentity,
    },
    OomBias {
        pid: i32,
        orig_adj: i32,
        identity: ProcessIdentity,
    },
    CpuCap {
        pid: i32,
        scope: String,
        orig_cgroup: String,
        identity: ProcessIdentity,
    },
}

/// Build the complete set of reversible process adjustments currently held
/// by the guard. SIGHUP must drain every v0.112.36 map before resetting the
/// runtime; keeping this plan separate makes it hard to restore only the
/// legacy renice map again when a new limiter is added.
fn runtime_adjustment_plan(state: &GuardRuntimeState) -> Vec<RuntimeAdjustment> {
    let mut plan = Vec::new();
    for (&pid, (_, identity)) in &state.reniced_pids {
        plan.push(RuntimeAdjustment::LegacyRenice {
            pid,
            identity: identity.clone(),
        });
    }
    for (&pid, entry) in &state.memory_reniced_pids {
        plan.push(RuntimeAdjustment::MemoryRenice {
            pid,
            original_nice: entry.original_nice,
            identity: entry.identity.clone(),
        });
    }
    for (&pid, (orig_adj, identity)) in &state.oom_biased_pids {
        plan.push(RuntimeAdjustment::OomBias {
            pid,
            orig_adj: *orig_adj,
            identity: identity.clone(),
        });
    }
    for (&pid, (scope, orig_cgroup, identity)) in &state.capped_pids {
        plan.push(RuntimeAdjustment::CpuCap {
            pid,
            scope: scope.clone(),
            orig_cgroup: orig_cgroup.clone(),
            identity: identity.clone(),
        });
    }
    plan
}

fn remove_legacy_renice(state: &mut GuardRuntimeState, pid: i32) {
    state.reniced_pids.remove(&pid);
    state.cooled_since.remove(&pid);
}

fn remove_memory_renice(state: &mut GuardRuntimeState, pid: i32) {
    state.memory_reniced_pids.remove(&pid);
    state.memory_cooled_since.remove(&pid);
}

fn oom_root_has_pending_descendants(state: &GuardRuntimeState, pid: i32) -> bool {
    state
        .oom_pending_descendants
        .values()
        .any(|pending| pending.root_pid == pid)
}

fn remove_oom_bias(state: &mut GuardRuntimeState, pid: i32) {
    if oom_root_has_pending_descendants(state, pid) {
        return;
    }
    state.oom_biased_pids.remove(&pid);
    state.oom_known_descendants.remove(&pid);
    state.oom_cooled_since.remove(&pid);
}

fn remove_cpu_cap(state: &mut GuardRuntimeState, pid: i32) {
    state.capped_pids.remove(&pid);
    state.cap_cooled_since.remove(&pid);
}

/// Restore all process-level mitigations before a policy reload discards the
/// old runtime. A PID that is gone or has a different starttime is safe to
/// drop; an unreadable live process, a failed command, or an unverified cgroup
/// remains tracked so the next guard pass can retry instead of losing a live
/// adjustment.
async fn restore_runtime_adjustments(state: &mut GuardRuntimeState) -> bool {
    let samples = process_samples().await.unwrap_or_default();
    restore_runtime_adjustments_with_samples(
        state,
        Path::new("renice"),
        Path::new("systemctl"),
        Path::new("/proc"),
        &samples,
    )
    .await
}

#[cfg(test)]
async fn restore_runtime_adjustments_with(
    state: &mut GuardRuntimeState,
    renice_bin: &Path,
    systemctl_bin: &Path,
    proc_root: &Path,
) -> bool {
    restore_runtime_adjustments_with_samples(state, renice_bin, systemctl_bin, proc_root, &[]).await
}

async fn restore_runtime_adjustments_with_samples(
    state: &mut GuardRuntimeState,
    renice_bin: &Path,
    systemctl_bin: &Path,
    proc_root: &Path,
    samples: &[ProcSample],
) -> bool {
    let sweep = sweep_stranded_oom_descendants(proc_root, samples, state, &HashSet::new());
    if sweep.deferred > 0 {
        eprintln!(
            "⚠ SIGHUP retaining {} pending descendant OOM restorations",
            sweep.deferred
        );
    }
    for adjustment in runtime_adjustment_plan(state) {
        match adjustment {
            RuntimeAdjustment::LegacyRenice { pid, identity } => {
                match process_identity_status(proc_root, pid, &identity) {
                    ProcessIdentityStatus::Match => {
                        match renice_process_with_bin(renice_bin, pid, 0).await {
                            Ok(()) => remove_legacy_renice(state, pid),
                            Err(e) => eprintln!(
                                "⚠ SIGHUP failed to restore nice value for pid={} comm={}: {}",
                                pid, identity.comm, e
                            ),
                        }
                    }
                    ProcessIdentityStatus::Gone => remove_legacy_renice(state, pid),
                    ProcessIdentityStatus::Mismatch => {
                        eprintln!(
                            "⚠ SIGHUP dropping legacy renice pid={} — PID incarnation changed",
                            pid
                        );
                        remove_legacy_renice(state, pid);
                    }
                    ProcessIdentityStatus::Unavailable => eprintln!(
                        "⚠ SIGHUP retaining legacy renice pid={} — process identity unavailable",
                        pid
                    ),
                }
            }
            RuntimeAdjustment::MemoryRenice {
                pid,
                original_nice,
                identity,
            } => match process_identity_status(proc_root, pid, &identity) {
                ProcessIdentityStatus::Match => {
                    match renice_process_with_bin(renice_bin, pid, original_nice).await {
                        Ok(()) => remove_memory_renice(state, pid),
                        Err(e) => eprintln!(
                            "⚠ SIGHUP failed to restore memory nice value for pid={} comm={}: {}",
                            pid, identity.comm, e
                        ),
                    }
                }
                ProcessIdentityStatus::Gone => remove_memory_renice(state, pid),
                ProcessIdentityStatus::Mismatch => {
                    eprintln!(
                        "⚠ SIGHUP dropping memory renice pid={} — PID incarnation changed",
                        pid
                    );
                    remove_memory_renice(state, pid);
                }
                ProcessIdentityStatus::Unavailable => eprintln!(
                    "⚠ SIGHUP retaining memory renice pid={} — process identity unavailable",
                    pid
                ),
            },
            RuntimeAdjustment::OomBias {
                pid,
                orig_adj,
                identity,
            } => match process_identity_status(proc_root, pid, &identity) {
                ProcessIdentityStatus::Match => {
                    if oom_root_has_pending_descendants(state, pid) {
                        eprintln!(
                            "⚠ SIGHUP deferring oom restore for pid={} until descendants recover",
                            pid
                        );
                        continue;
                    }
                    let adj_path = proc_root.join(pid.to_string()).join("oom_score_adj");
                    match fs::write(&adj_path, format!("{orig_adj}\n")) {
                        Ok(()) => remove_oom_bias(state, pid),
                        Err(e) => eprintln!(
                            "⚠ SIGHUP failed to restore oom_score_adj for pid={} comm={}: {}",
                            pid, identity.comm, e
                        ),
                    }
                }
                ProcessIdentityStatus::Gone => remove_oom_bias(state, pid),
                ProcessIdentityStatus::Mismatch => {
                    eprintln!(
                        "⚠ SIGHUP dropping oom bias pid={} — PID incarnation changed",
                        pid
                    );
                    remove_oom_bias(state, pid);
                }
                ProcessIdentityStatus::Unavailable => eprintln!(
                    "⚠ SIGHUP retaining oom bias pid={} — process identity unavailable",
                    pid
                ),
            },
            RuntimeAdjustment::CpuCap {
                pid,
                scope,
                orig_cgroup,
                identity,
            } => match process_identity_status(proc_root, pid, &identity) {
                ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => {
                    match uncap_cpu_process_with_bin(
                        systemctl_bin,
                        proc_root,
                        pid,
                        &scope,
                        &orig_cgroup,
                        true,
                    )
                    .await
                    {
                        Ok(()) => remove_cpu_cap(state, pid),
                        Err(e) => eprintln!(
                            "⚠ SIGHUP failed to restore CPU cgroup for pid={} scope={}: {}",
                            pid, scope, e
                        ),
                    }
                }
                ProcessIdentityStatus::Mismatch => {
                    // Do not move a different process out of the scope. The
                    // helper may still stop the scope when the PID is no
                    // longer inside it, and otherwise reports an error so
                    // this entry remains tracked.
                    match uncap_cpu_process_with_bin(
                        systemctl_bin,
                        proc_root,
                        pid,
                        &scope,
                        &orig_cgroup,
                        false,
                    )
                    .await
                    {
                        Ok(()) => remove_cpu_cap(state, pid),
                        Err(e) => eprintln!(
                            "⚠ SIGHUP retaining CPU cgroup pid={} scope={}: {}",
                            pid, scope, e
                        ),
                    }
                }
                ProcessIdentityStatus::Unavailable => eprintln!(
                    "⚠ SIGHUP retaining CPU cgroup pid={} — process identity unavailable",
                    pid
                ),
            },
        }
    }
    state.reniced_pids.is_empty()
        && state.memory_reniced_pids.is_empty()
        && state.oom_biased_pids.is_empty()
        && state.oom_pending_descendants.is_empty()
        && state.capped_pids.is_empty()
}

/// Detect active cargo/rustc processes and return their PIDs and working directories
async fn detect_active_rust_builds() -> Result<HashSet<i32>> {
    let out = Command::new("ps")
        .args(["-eo", "pid=,comm="])
        .output()
        .await?;

    if !out.status.success() {
        return Ok(HashSet::new());
    }

    let mut build_pids = HashSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.split_whitespace();
        let pid = match parts.next().and_then(|p| p.parse::<i32>().ok()) {
            Some(p) => p,
            None => continue,
        };
        let comm = parts.next().unwrap_or("");

        // Detect cargo, rustc, cargo-build, etc.
        if comm.contains("cargo") || comm.contains("rustc") || comm == "clippy-driver" {
            build_pids.insert(pid);
        }
    }

    Ok(build_pids)
}

/// Get the working directory of a process (to protect its target dir)
async fn get_process_cwd(pid: i32) -> Option<PathBuf> {
    let cwd_path = format!("/proc/{}/cwd", pid);
    std::fs::read_link(&cwd_path).ok()
}

/// Find all Rust target directories under the given search roots
async fn find_rust_target_dirs(roots: &[PathBuf]) -> Result<Vec<TargetDirInfo>> {
    use walkdir::WalkDir;

    let mut targets = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root)
            .max_depth(5) // Don't go too deep
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }

            if entry.file_name() != "target" {
                continue;
            }

            let path = entry.path().to_path_buf();

            // Check if there's a Cargo.toml in parent (confirm it's a Rust project)
            let parent = match path.parent() {
                Some(p) => p,
                None => continue,
            };

            if !parent.join("Cargo.toml").exists() {
                continue;
            }

            // Get directory size using du
            let bytes = match get_dir_size(&path).await {
                Ok(b) => b,
                Err(_) => continue,
            };

            let mtime_secs_ago = match fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(mtime) => SystemTime::now()
                    .duration_since(mtime)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };

            targets.push(TargetDirInfo {
                path,
                bytes,
                mtime_secs_ago,
            });
        }
    }

    // Sort ascending (smallest first); iteration cleans all above threshold so order is arbitrary
    targets.sort_by_key(|a| a.bytes);

    Ok(targets)
}

/// Get directory size using du command
async fn get_dir_size(path: &Path) -> Result<u64> {
    let out = Command::new("du")
        .args(["-sb", "--"])
        .arg(path)
        .output()
        .await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("du failed for {}", path.display()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let bytes = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("unexpected du output"))?
        .parse::<u64>()
        .context("failed to parse du output as byte count")?;

    Ok(bytes)
}

/// Perform automatic cleanup of Rust target directories
async fn auto_cleanup_rust_targets(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    apply: bool,
) -> Result<AutoCleanupResult> {
    let mut result = AutoCleanupResult {
        cleaned_count: 0,
        reclaimed_bytes: 0,
        cleaned_paths: Vec::new(),
        protected_paths: Vec::new(),
    };

    // Parse search roots
    let roots: Vec<PathBuf> = guard
        .rust_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if roots.is_empty() {
        return Ok(result);
    }

    // Find all target directories
    let targets = find_rust_target_dirs(&roots).await?;

    // Detect active builds - ONLY protection mechanism
    // We protect target dirs where cargo/rustc is actively running
    let active_builds = detect_active_rust_builds().await?;
    state.active_build_pids = active_builds.clone();

    // Get CWDs of active builds to protect their target dirs
    let mut protected_project_dirs: Vec<PathBuf> = Vec::new();
    for pid in &active_builds {
        if let Some(cwd) = get_process_cwd(*pid).await {
            // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7): walk
            // ALL ancestors collecting EVERY dir with a Cargo.toml,
            // not just the FIRST. In a cargo workspace, running
            // `cargo build` from a member crate (`ws/crates/foo`)
            // stops at the member's Cargo.toml, but the shared
            // target dir lives at the WORKSPACE root (`ws/target`) —
            // the pre-fix exact-equality check then failed and
            // `ws/target` was deleted mid-build.
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir.clone());
                }
                dir = parent.to_path_buf();
            }
        }
    }

    let min_size_bytes = guard
        .cleanup_min_size_mb
        .saturating_mul(1024)
        .saturating_mul(1024);

    for target in targets {
        // Skip if too small
        if target.bytes < min_size_bytes {
            continue;
        }

        // Only skip if there's an ACTIVELY RUNNING cargo/rustc in this project
        let target_project = target.path.parent().unwrap_or(&target.path);
        // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7):
        // ancestor-aware protection — a target dir is protected when
        // a protected project is EQUAL to its project dir, is an
        // ANCESTOR of it (workspace root building a nested member),
        // or is a DESCENDANT of it (member crate building into the
        // workspace-root target). The pre-fix exact-equality check
        // missed the workspace-member case entirely.
        let has_active_build = protected_project_dirs.iter().any(|proj| {
            proj == target_project
                || proj.starts_with(target_project)
                || target_project.starts_with(proj)
        });

        // ADDED 2026-07-21 (v0.112.33, audit M33/F4.7): cheap mtime
        // backstop — a target dir modified in the last 60s is almost
        // certainly being written by a build RIGHT NOW (builds touch
        // it constantly), regardless of process detection (which
        // misses `cargo build --manifest-path ...` run from an
        // unrelated CWD).
        let recently_active = std::fs::metadata(&target.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() < 60);

        if has_active_build || recently_active {
            result.protected_paths.push(format!(
                "{} ({})",
                target.path.display(),
                if has_active_build {
                    "active cargo/rustc process"
                } else {
                    "modified <60s ago (active build)"
                }
            ));
            continue;
        }

        if apply {
            let safe_path = match check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("⚠️ skipping {}: {}", target.path.display(), e);
                    result
                        .protected_paths
                        .push(target.path.display().to_string());
                    continue;
                }
            };
            if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                eprintln!("⚠️ failed to remove {}: {}", target.path.display(), e);
                continue;
            }
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += target.bytes;
        result.cleaned_paths.push(format!(
            "{} ({})",
            target.path.display(),
            human_bytes(target.bytes)
        ));
    }

    Ok(result)
}

/// Proactive cleanup: remove stale Rust target dirs (older than max_age_days)
/// even when disk is not at action/critical level. Only cleans targets that
/// haven't been touched in a while, skipping actively-built projects.
async fn proactive_cleanup_rust_targets(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    apply: bool,
) -> Result<AutoCleanupResult> {
    let mut result = AutoCleanupResult {
        cleaned_count: 0,
        reclaimed_bytes: 0,
        cleaned_paths: Vec::new(),
        protected_paths: Vec::new(),
    };

    let roots: Vec<PathBuf> = guard
        .rust_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if roots.is_empty() {
        return Ok(result);
    }

    let targets = find_rust_target_dirs(&roots).await?;
    let active_builds = detect_active_rust_builds().await?;
    state.active_build_pids = active_builds.clone();

    let mut protected_project_dirs: Vec<PathBuf> = Vec::new();
    for pid in &active_builds {
        if let Some(cwd) = get_process_cwd(*pid).await {
            // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7): walk
            // ALL ancestors collecting EVERY dir with a Cargo.toml
            // (workspace-member case — see the proactive path above).
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir.clone());
                }
                dir = parent.to_path_buf();
            }
        }
    }

    let min_size_bytes = guard
        .cleanup_min_size_mb
        .saturating_mul(1024)
        .saturating_mul(1024);
    let max_age_secs = guard
        .rust_target_max_age_days
        .saturating_mul(24)
        .saturating_mul(3600);

    for target in targets {
        if target.bytes < min_size_bytes {
            continue;
        }

        if target.mtime_secs_ago < max_age_secs {
            continue;
        }

        let target_project = target.path.parent().unwrap_or(&target.path);
        // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7):
        // ancestor-aware protection (workspace-member case — see the
        // proactive path above).
        let has_active_build = protected_project_dirs.iter().any(|proj| {
            proj == target_project
                || proj.starts_with(target_project)
                || target_project.starts_with(proj)
        });

        if has_active_build {
            result.protected_paths.push(format!(
                "{} (active cargo/rustc process)",
                target.path.display()
            ));
            continue;
        }

        if apply {
            let safe_path = match check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("⚠️ proactive: skipping {}: {}", target.path.display(), e);
                    result
                        .protected_paths
                        .push(target.path.display().to_string());
                    continue;
                }
            };
            if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                eprintln!(
                    "⚠️ proactive: failed to remove {}: {}",
                    target.path.display(),
                    e
                );
                continue;
            }
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += target.bytes;
        result.cleaned_paths.push(format!(
            "{} ({} days stale, {})",
            target.path.display(),
            target.mtime_secs_ago / 86400,
            human_bytes(target.bytes)
        ));
    }

    Ok(result)
}

async fn inode_use_percent() -> Result<u8> {
    let out = Command::new("df").args(["-Pi", "/"]).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("df -i command failed"));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    // Parse: Filesystem Inodes IUsed IFree IUse% Mounted on
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
        .and_then(|v| v.trim_end_matches('%').parse::<u8>().ok())
        .ok_or_else(|| anyhow::anyhow!("failed parsing df -i output"))
}

/// Get inode info for root filesystem
async fn get_inode_info() -> Result<(u64, u64, u64)> {
    let out = Command::new("df").args(["-Pi", "/"]).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("df -i command failed"));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    // Parse: Filesystem Inodes IUsed IFree IUse% Mounted on
    let line = text
        .lines()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("no data line"))?;
    let parts: Vec<&str> = line.split_whitespace().collect();

    let total = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    let used = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    let free = parts.get(3).and_then(|v| v.parse().ok()).unwrap_or(0);

    Ok((total, used, free))
}

/// Clean Docker resources
async fn docker_prune(apply: bool, all: bool, volumes: bool) -> Result<u64> {
    if !apply {
        // Dry-run: do not execute destructive docker commands
        return Ok(0);
    }
    let mut args = vec!["system", "prune", "-f"];
    if all {
        args.push("--all");
    }
    if volumes {
        args.push("--volumes");
    }

    let out = Command::new("docker").args(&args).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("docker prune failed"));
    }

    // Try to parse reclaimed space from output
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("reclaimed") {
            // Parse "Total reclaimed space: 1.5GB"
            if let Some(pos) = line.find(':') {
                let size_str = line[pos + 1..].trim();
                // Parse size - this is approximate
                let bytes = parse_docker_size(size_str);
                return Ok(bytes);
            }
        }
    }

    Ok(0)
}

fn parse_docker_size(s: &str) -> u64 {
    let s = s.trim();
    let num: String = s
        .chars()
        .take_while(|c| c.is_numeric() || *c == '.')
        .collect();
    let unit: String = s
        .chars()
        .skip_while(|c| c.is_numeric() || *c == '.' || *c == ' ')
        .collect();

    let value: f64 = match num.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("⚠️ parse_docker_size: failed to parse number from '{}'", s);
            0.0
        }
    };
    let multiplier = match unit.to_uppercase().as_str() {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };

    (value * multiplier) as u64
}

/// Try to remove a cache directory, returning whether it succeeded.
async fn try_remove_cache_dir(
    path: &Path,
    name: &str,
    apply: bool,
    protected_paths: &[String],
) -> bool {
    if !apply {
        return true;
    }
    match check_safe_to_delete_guard(path, protected_paths) {
        Ok(ref safe_path) => {
            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                eprintln!("⚠️ failed to remove {name} cache: {e}");
                false
            } else {
                true
            }
        }
        Err(e) => {
            eprintln!("⚠️ skipping {name} cache: {e}");
            false
        }
    }
}

/// Clean package manager caches
async fn clean_package_caches(
    cargo: bool,
    npm: bool,
    pip: bool,
    go: bool,
    apply: bool,
    protected_paths: &[String],
) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    let home = dirs::home_dir().unwrap_or_default();
    let targets: Vec<(&str, bool, &str)> = vec![
        ("cargo registry cache", cargo, ".cargo/registry/cache"),
        ("npm cache", npm, ".npm"),
        ("pip cache", pip, ".cache/pip"),
        ("go build cache", go, ".cache/go-build"),
    ];

    for (label, enabled, rel_path) in targets {
        if !enabled {
            continue;
        }
        let cache_path = home.join(rel_path);
        if !cache_path.exists() {
            continue;
        }
        let size = get_dir_size(&cache_path).await.unwrap_or(0);
        if size == 0 {
            continue;
        }
        if try_remove_cache_dir(&cache_path, label, apply, protected_paths).await {
            cleaned.push(format!("{label} ({})", human_bytes(size)));
            reclaimed += size;
        }
    }

    Ok((reclaimed, cleaned))
}

/// Empty trash. When `credential_guard` is set, the trash is first
/// scanned for credential-signal filenames (see
/// looks_credential_like and docs/design/disk-full-credentials-
/// 2026-08-10.md); a single match aborts deletion and the dry-run estimate.
/// The 2026-08-10 scan found 665 credential-pattern matches in the 56 GiB
/// trash, so blind emptying is unsafe by default.
async fn empty_trash(
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
) -> Result<(u64, Vec<String>)> {
    let Some(home) = dirs::home_dir() else {
        return Ok((0, Vec::new()));
    };
    empty_trash_at(&home, apply, protected_paths, credential_guard).await
}

async fn empty_trash_at(
    home: &Path,
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    {
        let trash_files = home.join(".local/share/Trash/files");
        let trash_info = home.join(".local/share/Trash/info");

        if trash_files.exists() {
            let size = get_dir_size(&trash_files).await.unwrap_or(0);
            if size > 0 {
                if credential_guard {
                    let mut matches = Vec::new();
                    for entry in walkdir::WalkDir::new(&trash_files).max_depth(8) {
                        match entry {
                            Ok(e) if e.file_type().is_file() => {
                                if let Some(name) = e.file_name().to_str() {
                                    if looks_credential_like(name) {
                                        matches.push(e.path().display().to_string());
                                        if matches.len() >= 20 {
                                            break;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if !matches.is_empty() {
                        eprintln!(
                            "🛡️ trash NOT emptied: {} credential-like entr{} (e.g. {})",
                            matches.len(),
                            if matches.len() == 1 { "y" } else { "ies" },
                            matches
                                .iter()
                                .take(3)
                                .map(|m| m.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "trash/credential-guard",
                            format!(
                                "trash emptying blocked: {} credential-like entries",
                                matches.len()
                            ),
                        ));
                        return Ok((0, Vec::new()));
                    }
                }
                let mut succeeded = true;
                if apply {
                    match check_safe_to_delete_guard(&trash_files, protected_paths) {
                        Ok(ref safe_path) => {
                            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                                eprintln!("⚠️ failed to remove trash files: {}", e);
                                succeeded = false;
                            } else if let Err(e) = tokio::fs::create_dir_all(&trash_files).await {
                                eprintln!("⚠️ failed to recreate trash dir: {}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("⚠️ skipping trash files: {}", e);
                            succeeded = false;
                        }
                    }
                }
                if !apply || succeeded {
                    cleaned.push(format!("trash files ({})", human_bytes(size)));
                    reclaimed += size;
                }
            }
        }

        if trash_info.exists() {
            let info_size = get_dir_size(&trash_info).await.unwrap_or(0);
            if info_size > 0 {
                let mut succeeded = true;
                if apply {
                    match check_safe_to_delete_guard(&trash_info, protected_paths) {
                        Ok(ref safe_path) => {
                            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                                eprintln!("⚠️ failed to remove trash info: {}", e);
                                succeeded = false;
                            } else if let Err(e) = tokio::fs::create_dir_all(&trash_info).await {
                                eprintln!("⚠️ failed to recreate trash info dir: {}", e);
                                // Note: we still count this as success since the files were removed
                            }
                        }
                        Err(e) => {
                            eprintln!("⚠️ skipping trash info: {}", e);
                            succeeded = false;
                        }
                    }
                }
                if !apply || succeeded {
                    cleaned.push(format!("trash info ({})", human_bytes(info_size)));
                    reclaimed += info_size;
                }
            }
        }
    }

    Ok((reclaimed, cleaned))
}

static RESOLVE_BIN_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();

fn resolve_bin(name: &str) -> String {
    let cache =
        RESOLVE_BIN_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        if let Some(cached) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(name) {
            return cached.clone();
        }
    }
    let nixos_paths = [
        "/run/current-system/sw/bin",
        "/etc/profiles/per-user/dracon/bin",
        "/nix/var/nix/profiles/default/bin",
    ];
    let result = nixos_paths
        .iter()
        .find(|dir| std::path::Path::new(dir).join(name).exists())
        .map(|dir| {
            std::path::Path::new(dir)
                .join(name)
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| name.to_string());
    cache
        .lock()
        .unwrap()
        .insert(name.to_string(), result.clone());
    result
}

/// Run nix-collect-garbage
async fn clean_nix_garbage(keep_generations: u32, apply: bool) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();
    let mut errs = Vec::new();

    if apply && keep_generations > 0 {
        let gen_arg = keep_generations.to_string();
        let nix_env = resolve_bin("nix-env");
        if let Err(e) = Command::new(&nix_env)
            .arg("--delete-generations")
            .arg(&gen_arg)
            .output()
            .await
        {
            errs.push(format!("nix-env delete generations: {}", e));
        }

        if let Err(e) = Command::new(&nix_env)
            .arg("--delete-generations")
            .arg(&gen_arg)
            .arg("-p")
            .arg("/nix/var/nix/profiles/default")
            .output()
            .await
        {
            errs.push(format!("nix-env delete user profile generations: {}", e));
        }
    }

    let mut args: Vec<&str> = Vec::new();
    if apply {
        args.push("-d");
    } else {
        args.push("--dry-run");
    }

    let nix_gc = resolve_bin("nix-collect-garbage");
    let out = Command::new(&nix_gc)
        .args(&args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run nix-collect-garbage: {}", e))?;

    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "nix-collect-garbage failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let delete_count = text.lines().filter(|l| l.contains("deleting")).count();
    if delete_count > 0 {
        cleaned.push(format!("nix store garbage ({} paths)", delete_count));
        reclaimed = delete_count as u64 * 1024 * 1024;
    }

    if !errs.is_empty() && reclaimed == 0 {
        return Err(anyhow::anyhow!(
            "nix cleanup had {} error(s): {}",
            errs.len(),
            errs.join("; ")
        ));
    }

    Ok((reclaimed, cleaned))
}

/// Clean old node_modules directories
async fn clean_old_node_modules(
    roots: &[PathBuf],
    max_age_days: u64,
    apply: bool,
    protected_paths: &[String],
) -> Result<(u64, Vec<String>)> {
    use walkdir::WalkDir;

    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();
    let now = SystemTime::now();
    let max_age_secs = max_age_days * 24 * 3600;

    for root in roots {
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root)
            .max_depth(5)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }

            if entry.file_name() != "node_modules" {
                continue;
            }

            let path = entry.path().to_path_buf();

            // Check age
            let modified_secs_ago = match fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(mtime) => now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0),
                Err(_) => continue,
            };

            if modified_secs_ago < max_age_secs {
                continue;
            }

            let size = match get_dir_size(&path).await {
                Ok(s) => s,
                Err(_) => continue,
            };

            if size > 0 {
                let mut succeeded = true;
                if apply {
                    match check_safe_to_delete_guard(&path, protected_paths) {
                        Ok(ref safe_path) => {
                            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                                eprintln!("⚠️ failed to remove {}: {}", path.display(), e);
                                succeeded = false;
                            }
                        }
                        Err(e) => {
                            eprintln!("⚠️ skipping {}: {}", path.display(), e);
                            continue;
                        }
                    }
                }
                if !apply || succeeded {
                    cleaned.push(format!(
                        "{} ({} days old, {})",
                        path.display(),
                        modified_secs_ago / 86400,
                        human_bytes(size)
                    ));
                    reclaimed += size;
                }
            }
        }
    }

    Ok((reclaimed, cleaned))
}

/// Find large log files
async fn find_large_log_files(
    dirs: &[PathBuf],
    min_size_bytes: u64,
) -> Result<Vec<(PathBuf, u64)>> {
    use walkdir::WalkDir;

    let mut logs = Vec::new();

    for dir in dirs {
        if !dir.exists() {
            continue;
        }

        for entry in WalkDir::new(dir)
            .max_depth(3)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Check if it looks like a log file
            if !name.ends_with(".log")
                && !name.ends_with(".log.old")
                && !name.contains(".log.")
                && name != "journal"
                && !name.ends_with(".journal")
            {
                continue;
            }

            let size = match fs::metadata(path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if size >= min_size_bytes {
                logs.push((path.to_path_buf(), size));
            }
        }
    }

    // Sort ascending by size (smallest first)
    logs.sort_by_key(|a| a.1);

    Ok(logs)
}

/// Truncate a log file to a maximum size while optionally preserving header lines.
/// Returns the number of bytes reclaimed, or an error on failure.
fn truncate_log_file(
    path: &Path,
    max_size_bytes: u64,
    preserve_header_lines: usize,
) -> Result<u64> {
    use std::io::{BufRead, BufReader, Write};

    let original_size = std::fs::metadata(path)?.len();
    if original_size <= max_size_bytes {
        return Ok(0);
    }

    if preserve_header_lines == 0 {
        // Simple truncate: open with truncate flag
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.set_len(max_size_bytes)?;
        let new_size = file.metadata()?.len();
        return Ok(original_size.saturating_sub(new_size));
    }

    // Preserve header lines: read first N lines, write them to temp file,
    // then rename temp over original
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut header_lines: Vec<Vec<u8>> = Vec::new();

    for (i, line_result) in reader.lines().enumerate() {
        if i >= preserve_header_lines {
            break;
        }
        if let Ok(line) = line_result {
            header_lines.push(line.into_bytes());
        } else {
            break;
        }
    }

    // Write header + max content to temp file
    let temp_path = path.with_extension(format!(
        "{}.truncated.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("log"),
        std::process::id()
    ));
    {
        let mut temp_file = std::fs::File::create(&temp_path)?;
        let mut total_written = 0u64;
        for line_bytes in &header_lines {
            temp_file.write_all(line_bytes)?;
            temp_file.write_all(b"\n")?;
            total_written += line_bytes.len() as u64 + 1;
        }

        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        for line in reader.lines().skip(preserve_header_lines).flatten() {
            let line_bytes = line.into_bytes();
            let line_len = line_bytes.len() as u64;

            if total_written + line_len + 1 > max_size_bytes {
                break;
            }

            temp_file.write_all(&line_bytes)?;
            temp_file.write_all(b"\n")?;
            total_written += line_len + 1;
        }
    }

    // Atomically replace original
    std::fs::rename(&temp_path, path)?;
    let new_size = std::fs::metadata(path)?.len();
    Ok(original_size.saturating_sub(new_size))
}

/// Predict when disk will fill based on trend
pub(crate) fn predict_fill_time(history: &[(Instant, u8)]) -> Option<f64> {
    if history.len() < 3 {
        return None;
    }

    // Simple linear regression on the last N samples
    let n = history.len().min(20); // Use up to 20 samples
    let recent = &history[history.len().saturating_sub(n)..];

    if recent.len() < 3 {
        return None;
    }

    // Calculate rate of change (percent per second)
    let mut total_rate = 0.0;
    let mut count = 0;

    for i in 1..recent.len() {
        let dt = recent[i].0.duration_since(recent[i - 1].0).as_secs_f64();
        if dt <= 0.0 {
            continue;
        }
        let dp = (recent[i].1 as f64) - (recent[i - 1].1 as f64);
        total_rate += dp / dt;
        count += 1;
    }

    if count == 0 {
        return None;
    }

    let avg_rate = total_rate / count as f64;

    // If rate is negative or zero, disk isn't filling
    if avg_rate <= 0.0 {
        return None;
    }

    // Time until 100% from current
    let current = recent.last()?.1 as f64;
    let remaining_percent = 100.0 - current;
    let seconds_until_full = remaining_percent / avg_rate;

    Some(seconds_until_full / 3600.0) // Return hours
}

async fn check_disk_trends(guard: &GuardPolicy, state: &mut GuardRuntimeState, used: u8) {
    if !guard.track_trends {
        return;
    }
    let now = Instant::now();
    state.disk_history.push((now, used));
    if state.disk_history.len() > 100 {
        let excess = state.disk_history.len() - 100;
        state.disk_history.drain(0..excess);
    }
    if let Some(hours_until_full) = predict_fill_time(&state.disk_history) {
        if hours_until_full > 0.0 && hours_until_full <= guard.trend_warn_hours as f64 {
            let key = "disk-trend-warning".to_string();
            if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
                send_notification(
                    guard,
                    "Dracon System Guard - Disk Trend Warning",
                    &format!(
                        "Disk predicted to fill in {:.1} hours (currently {}%)",
                        hours_until_full, used
                    ),
                )
                .await;
            }
        }
    }
}

async fn check_disk_early_warning(guard: &GuardPolicy, state: &mut GuardRuntimeState, used: u8) {
    if used >= guard.disk_early_warn_percent && used < guard.disk_warn_percent {
        let key = "disk-early-warn".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(1800)) {
            send_notification(
                guard,
                "Dracon System Guard - Early Warning",
                &format!(
                    "Disk usage at {}% (early warning threshold: {}%)",
                    used, guard.disk_early_warn_percent
                ),
            )
            .await;
        }
    }
}

/// Byte-precise rapid disk-fill detection (ADDED 2026-08-10,
/// v0.112.35). The percent-based trend check is too coarse on large
/// disks — 1% of a 907 GiB disk is ~9 GiB — so this tracks df used
/// bytes and alerts on a sustained fill rate in GiB/hour.
async fn check_rapid_disk_fill(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    used_bytes: u64,
    used_pct: u8,
) -> Option<f64> {
    let now = Instant::now();
    state.disk_bytes_history.push((now, used_bytes));
    if state.disk_bytes_history.len() > 200 {
        let excess = state.disk_bytes_history.len() - 200;
        state.disk_bytes_history.drain(0..excess);
    }
    let rate = disk_fill_rate_gbph(&state.disk_bytes_history)?;
    if rate >= guard.disk_rapid_fill_gbph {
        let key = "disk-rapid-fill".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(1800)) {
            send_notification(
                guard,
                "Dracon System Guard - Disk Filling Rapidly",
                &format!(
                    "Disk growing at {:.1} GiB/h (currently {}% used) — check recent writes",
                    rate, used_pct
                ),
            )
            .await;
        }
        emit_event(&DraconEvent::new(
            "system",
            EventSeverity::Warn,
            "disk/rapid-fill",
            format!("growing at {:.1} GiB/h ({}% used)", rate, used_pct),
        ));
    }
    Some(rate)
}

/// Memory/swap pressure guard (ADDED 2026-08-10, v0.112.35).
/// Detects the 2026-08-09/10 failure mode: RAM exhausted, swap
/// thrashing (PSI full avg10 high), everything crawling while
/// kswapd spins. Reports the top RSS offenders and, when configured,
/// applies reversible renice, OOM-bias, and CPUQuota mitigation; it
/// never kills a process itself.
async fn check_memory_pressure(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
) -> Option<MemoryReport> {
    if !guard.monitor_memory {
        return None;
    }
    let sample = memory_sample().await?;
    let mem_available_percent = sample.mem_available_percent();
    let swap_used_percent = sample.swap_used_percent();
    let psi_full_avg10 = psi_full_avg10().await;

    // Swap-in rate fallback (pages/s) when PSI is unavailable.
    let mut pswpin_rate = None;
    if psi_full_avg10.is_none() {
        if let Some((pin, _)) = vmstat_swap_counters().await {
            if let Some((prev_at, prev_pin, _)) = state.prev_swap_counters {
                let dt = prev_at.elapsed().as_secs_f64();
                if dt > 0.0 {
                    pswpin_rate = Some(pin.saturating_sub(prev_pin) as f64 / dt);
                }
            }
            state.prev_swap_counters = Some((Instant::now(), pin, 0));
        }
    } else {
        state.prev_swap_counters = None;
    }

    let mem_low = mem_available_percent <= guard.mem_available_warn_percent;
    let swap_high = swap_used_percent >= guard.swap_used_warn_percent;
    let psi_thrash = psi_full_avg10.is_some_and(|v| v >= guard.mem_psi_full_warn)
        || pswpin_rate.is_some_and(|r| r >= 1000.0);

    let pressure = if mem_low && (swap_high || psi_thrash) {
        "critical"
    } else if mem_low || swap_high || psi_thrash {
        "warn"
    } else {
        "ok"
    };

    // Top RSS offenders (skipping kernel threads and exempt names).
    let exempt = parse_kinds(&guard.process_exempt_names);
    let all_processes = process_samples().await.unwrap_or_default();
    let top_rss: Vec<ProcSample> = all_processes
        .iter()
        .filter(|p| !exempt.contains(&p.command) && !is_kernel_process(&p.command))
        .cloned()
        .fold(Vec::new(), |mut acc, p| {
            if acc.len() < 5 || p.rss_mb > acc.last().map(|x: &ProcSample| x.rss_mb).unwrap_or(0) {
                acc.push(p);
                acc.sort_by_key(|b| std::cmp::Reverse(b.rss_mb));
                acc.truncate(5);
            }
            acc
        });

    // ── Limiting (ADDED 2026-08-10, v0.112.36) ──────────────────
    // Deprioritize (never kill) the offenders while pressure lasts;
    // restore when it drops. All mechanisms are per-process,
    // reversible, and skip whitelisted/kernel pids. Memory is NOT
    // capped: a memory cap frees nothing and only kills (MemoryMax)
    // or freezes (MemoryHigh) the offender — renice fixes the
    // responsiveness symptom, oom_score_adj steers the kill the
    // kernel would do anyway, and CPUQuota throttles a stuck busy-
    // loop that renice can't tame. See the user discussion
    // 2026-08-10 in AGENTS.md.
    let mut limited: Vec<String> = Vec::new();
    if pressure != "ok" && guard.auto_renice_on_memory {
        for p in &top_rss {
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            let nice_val = graduated_nice_value(p.cpu_percent, p.rss_mb, guard.renice_value);
            let applied_nice = state
                .memory_reniced_pids
                .get(&p.pid)
                .map(|entry| entry.applied_nice);
            if applied_nice != Some(nice_val) {
                let original_nice = state
                    .memory_reniced_pids
                    .get(&p.pid)
                    .map(|entry| entry.original_nice)
                    .unwrap_or(p.nice);
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        state.memory_reniced_pids.insert(
                            p.pid,
                            MemoryReniceState {
                                original_nice,
                                applied_nice: nice_val,
                                identity: process_sample_identity(p),
                            },
                        );
                        eprintln!(
                            "🛡️ mem-renice pid={} cmd={} -> nice {} (pressure {})",
                            p.pid, p.command, nice_val, pressure
                        );
                        limited.push(format!("renice {}={}", p.command, nice_val));
                    }
                    Err(e) => eprintln!(
                        "⚠️ mem-renice failed for pid={} cmd={}: {}",
                        p.pid, p.command, e
                    ),
                }
            }
        }
    }
    if pressure == "critical" && guard.bias_oom_on_pressure {
        for p in &top_rss {
            if state.oom_biased_pids.contains_key(&p.pid) {
                continue;
            }
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            let adj_path = format!("/proc/{}/oom_score_adj", p.pid);
            let cur = fs::read_to_string(&adj_path)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok());
            if let Some(orig) = cur {
                if let Some(target) = oom_bias_target(orig) {
                    if fs::write(&adj_path, format!("{target}\n")).is_ok() {
                        let known_descendants = process_descendant_samples(&all_processes, p.pid)
                            .into_iter()
                            .map(|child| (child.pid, child.starttime))
                            .collect();
                        state
                            .oom_biased_pids
                            .insert(p.pid, (orig, process_sample_identity(p)));
                        state.oom_known_descendants.insert(p.pid, known_descendants);
                        eprintln!(
                            "🛡️ oom-bias pid={} cmd={} adj {} -> {} (critical pressure)",
                            p.pid, p.command, orig, target
                        );
                        limited.push(format!("oom-bias {}={}", p.command, target));
                    }
                }
            }
        }
    }
    if pressure == "critical" && guard.cap_offenders_cpu_percent > 0 {
        for p in &top_rss {
            if state.capped_pids.contains_key(&p.pid) {
                continue;
            }
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            match cap_cpu_process(p.pid, guard.cap_offenders_cpu_percent).await {
                Ok((scope, orig_cgroup)) => {
                    state.capped_pids.insert(
                        p.pid,
                        (scope.clone(), orig_cgroup, process_sample_identity(p)),
                    );
                    eprintln!(
                        "🛡️ cpu-cap pid={} cmd={} -> CPUQuota={}% (scope {})",
                        p.pid, p.command, guard.cap_offenders_cpu_percent, scope
                    );
                    limited.push(format!(
                        "cpu-cap {}={}%",
                        p.command, guard.cap_offenders_cpu_percent
                    ));
                }
                Err(e) => eprintln!(
                    "⚠️ cpu-cap failed for pid={} cmd={}: {}",
                    p.pid, p.command, e
                ),
            }
        }
    }

    // A child forked after its parent was biased inherits oom_score_adj=250,
    // but is not present in `oom_biased_pids`. Sweep those descendants on
    // every pass, including before a tracked parent is released or removed.
    let sweep = sweep_stranded_oom_descendants(Path::new("/proc"), &all_processes, state, &exempt);
    if sweep.deferred > 0 {
        eprintln!(
            "⚠️ retaining {} pending descendant OOM restorations",
            sweep.deferred
        );
    }
    limited.extend(sweep.restored);

    if pressure == "ok" {
        let now = Instant::now();
        let release_dur = Duration::from_secs(guard.release_after_secs);
        // Un-renice memory-limited pids after the release window.
        let mut to_unrenice = Vec::new();
        for &pid in state.memory_reniced_pids.keys() {
            let cooled_at = state.memory_cooled_since.entry(pid).or_insert(now);
            if now.duration_since(*cooled_at) >= release_dur {
                to_unrenice.push(pid);
            }
        }
        for pid in to_unrenice {
            let (original_nice, identity) = match state.memory_reniced_pids.get(&pid) {
                Some(entry) => (entry.original_nice, entry.identity.clone()),
                None => continue,
            };
            match process_identity_status(Path::new("/proc"), pid, &identity) {
                ProcessIdentityStatus::Match => {}
                ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                    remove_memory_renice(state, pid);
                    continue;
                }
                ProcessIdentityStatus::Unavailable => {
                    eprintln!(
                        "⚠️ mem-unrenice deferred for pid={} — process identity unavailable",
                        pid
                    );
                    continue;
                }
            }
            if let Err(e) = renice_process(pid, original_nice).await {
                eprintln!("⚠️ mem-unrenice failed for pid={}: {}", pid, e);
                continue;
            }
            eprintln!(
                "🛡️ mem-unrenice pid={} -> nice {} (pressure released)",
                pid, original_nice
            );
            remove_memory_renice(state, pid);
        }
        state
            .memory_cooled_since
            .retain(|pid, _| state.memory_reniced_pids.contains_key(pid));
        // Restore oom_score_adj after the release window.
        let mut to_unbias = Vec::new();
        for &pid in state.oom_biased_pids.keys() {
            let cooled_at = state.oom_cooled_since.entry(pid).or_insert(now);
            if now.duration_since(*cooled_at) >= release_dur {
                to_unbias.push(pid);
            }
        }
        for pid in to_unbias {
            let (orig, identity) = match state.oom_biased_pids.get(&pid).cloned() {
                Some(entry) => entry,
                None => continue,
            };
            match process_identity_status(Path::new("/proc"), pid, &identity) {
                ProcessIdentityStatus::Match => {}
                ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                    remove_oom_bias(state, pid);
                    continue;
                }
                ProcessIdentityStatus::Unavailable => {
                    eprintln!(
                        "⚠️ oom-restore deferred for pid={} — process identity unavailable",
                        pid
                    );
                    continue;
                }
            }
            if oom_root_has_pending_descendants(state, pid) {
                eprintln!(
                    "⚠️ oom-restore deferred for pid={} until descendants recover",
                    pid
                );
                continue;
            }
            if let Err(e) = fs::write(format!("/proc/{pid}/oom_score_adj"), format!("{orig}\n")) {
                eprintln!("⚠️ oom-restore failed for pid={}: {}", pid, e);
                continue;
            }
            eprintln!(
                "🛡️ oom-restore pid={} adj -> {} (pressure released)",
                pid, orig
            );
            remove_oom_bias(state, pid);
        }
        state
            .oom_cooled_since
            .retain(|pid, _| state.oom_biased_pids.contains_key(pid));
        // Lift CPUQuota scopes after the release window.
        let mut to_uncap = Vec::new();
        for &pid in state.capped_pids.keys() {
            let cooled_at = state.cap_cooled_since.entry(pid).or_insert(now);
            if now.duration_since(*cooled_at) >= release_dur {
                to_uncap.push(pid);
            }
        }
        for pid in to_uncap {
            let (scope, orig_cgroup, identity) = match state.capped_pids.get(&pid).cloned() {
                Some(entry) => entry,
                None => continue,
            };
            let allow_pid_move = match process_identity_status(Path::new("/proc"), pid, &identity) {
                ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => true,
                ProcessIdentityStatus::Mismatch => false,
                ProcessIdentityStatus::Unavailable => {
                    eprintln!(
                        "⚠️ cpu-un cap deferred for pid={} — process identity unavailable",
                        pid
                    );
                    continue;
                }
            };
            if let Err(e) = uncap_cpu_process_with_bin(
                Path::new("systemctl"),
                Path::new("/proc"),
                pid,
                &scope,
                &orig_cgroup,
                allow_pid_move,
            )
            .await
            {
                eprintln!("⚠️ cpu-uncap failed for pid={} scope={}: {}", pid, scope, e);
                continue;
            }
            remove_cpu_cap(state, pid);
        }
        state
            .cap_cooled_since
            .retain(|pid, _| state.capped_pids.contains_key(pid));
    } else {
        // Pressure persists: restart any release timers from zero.
        state.memory_cooled_since.clear();
        state.oom_cooled_since.clear();
        state.cap_cooled_since.clear();
    }
    // Prune only known-gone or PID-reused entries. Identity read errors are
    // retained so a transient /proc failure cannot silently lose a limiter.
    state.memory_reniced_pids.retain(|pid, entry| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, &entry.identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });
    let pending_oom_roots: HashSet<i32> = state
        .oom_pending_descendants
        .values()
        .map(|pending| pending.root_pid)
        .collect();
    state.oom_biased_pids.retain(|pid, (_, identity)| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        ) || pending_oom_roots.contains(pid)
    });
    state
        .oom_known_descendants
        .retain(|pid, _| state.oom_biased_pids.contains_key(pid));
    state.capped_pids.retain(|pid, (_, _, identity)| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone
        )
    });

    if pressure != "ok" {
        let key = "memory-pressure".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(600)) {
            let offenders: String = top_rss
                .iter()
                .map(|p| format!("{} pid={} {}MiB", p.command, p.pid, p.rss_mb))
                .collect::<Vec<_>>()
                .join(", ");
            send_notification(
                guard,
                "Dracon System Guard - Memory Pressure",
                &format!(
                    "[{}] mem available {}%, swap used {}%{} top: {}",
                    pressure,
                    mem_available_percent,
                    swap_used_percent,
                    psi_full_avg10
                        .map(|v| format!(", PSI full {:.1}%", v))
                        .unwrap_or_default(),
                    if offenders.is_empty() {
                        "(none)".to_string()
                    } else {
                        offenders
                    }
                ),
            )
            .await;
        }
        emit_event(&DraconEvent::new(
            "system",
            if pressure == "critical" {
                EventSeverity::Error
            } else {
                EventSeverity::Warn
            },
            "memory/pressure",
            format!(
                "{}: mem available {}%, swap used {}%, PSI full {:?}, top rss: {}",
                pressure,
                mem_available_percent,
                swap_used_percent,
                psi_full_avg10,
                top_rss
                    .iter()
                    .map(|p| format!("{}={}MiB", p.command, p.rss_mb))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ));
    }

    Some(MemoryReport {
        mem_available_percent,
        swap_used_percent,
        psi_full_avg10,
        pswpin_rate,
        pressure: pressure.to_string(),
        top_rss,
        limited,
    })
}

/// Enumerate zombies with parent/age context (ADDED 2026-08-10,
/// v0.112.35). Zombies can't be killed; the diagnostic value is the
/// count, their parents (a live parent that never wait()s), and how
/// long they have lingered.
fn zombie_details(state: &mut GuardRuntimeState) -> Vec<ZombieInfo> {
    let now = Instant::now();
    let mut zombies = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let Ok(pid) = pid_str.to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, comm, ppid, _)) = parse_proc_stat_zombie(&stat) else {
                continue;
            };
            let first_seen = *state.zombies_since.entry(pid).or_insert(now);
            let parent_comm = fs::read_to_string(format!("/proc/{ppid}/comm"))
                .map(|c| c.trim().to_string())
                .unwrap_or_else(|_| "(unknown)".to_string());
            let parent_alive = Path::new(&format!("/proc/{ppid}")).exists();
            zombies.push(ZombieInfo {
                pid,
                ppid,
                comm,
                parent_comm,
                age_secs: now.duration_since(first_seen).as_secs(),
                parent_alive,
            });
        }
    }
    state
        .zombies_since
        .retain(|pid, _| zombies.iter().any(|z| z.pid == *pid));
    zombies.sort_by_key(|b| std::cmp::Reverse(b.age_secs));
    zombies
}

fn manage_sync_freeze(guard: &GuardPolicy, used: u8, dstate: &str, sync_frozen: &mut bool) {
    let marker = sync_freeze_marker_path(guard);
    if guard.freeze_sync_at_action && (dstate == "action" || dstate == "critical") {
        if !*sync_frozen {
            if let Some(parent) = marker.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("failed to create freeze marker dir: {}", e);
                }
            }
            if let Err(e) = fs::write(
                &marker,
                format!("dracon-system guard freeze: disk={}%\n", used),
            ) {
                eprintln!("failed to write freeze marker: {}", e);
            } else {
                *sync_frozen = true;
                emit_event(&DraconEvent::new(
                    "system",
                    EventSeverity::Warn,
                    "disk/freeze",
                    format!("sync frozen at {}%", used),
                ));
            }
        }
    } else if *sync_frozen && used <= guard.unfreeze_below_percent {
        if let Err(e) = fs::remove_file(&marker) {
            eprintln!("failed to remove freeze marker: {}", e);
        } else {
            *sync_frozen = false;
            emit_event(&DraconEvent::new(
                "system",
                EventSeverity::Info,
                "disk/unfreeze",
                format!("sync unfrozen at {}%", used),
            ));
        }
    }
}

async fn run_auto_cleanup(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    used: u8,
) -> Result<()> {
    let apply = guard.auto_cleanup_apply;
    if !apply {
        eprintln!("💡 disk at {}% — auto-cleanup is in dry-run mode (set auto_cleanup_apply = true to execute)", used);
    }
    let mut total_reclaimed = 0u64;
    let mut all_cleaned: Vec<String> = Vec::new();

    if guard.auto_cleanup_rust {
        match auto_cleanup_rust_targets(guard, state, apply).await {
            Ok(result) => {
                total_reclaimed += result.reclaimed_bytes;
                for p in &result.cleaned_paths {
                    eprintln!("🧹 Rust: {}", p);
                }
                all_cleaned.extend(result.cleaned_paths);
            }
            Err(e) => eprintln!("⚠️ Rust target cleanup failed: {}", e),
        }
    }

    if guard.clean_trash {
        match empty_trash(apply, &guard.protected_paths, guard.trash_credential_guard).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Trash: {}", s)));
                for c in &cleaned {
                    eprintln!("🗑️ {}", c);
                }
            }
            Err(e) => eprintln!("⚠️ Trash cleanup failed: {}", e),
        }
    }

    if guard.clean_nix_garbage {
        match clean_nix_garbage(guard.nix_keep_generations, apply).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Nix: {}", s)));
                for c in &cleaned {
                    eprintln!("📦 {}", c);
                }
            }
            Err(e) => eprintln!("⚠️ Nix cleanup failed: {}", e),
        }
    }

    let roots: Vec<PathBuf> = guard
        .node_modules_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    let (bytes, cleaned) = match clean_old_node_modules(
        &roots,
        guard.node_modules_max_age_days,
        apply,
        &guard.protected_paths,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️ Node modules cleanup failed: {}", e);
            (0, vec![])
        }
    };
    total_reclaimed += bytes;
    all_cleaned.extend(cleaned.iter().map(|s| format!("Node: {}", s)));
    for c in &cleaned {
        eprintln!("📂 {}", c);
    }

    if guard.clean_package_caches {
        match clean_package_caches(true, true, true, true, apply, &guard.protected_paths).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Cache: {}", s)));
                for c in &cleaned {
                    eprintln!("💾 {}", c);
                }
            }
            Err(e) => eprintln!("⚠️ Package cache cleanup failed: {}", e),
        }
    }

    if guard.docker_prune {
        if apply {
            match docker_prune(guard.auto_cleanup_apply, true, guard.docker_prune_volumes).await {
                Ok(bytes) => {
                    total_reclaimed += bytes;
                    if bytes > 0 {
                        eprintln!("🐳 Docker prune: {}", human_bytes(bytes));
                    }
                }
                Err(e) => eprintln!("⚠️ Docker prune failed: {}", e),
            }
        } else {
            eprintln!("🐳 Would prune Docker (dry-run)");
        }
    }

    if total_reclaimed > 0 {
        let key = "auto-cleanup".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(600)) {
            send_notification(
                guard,
                "Dracon System Guard - Auto Cleanup",
                &format!(
                    "Reclaimed {} ({} items cleaned)",
                    human_bytes(total_reclaimed),
                    all_cleaned.len()
                ),
            )
            .await;
        }
    }

    Ok(())
}

async fn check_disk_state_change(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    used: u8,
    dstate: &str,
) {
    if state.last_disk_state != dstate {
        let key = format!("disk-state-{dstate}");
        if should_notify(state, &key, guard.notify_cooldown_secs) {
            send_notification(
                guard,
                "Dracon System Guard",
                &format!("Disk pressure state changed to {} (used={}%)", dstate, used),
            )
            .await;
        }
        state.last_disk_state = dstate.to_string();
    }
}

async fn check_heavy_processes(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
) -> Result<Vec<GuardProcessAlert>> {
    let exempt = parse_kinds(&guard.process_exempt_names);
    let samples = process_samples().await?;
    let mut current_heavy = HashSet::new();
    let mut alerts = Vec::new();

    for p in samples {
        if exempt.contains(&p.command) {
            continue;
        }
        let heavy = p.cpu_percent >= guard.process_cpu_percent || p.rss_mb >= guard.process_rss_mb;
        if !heavy {
            continue;
        }
        current_heavy.insert(p.pid);
        let now = Instant::now();
        // ADDED 2026-07-21 (v0.112.33, audit M34/F4.8): record the
        // /proc starttime at first sight and detect PID reuse — a
        // recycled PID gets a different starttime, so its (forged
        // or stale) sustain window resets instead of letting the
        // guard renice an innocent process.
        let live_start = p.starttime;
        let entry = state.heavy_since.entry(p.pid).or_insert((now, live_start));
        if live_start != 0 && entry.1 != 0 && entry.1 != live_start {
            *entry = (now, live_start);
        }
        let since_instant = entry.0;
        let recorded_start = entry.1;
        let sustained = now.duration_since(since_instant).as_secs();
        let is_sustained = sustained >= guard.process_sustain_secs;

        log_guard_event(
            guard,
            if is_sustained {
                "heavy-sustained"
            } else {
                "heavy-brief"
            },
            &format!(
                "pid={} ppid={} cmd={} args={} cpu={:.1}% rss={}MiB sustained={}s",
                p.pid, p.ppid, p.command, p.args, p.cpu_percent, p.rss_mb, sustained
            ),
        );

        if !is_sustained {
            continue;
        }

        let mut action = "notify".to_string();
        let mut nice_applied = 0;
        // ADDED 2026-08-10 (v0.112.35): sustained-heavy escalation.
        // After process_stuck_after_secs the alert is labelled a
        // "stuck candidate" — e.g. the 4 svelte-check processes at
        // ~285% CPU holding 6 GiB that never finished during the
        // 2026-08-09 incident. Notification only; no auto-kill.
        let stuck = sustained >= guard.process_stuck_after_secs;

        if guard.auto_renice {
            // ADDED 2026-07-21 (v0.112.33, audit M34/F4.8): final
            // identity check before touching the process — the comm
            // must still match AND the starttime must equal the
            // first-sighting value (PID-reuse window).
            let identity_ok = process_sample_is_current(&p)
                && (recorded_start == 0 || p.starttime == recorded_start);
            if !identity_ok {
                eprintln!(
                    "⚠️ skipping renice for pid={} cmd={} (identity changed — PID reused?)",
                    p.pid, p.command
                );
                alerts.push(GuardProcessAlert {
                    pid: p.pid,
                    ppid: p.ppid,
                    command: p.command,
                    args: p.args,
                    cpu_percent: p.cpu_percent,
                    rss_mb: p.rss_mb,
                    sustained_secs: sustained,
                    action,
                    nice_value: nice_applied,
                });
                continue;
            }
            let already_niced = state.reniced_pids.get(&p.pid).map(|(n, _)| *n);
            let nice_val = graduated_nice_value(p.cpu_percent, p.rss_mb, guard.renice_value);
            if already_niced != Some(nice_val) {
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        state
                            .reniced_pids
                            .insert(p.pid, (nice_val, process_sample_identity(&p)));
                        eprintln!(
                            "🔧 renice pid={} cmd={} -> nice {} (cpu={:.1}% rss={}MiB)",
                            p.pid, p.command, nice_val, p.cpu_percent, p.rss_mb
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "⚠️ renice failed for pid={} cmd={} nice={} ({}); leaving state unchanged",
                            p.pid, p.command, nice_val, e
                        );
                    }
                }
            }
            if state.reniced_pids.get(&p.pid).map(|(n, _)| *n) == Some(nice_val) {
                nice_applied = nice_val;
                action = format!("renice:{}", nice_val);
            }
        }

        let key = format!("proc-{}", p.pid);
        if should_notify(state, &key, guard.notify_cooldown_secs) {
            send_notification(
                guard,
                "Dracon System Guard",
                &format!(
                    "Heavy process {} (pid={} cpu={:.1}% rss={}MiB) sustained {}s{}{}",
                    p.command,
                    p.pid,
                    p.cpu_percent,
                    p.rss_mb,
                    sustained,
                    if nice_applied > 0 {
                        format!(" reniced={}", nice_applied)
                    } else {
                        String::new()
                    },
                    if stuck {
                        " — POSSIBLY STUCK (still heavy after {}s)".to_string()
                    } else {
                        String::new()
                    }
                ),
            )
            .await;
        }
        if stuck {
            action = format!("{} stuck-candidate", action);
        }

        alerts.push(GuardProcessAlert {
            pid: p.pid,
            ppid: p.ppid,
            command: p.command,
            args: p.args,
            cpu_percent: p.cpu_percent,
            rss_mb: p.rss_mb,
            sustained_secs: sustained,
            action,
            nice_value: nice_applied,
        });
    }

    state
        .heavy_since
        .retain(|pid, _| current_heavy.contains(pid));

    // Un-renice recovery: processes that are no longer heavy
    let now = Instant::now();
    let release_dur = Duration::from_secs(guard.release_after_secs);
    let mut to_unrenice = Vec::new();
    for &pid in state.reniced_pids.keys() {
        if current_heavy.contains(&pid) {
            state.cooled_since.remove(&pid);
            continue;
        }
        let cooled_at = state.cooled_since.entry(pid).or_insert(now);
        if now.duration_since(*cooled_at) >= release_dur {
            to_unrenice.push(pid);
        }
    }
    for pid in to_unrenice {
        let identity = match state.reniced_pids.get(&pid) {
            Some((_, identity)) => identity.clone(),
            None => continue,
        };
        match process_identity_status(Path::new("/proc"), pid, &identity) {
            ProcessIdentityStatus::Match => {}
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                remove_legacy_renice(state, pid);
                continue;
            }
            ProcessIdentityStatus::Unavailable => {
                eprintln!(
                    "⚠️ un-renice deferred for pid={} — process identity unavailable",
                    pid
                );
                continue;
            }
        }
        if let Err(e) = renice_process(pid, 0).await {
            eprintln!("⚠️ un-renice failed for pid={}: {}", pid, e);
            continue;
        }
        eprintln!("🔧 un-renice pid={} -> nice 0 (pressure released)", pid);
        remove_legacy_renice(state, pid);
    }
    state
        .cooled_since
        .retain(|pid, _| state.reniced_pids.contains_key(pid));

    // Clean up only known-gone or PID-reused entries; retain identity read
    // failures for a later retry.
    state.reniced_pids.retain(|pid, (_, identity)| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });

    // Summary feedback
    if !state.reniced_pids.is_empty() {
        let summary: Vec<String> = state
            .reniced_pids
            .iter()
            .map(|(pid, (nice, _))| format!("pid={}:nice={}", pid, nice))
            .collect();
        eprintln!("🔧 reniced active: [{}]", summary.join(", "));
    }

    Ok(alerts)
}

fn cleanup_stale_cooldowns(state: &mut GuardRuntimeState, cooldown_secs: u64) {
    let cutoff = Instant::now() - Duration::from_secs(cooldown_secs.saturating_mul(2));
    state
        .notify_cooldowns
        .retain(|_, &mut since| since > cutoff);
}

async fn check_inode_usage(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    if !guard.monitor_inodes {
        return;
    }
    if let Ok(inode_percent) = inode_use_percent().await {
        if inode_percent >= guard.inode_warn_percent {
            let key = "inode-warning".to_string();
            if should_notify(state, &key, guard.notify_cooldown_secs.max(1800)) {
                send_notification(
                    guard,
                    "Dracon System Guard - Inode Warning",
                    &format!(
                        "Inode usage at {}% (threshold: {}%) - disk may have space but no file slots",
                        inode_percent, guard.inode_warn_percent
                    ),
                )
                .await;
            }
        }
    }
}

async fn check_zombie_processes(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
) -> Vec<ZombieInfo> {
    if !guard.monitor_zombies {
        return Vec::new();
    }
    let zombies = zombie_details(state);
    if zombies.len() as u64 > guard.zombie_threshold {
        let key = "zombie-warning".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
            let top: Vec<String> = zombies
                .iter()
                .take(3)
                .map(|z| {
                    format!(
                        "pid={} comm={} parent={}{} ({}s)",
                        z.pid,
                        z.comm,
                        z.parent_comm,
                        if z.parent_alive { "" } else { ", parent dead" },
                        z.age_secs
                    )
                })
                .collect();
            send_notification(
                guard,
                "Dracon System Guard - Zombie Processes",
                &format!(
                    "Detected {} zombie processes (threshold: {}). Oldest: {}",
                    zombies.len(),
                    guard.zombie_threshold,
                    top.join(" | ")
                ),
            )
            .await;
        }
        emit_event(&DraconEvent::new(
            "system",
            EventSeverity::Warn,
            "process/zombies",
            format!(
                "{} zombies: {}",
                zombies.len(),
                zombies
                    .iter()
                    .take(5)
                    .map(|z| format!("{}={}", z.comm, z.age_secs))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ));
    }
    zombies
}

async fn check_large_logs(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    if !guard.monitor_logs || guard.log_dirs.trim().is_empty() {
        return;
    }

    let log_dirs: Vec<PathBuf> = guard
        .log_dirs
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if log_dirs.is_empty() {
        return;
    }

    let min_size = guard.log_size_mb.saturating_mul(1024).saturating_mul(1024);
    match find_large_log_files(&log_dirs, min_size).await {
        Ok(logs) if !logs.is_empty() => {
            let key = "log-size-warning".to_string();
            if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
                let top_logs: Vec<_> = logs.iter().take(3).collect();
                let msg = format!(
                    "Found {} large log files (>{:.0} MiB): {}",
                    logs.len(),
                    guard.log_size_mb,
                    top_logs
                        .iter()
                        .map(|(p, s)| format!("{} ({})", p.display(), human_bytes(*s)))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                send_notification(guard, "Dracon System Guard - Large Log Files", &msg).await;
            }

            if guard.auto_truncate_logs && guard.auto_cleanup_apply {
                let max_size = guard
                    .log_max_truncate_mb
                    .saturating_mul(1024)
                    .saturating_mul(1024);
                let preserve = guard.log_preserve_header_lines;
                let mut total_reclaimed = 0u64;
                for (path, original_size) in &logs {
                    let safe_path = match check_safe_to_delete(path, &guard.protected_paths) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("⚠️ skipping log truncate {}: {}", path.display(), e);
                            continue;
                        }
                    };
                    match truncate_log_file(&safe_path, max_size, preserve) {
                        Ok(reclaimed) if reclaimed > 0 => {
                            eprintln!(
                                "📝 truncated {}: {} -> {} (reclaimed {})",
                                path.display(),
                                human_bytes(*original_size),
                                human_bytes(original_size.saturating_sub(reclaimed)),
                                human_bytes(reclaimed)
                            );
                            total_reclaimed += reclaimed;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("⚠️ failed to truncate {}: {}", path.display(), e);
                        }
                    }
                }
                if total_reclaimed > 0 {
                    let key = "log-truncated".to_string();
                    if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
                        send_notification(
                            guard,
                            "Dracon System Guard - Logs Truncated",
                            &format!(
                                "Reclaimed {} from {} log file(s) (max now: {} MiB)",
                                human_bytes(total_reclaimed),
                                logs.len(),
                                guard.log_max_truncate_mb
                            ),
                        )
                        .await;
                    }
                }
            }
        }
        _ => {}
    }
}

async fn run_proactive_cleanup(guard: &GuardPolicy, state: &mut GuardRuntimeState) -> Result<()> {
    let apply = guard.auto_cleanup_apply;
    if !apply {
        eprintln!(
            "💡 proactive cleanup in dry-run mode (set auto_cleanup_apply = true to execute)"
        );
    }

    let mut total_reclaimed = 0u64;
    let mut all_cleaned: Vec<String> = Vec::new();

    if guard.auto_cleanup_rust {
        match proactive_cleanup_rust_targets(guard, state, apply).await {
            Ok(result) => {
                total_reclaimed += result.reclaimed_bytes;
                for p in &result.cleaned_paths {
                    eprintln!("🧹 Proactive Rust: {}", p);
                }
                all_cleaned.extend(result.cleaned_paths);
            }
            Err(e) => eprintln!("⚠️ Proactive Rust target cleanup failed: {}", e),
        }
    }

    if total_reclaimed > 0 {
        let key = "proactive-cleanup".to_string();
        if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
            send_notification(
                guard,
                "Dracon System Guard - Proactive Cleanup",
                &format!(
                    "Proactively reclaimed {} ({} stale items)",
                    human_bytes(total_reclaimed),
                    all_cleaned.len()
                ),
            )
            .await;
        }
        emit_event(&DraconEvent::new(
            "system",
            EventSeverity::Info,
            "guard/proactive-cleanup",
            format!(
                "reclaimed {} from {} stale items",
                human_bytes(total_reclaimed),
                all_cleaned.len()
            ),
        ));
    }

    Ok(())
}

pub(crate) async fn run_guard_once(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
) -> Result<GuardReport> {
    let details = disk_details_for(&guard.disk_mount_path).await?;
    let used = details.use_percent;
    let dstate = disk_state(used, guard).to_string();
    let marker = sync_freeze_marker_path(guard);
    let mut sync_frozen = marker.exists();

    check_disk_trends(guard, state, used).await;
    check_disk_early_warning(guard, state, used).await;
    let fill_gbph = check_rapid_disk_fill(guard, state, details.used_bytes, used).await;
    manage_sync_freeze(guard, used, &dstate, &mut sync_frozen);

    if dstate == "action" || dstate == "critical" {
        run_auto_cleanup(guard, state, used).await?;
    } else if used >= guard.proactive_cleanup_percent && guard.auto_cleanup_rust {
        state.guard_cycle += 1;
        let interval = guard.proactive_cleanup_interval_cycles;
        let due = state.guard_cycle.is_multiple_of(interval);
        let cooldown_ok = state
            .last_proactive_cleanup
            .is_none_or(|t| t.elapsed().as_secs() >= interval.saturating_mul(guard.interval_secs));
        if due && cooldown_ok {
            run_proactive_cleanup(guard, state).await?;
            state.last_proactive_cleanup = Some(Instant::now());
        }
    }

    check_disk_state_change(guard, state, used, &dstate).await;

    let alerts = check_heavy_processes(guard, state).await?;
    cleanup_stale_cooldowns(state, guard.notify_cooldown_secs);

    check_inode_usage(guard, state).await;
    let zombies = check_zombie_processes(guard, state).await;
    check_large_logs(guard, state).await;
    let memory = check_memory_pressure(guard, state).await;

    Ok(GuardReport {
        enabled: guard.enabled,
        disk_use_percent: used,
        disk_state: dstate,
        sync_frozen,
        alerts,
        memory,
        zombies,
        disk_fill_gbph: fill_gbph,
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct LinkEntryStatus {
    pub(crate) link: String,
    pub(crate) target: String,
    pub(crate) exists: bool,
    pub(crate) is_symlink: bool,
    pub(crate) target_exists: bool,
    pub(crate) points_to: String,
    pub(crate) in_sync: bool,
    pub(crate) issue: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LinkStatusReport {
    pub(crate) entries: Vec<LinkEntryStatus>,
    pub(crate) total: usize,
    pub(crate) healthy: usize,
    pub(crate) drifted: usize,
    pub(crate) missing_target: usize,
    pub(crate) missing_link: usize,
}

fn resolve_system_policy_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("DRACON_SYSTEM_POLICY") {
        let p = PathBuf::from(custom);
        if p.exists() {
            return Some(p);
        }
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home"));
    let candidates = [
        home.join(".dracon/utilities/system/dracon-system.toml"),
        home.join(".dracon/utilities/system/config.toml"),
        home.join(".dracon/system/dracon-system.toml"),
        home.join(".dracon/system/config.toml"),
    ];

    candidates.into_iter().find(|p| p.exists())
}

pub(crate) fn load_system_policy() -> Result<(Option<PathBuf>, SystemPolicy)> {
    let Some(path) = resolve_system_policy_path() else {
        return Ok((None, SystemPolicy::default()));
    };
    // FIXED 2026-07-21 (v0.112.33, audit F4.12): read errors are now
    // PROPAGATED like parse errors — the pre-fix code conflated
    // "policy exists but is unreadable" (permissions, I/O) with "no
    // policy" and silently ran the guard on defaults, with the
    // error DISCARDED (`Err(_e)`). An operator who chmods the
    // policy file would have the guard silently run on defaults
    // (thresholds, marker paths diverging from what they believe is
    // active).
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", path.display(), e))?;
    let parsed: SystemPolicy = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))?;
    Ok((Some(path), parsed))
}

async fn is_user_service_active(service: &str) -> bool {
    let output = Command::new("systemctl")
        .args(["--user", "is-active", service])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "active",
        _ => false,
    }
}

async fn build_status_report() -> Result<StatusReport> {
    let root = canonical_system_root();
    let system_policy_path = root.join("utilities/system/dracon-system.toml");
    Ok(StatusReport {
        system_root: root.display().to_string(),
        nixos_root: root.join("nixos").display().to_string(),
        sync_policy: root
            .join("utilities/sync/dracon-sync.toml")
            .display()
            .to_string(),
        system_policy: system_policy_path.display().to_string(),
        system_policy_exists: system_policy_path.exists(),
        sync_service_active: is_user_service_active("dracon-sync.service").await,
    })
}

pub(crate) fn normalize_guard_policy(policy: &mut GuardPolicy) {
    policy.interval_secs = policy.interval_secs.max(5);
    policy.disk_warn_percent = policy.disk_warn_percent.clamp(1, 100);
    policy.disk_action_percent = policy
        .disk_action_percent
        .max(policy.disk_warn_percent)
        .min(100);
    policy.disk_critical_percent = policy
        .disk_critical_percent
        .max(policy.disk_action_percent)
        .min(100);
    policy.proactive_cleanup_percent = policy
        .proactive_cleanup_percent
        .min(policy.disk_action_percent.saturating_sub(1));
    policy.unfreeze_below_percent = policy
        .unfreeze_below_percent
        .min(policy.disk_action_percent.saturating_sub(1));
    policy.process_cpu_percent = policy.process_cpu_percent.max(1.0);
    policy.process_rss_mb = policy.process_rss_mb.max(64);
    policy.process_sustain_secs = policy.process_sustain_secs.max(5);
    policy.process_stuck_after_secs = policy
        .process_stuck_after_secs
        .max(policy.process_sustain_secs);
    policy.mem_available_warn_percent = policy.mem_available_warn_percent.clamp(1, 100);
    policy.swap_used_warn_percent = policy.swap_used_warn_percent.clamp(1, 100);
    policy.mem_psi_full_warn = policy.mem_psi_full_warn.max(0.0);
    policy.disk_rapid_fill_gbph = policy.disk_rapid_fill_gbph.max(0.5);
    policy.notify_cooldown_secs = policy.notify_cooldown_secs.max(5);
    policy.rust_target_max_age_days = policy.rust_target_max_age_days.max(1);
    policy.proactive_cleanup_interval_cycles = policy.proactive_cleanup_interval_cycles.max(1);
    if policy.sync_freeze_marker.trim().is_empty() {
        policy.sync_freeze_marker = default_sync_freeze_marker();
    }
    if policy.notify_command.trim().is_empty() {
        policy.notify_command = default_notify_command();
    }
}

async fn is_git_tracked_dir(path: &Path) -> Result<bool> {
    let parent = match path.parent() {
        Some(p) => p,
        None => return Ok(false),
    };
    let name = match path.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return Ok(false),
    };

    let top_out = Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await;
    let top_out = match top_out {
        Ok(o) if o.status.success() => o,
        _ => {
            return Err(anyhow::anyhow!(
                "git rev-parse failed for {}",
                parent.display()
            ))
        }
    };

    let repo_root = String::from_utf8_lossy(&top_out.stdout).trim().to_string();
    if repo_root.is_empty() {
        return Err(anyhow::anyhow!(
            "git rev-parse returned empty root for {}",
            parent.display()
        ));
    }

    // FIXED 2026-07-21 (v0.112.33, audit F4.10): the pre-fix code
    // passed only the BASENAME to `ls-files` at the repo ROOT — a
    // nested tracked dir (`repo/web/node_modules`) only matched if
    // the root itself tracked a same-named entry, so nested tracked
    // dirs were misdetected as untracked and `storage --cleanup
    // --apply` deleted them without `--allow-tracked`. Compute the
    // path RELATIVE to the resolved toplevel and query that.
    let rel_path = path
        .strip_prefix(&repo_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| name.clone());

    let ls_out = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["ls-files", "--", &rel_path])
        .output()
        .await;
    let ls_out = match ls_out {
        Ok(o) if o.status.success() => o,
        _ => {
            return Err(anyhow::anyhow!(
                "git ls-files failed for {} in {}",
                rel_path,
                repo_root
            ))
        }
    };

    Ok(!String::from_utf8_lossy(&ls_out.stdout).trim().is_empty())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests;

mod print;

async fn cmd_status(json: bool) -> Result<()> {
    let report = build_status_report().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("STATUS"),
                Cell::new("KEY"),
                Cell::new("VALUE"),
            ]);

        // ---- Summary row (one-liner for quick scanning) ----
        let summary = format!(
            "{} · sync service {}",
            report.system_root,
            if report.sync_service_active {
                "active"
            } else {
                "inactive"
            }
        );
        table.add_row(vec![
            Cell::new("📋 Summary"),
            Cell::new(summary.clone()),
            Cell::new(""),
        ]);

        // ---- Section: Roots ----
        table.add_row(vec![
            Cell::new(" "),
            Cell::new("🏠 system root"),
            Cell::new(&report.system_root),
        ]);
        table.add_row(vec![
            Cell::new(" "),
            Cell::new("🐧 nixos root"),
            Cell::new(&report.nixos_root),
        ]);

        // ---- Section: Policies ----
        table.add_row(vec![
            Cell::new(" "),
            Cell::new("📜 sync policy"),
            Cell::new(&report.sync_policy),
        ]);
        table.add_row(vec![
            Cell::new(" "),
            Cell::new("⚙️ system policy"),
            Cell::new(&report.system_policy),
        ]);

        // ---- Section: Services ----
        let (icon, color) = if report.sync_service_active {
            ("\u{2705}", Color::Green)
        } else {
            ("\u{274c}", Color::Red)
        };
        let _ = dr_print::onoff; // currently used by future commands
        table.add_row(vec![
            Cell::new(icon).fg(color),
            Cell::new("sync service"),
            Cell::new(if report.sync_service_active {
                "active"
            } else {
                "inactive"
            }),
        ]);

        println!("{table}");
    }
    Ok(())
}

async fn cmd_storage(
    root: Option<PathBuf>,
    json: bool,
    cleanup: bool,
    apply: bool,
    allow_tracked: bool,
    min_size_mb: Option<u64>,
    kinds: Option<String>,
) -> Result<()> {
    use comfy_table::{
        presets::UTF8_FULL_CONDENSED, Attribute, Cell, Color, ContentArrangement, Table,
    };

    let (_, policy) = load_system_policy()?;
    let root = root.unwrap_or_else(|| {
        if !policy.storage.default_root.trim().is_empty() {
            return PathBuf::from(policy.storage.default_root.clone());
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/home"))
            .join("Dev")
    });
    let min_size_mb = min_size_mb.unwrap_or(policy.storage.min_size_mb);
    let kinds = kinds.unwrap_or_else(|| policy.storage.kinds.clone());

    let report = analyze_workspace_storage(&root, 15, 25).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // ── Disk health header ──
    let disk = disk_details_for(&root.to_string_lossy()).await.ok();
    if let Some(ref d) = disk {
        let state_icon = match disk_state(d.use_percent, &policy.guard) {
            "ok" => "✅",
            "warn" => "⚠️",
            "action" => "🟠",
            "critical" => "🔴",
            _ => "",
        };
        let state_label = disk_state(d.use_percent, &policy.guard);
        println!(
            "💻 Disk: {} / {} ({}% used, {} free) — {} {}",
            human_bytes(d.used_bytes),
            human_bytes(d.total_bytes),
            d.use_percent,
            human_bytes(d.avail_bytes),
            state_icon,
            state_label,
        );
        println!(
            "   Mount: {}  Thresholds: warn={}%, action={}%, critical={}%",
            d.mount,
            policy.guard.disk_warn_percent,
            policy.guard.disk_action_percent,
            policy.guard.disk_critical_percent,
        );
    }

    // ── Per-kind subtotals ──
    let mut kind_totals: HashMap<String, u64> = HashMap::new();
    for item in &report.top_hotspots {
        *kind_totals.entry(item.kind.clone()).or_default() += item.bytes;
    }
    let mut kind_vec: Vec<_> = kind_totals.into_iter().collect();
    kind_vec.sort_by_key(|b| std::cmp::Reverse(b.1));
    if !kind_vec.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![Cell::new("SIZE"), Cell::new("KIND")]);
        for (kind, bytes) in &kind_vec {
            table.add_row(vec![
                Cell::new(human_bytes(*bytes)).add_attribute(Attribute::Bold),
                Cell::new(kind),
            ]);
        }
        println!();
        println!("Breakdown by kind:");
        println!("{table}");
    }

    // ── Total workspace size ──
    let total_workspace: u64 = report.top_projects.iter().map(|p| p.bytes).sum();
    println!();
    println!(
        "📁 Workspace: {} ({})",
        report.root.display(),
        human_bytes(total_workspace)
    );

    if !report.top_projects.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![Cell::new("SIZE"), Cell::new("PROJECT")]);
        for item in &report.top_projects {
            table.add_row(vec![
                Cell::new(human_bytes(item.bytes)).add_attribute(Attribute::Bold),
                Cell::new(item.path.display().to_string()),
            ]);
        }
        println!();
        println!("Top projects:");
        println!("{table}");
    }

    if !report.top_hotspots.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("SIZE"),
                Cell::new("KIND"),
                Cell::new("PATH"),
            ]);
        for item in &report.top_hotspots {
            table.add_row(vec![
                Cell::new(human_bytes(item.bytes)).add_attribute(Attribute::Bold),
                Cell::new(&item.kind),
                Cell::new(item.path.display().to_string()),
            ]);
        }
        println!();
        println!("Top hotspots:");
        println!("{table}");
    }

    if cleanup {
        let cfg = CleanupConfig {
            apply,
            allow_tracked,
            min_size_mb,
            kinds: parse_kinds(&kinds),
        };
        let threshold = cfg.min_size_mb.saturating_mul(1024 * 1024);
        let selected: Vec<_> = report
            .top_hotspots
            .iter()
            .filter(|h| cfg.kinds.contains(&h.kind) && h.bytes >= threshold)
            .cloned()
            .collect();

        // ── Available cleanup kinds hint ──
        let all_kinds: Vec<_> = report
            .top_hotspots
            .iter()
            .map(|h| h.kind.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        println!();
        println!(
            "Cleanup mode: {}",
            if cfg.apply { "APPLY" } else { "DRY-RUN" }
        );
        println!("Kinds: {}", {
            let mut v: Vec<_> = cfg.kinds.iter().cloned().collect();
            v.sort();
            v.join(",")
        });
        println!("Min size: {} MiB", cfg.min_size_mb);
        println!("Allow tracked: {}", cfg.allow_tracked);
        println!("Available kinds: {}", {
            let mut v: Vec<_> = all_kinds;
            v.sort();
            v.join(", ")
        });

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("SIZE"),
                Cell::new("KIND"),
                Cell::new("PATH"),
                Cell::new("STATUS"),
            ]);

        let mut total = 0u64;
        let mut actionable = Vec::new();
        // Per-kind reclaim tracking
        let mut reclaim_by_kind: HashMap<String, u64> = HashMap::new();
        for item in &selected {
            let tracked = is_git_tracked_dir(&item.path).await.unwrap_or(true);
            if tracked && !cfg.allow_tracked {
                table.add_row(vec![
                    Cell::new(human_bytes(item.bytes)),
                    Cell::new(&item.kind),
                    Cell::new(item.path.display().to_string()),
                    Cell::new("SKIP tracked").fg(Color::Yellow),
                ]);
                continue;
            }
            total += item.bytes;
            *reclaim_by_kind.entry(item.kind.clone()).or_default() += item.bytes;
            let status = if tracked { "tracked" } else { "untracked" };
            table.add_row(vec![
                Cell::new(human_bytes(item.bytes)),
                Cell::new(&item.kind),
                Cell::new(item.path.display().to_string()),
                Cell::new(status),
            ]);
            actionable.push(item.path.clone());
        }

        println!();
        println!("Selected {} paths:", selected.len());
        println!("{table}");

        // ── Per-kind reclaim summary ──
        if !reclaim_by_kind.is_empty() {
            let mut rk: Vec<_> = reclaim_by_kind.into_iter().collect();
            rk.sort_by_key(|b| std::cmp::Reverse(b.1));
            let summary: Vec<String> = rk
                .iter()
                .map(|(k, b)| format!("{} ({})", k, human_bytes(*b)))
                .collect();
            println!("Reclaim by kind: {}", summary.join(", "));
        }

        println!("Estimated reclaimed: {}", human_bytes(total));

        // ── Disk % projection ──
        if let Some(ref d) = disk {
            let projected_used = d.used_bytes.saturating_sub(total);
            let projected_pct =
                (projected_used as f64 / d.total_bytes as f64 * 100.0).round() as u8;
            println!(
                "Disk projection: {}% → {}% ({} free → {} free)",
                d.use_percent,
                projected_pct,
                human_bytes(d.avail_bytes),
                human_bytes(d.avail_bytes.saturating_add(total)),
            );
        }

        let user_protected = policy.guard.protected_paths.clone();
        if cfg.apply {
            for path in actionable {
                let safe_path = check_safe_to_delete(&path, &user_protected)?;
                if safe_path.exists() {
                    println!("🗑️  Deleting {}", path.display());
                    tokio::fs::remove_dir_all(&safe_path).await?;
                }
            }
            println!("✅ Cleanup complete.");
        } else {
            println!("💡 No changes made. Re-run with --apply to execute cleanup.");
        }
    } else {
        // Hint when not in cleanup mode
        println!();
        println!("💡 Run with --cleanup to see reclaimable space, or --cleanup --apply to delete.");
    }

    Ok(())
}

async fn cmd_guard_once(guard: &GuardPolicy, json: bool) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    let mut runtime = GuardRuntimeState::default();
    let report = run_guard_once(guard, &mut runtime).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // ── Disk health ──
    let disk = disk_details_for(&guard.disk_mount_path).await.ok();
    let state_label = report.disk_state.as_str();
    let state_icon = match state_label {
        "ok" => "✅",
        "warn" => "⚠️",
        "action" => "🟠",
        "critical" => "🔴",
        _ => "",
    };

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("STATUS"),
            Cell::new("CHECK"),
            Cell::new("VALUE"),
        ]);

    table.add_row(vec![
        Cell::new(if report.enabled { "✅" } else { "❌" }),
        Cell::new("Guard"),
        Cell::new(if report.enabled {
            "enabled"
        } else {
            "disabled"
        }),
    ]);

    if let Some(ref d) = disk {
        table.add_row(vec![
            Cell::new(state_icon),
            Cell::new("Disk Usage"),
            Cell::new(format!(
                "{}% ({}) — {} / {}",
                d.use_percent,
                state_label,
                human_bytes(d.used_bytes),
                human_bytes(d.total_bytes)
            )),
        ]);
        table.add_row(vec![
            Cell::new(""),
            Cell::new("Disk Free"),
            Cell::new(format!("{} on {}", human_bytes(d.avail_bytes), d.mount,)),
        ]);
    } else {
        table.add_row(vec![
            Cell::new(state_icon),
            Cell::new("Disk Usage"),
            Cell::new(format!("{}% ({})", report.disk_use_percent, state_label)),
        ]);
    }

    table.add_row(vec![
        Cell::new(if report.sync_frozen { "⏸️" } else { "" }),
        Cell::new("Sync Frozen"),
        Cell::new(if report.sync_frozen { "yes" } else { "no" }),
    ]);

    table.add_row(vec![
        Cell::new(""),
        Cell::new("Thresholds"),
        Cell::new(format!(
            "warn={}% action={}% critical={}%",
            guard.disk_warn_percent, guard.disk_action_percent, guard.disk_critical_percent
        )),
    ]);

    table.add_row(vec![
        Cell::new(""),
        Cell::new("Process Monitor"),
        Cell::new(format!(
            "cpu>{}% for >{}s, auto_renice={}",
            guard.process_cpu_percent, guard.process_sustain_secs, guard.auto_renice
        )),
    ]);

    if report.alerts.is_empty() {
        table.add_row(vec![
            Cell::new("✅"),
            Cell::new("Heavy Processes"),
            Cell::new("none"),
        ]);
    } else {
        table.add_row(vec![
            Cell::new("⚠️"),
            Cell::new("Heavy Processes"),
            Cell::new(format!("{} active", report.alerts.len())),
        ]);
    }

    // ADDED 2026-08-10 (v0.112.35): memory/swap pressure row.
    if let Some(ref m) = report.memory {
        let icon = match m.pressure.as_str() {
            "critical" => "🔴",
            "warn" => "⚠️",
            _ => "✅",
        };
        table.add_row(vec![
            Cell::new(icon),
            Cell::new("Memory Pressure"),
            Cell::new(format!(
                "{}: avail {}% swap {}%{} limited: {}",
                m.pressure,
                m.mem_available_percent,
                m.swap_used_percent,
                m.psi_full_avg10
                    .map(|v| format!(" PSI-full {:.1}%", v))
                    .unwrap_or_default(),
                if m.limited.is_empty() {
                    "-".to_string()
                } else {
                    m.limited.join(", ")
                }
            )),
        ]);
    }

    // ADDED 2026-08-10 (v0.112.35): zombie + fill-rate rows.
    table.add_row(vec![
        Cell::new(if report.zombies.is_empty() {
            "✅"
        } else {
            "⚠️"
        }),
        Cell::new("Zombies"),
        Cell::new(if report.zombies.is_empty() {
            "none".to_string()
        } else {
            format!(
                "{} (oldest {}s, e.g. pid={} {})",
                report.zombies.len(),
                report.zombies[0].age_secs,
                report.zombies[0].pid,
                report.zombies[0].comm
            )
        }),
    ]);
    table.add_row(vec![
        Cell::new(""),
        Cell::new("Disk Fill Rate"),
        Cell::new(
            report
                .disk_fill_gbph
                .map(|r| format!("{:.1} GiB/h", r))
                .unwrap_or_else(|| "< 0.1 GiB/h or settling".to_string()),
        ),
    ]);

    println!("{table}");

    // ── Process detail table ──
    if !report.alerts.is_empty() {
        let mut ptable = Table::new();
        ptable
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("PID"),
                Cell::new("CPU"),
                Cell::new("RSS"),
                Cell::new("SUSTAINED"),
                Cell::new("ACTION"),
                Cell::new("NICE"),
                Cell::new("COMMAND"),
            ]);
        for a in &report.alerts {
            ptable.add_row(vec![
                Cell::new(a.pid),
                Cell::new(format!("{:.1}%", a.cpu_percent)),
                Cell::new(format!("{}MiB", a.rss_mb)),
                Cell::new(format!("{}s", a.sustained_secs)),
                Cell::new(&a.action),
                Cell::new(a.nice_value),
                Cell::new(if a.args.is_empty() {
                    a.command.clone()
                } else {
                    format!("{} {}", a.command, a.args)
                }),
            ]);
        }
        println!();
        println!("Heavy processes:");
        println!("{ptable}");
    }

    Ok(())
}

async fn cmd_guard_daemon(guard: &mut GuardPolicy) -> Result<()> {
    if !guard.enabled {
        println!("guard disabled in policy");
        return Ok(());
    }
    let _lock = acquire_daemon_lock("dracon-system-guard")
        .with_context(|| "failed to acquire guard daemon lock")?;

    // ── Startup cleanup: rotate guard log if oversized ──
    {
        let log_path = if guard.guard_log_file.is_empty() {
            PathBuf::from("/tmp/dracon-system-guard.log")
        } else {
            PathBuf::from(&guard.guard_log_file)
        };
        let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
        if max_bytes > 0 {
            if let Ok(meta) = std::fs::metadata(&log_path) {
                if meta.len() > max_bytes {
                    if let Err(e) = std::fs::remove_file(&log_path) {
                        eprintln!("⚠️ startup: failed to rotate guard log: {}", e);
                    } else {
                        eprintln!("🧹 startup: rotated guard log (was {} bytes)", meta.len());
                    }
                }
            }
        }
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_sigterm = shutdown.clone();
    let shutdown_sigint = shutdown.clone();
    let reload = Arc::new(AtomicBool::new(false));
    let reload_sighup = reload.clone();
    let reload_sighup_handler = reload.clone();

    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
            veprintln!(1, "system: received SIGTERM, shutting down gracefully...");
            shutdown_sigterm.store(true, Ordering::SeqCst);
        } else {
            eprintln!("system: failed to set up SIGTERM handler");
        }
    });

    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            sig.recv().await;
            veprintln!(1, "system: received SIGINT, shutting down gracefully...");
            shutdown_sigint.store(true, Ordering::SeqCst);
        } else {
            eprintln!("system: failed to set up SIGINT handler");
        }
    });

    tokio::spawn(async move {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        {
            while sig.recv().await.is_some() {
                veprintln!(1, "system: received SIGHUP, reloading policy...");
                reload_sighup_handler.store(true, Ordering::SeqCst);
            }
        } else {
            eprintln!("system: failed to set up SIGHUP handler");
        }
    });

    veprintln!(
        1,
        "guard daemon started (interval={}s)",
        guard.interval_secs
    );
    let mut interval = guard.interval_secs;
    let mut runtime = GuardRuntimeState::default();
    while !shutdown.load(Ordering::SeqCst) {
        if reload_sighup.load(Ordering::SeqCst) {
            reload_sighup.store(false, Ordering::SeqCst);
            let result = load_system_policy();
            match result {
                Ok((policy_path, new_policy)) => {
                    if policy_path.is_none() {
                        eprintln!(
                            "system: SIGHUP reload warning: no policy file found, using defaults"
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "guard/policy-reload",
                            "SIGHUP reload: no policy file found, using defaults".to_string(),
                        ));
                    }
                    // Restore every reversible process adjustment before
                    // discarding the old runtime. This includes the v0.112.36
                    // memory-renice, OOM-bias, and CPUQuota maps as well as
                    // the legacy heavy-process renice map.
                    let adjustments_restored = restore_runtime_adjustments(&mut runtime).await;
                    *guard = new_policy.guard;
                    normalize_guard_policy(guard);
                    if adjustments_restored {
                        runtime = GuardRuntimeState::default();
                    } else {
                        eprintln!(
                            "⚠ SIGHUP retaining process-adjustment state after partial restore"
                        );
                    }
                    interval = guard.interval_secs;
                    veprintln!(
                        2,
                        "system: policy reloaded on SIGHUP (disk_warn={}%, disk_critical={}%)",
                        guard.disk_warn_percent,
                        guard.disk_critical_percent
                    );
                }
                Err(e) => {
                    eprintln!(
                        "system: SIGHUP reload warning: corrupted policy file, using defaults: {}",
                        e
                    );
                    emit_event(&DraconEvent::new(
                        "system",
                        EventSeverity::Error,
                        "guard/policy-reload",
                        format!("SIGHUP reload: policy corrupted, using defaults: {}", e),
                    ));
                }
            }
        }
        if let Err(e) = run_guard_once(guard, &mut runtime).await {
            eprintln!("guard pass failed: {}", e);
            emit_event(&DraconEvent::new(
                "system",
                EventSeverity::Error,
                "guard",
                format!("pass failed: {e}"),
            ));
        }
        // FIXED 2026-07-26 (audit H-12): `elapsed` must reset EVERY pass.
        // Previously declared once before the outer loop, so after the
        // first interval the inner sleep loop never ran again — the daemon
        // busy-looped guard passes back-to-back forever (continuous
        // df/ps/du + walkdir scans).
        let mut elapsed = 0u64;
        while !shutdown.load(Ordering::SeqCst) && elapsed < interval {
            sleep(Duration::from_secs(1)).await;
            elapsed += 1;
        }
    }
    if !restore_runtime_adjustments(&mut runtime).await {
        eprintln!("⚠ shutdown: some process adjustments could not be restored");
    }
    veprintln!(1, "system: guard daemon shutdown complete");
    Ok(())
}

async fn cmd_guard_prune(
    guard: &GuardPolicy,
    json: bool,
    docker: bool,
    docker_volumes: bool,
    package_caches: bool,
    apply: bool,
) -> Result<()> {
    let mut reclaimed_total = 0u64;
    let mut actions = Vec::new();

    if docker || docker_volumes {
        if apply {
            match docker_prune(apply, docker, docker_volumes).await {
                Ok(bytes) => {
                    actions.push(format!("Docker prune: {}", human_bytes(bytes)));
                    reclaimed_total += bytes;
                }
                Err(e) => {
                    actions.push(format!("Docker prune failed: {}", e));
                }
            }
        } else {
            actions.push("Docker prune (dry-run, skipped)".to_string());
        }
    }

    if package_caches {
        match clean_package_caches(true, true, true, true, apply, &guard.protected_paths).await {
            Ok((bytes, cleaned)) => {
                for c in cleaned {
                    actions.push(format!("Package cache: {}", c));
                }
                reclaimed_total += bytes;
            }
            Err(e) => {
                actions.push(format!("Package cache cleanup failed: {}", e));
            }
        }
    }

    if !docker && !docker_volumes && !package_caches {
        let disk = disk_use_percent_for(&guard.disk_mount_path).await?;
        println!("Disk usage: {}% (mount: {})", disk, guard.disk_mount_path);

        if let Ok((total, used, _free)) = get_inode_info().await {
            let pct = used.saturating_mul(100).checked_div(total).unwrap_or(0) as u8;
            println!("Inode usage: {}% ({}/{} inodes used)", pct, used, total);
        }

        println!();
        println!("Potential cleanup targets:");
        println!("  --docker          Prune unused Docker images/containers");
        println!("  --docker-volumes  Prune Docker volumes too (aggressive)");
        println!("  --package-caches  Clean cargo/npm/pip/go caches");
        println!();
        println!("Add --apply to execute cleanup.");
    }

    if json {
        #[derive(Serialize)]
        struct PruneReport {
            reclaimed_bytes: u64,
            reclaimed_human: String,
            actions: Vec<String>,
        }
        let report = PruneReport {
            reclaimed_bytes: reclaimed_total,
            reclaimed_human: human_bytes(reclaimed_total),
            actions,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if !actions.is_empty() {
        println!("Prune results:");
        for a in &actions {
            println!("  - {}", a);
        }
        println!("Total reclaimed: {}", human_bytes(reclaimed_total));

        if !apply && (docker || docker_volumes || package_caches) {
            println!();
            println!("Note: This was a dry-run. Add --apply to execute.");
        }
    }

    Ok(())
}

/// Represents which cleanup targets are enabled.
#[derive(Debug, Clone, Default)]
struct CleanTargets {
    rust: bool,
    trash: bool,
    nix: bool,
    caches: bool,
    node_modules: bool,
    docker: bool,
}

impl CleanTargets {
    /// Returns true if no targets are enabled.
    fn is_empty(&self) -> bool {
        !self.rust && !self.trash && !self.nix && !self.caches && !self.node_modules && !self.docker
    }

    /// Returns true if ANY target is enabled.
    fn any(&self) -> bool {
        self.rust || self.trash || self.nix || self.caches || self.node_modules || self.docker
    }
}

async fn cmd_guard_clean(
    guard: &GuardPolicy,
    json: bool,
    apply: bool,
    targets: CleanTargets,
    min_size_mb: Option<u64>,
) -> Result<()> {
    let do_all = targets.is_empty();
    if !do_all && !targets.any() {
        eprintln!("⚠️ No cleanup targets specified. Use --all to clean everything, or specify individual flags (--rust, --trash, --nix, --caches, --node-modules, --docker).");
        return Ok(());
    }
    let do_rust = targets.rust || do_all;
    let do_trash = targets.trash || do_all;
    let do_nix = targets.nix || do_all;
    let do_caches = targets.caches || do_all;
    let do_node = targets.node_modules || do_all;
    let do_docker = targets.docker || do_all;

    let mut guard_clone = guard.clone();
    if let Some(mb) = min_size_mb {
        guard_clone.cleanup_min_size_mb = mb;
    }

    let mut total_reclaimed = 0u64;
    let mut actions: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    if do_rust {
        let mut runtime = GuardRuntimeState::default();
        let result = auto_cleanup_rust_targets(&guard_clone, &mut runtime, apply).await?;
        total_reclaimed += result.reclaimed_bytes;
        for p in result.cleaned_paths {
            actions.push(format!("Rust: {}", p));
        }
        for p in result.protected_paths {
            actions.push(format!("Protected: {}", p));
        }
    }

    if do_trash {
        match empty_trash(
            apply,
            &guard_clone.protected_paths,
            guard_clone.trash_credential_guard,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Trash: {}", c));
                }
            }
            Err(e) => failures.push(format!("Trash: {}", e)),
        }
    }

    if do_nix {
        match clean_nix_garbage(guard_clone.nix_keep_generations, apply).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Nix: {}", c));
                }
            }
            Err(e) => failures.push(format!("Nix: {}", e)),
        }
    }

    if do_node {
        let roots: Vec<PathBuf> = guard_clone
            .node_modules_search_roots
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    return None;
                }
                let p = expand_tilde(s);
                if p.exists() {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        match clean_old_node_modules(
            &roots,
            guard_clone.node_modules_max_age_days,
            apply,
            &guard_clone.protected_paths,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Node: {}", c));
                }
            }
            Err(e) => failures.push(format!("Node: {}", e)),
        }
    }

    if do_caches {
        match clean_package_caches(true, true, true, true, apply, &guard_clone.protected_paths)
            .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                for c in cleaned {
                    actions.push(format!("Cache: {}", c));
                }
            }
            Err(e) => failures.push(format!("Cache: {}", e)),
        }
    }

    if do_docker {
        match docker_prune(apply, apply, guard_clone.docker_prune_volumes).await {
            Ok(bytes) => {
                total_reclaimed += bytes;
                if bytes > 0 {
                    actions.push(format!("Docker: {}", human_bytes(bytes)));
                }
            }
            Err(e) => failures.push(format!("Docker: {}", e)),
        }
    }

    if json {
        #[derive(Serialize)]
        struct CleanReport {
            reclaimed_bytes: u64,
            reclaimed_human: String,
            actions: Vec<String>,
            failures: Vec<String>,
            apply: bool,
        }
        let report = CleanReport {
            reclaimed_bytes: total_reclaimed,
            reclaimed_human: human_bytes(total_reclaimed),
            actions,
            failures,
            apply,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if actions.is_empty() && failures.is_empty() {
            println!("Nothing to clean.");
        } else {
            if !failures.is_empty() {
                eprintln!("⚠️ {} cleanup step(s) failed:", failures.len());
                for f in &failures {
                    eprintln!("  • {}", f);
                }
                println!();
            }
            println!(
                "Cleanup {}:",
                if apply {
                    "results"
                } else {
                    "preview (dry-run)"
                }
            );
            for a in &actions {
                println!("  • {}", a);
            }
            println!();
            println!("Total reclaimable: {}", human_bytes(total_reclaimed));
            if !apply {
                println!("Add --apply to execute cleanup.");
            }
        }
    }

    Ok(())
}

async fn cmd_guard(cmd: GuardCommands) -> Result<()> {
    let (_, policy) = load_system_policy()?;
    let mut guard = policy.guard;
    normalize_guard_policy(&mut guard);
    match cmd {
        GuardCommands::Once { json } => cmd_guard_once(&guard, json).await,
        GuardCommands::Daemon => cmd_guard_daemon(&mut guard).await,
        GuardCommands::Prune {
            json,
            docker,
            docker_volumes,
            package_caches,
            apply,
        } => cmd_guard_prune(&guard, json, docker, docker_volumes, package_caches, apply).await,
        GuardCommands::Clean {
            json,
            apply,
            rust,
            trash,
            nix,
            caches,
            node_modules,
            docker,
            all: _,
            min_size_mb,
        } => {
            let targets = CleanTargets {
                rust,
                trash,
                nix,
                caches,
                node_modules,
                docker,
            };
            cmd_guard_clean(&guard, json, apply, targets, min_size_mb).await
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    VERBOSITY.store(cli.verbose, Ordering::SeqCst);

    match cli.cmd {
        Commands::Status { json } => cmd_status(json).await,
        Commands::Doctor { json, strict } => cmd_doctor(json, strict).await,
        Commands::Storage {
            root,
            json,
            cleanup,
            apply,
            allow_tracked,
            min_size_mb,
            kinds,
        } => {
            cmd_storage(
                root,
                json,
                cleanup,
                apply,
                allow_tracked,
                min_size_mb,
                kinds,
            )
            .await
        }
        Commands::Link { cmd } => cmd_link(cmd),
        Commands::Symlinks {
            roots,
            json,
            max_depth,
        } => crate::links::cmd_symlinks(roots, json, max_depth),
        Commands::Guard { cmd } => cmd_guard(cmd).await,
        Commands::Events {
            tail,
            source,
            severity,
            dedup,
            json,
        } => cmd_events(tail, source, severity, dedup, json),
        Commands::Zram {
            status,
            gen_config,
            memory_percent,
            algorithm,
        } => cmd_zram(status, gen_config, memory_percent, algorithm),
    }
}
