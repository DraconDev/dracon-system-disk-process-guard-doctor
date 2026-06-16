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
}

#[derive(Debug, Clone)]
pub(crate) struct ProcSample {
    pub(crate) pid: i32,
    pub(crate) ppid: i32,
    pub(crate) cpu_percent: f32,
    pub(crate) rss_mb: u64,
    pub(crate) command: String,
    pub(crate) args: String,
}

#[derive(Default, Debug)]
pub(crate) struct GuardRuntimeState {
    pub(crate) heavy_since: HashMap<i32, Instant>,
    pub(crate) notify_cooldowns: HashMap<String, Instant>,
    pub(crate) last_disk_state: String,
    pub(crate) disk_history: Vec<(Instant, u8)>,
    pub(crate) active_build_pids: HashSet<i32>,
    pub(crate) reniced_pids: HashMap<i32, (i32, String)>,
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
            // Format: pid ppid pcpu rss comm args...
            let mut parts = trimmed.split_whitespace();
            let pid = parts.next()?.parse::<i32>().ok()?;
            let ppid = parts.next()?.parse::<i32>().ok()?;
            let cpu_percent = parts.next()?.parse::<f32>().ok()?;
            let rss_kb = parts.next()?.parse::<u64>().ok()?;
            let command = parts.next()?.to_string();
            let args = parts.collect::<Vec<_>>().join(" ");
            Some(ProcSample {
                pid,
                ppid,
                cpu_percent,
                rss_mb: rss_kb / 1024,
                command,
                args,
            })
        })
        .collect()
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
        .args(["-eo", "pid,ppid,pcpu,rss,comm,args", "--no-headers"])
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
    Ok(parse_ps_output(&String::from_utf8_lossy(&out.stdout)))
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
/// **INVARIANT: This function is the ONLY process management action the guard takes.**
/// The guard NEVER kills processes — it only renices. Killing is explicitly banned.
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
            // Find the project root (where Cargo.toml is)
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir);
                    break;
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
        let has_active_build = protected_project_dirs
            .iter()
            .any(|proj| target_project == proj);

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
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir);
                    break;
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
        let has_active_build = protected_project_dirs
            .iter()
            .any(|proj| target_project == proj);

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

/// Count zombie processes
async fn count_zombie_processes() -> Result<u64> {
    let out = Command::new("ps").args(["-eo", "stat="]).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "ps stat failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let count = text
        .lines()
        .filter(|line| {
            let stat = line.trim();
            // Zombie processes have 'Z' in their stat
            stat.contains('Z') || stat.starts_with('Z')
        })
        .count();

    Ok(count as u64)
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

/// Empty trash
async fn empty_trash(apply: bool, protected_paths: &[String]) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let trash_files = home.join(".local/share/Trash/files");
        let trash_info = home.join(".local/share/Trash/info");

        if trash_files.exists() {
            let size = get_dir_size(&trash_files).await.unwrap_or(0);
            if size > 0 {
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
        match empty_trash(apply, &guard.protected_paths).await {
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
        let since = state.heavy_since.entry(p.pid).or_insert(now);
        let sustained = now.duration_since(*since).as_secs();
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

        if guard.auto_renice {
            let already_niced = state.reniced_pids.get(&p.pid).map(|(n, _)| *n);
            let nice_val = graduated_nice_value(p.cpu_percent, p.rss_mb, guard.renice_value);
            if already_niced != Some(nice_val) {
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        state
                            .reniced_pids
                            .insert(p.pid, (nice_val, p.command.clone()));
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
                    "Heavy process {} (pid={} cpu={:.1}% rss={}MiB) sustained {}s{}",
                    p.command,
                    p.pid,
                    p.cpu_percent,
                    p.rss_mb,
                    sustained,
                    if nice_applied > 0 {
                        format!(" reniced={}", nice_applied)
                    } else {
                        String::new()
                    }
                ),
            )
            .await;
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
        if let Some((_nice, ref orig_cmd)) = state.reniced_pids.get(&pid) {
            let proc_cmdline = PathBuf::from(format!("/proc/{}/cmdline", pid));
            let same_process = match std::fs::read_to_string(&proc_cmdline) {
                Ok(content) => {
                    let cmd = content.replace('\0', " ");
                    let exe = cmd.split_whitespace().next().unwrap_or("");
                    let exe_name = std::path::Path::new(exe)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    exe_name == orig_cmd.as_str()
                }
                Err(_) => false,
            };
            if !same_process {
                eprintln!(
                    "🔧 skip un-renice pid={} — PID recycled (was {}, now different)",
                    pid, orig_cmd
                );
                state.reniced_pids.remove(&pid);
                state.cooled_since.remove(&pid);
                continue;
            }
        }
        let _ = renice_process(pid, 0).await;
        eprintln!("🔧 un-renice pid={} -> nice 0 (pressure released)", pid);
        state.reniced_pids.remove(&pid);
        state.cooled_since.remove(&pid);
    }
    state
        .cooled_since
        .retain(|pid, _| state.reniced_pids.contains_key(pid));

    // Clean up reniced_pids for processes that no longer exist
    state
        .reniced_pids
        .retain(|pid, _| PathBuf::from(format!("/proc/{}", pid)).exists());

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

async fn check_zombie_processes(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    if !guard.monitor_zombies {
        return;
    }
    if let Ok(zombie_count) = count_zombie_processes().await {
        if zombie_count > guard.zombie_threshold {
            let key = "zombie-warning".to_string();
            if should_notify(state, &key, guard.notify_cooldown_secs.max(3600)) {
                send_notification(
                    guard,
                    "Dracon System Guard - Zombie Processes",
                    &format!(
                        "Detected {} zombie processes (threshold: {})",
                        zombie_count, guard.zombie_threshold
                    ),
                )
                .await;
            }
        }
    }
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
    let used = disk_use_percent_for(&guard.disk_mount_path).await?;
    let dstate = disk_state(used, guard).to_string();
    let marker = sync_freeze_marker_path(guard);
    let mut sync_frozen = marker.exists();

    check_disk_trends(guard, state, used).await;
    check_disk_early_warning(guard, state, used).await;
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
    check_zombie_processes(guard, state).await;
    check_large_logs(guard, state).await;

    Ok(GuardReport {
        enabled: guard.enabled,
        disk_use_percent: used,
        disk_state: dstate,
        sync_frozen,
        alerts,
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
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_e) => {
            return Ok((Some(path), SystemPolicy::default()));
        }
    };
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

    let ls_out = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["ls-files", "--", &name])
        .output()
        .await;
    let ls_out = match ls_out {
        Ok(o) if o.status.success() => o,
        _ => {
            return Err(anyhow::anyhow!(
                "git ls-files failed for {} in {}",
                name,
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
            &report.system_root,
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
    let mut elapsed = 0u64;
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
                    *guard = new_policy.guard;
                    normalize_guard_policy(guard);
                    for (&pid, (_, ref orig_cmd)) in &runtime.reniced_pids {
                        let proc_cmdline = PathBuf::from(format!("/proc/{}/cmdline", pid));
                        let same_process = match std::fs::read_to_string(&proc_cmdline) {
                            Ok(content) => {
                                let cmd = content.replace('\0', " ");
                                let exe = cmd.split_whitespace().next().unwrap_or("");
                                let exe_name = Path::new(exe)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                exe_name == orig_cmd.as_str()
                            }
                            Err(_) => false,
                        };
                        if !same_process {
                            eprintln!(
                                "⚠ SIGHUP skip un-renice pid={} — PID recycled (was {})",
                                pid, orig_cmd
                            );
                            continue;
                        }
                        let _ = renice_process(pid, 0).await;
                    }
                    runtime = GuardRuntimeState::default();
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
        while !shutdown.load(Ordering::SeqCst) && elapsed < interval {
            sleep(Duration::from_secs(1)).await;
            elapsed += 1;
        }
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
        match empty_trash(apply, &guard_clone.protected_paths).await {
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
