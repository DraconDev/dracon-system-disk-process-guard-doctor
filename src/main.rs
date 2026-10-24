use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use fs2::FileExt;
use print as dr_print;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(test)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::sleep;

use dracon_system_lib::analyze_workspace_storage;

// Re-export policy items (types and utility fns that live in policy.rs)
// Note: GuardRuntimeState, ProcSample, AutoCleanupResult are in main.rs,
// so tests use crate::* to access them without explicit re-exports.
mod cpu_limit;
mod doctor;
#[cfg(test)]
mod ledger_safety_tests;
mod managed_oom;
mod oom_job;
mod oom_restore;
mod protection_probe;
#[cfg(test)]
mod protection_tests;
pub(crate) use doctor::*;
mod events;
pub(crate) use events::*;
mod forgotten;
pub(crate) use forgotten::*;
#[cfg(test)]
mod forgotten_tests;
mod links;
pub(crate) use links::*;
mod policy;
pub(crate) use policy::*;
mod stash;
pub(crate) use stash::*;
mod reap;
pub(crate) use reap::*;
mod relocate;
pub(crate) use relocate::*;
mod safety;
pub(crate) use safety::*;
mod setup;
pub(crate) use setup::*;
mod zram;
pub(crate) use zram::*;

#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod links_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod stash_tests;
#[cfg(test)]
mod reap_tests;
#[cfg(test)]
mod relocate_tests;
#[cfg(test)]
mod setup_tests;
#[cfg(test)]
mod zram_tests;

// Memory-leak fix: the unrenice loops used to `continue` forever on renice
// failure or ProcessIdentityStatus::Unavailable, never removing the entry from
// `memory_reniced_pids` / `reniced_pids`. That grew the map unboundedly and
// eventually OOM-killed the daemon inside its 250 MiB cgroup, which in turn
// dragged the wezterm cgroup down with it. We now bound the deferrals and
// drop the entry after this many attempts.
const IDENTITY_UNAVAILABLE_RETRY_LIMIT: u32 = 3;
const UNRENICE_RETRY_LIMIT: u32 = 3;

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

    acquire_daemon_lock_at(&lock_file)
}

fn acquire_daemon_lock_at(lock_file: &Path) -> Result<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_file)?;

    if FileExt::try_lock_exclusive(&file).is_err() {
        return Err(anyhow::anyhow!("lock file is held by another process"));
    }

    // Never truncate before acquiring the lock: another guard process could
    // otherwise erase the first process's lock-file contents before learning
    // that it must exit. Once the exclusive lock is held, clearing stale
    // diagnostic contents is safe.
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
    /// Launch a future job in a registered cooperative recovery boundary.
    ManagedJob {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<std::ffi::OsString>,
    },
    #[command(hide = true)]
    OomJobRecover,
    /// Retire an orphaned managed-job debt after its boundary is gone.
    OomJobReconcile {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    #[command(hide = true)]
    ManagedJobEnter {
        id: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Internal expiry recovery for a reversible CPU lease.
    #[command(hide = true)]
    CpuLeaseRelease {
        /// Durable lease key of the CPU lease to release.
        key: String,
        /// Generation of that lease; older generations are obsolete timers.
        generation: String,
    },
    /// Bounded disposable-process diagnostics under the caller's sandbox.
    #[command(hide = true)]
    ProtectionProbe { root: PathBuf, phase: String },
    #[command(hide = true)]
    ProtectionWorker {
        root: PathBuf,
        #[arg(long)]
        child: bool,
    },
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
        // CHANGED 2026-09-09 (audit F47): help advertised `critical`,
        // but EventSeverity has only Info/Warn/Error — nothing this
        // binary emits could ever match a critical filter.
        /// Filter by severity (info, warn, error).
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
        /// Minimum file size to consider (MiB). Unset falls back to the policy
        /// `storage.min_size_mb` (built-in default 512).
        #[arg(long)]
        min_size_mb: Option<u64>,
        // CHANGED 2026-09-09 (audit F36): the old help named kinds
        // (targets, trash, nix, caches, node_modules, docker) that match
        // NOTHING — the real vocabulary comes from the analyzer
        // (dracon-system-lib hotspot_kind) and the policy default:
        // rust-build, node-deps, build-output, cache (plus report-only
        // git-db, which is never deletable). A user-passed `--kinds
        // node_modules` silently selected zero hotspots.
        /// Comma-separated kinds to clean (rust-build, node-deps, build-output, cache).
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
        // CHANGED 2026-09-09 (audit F46): help listed 4 algos, code accepts 7.
        /// Compression algorithm (lzo, lzo-rle, lz4, lz4hc, zstd, deflate, 842).
        #[arg(long)]
        algorithm: Option<String>,
    },
    /// Guard runtime: monitor disk/process pressure and notify/mitigate.
    Guard {
        #[command(subcommand)]
        cmd: GuardCommands,
    },
    /// Relocate a cold directory to another root and leave a symlink (dry-run unless --apply).
    Relocate {
        /// Directory to relocate.
        path: PathBuf,
        /// Destination root (must exist, typically on the second disk).
        #[arg(long)]
        to: String,
        /// Perform the move (copy, verify, remove, link).
        #[arg(long)]
        apply: bool,
        /// Also relocate git-tracked directories (replaces the dir with a
        /// symlink, which breaks the repo — untrack first instead).
        #[arg(long)]
        allow_tracked: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stash: hold-then-delete staging with a TTL.
    Stash {
        #[command(subcommand)]
        cmd: StashCommands,
    },
    /// Check space-tier setup readiness (cold root, stash, mounts).
    Setup {
        /// Create missing cold/stash directories.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
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
enum StashCommands {
    /// Move a directory into stash (dry-run unless --apply).
    Move {
        /// Directory to stash.
        path: PathBuf,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
    /// List stash entries.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Restore an entry to its recorded origin.
    Restore {
        /// Entry name from `stash list`.
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete entries older than the TTL (dry-run unless --apply).
    Expire {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete ONE named entry, even if its manifest is unreadable.
    ///
    /// The escape hatch for an entry the fail-safe is pinning. It is always
    /// for a single named entry, never a bulk sweep of everything
    /// unreadable, and dry-run unless --apply.
    Purge {
        /// Entry name from `stash list`.
        name: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum GuardCommands {
    /// Run one guard evaluation pass.
    Once {
        #[arg(long)]
        json: bool,
    },
    /// Run continuous guard loop. A disabled policy exits 0; invalid startup policy exits 78 (EX_CONFIG).
    Daemon,
    /// Send one test notification through the configured notify_command.
    ///
    /// Exists because a broken notify_command is otherwise invisible until a
    /// real alert fires, and because it is the only way to confirm that
    /// argument syntax (`notify-send -u critical`) survives the round trip.
    /// Bypasses the notify cooldown; does not require `notify = true`.
    NotifyTest {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
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
    /// Clean all reclaimable space (rust, trash, nix, caches, node_modules, docker).
    /// A bare invocation selects every target; use target flags for a subset.
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
        /// Select every target and enable Docker's aggressive all-unused-images mode.
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
    // FIXED 2026-10-22: the field used to be "the legacy ~/.config/dracon
    // DIRECTORY exists". The directory is shared with other tools — on this
    // host it holds another tool's app state and two status tokens — so its
    // mere presence failed a REQUIRED check on a correctly deployed fleet,
    // and the remediation told the operator to move credentials. The check
    // now asks the question that matters: is a LEGACY DAEMON/GUARD CONFIG
    // FILE still there?
    pub(crate) legacy_daemon_config_present: bool,
    pub(crate) sync_service_active: bool,
    // ADDED 2026-10-01 (audit): the guard's OWN policy and service. This is the
    // guard utility — `doctor` used to audit only dracon-sync, so it said
    // nothing about whether the guard it ships is even configured or running.
    // `system_policy_exists` is resolved through effective_system_policy_path(),
    // so a DRACON_SYSTEM_POLICY override is honoured.
    pub(crate) system_policy_exists: bool,
    pub(crate) guard_service_active: bool,
    // ADDED 2026-10-03 (audit R3-L26): the M8 watchdog timers. Doctor
    // checked the two main services but not the backstops, so missing
    // timers still reported all-green.
    pub(crate) sync_watchdog_timer_active: bool,
    pub(crate) freeze_watchdog_timer_active: bool,
    pub(crate) guard_watchdog_timer_active: bool,
    /// Whether systemctl could be found at all. `sync_service_active` /
    /// `guard_service_active` are `false` when it could not, and before this
    /// field existed that was indistinguishable from "the service is down":
    //  a host without systemd was told to run `systemctl --user enable`.
    pub(crate) service_probe_available: bool,
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

/// ADDED 2026-09-26 (space tiers): per-mount status for extra mounts.
#[derive(Debug, Serialize)]
pub(crate) struct MountStatus {
    pub(crate) mount: String,
    pub(crate) use_percent: u8,
    pub(crate) state: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct GuardReport {
    enabled: bool,
    disk_use_percent: u8,
    disk_state: String,
    sync_frozen: bool,
    /// ADDED 2026-09-26 (space tiers): extra mounts from
    /// `disk_extra_mounts`. Visibility only; decisions key off primary.
    extra_mounts: Vec<MountStatus>,
    /// ADDED 2026-09-27 (space tiers Phase 2): cold candidates scanned at
    /// action/critical (empty below action or when unconfigured), plus
    /// what this pass actually moved.
    relocate_candidates: Vec<ColdCandidate>,
    relocated_count: usize,
    relocated_bytes: u64,
    alerts: Vec<GuardProcessAlert>,
    /// ADDED 2026-08-10 (v0.112.35): memory/swap pressure snapshot.
    memory: Option<MemoryReport>,
    /// ADDED 2026-08-10 (v0.112.35): zombie detail (pid/ppid/age/parent).
    zombies: Vec<ZombieInfo>,
    /// Abandoned dev/test processes. A worklist for a human by default;
    /// under the `reap_stale_dev_servers` opt-in the guard also acted on
    /// it -- see `reaped` for what each candidate got. Details in reap.rs.
    reap_candidates: Vec<ReapCandidate>,
    /// What the opt-in auto-reap pass did this cycle (empty unless
    /// `reap_stale_dev_servers` is set): kills AND skipped re-verifies.
    reaped: Vec<ReapedProcess>,
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
    /// Swap-in rate (pages/s) — first-class thrash signal alongside PSI
    /// (was a PSI-absent fallback only, until 2026-10-04).
    pub(crate) pswpin_rate: Option<f64>,
    /// The instantaneous classification before persistence/hysteresis.
    pub(crate) observed_pressure: String,
    /// The stabilized classification used for notifications and mitigation:
    /// "ok" | "warn" | "critical".
    pub(crate) pressure: String,
    /// Top RSS offenders for diagnostics and optional pressure mitigation.
    pub(crate) top_rss: Vec<ProcSample>,
    /// ADDED 2026-08-10 (v0.112.36): actions taken this pass, e.g.
    /// "renice svelte-check=10", "oom-bias node=250".
    pub(crate) limited: Vec<String>,
    /// What the pressure-gated orphan pass did this cycle (empty unless
    /// `reap_orphans_on_pressure` is set and pressure is warn/critical):
    /// kills AND skipped re-verifies, same record shape as `reaped`.
    pub(crate) pressure_reaped: Vec<ReapedProcess>,
    /// ADDED 2026-10-22; CHANGED 2026-10-23 to trees: heavy, idle,
    /// orphaned TREES that are provably TEST jobs. Reported on every pass;
    /// ended only under memory pressure (see `forgotten_reaped`). Details
    /// in forgotten.rs.
    pub(crate) forgotten_trees: Vec<ForgottenTree>,
    /// ADDED 2026-10-22; CHANGED 2026-10-23 to trees: heavy trees that
    /// were CONSIDERED and vetoed because they look like a live desktop
    /// session. Reported so the operator can see that a heavy Chrome was
    /// weighed and left alone.
    pub(crate) vetoed_trees: Vec<VetoedTree>,
    /// ADDED 2026-10-22; CHANGED 2026-10-23 to trees: what the
    /// pressure-gated forgotten-orphan pass ended this cycle (empty unless
    /// pressure is warn/critical).
    pub(crate) forgotten_reaped: Vec<ReapedTree>,
    /// ADDED 2026-10-24 (audit): heavy orphaned trees that were weighed and
    /// deliberately NOT ended — a client is still connected to one, or it is
    /// busy with no debugging port to check. Reported because a heavy
    /// orphan the guard is leaving alone is exactly what the operator needs
    /// to see; the old code skipped such trees SILENTLY.
    pub(crate) protected_trees: Vec<ProtectedTree>,
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

/// `Serialize`/`Deserialize` are for the on-disk mitigation ledger
/// (audit 2026-10-01): a crash during a critical-pressure episode used to
/// restart the guard with empty maps, stranding `nice`/`oom_score_adj`
/// adjustments on live processes with no record that they needed restoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Swapped-out footprint in MiB.
    ///
    /// ADDED 2026-10-22: RSS alone misses the largest processes once the
    /// kernel has evicted most of their pages — a process can be down to a
    /// few MiB resident while holding tens of GiB in swap. The mitigation
    /// ranking uses RSS + swap (see `memory_pressure_footprint_mb`), and
    /// this is the swapped half of that figure.
    pub(crate) swap_mb: u64,
    /// Current Unix nice value from `ps ni`, captured before any limiter
    /// changes it so memory-pressure release can restore the original value.
    pub(crate) nice: i32,
    pub(crate) command: String,
    pub(crate) args: String,
    pub(crate) starttime: u64,
}

/// The footprint that decides who is an offender: resident PLUS swapped.
///
/// ADDED 2026-10-22: the kernel evicts pages under pressure, so the process
/// holding the most memory is often the one with the LEAST resident. Ranking
/// by RSS alone therefore throttles the wrong processes (and misses the
/// swap hogs entirely) exactly when memory is short.
pub(crate) fn memory_pressure_footprint_mb(sample: &ProcSample) -> u64 {
    sample.rss_mb.saturating_add(sample.swap_mb)
}

/// The swapped-out footprint of a process, in MiB.
///
/// ADDED 2026-10-22: `VmSwap` from `/proc/<pid>/status`. None when the
/// process is gone or the field is unreadable — the caller then treats the
/// swap as unknown rather than as zero, so a missing reading never makes a
/// heavy process look small.
pub(crate) fn read_proc_swap_mb(pid: i32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmSwap:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    Some(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyReniceState {
    pub(crate) original_nice: i32,
    pub(crate) applied_nice: i32,
    pub(crate) identity: ProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryReniceState {
    pub(crate) original_nice: i32,
    pub(crate) applied_nice: i32,
    pub(crate) identity: ProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OomPendingDescendant {
    pub(crate) root_pid: i32,
    pub(crate) original_adj: i32,
    pub(crate) identity: ProcessIdentity,
    /// Retry/escalation counter, persisted with the obligation. Failed reads
    /// or writes are not proof of restoration; retain them across restarts
    /// until restored, externally changed, gone, or provably reused.
    pub(crate) attempts: u32,
}

/// Escalate after this many failed passes. This bounds initial warning noise,
/// not the lifetime of unresolved restoration state. Roots restore promptly
/// while independent descendant obligations remain durable.
const OOM_DESCENDANT_RETRY_LIMIT: u32 = 5;

/// Cap on remembered descendant incarnations per biased root. ADDED
/// 2026-10-01 (audit): the set grew by one entry per descendant ever seen and
/// was only cleared when the root left `oom_biased_pids`, so a long
/// critical-pressure episode under a forking root accumulated thousands of dead
/// keys. Dropping an entry is safe: the worst case is that a re-forked PID is
/// treated as new, which is exactly what happens for a genuinely new child.
const OOM_KNOWN_DESCENDANTS_CAP: usize = 256;

/// Keep the remembered-descendant bookkeeping bounded.
///
/// ADDED 2026-10-01 (audit): a root that is no longer biased loses its set
/// entirely; a root that IS still biased keeps only descendants that are still
/// alive, once its set grows past `OOM_KNOWN_DESCENDANTS_CAP`. Dropping an entry
/// is safe — the worst case is that a re-forked PID is treated as new, which is
/// exactly what happens for a genuinely new child.
fn prune_oom_known_descendants(state: &mut GuardRuntimeState, all_processes: &[ProcSample]) {
    let live: HashSet<(i32, u64)> = all_processes.iter().map(|p| (p.pid, p.starttime)).collect();
    state.oom_known_descendants.retain(|pid, known| {
        if !state.oom_biased_pids.contains_key(pid) {
            return false;
        }
        if known.len() > OOM_KNOWN_DESCENDANTS_CAP {
            known.retain(|key| live.contains(key));
        }
        true
    });
}

/// Bump the attempt counter; true once failures warrant escalation.
fn oom_descendant_attempts_exhausted(pending: &mut OomPendingDescendant) -> bool {
    pending.attempts = pending.attempts.saturating_add(1);
    pending.attempts >= OOM_DESCENDANT_RETRY_LIMIT
}

fn record_oom_retry(
    state: &mut GuardRuntimeState,
    key: (i32, u64),
    pending: &mut OomPendingDescendant,
) -> bool {
    let escalated = oom_descendant_attempts_exhausted(pending);
    state.oom_pending_descendants.insert(key, pending.clone());
    // Bound warning noise, not the lifetime of a real restoration obligation.
    escalated
        && (pending.attempts == OOM_DESCENDANT_RETRY_LIMIT || pending.attempts.is_power_of_two())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ReportState {
    pub(crate) value: String,
    pub(crate) last_emitted: Option<Instant>,
}

/// What a background disk-cleanup pass produced, handed back to the guard
/// pass so the report can still show it.
///
/// ADDED 2026-10-24: the cleanup runs in its own task, so its result
/// arrives on a LATER pass than the one that started it.
#[derive(Debug, Clone, Default)]
pub(crate) struct CleanupSummary {
    pub(crate) relocated_count: usize,
    pub(crate) relocated_bytes: u64,
    pub(crate) relocate_candidates: Vec<ColdCandidate>,
}

#[derive(Default, Debug)]
pub(crate) struct GuardRuntimeState {
    /// CHANGED 2026-07-21 (v0.112.33, audit M34/F4.8): value is now
    /// `(first_seen, proc_starttime)` — the `/proc/<pid>/stat`
    /// starttime is recorded at first sight and re-verified before
    /// any renice, closing the PID-reuse window (a PID recycled
    /// during the sustain window gets a DIFFERENT starttime and is
    /// skipped).
    pub(crate) heavy_since: HashMap<i32, (Instant, u64)>,
    pub(crate) notify_cooldowns: HashMap<String, Instant>,
    /// State-aware event throttling. Repeated unchanged observations are
    /// retained for on-demand inspection but are not emitted every cycle.
    pub(crate) report_states: HashMap<String, ReportState>,
    pub(crate) last_disk_state: String,
    pub(crate) disk_history: Vec<(Instant, u8)>,
    /// ADDED 2026-08-10 (v0.112.35): byte-precise df history for
    /// rapid-fill rate detection (percent deltas are too coarse on
    /// large disks: 1% of 907 GiB is ~9 GiB).
    pub(crate) disk_bytes_history: Vec<(Instant, u64)>,
    /// ADDED 2026-08-10 (v0.112.35): first-sight timestamps for
    /// zombie processes so alerts can report zombie age.
    pub(crate) zombies_since: HashMap<(i32, u64), Instant>,
    /// ADDED 2026-08-10 (v0.112.35): last pswpin/pswpout counters
    /// for the swap-thrash fallback when PSI is unavailable.
    pub(crate) prev_swap_counters: Option<(Instant, u64, u64)>,
    /// Hysteresis state for memory pressure. A transient observed state
    /// must persist before it can trigger mitigation or notification.
    pub(crate) memory_pressure_state: String,
    pub(crate) memory_pressure_candidate: String,
    pub(crate) memory_pressure_candidate_since: Option<Instant>,
    /// ADDED 2026-08-10 (v0.112.36): pids reniced by the memory-
    /// pressure limiter (original/applied nice, stable process identity),
    /// and their release timers.
    pub(crate) memory_reniced_pids: HashMap<i32, MemoryReniceState>,
    pub(crate) memory_cooled_since: HashMap<i32, Instant>,
    /// A user service without CAP_SYS_NICE can lower a process's priority but
    /// cannot raise it back during recovery. Remember that limitation so the
    /// guard disables reversible renice actions instead of leaving processes
    /// permanently deprioritized and retrying noisy restorations forever.
    pub(crate) nice_restore_capability_warned: bool,
    /// ADDED 2026-10-24: pids the guard refused to renice because it could
    /// not restore their priority afterwards (a different user namespace).
    /// Reported once per pid, so the operator can see WHY a mitigation that
    /// used to apply has stopped applying.
    pub(crate) nice_restore_skipped_pids: std::collections::HashSet<i32>,
    /// ADDED 2026-10-24: the slot a background disk-cleanup task writes its
    /// summary into. The pass never waits for the cleanup, so the summary
    /// arrives on a later pass.
    pub(crate) cleanup_slot: Option<Arc<tokio::sync::Mutex<CleanupSummary>>>,
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
    pub(crate) reniced_pids: HashMap<i32, LegacyReniceState>,
    pub(crate) cooled_since: HashMap<i32, Instant>,
    pub(crate) guard_cycle: u64,
    /// The exact bytes last written to the on-disk mitigation ledger, so an
    /// unchanged pass does not rewrite the file every interval (audit
    /// 2026-10-01).
    pub(crate) last_persisted_mitigations: Option<Vec<u8>>,
    /// Only the long-running daemon persists its ledger. A one-shot
    /// `guard once` restores every limiter before it exits (cmd_guard_once), and
    /// the test suite calls `run_guard_once` directly — neither should write
    /// into the operator's `~/.dracon` (audit 2026-10-01).
    pub(crate) persist_mitigations: bool,
    /// Existing durable intent could not be safely loaded. Continue observing,
    /// but never overwrite it or apply new process mitigations.
    pub(crate) mitigation_ledger_unavailable: bool,
    pub(crate) last_proactive_cleanup: Option<Instant>,
    /// Last action-level cleanup scan. Even report-only scans are bounded
    /// because they walk large Rust and Node trees.
    pub(crate) last_auto_cleanup: Option<Instant>,
    /// ADDED 2026-10-02: last time the guard ran stash expiry itself.
    /// Separate from `last_auto_cleanup` on purpose: expiry drains the SECOND
    /// disk and must not inherit the reclaim cadence, or an operator tuning how
    /// often cleanup runs would silently change when a TTL is enforced.
    pub(crate) last_stash_expire: Option<Instant>,
    /// ADDED 2026-08-30 (memory-leak fix): bounded retry counters for the
    /// unrenice loops. The loops used to `continue` forever on
    /// ProcessIdentityStatus::Unavailable or a failed renice, never
    /// dropping the entry from `memory_reniced_pids` / `reniced_pids`.
    /// That grew the maps unboundedly and OOM-killed the daemon inside
    /// its 250 MiB cgroup, dragging wezterm down with it. We now drop
    /// the entry after IDENTITY_UNAVAILABLE_RETRY_LIMIT / UNRENICE_RETRY_LIMIT.
    pub(crate) memory_identity_unavailable_attempts: HashMap<i32, u32>,
    pub(crate) memory_unrenice_failures: HashMap<i32, u32>,
    /// Same bounded defer as the unrenice loops: an oom-restore or
    /// cpu-uncap whose process identity stays unreadable is dropped after
    /// IDENTITY_UNAVAILABLE_RETRY_LIMIT instead of retaining the entry
    /// forever.
    pub(crate) oom_identity_unavailable_attempts: HashMap<i32, u32>,
    pub(crate) cap_identity_unavailable_attempts: HashMap<i32, u32>,
    pub(crate) legacy_identity_unavailable_attempts: HashMap<i32, u32>,
    pub(crate) legacy_unrenice_failures: HashMap<i32, u32>,
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
    // FIXED 2026-10-03 (audit R4-SYS-15): saturate, don't panic/wrap.
    // `df` output is local so absurd input is unlikely, but a plain
    // `* 1024` panics in debug (killing the daemon pass) and wraps in
    // release — the file's own convention is `saturating_mul`.
    let total_bytes = parts[1].parse::<u64>().ok()?.saturating_mul(1024);
    let used_bytes = parts[2].parse::<u64>().ok()?.saturating_mul(1024);
    let avail_bytes = parts[3].parse::<u64>().ok()?.saturating_mul(1024);
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

/// Keep the untrusted process-table payload comfortably below the guard's
/// 250 MiB cgroup limit. This is deliberately a cap on the *transport*, not
/// just on the parsed rows: `Command::output()` would otherwise retain an
/// attacker-controlled or pathological process list before parsing it.
const MAX_PROCESS_SAMPLE_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// `/proc/<pid>/cmdline` is only read for selected heavy/top-RSS processes,
/// never for the whole process table. Keep even those diagnostic strings
/// bounded because argv is controlled by the process being inspected.
const MAX_PROCESS_CMDLINE_BYTES: usize = 4096;

/// Read `ps` stdout incrementally and fail closed if it exceeds the bounded
/// metadata-only process-table budget. Killing the child on overflow prevents
/// a giant process population from continuing to consume the pipe/cgroup.
async fn bounded_process_table_output() -> Result<Vec<u8>> {
    let mut child = Command::new("ps")
        // Do not request `args` here. A process can make argv arbitrarily
        // large, and `ps ... args` plus `Command::output()` was the direct
        // path to the guard's 250 MiB OOM incident. Command lines are loaded
        // later, bounded, and only for the few selected candidates.
        .args(["-eo", "pid=,ppid=,pcpu=,rss=,ni=,comm=", "--no-headers"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "ps spawn failed: {} (is /run/current-system/sw/bin on PATH?)",
                e
            )
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("ps stdout pipe was not captured"))?;
    let mut output = Vec::with_capacity(MAX_PROCESS_SAMPLE_OUTPUT_BYTES.min(64 * 1024));
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = match stdout.read(&mut chunk).await {
            Ok(read) => read,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error.into());
            }
        };
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > MAX_PROCESS_SAMPLE_OUTPUT_BYTES {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(anyhow::anyhow!(
                "ps metadata output exceeded {} MiB; process sample skipped",
                MAX_PROCESS_SAMPLE_OUTPUT_BYTES / (1024 * 1024)
            ));
        }
        output.extend_from_slice(&chunk[..read]);
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "ps command failed (exit {}): metadata output was {} bytes",
            status.code().unwrap_or(-1),
            output.len()
        ));
    }
    Ok(output)
}

/// Escape command-line control characters before putting them in journal or
/// JSON output. In particular, argv may contain newlines; never let a process
/// forge additional log records.
fn sanitize_process_cmdline(raw: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut sanitized = String::with_capacity(text.len().min(MAX_PROCESS_CMDLINE_BYTES));
    for ch in text.chars() {
        match ch {
            '\0' => sanitized.push(' '),
            '\n' => sanitized.push_str("\\n"),
            '\r' => sanitized.push_str("\\r"),
            '\t' => sanitized.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(&mut sanitized, "\\u{{{:04x}}}", ch as u32);
            }
            ch => sanitized.push(ch),
        }
    }
    let sanitized = sanitized.trim_end().to_string();
    if truncated {
        if sanitized.is_empty() {
            "[truncated]".to_string()
        } else {
            format!("{} [truncated]", sanitized)
        }
    } else {
        sanitized
    }
}

fn read_process_cmdline_from(root: &Path, pid: i32) -> String {
    let path = root.join(pid.to_string()).join("cmdline");
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut reader = file.take((MAX_PROCESS_CMDLINE_BYTES + 1) as u64);
    let mut raw = Vec::with_capacity(MAX_PROCESS_CMDLINE_BYTES + 1);
    if reader.read_to_end(&mut raw).is_err() {
        return String::new();
    }
    let truncated = raw.len() > MAX_PROCESS_CMDLINE_BYTES;
    if truncated {
        raw.truncate(MAX_PROCESS_CMDLINE_BYTES);
    }
    sanitize_process_cmdline(&raw, truncated)
}

/// Read command-line metadata for cache protection without treating an
/// unavailable `/proc` entry as proof that an unknown process is harmless.
/// ENOENT is expected when a process exits during the snapshot; all other
/// failures abort cache cleanup so interpreter/script wrappers fail closed.
fn read_package_process_cmdline(root: &Path, pid: i32) -> Result<Option<String>> {
    let process_dir = root.join(pid.to_string());
    let path = process_dir.join("cmdline");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing PID directory means the process exited during the
            // snapshot. An existing PID directory without cmdline is an
            // incomplete/uninspectable record and must fail closed.
            match fs::metadata(&process_dir) {
                Ok(_) => {
                    return Err(anyhow::anyhow!(
                        "process {} has no command line for package-cache protection",
                        pid
                    ));
                }
                Err(directory_error) if directory_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(directory_error) => {
                    return Err(directory_error).with_context(|| {
                        format!(
                            "cannot inspect process {} for package-cache protection",
                            pid
                        )
                    })
                }
            }
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot inspect process {} command line for package-cache protection",
                    pid
                )
            })
        }
    };

    let mut reader = file.take((MAX_PROCESS_CMDLINE_BYTES + 1) as u64);
    let mut raw = Vec::with_capacity(MAX_PROCESS_CMDLINE_BYTES + 1);
    if let Err(error) = reader.read_to_end(&mut raw) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return match fs::metadata(&process_dir) {
                Ok(_) => Err(anyhow::anyhow!(
                    "process {} command line disappeared while inspecting package-cache protection",
                    pid
                )),
                Err(directory_error) if directory_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(directory_error) => Err(directory_error).with_context(|| {
                    format!(
                        "cannot inspect process {} after command-line read failure",
                        pid
                    )
                }),
            };
        }
        return Err(error).with_context(|| {
            format!(
                "cannot read process {} command line for package-cache protection",
                pid
            )
        });
    }
    let truncated = raw.len() > MAX_PROCESS_CMDLINE_BYTES;
    if truncated {
        raw.truncate(MAX_PROCESS_CMDLINE_BYTES);
    }
    Ok(Some(sanitize_process_cmdline(&raw, truncated)))
}

fn load_process_cmdline(mut sample: ProcSample) -> ProcSample {
    sample.args = read_process_cmdline_from(Path::new("/proc"), sample.pid);
    sample
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
                swap_mb: 0,
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

/// Test whether a Linux process has the privilege needed to restore a nice
/// value after lowering a process's priority. `NoNewPrivileges=true` user
/// services commonly lack this capability even when they can successfully
/// perform the initial `renice`.
pub(crate) fn has_nice_restore_privilege_from_status(status: &str) -> bool {
    let uid = status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == "Uid")
            .then(|| value.split_whitespace().next()?.parse::<u64>().ok())
            .flatten()
    });
    if uid == Some(0) {
        return true;
    }
    let cap_eff = status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == "CapEff")
            .then(|| u64::from_str_radix(value.trim(), 16).ok())
            .flatten()
    });
    // Linux CAP_SYS_NICE is capability 23.
    cap_eff.is_some_and(|caps| caps & (1u64 << 23) != 0)
}

fn has_nice_restore_privilege() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .map(|status| has_nice_restore_privilege_from_status(&status))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // The guard's process controls are Linux-specific in practice; keep
        // other platforms permissive so their existing behavior is retained.
        true
    }
}

fn nice_restore_capability_available(state: &mut GuardRuntimeState) -> bool {
    let available = has_nice_restore_privilege();
    if !available && !state.nice_restore_capability_warned {
        eprintln!(
            "⚠️ renice mitigation disabled: guard lacks CAP_SYS_NICE needed to restore priority"
        );
        state.nice_restore_capability_warned = true;
    }
    available
}

/// The inode number of the user namespace a process lives in.
///
/// ADDED 2026-10-24. `CAP_SYS_NICE` is NAMESPACED: a process that holds it
/// in a nested user namespace has it in that namespace and in its
/// DESCENDANTS, never in an ancestor. The guard's unit runs in a nested
/// user namespace while most of the host's processes live in the initial
/// one, so a capability check on the guard's own `/proc/self/status` says
/// "yes, I can restore priority" for a process the guard provably cannot
/// touch.
pub(crate) fn user_namespace_inode(proc_root: &Path, pid: i32) -> Option<u64> {
    let link = fs::read_link(proc_root.join(pid.to_string()).join("ns/user")).ok()?;
    parse_namespace_inode(&link)
}

/// `user:[4026531837]` -> `4026531837`.
pub(crate) fn parse_namespace_inode(link: &Path) -> Option<u64> {
    let text = link.to_string_lossy();
    let inner = text
        .strip_prefix("user:[")
        .and_then(|rest| rest.strip_suffix(']'))?;
    inner.parse().ok()
}

/// Whether the guard's own user namespace is the target's, so that a
/// `CAP_SYS_NICE` the guard holds applies to the target.
///
/// A capability held in a namespace applies in that namespace and in its
/// DESCENDANTS only. Two different namespaces therefore mean the guard's
/// capability does not reach the target, and the target's priority cannot
/// be restored. An unreadable namespace on either side is treated as
/// "different": the cost of being wrong there is a process left at a
/// degraded priority, while the cost of being wrong the other way is a
/// mitigation that can never be undone.
pub(crate) fn nice_restore_namespace_ok(own_ns: Option<u64>, target_ns: Option<u64>) -> bool {
    matches!((own_ns, target_ns), (Some(own), Some(target)) if own == target)
}

/// Whether the guard can put this process's priority back the way it found
/// it — the NAMESPACE half of that question.
///
/// ADDED 2026-10-24. The capability half (`CAP_SYS_NICE`) is checked
/// separately, where the guard decides whether to apply a mitigation at
/// all. This half answers the question that actually broke: the guard's
/// unit runs in a nested user namespace while most of the host's
/// processes live in the initial one, so the guard could always LOWER a
/// process's priority (which needs no capability at all) and could never
/// RAISE it again. The renice mitigation was a one-way ratchet, and
/// because the undo always failed with `Permission denied`, EVERY SIGHUP
/// policy reload was deferred forever — the guard became impossible to
/// reconfigure on this host.
///
/// The guard's own namespace is read from the REAL `/proc`, not from
/// `proc_root`: it is a property of the running guard, and `proc_root` is
/// a fixture tree in tests that does not contain the guard.
pub(crate) fn can_restore_nice_for(proc_root: &Path, pid: i32) -> bool {
    let own = user_namespace_inode(Path::new("/proc"), std::process::id() as i32);
    let target = user_namespace_inode(proc_root, pid);
    nice_restore_namespace_ok(own, target)
}

/// Whether a failed `renice` is a PERMANENT authority failure: the guard
/// is not permitted to set this process's priority at all, so retrying
/// will never succeed.
///
/// ADDED 2026-10-24. A restore that failed for a permanent reason must
/// not hold the policy reload hostage forever — the old behaviour kept
/// the previous policy live and deferred every subsequent reload, which
/// is how a single un-restorable renice made the whole guard
/// unreconfigurable.
pub(crate) fn nice_restore_failure_is_permanent(message: &str) -> bool {
    message.contains("Permission denied") || message.contains("Operation not permitted")
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

fn record_swap_counters(state: &mut GuardRuntimeState, pswpin: u64, pswpout: u64) {
    state.prev_swap_counters = Some((Instant::now(), pswpin, pswpout));
}

/// Parse one `/proc/<pid>/stat` line for zombie detection.
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
    // Reclamation starts a new growth window. Otherwise a large cleanup can
    // hide fresh rapid writes behind a negative slope for up to 30 samples.
    let start = recent
        .windows(2)
        .rposition(|pair| pair[1].1 < pair[0].1)
        .map_or(0, |index| index + 1);
    let recent = &recent[start..];
    if recent.len() < 3 {
        return None;
    }
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
    let output = bounded_process_table_output().await?;
    let parsed = parse_ps_output(&String::from_utf8_lossy(&output));
    // ADDED 2026-07-21 (v0.112.33, audit M34/F4.8): verify each
    // sample's pid/comm pair out-of-band via /proc/<pid>/comm. A
    // process can embed `\n` in its comm value, and `ps` then prints
    // it raw — `parse_ps_output` could treat the injected text as
    // additional rows, letting a local process FABRICATE a heavy
    // sample for an arbitrary victim PID (which the guard would then
    // renice). The injected row's pid/comm pair doesn't match a real
    // process, so this filter kills it. Rows for just-exited PIDs are
    // dropped the same way. Preserve the verified starttime in the
    // sample so any later adjustment is tied to this exact PID
    // incarnation. Command lines are intentionally not part of this
    // broad scan; `load_process_cmdline` reads a bounded value only
    // after a row has been selected for diagnostics.
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
    // Argument support (audit DECIDE #1 follow-up, 2026-09-29): the value is
    // split into a program plus an argv array, so
    // `/usr/bin/notify-send -u critical` works without a wrapper script.
    // Parsing happens in policy.rs::NotifyCommand::parse, which rejects
    // unterminated quotes, relative programs, and shell / privilege /
    // pass-through programs. `Command::new` execs directly — no shell is
    // involved, so metacharacters in the title or body are inert.
    let cmd = match NotifyCommand::parse(&guard.notify_command) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("⚠️ notify_command rejected: {e}");
            return;
        }
    };
    // Operator-supplied arguments come first, then the notification text —
    // which is the order `notify-send` and friends expect (flags, summary,
    // body).
    let mut command = Command::new(&cmd.program);
    command.args(&cmd.args).arg(title).arg(body);
    if let Err(e) = command.output().await {
        eprintln!("⚠️ notification failed: {}", e);
    }
}

fn rotate_guard_log_if_oversized(path: &Path, max_bytes: u64, startup: bool) {
    if max_bytes == 0 {
        return;
    }
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }

    if let Err(e) = fs::remove_file(path) {
        if startup {
            eprintln!("⚠️ startup: failed to rotate guard log: {}", e);
        } else {
            eprintln!("⚠️ failed to rotate guard log: {}", e);
        }
    } else if startup {
        eprintln!("🧹 startup: rotated guard log (was {} bytes)", meta.len());
    }
}

fn log_guard_event(guard: &GuardPolicy, event: &str, details: &str) {
    log_guard_event_with_home(guard, event, details, dirs::home_dir().as_deref());
}

fn log_guard_event_with_home(guard: &GuardPolicy, event: &str, details: &str, home: Option<&Path>) {
    let Some(path) = resolve_guard_log_path_with_home(&guard.guard_log_file, home) else {
        return;
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("⚠️ failed to create log dir: {}", e);
            return;
        }
    }
    let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
    rotate_guard_log_if_oversized(&path, max_bytes, false);
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

/// Update a report state and decide whether a structured event is due.
/// State transitions are emitted immediately; an unchanged non-OK state is
/// repeated only after `repeat_secs`. This keeps the event stream useful for
/// diagnosis without turning a 30-second guard loop into notification spam.
pub(crate) fn report_state_transition(
    state: &mut GuardRuntimeState,
    key: &str,
    value: &str,
    repeat_secs: u64,
) -> (Option<String>, bool) {
    let now = Instant::now();
    let entry = state.report_states.entry(key.to_string()).or_default();
    let previous = if entry.value.is_empty() {
        None
    } else {
        Some(entry.value.clone())
    };
    let changed = previous.as_deref() != Some(value);
    let repeat_due = entry
        .last_emitted
        .is_none_or(|last| now.duration_since(last).as_secs() >= repeat_secs.max(1));
    let should_emit = changed || (value != "ok" && repeat_due);
    entry.value = value.to_string();
    if should_emit {
        entry.last_emitted = Some(now);
    }
    (previous, should_emit)
}

/// Classify memory pressure from active signals. Swap occupancy alone is not
/// pressure: it becomes relevant when paired with low available memory.
/// True when swap-in velocity reads as thrashing: pages faulting back in
/// faster than the operator's threshold. `None` (first pass after start,
/// or unreadable counters) is never thrash — an unmeasured signal fails
/// closed, like every other pressure input.
pub(crate) fn swapin_velocity_thrash(pswpin_rate: Option<f64>, warn_pages_per_sec: u64) -> bool {
    pswpin_rate.is_some_and(|r| r >= warn_pages_per_sec as f64)
}

pub(crate) fn classify_memory_pressure(
    mem_low: bool,
    swap_high: bool,
    psi_or_swapin_active: bool,
) -> &'static str {
    if mem_low && (swap_high || psi_or_swapin_active) {
        "critical"
    } else if mem_low || psi_or_swapin_active {
        "warn"
    } else {
        "ok"
    }
}

/// Apply persistence/hysteresis to an instantaneous pressure observation.
/// Returns `(stable_state, previous_state, transitioned)`.
pub(crate) fn stabilize_memory_pressure_at(
    state: &mut GuardRuntimeState,
    observed: &str,
    sustain_secs: u64,
    now: Instant,
) -> (String, Option<String>, bool) {
    if state.memory_pressure_state.is_empty() {
        state.memory_pressure_state = "ok".to_string();
    }

    if state.memory_pressure_state == observed {
        state.memory_pressure_candidate.clear();
        state.memory_pressure_candidate_since = None;
        return (state.memory_pressure_state.clone(), None, false);
    }

    if state.memory_pressure_candidate != observed {
        state.memory_pressure_candidate = observed.to_string();
        state.memory_pressure_candidate_since = Some(now);
        return (state.memory_pressure_state.clone(), None, false);
    }

    let candidate_since = state.memory_pressure_candidate_since.unwrap_or(now);
    if now.duration_since(candidate_since).as_secs() < sustain_secs {
        return (state.memory_pressure_state.clone(), None, false);
    }

    let previous = state.memory_pressure_state.clone();
    state.memory_pressure_state = observed.to_string();
    state.memory_pressure_candidate.clear();
    state.memory_pressure_candidate_since = None;
    (state.memory_pressure_state.clone(), Some(previous), true)
}

/// Resolve the guard's sync-freeze marker path.
///
/// FIXED 2026-10-09 (audit F130): this was `PathBuf::from(raw)` with no
/// expansion at all, while every sibling storage root expanded it
/// (`stash_root`, `resolve_policy_path`, the log dirs). The code
/// default is expanded at construction, but the shipped example config
/// documents the value as `~/.dracon/dracon-sync.freeze`
/// (dracon-system.example.toml), so following the example wrote a
/// literal RELATIVE `~/.dracon/...` path — one that `dracon-sync pause`
/// never watches, so sync was silently never frozen.
///
/// `resolve_policy_path` (not bare `expand_tilde`) is deliberate: the
/// shipped unit sets `WorkingDirectory=%h`, so a relative policy value
/// means "under $HOME" — the same fix audit 2026-10-01 applied to
/// `relocate_cold_root`.
fn sync_freeze_marker_path(guard: &GuardPolicy) -> PathBuf {
    crate::resolve_policy_path(guard.sync_freeze_marker.trim())
}

// Only remove markers written by this guard. The marker path is shared with
// `dracon-sync pause`, so mere existence is not proof that the guard owns it.
const GUARD_FREEZE_MARKER_PREFIX: &str = "dracon-system guard freeze:";

fn guard_owns_sync_freeze_marker(marker: &Path) -> bool {
    // Read only the small ownership prefix. Besides avoiding an unnecessary
    // allocation, this keeps a malformed/oversized marker from becoming a
    // memory-pressure input to the guard itself.
    let mut prefix = [0_u8; GUARD_FREEZE_MARKER_PREFIX.len()];
    let Ok(mut file) = File::open(marker) else {
        return false;
    };
    file.read_exact(&mut prefix).is_ok()
        && prefix.as_slice() == GUARD_FREEZE_MARKER_PREFIX.as_bytes()
}

/// Graduated auto-renice: higher CPU/memory usage = higher nice value (lower priority).
/// The process still gets full CPU when nothing else needs it — it just yields to the DE
/// and other interactive processes.
///
/// `current_nice` is part of the target calculation so an externally nicer process is
/// never raised in priority merely because its current tier maps to a smaller value.
///
/// Process mitigation is limited to reversible renice, optional `oom_score_adj`
/// biasing, and optional CPUQuota capping. The guard never invokes `kill`:
/// OOM biasing only influences the kernel's last-resort choice if an OOM occurs,
/// while CPUQuota throttles the process without killing it.
pub(crate) fn graduated_nice_value(
    cpu_percent: f32,
    rss_mb: u64,
    base_nice: i32,
    current_nice: i32,
) -> i32 {
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
    cpu_nice.max(mem_nice).clamp(0, 19).max(current_nice)
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
    // Never exec a bare PATH-relative name for a privilege op: resolve the
    // NixOS store path first (falls back to the bare name off NixOS, which
    // is the old behavior there).
    renice_process_with_bin(&PathBuf::from(resolve_bin_strict("renice")?), pid, value).await
}

/// OOM-killer steering target (v0.112.36). Higher oom_score_adj =
/// more likely to be picked when the kernel's last-resort OOM killer
/// fires. Writing it NEVER triggers a kill — it only steers the
/// victim choice IF the kernel kills anyway. Returns the target to
/// write, or None when the process should be left alone (already at/
/// above the bias, or deliberately protected with adj <= -500).
pub(crate) const OOM_BIAS_TARGET: i32 = 250;
pub(crate) const OOM_PROTECTED_ADJ: i32 = -500;

fn apply_oom_bias(
    p: &ProcSample,
    samples: &[ProcSample],
    state: &mut GuardRuntimeState,
) -> Result<Option<(i32, i32)>, String> {
    if state.mitigation_ledger_unavailable
        || state.oom_biased_pids.contains_key(&p.pid)
        || !process_sample_is_current(p)
    {
        return Ok(None);
    }
    use std::io::{Read, Seek, SeekFrom, Write};
    let path = format!("/proc/{}/oom_score_adj", p.pid);
    // Hold one proc descriptor across identity checks, original-value capture,
    // durable intent and write. Reopening by PID could target a replacement.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("open {path}: {e}"))?;
    if !process_sample_is_current(p) {
        return Err("process changed after OOM descriptor open".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let original = text.trim().parse::<i32>().map_err(|e| e.to_string())?;
    let Some(target) = oom_bias_target(original) else {
        return Ok(None);
    };
    state
        .oom_biased_pids
        .insert(p.pid, (original, process_sample_identity(p)));
    state.oom_known_descendants.insert(
        p.pid,
        process_descendant_samples(samples, p.pid)
            .into_iter()
            .map(|child| (child.pid, child.starttime))
            .collect(),
    );
    if !persist_mitigations(state) {
        remove_oom_bias(state, p.pid);
        return Err("write-ahead restoration intent could not be persisted".into());
    }
    if !process_sample_is_current(p) {
        return Err("process changed before OOM bias".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(format!("{target}\n").as_bytes())
        .map_err(|e| format!("write opened {path}: {e}; recovery intent retained"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    text.clear();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.trim().parse::<i32>().ok() != Some(target) {
        return Err("OOM apply verification failed; recovery intent retained".into());
    }
    Ok(Some((original, target)))
}

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
        let Some(mut pending) = state.oom_pending_descendants.get(&key).cloned() else {
            continue;
        };
        match process_identity_status(proc_root, key.0, &pending.identity) {
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                state.oom_pending_descendants.remove(&key);
                continue;
            }
            ProcessIdentityStatus::Unavailable => {
                result.deferred += 1;
                if record_oom_retry(state, key, &mut pending) {
                    eprintln!("⚠️ oom-descendant-restore retaining pid={} parent={} after {} unavailable identity reads", key.0, pending.root_pid, pending.attempts);
                    state.oom_pending_descendants.insert(key, pending.clone());
                }
                continue;
            }
            ProcessIdentityStatus::Match => {}
        }
        let current_identity = match read_proc_identity(proc_root, key.0) {
            Ok(identity) => identity,
            Err(_) => {
                result.deferred += 1;
                record_oom_retry(state, key, &mut pending);
                continue;
            }
        };
        if exempt_names.contains(&current_identity.comm)
            || is_kernel_process(&current_identity.comm)
        {
            // Exemption forbids mutation, not remembering an unresolved debt.
            result.deferred += 1;
            record_oom_retry(state, key, &mut pending);
            continue;
        }
        let changed = match oom_restore::restore(
            proc_root,
            key.0,
            &pending.identity,
            pending.original_adj,
        ) {
            Ok(changed) => changed,
            Err(error) => {
                result.deferred += 1;
                if record_oom_retry(state, key, &mut pending) {
                    eprintln!("⚠️ oom-descendant-restore retaining pid={} parent={} after {} failures: {}",key.0,pending.root_pid,pending.attempts,error);
                }
                continue;
            }
        };
        if changed {
            eprintln!(
                "🛡️ oom-descendant-restore pid={} parent={} adj -> {}",
                key.0, pending.root_pid, pending.original_adj
            );
            result.restored.push(format!(
                "oom-restore-descendant {}={}",
                pending.identity.comm, pending.original_adj
            ));
        }
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
                attempts: 0,
            },
        );
    }
    if !persist_mitigations(state) {
        return OomSweepResult {
            restored: Vec::new(),
            deferred: state.oom_pending_descendants.len(),
        };
    }
    restore_pending_oom_descendants(proc_root, state, exempt_names)
}

/// Apply a manager-owned lease to an existing isolated workload scope.
/// Returns (durable lease key, generation). No process is moved or signalled;
/// an independent timer restores quota without terminating the workload.
/// Shared/unknown units and units containing exempt processes fail closed.
async fn cap_cpu_process(
    pid: i32,
    percent: u32,
    exempt: &HashSet<String>,
) -> Result<(String, String), String> {
    cpu_limit::cap(pid, percent, exempt).await
}

fn parse_cgroup_rel_path(cg_line: &str) -> Option<&str> {
    let path = cg_line
        .lines()
        .next()?
        .split_once("::")
        .map(|(_, path)| path.trim_start_matches('/'))?;
    (!path.is_empty()).then_some(path)
}

/// Where a capped pid has to be written back to before the transient unit is
/// stopped. The recorded cgroup is authoritative when the ledger has one.
///
/// A ledger written before the cgroup was persisted leaves it empty, and an
/// empty cgroup must NEVER be interpolated into a path: it would resolve to
/// `/sys/fs/cgroup/cgroup.procs`, i.e. the cgroup ROOT. The transient unit's own
/// cgroup path is known here, and its PARENT is the slice systemd created the
/// unit under — exactly where the processes go back to when the scope stops, so
/// deriving the parent is not a guess (audit 2026-10-01).
fn restore_cgroup_target(orig_cgroup: &str, current_rel: &str) -> Option<String> {
    let orig = orig_cgroup.trim();
    if !orig.is_empty() {
        return Some(orig.to_string());
    }
    let parent = current_rel
        .rsplit_once('/')
        .map(|(parent, _)| parent)?
        .trim();
    (!parent.is_empty() && parent != "/").then(|| parent.to_string())
}

/// Release a manager-owned lease. Legacy migrated services are never stopped:
/// a gone root does not prove their descendants exited. Keep unresolved legacy
/// ownership debt unless the manager positively reports the unit absent.
async fn uncap_cpu_process_with_bin(
    systemctl_bin: &Path,
    proc_root: &Path,
    pid: i32,
    scope: &str,
    orig_cgroup: &str,
    allow_pid_move: bool,
) -> Result<(), String> {
    if cpu_limit::is_lease(scope) {
        // A lease owns its complete scope, including inherited descendants.
        // Release never signals members or depends on the root PID surviving.
        return cpu_limit::release(scope, orig_cgroup).await;
    }
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
                let target = restore_cgroup_target(orig_cgroup, rel).ok_or_else(|| {
                    format!("pid {pid} remains in {scope} and its original cgroup is unknown")
                })?;
                cpu_limit::restore_legacy(pid, &target).await?;
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

    let _ = process_was_read;
    // A missing/moved root proves nothing about surviving children. NEVER
    // stop/reset a legacy service. Only a positively absent unit resolves it.
    let output = Command::new(systemctl_bin)
        .args(["--user", "show", scope, "--property=LoadState"])
        .output()
        .await
        .map_err(|e| format!("inspect legacy unit {scope}: {e}"))?;
    if output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "LoadState=not-found")
    {
        return Ok(());
    }
    Err(format!("legacy CPU service {scope} requires explicit ownership recovery; no unit was stopped and its debt remains recorded"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NiceRestoreScope {
    Legacy,
    Memory,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeAdjustment {
    /// A restoration for a nice limiter on one PID. Overlapping limiters on
    /// the same process incarnation use `Both`; entries for different PID
    /// incarnations remain separate so a stale entry cannot discard the
    /// current process's live limiter.
    Nice {
        pid: i32,
        original_nice: i32,
        identity: ProcessIdentity,
        scope: NiceRestoreScope,
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
    let mut nice_restores: HashMap<i32, Vec<(i32, ProcessIdentity, NiceRestoreScope)>> =
        HashMap::new();
    for (&pid, entry) in &state.reniced_pids {
        nice_restores.entry(pid).or_default().push((
            entry.original_nice,
            entry.identity.clone(),
            NiceRestoreScope::Legacy,
        ));
    }
    for (&pid, entry) in &state.memory_reniced_pids {
        let restores = nice_restores.entry(pid).or_default();
        if let Some((_, identity, scope)) = restores
            .iter_mut()
            .find(|(_, identity, _)| same_process_incarnation(identity, &entry.identity))
        {
            // Both maps captured the same pre-limiter nice value. Keep one
            // restore operation and remove both entries only after it works.
            *scope = NiceRestoreScope::Both;
            debug_assert_eq!(identity.starttime, entry.identity.starttime);
        } else {
            // A PID may have been reused between limiter passes. Keep the
            // old and current incarnations independent so a mismatch in one
            // cannot remove the live entry belonging to the other.
            restores.push((
                entry.original_nice,
                entry.identity.clone(),
                NiceRestoreScope::Memory,
            ));
        }
    }
    for (pid, restores) in nice_restores {
        for (original_nice, identity, scope) in restores {
            plan.push(RuntimeAdjustment::Nice {
                pid,
                original_nice,
                identity,
                scope,
            });
        }
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

fn same_process_incarnation(left: &ProcessIdentity, right: &ProcessIdentity) -> bool {
    left.starttime != 0 && left.starttime == right.starttime
}

fn drop_stale_nice_adjustments(
    state: &mut GuardRuntimeState,
    pid: i32,
    current_identity: &ProcessIdentity,
) {
    if state
        .reniced_pids
        .get(&pid)
        .is_some_and(|entry| !same_process_incarnation(&entry.identity, current_identity))
    {
        remove_legacy_renice(state, pid);
    }
    if state
        .memory_reniced_pids
        .get(&pid)
        .is_some_and(|entry| !same_process_incarnation(&entry.identity, current_identity))
    {
        remove_memory_renice(state, pid);
    }
}

fn remove_nice_scope(state: &mut GuardRuntimeState, pid: i32, scope: NiceRestoreScope) {
    match scope {
        NiceRestoreScope::Legacy => remove_legacy_renice(state, pid),
        NiceRestoreScope::Memory => remove_memory_renice(state, pid),
        NiceRestoreScope::Both => remove_nice_adjustments(state, pid),
    }
}

fn remove_legacy_renice(state: &mut GuardRuntimeState, pid: i32) {
    state.reniced_pids.remove(&pid);
    state.cooled_since.remove(&pid);
}

fn remove_memory_renice(state: &mut GuardRuntimeState, pid: i32) {
    state.memory_reniced_pids.remove(&pid);
    state.memory_cooled_since.remove(&pid);
}

fn remove_nice_adjustments(state: &mut GuardRuntimeState, pid: i32) {
    remove_legacy_renice(state, pid);
    remove_memory_renice(state, pid);
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
    state.oom_identity_unavailable_attempts.remove(&pid);
}

fn remove_cpu_cap(state: &mut GuardRuntimeState, pid: i32) {
    state.capped_pids.remove(&pid);
    state.cap_cooled_since.remove(&pid);
    state.cap_identity_unavailable_attempts.remove(&pid);
}

/// Restore all process-level mitigations before a policy reload discards the
/// old runtime. A PID that is gone or has a different starttime is safe to
/// drop; an unreadable live process, a failed command, or an unverified cgroup
/// remains tracked so the next guard pass can retry instead of losing a live
/// adjustment.
async fn restore_runtime_adjustments(state: &mut GuardRuntimeState) -> bool {
    let samples = process_samples().await.unwrap_or_default();
    let renice_bin = match resolve_bin_strict("renice") {
        Ok(b) => PathBuf::from(b),
        Err(e) => {
            eprintln!("SIGHUP restore unavailable: {e:#}");
            return false;
        }
    };
    let systemctl_bin = match resolve_bin_strict("systemctl") {
        Ok(b) => PathBuf::from(b),
        Err(e) => {
            eprintln!("SIGHUP restore unavailable: {e:#}");
            return false;
        }
    };
    restore_runtime_adjustments_with_samples(
        state,
        &renice_bin,
        &systemctl_bin,
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
    // Unit leases do not depend on the sampled root remaining readable/alive.
    if state.mitigation_ledger_unavailable {
        return false;
    }
    let leases: Vec<_> = state
        .capped_pids
        .iter()
        .filter(|(_, (key, _, _))| cpu_limit::is_lease(key))
        .map(|(pid, (key, generation, _))| (*pid, key.clone(), generation.clone()))
        .collect();
    for (pid, key, generation) in leases {
        match cpu_limit::release(&key, &generation).await {
            Ok(()) => remove_cpu_cap(state, pid),
            Err(e) => eprintln!("CPU lease recovery retained {key}: {e}"),
        }
    }
    let sweep = sweep_stranded_oom_descendants(proc_root, samples, state, &HashSet::new());
    if sweep.deferred > 0 {
        eprintln!(
            "⚠ SIGHUP retaining {} pending descendant OOM restorations",
            sweep.deferred
        );
    }
    for adjustment in runtime_adjustment_plan(state) {
        match adjustment {
            RuntimeAdjustment::Nice {
                pid,
                original_nice,
                identity,
                scope,
            } => match process_identity_status(proc_root, pid, &identity) {
                ProcessIdentityStatus::Match => {
                    // ADDED 2026-10-24: a priority the guard provably CANNOT
                    // restore is a PERMANENT failure, not a transient one.
                    // Retrying it forever kept the previous policy live and
                    // deferred every later reload — one unrestorable renice
                    // made the whole guard impossible to reconfigure.
                    //
                    // Dropping the entry does NOT undo the priority change:
                    // the process stays at its degraded nice until it exits.
                    // It stops being TRACKED as pending, so the reload can
                    // proceed and the mitigation ledger shrinks instead of
                    // carrying a debt that can never be paid.
                    if !can_restore_nice_for(proc_root, pid) {
                        eprintln!(
                            "⚠ SIGHUP dropping unrestorable nice adjustment pid={} comm={} — the guard cannot raise priority back in that process's user namespace, so it keeps its current nice until it exits",
                            pid, identity.comm
                        );
                        remove_nice_scope(state, pid, scope);
                        continue;
                    }
                    match renice_process_with_bin(renice_bin, pid, original_nice).await {
                        Ok(()) => remove_nice_scope(state, pid, scope),
                        Err(e) => {
                            let message = e.to_string();
                            if nice_restore_failure_is_permanent(&message) {
                                eprintln!(
                                    "⚠ SIGHUP dropping unrestorable nice adjustment pid={} comm={} — {message}",
                                    pid, identity.comm
                                );
                                remove_nice_scope(state, pid, scope);
                            } else {
                                eprintln!(
                                    "⚠ SIGHUP failed to restore nice value for pid={} comm={}: {}",
                                    pid, identity.comm, e
                                );
                            }
                        }
                    }
                }
                ProcessIdentityStatus::Gone => remove_nice_scope(state, pid, scope),
                ProcessIdentityStatus::Mismatch => {
                    eprintln!(
                        "⚠ SIGHUP dropping stale nice adjustment pid={} — PID incarnation changed",
                        pid
                    );
                    remove_nice_scope(state, pid, scope);
                }
                ProcessIdentityStatus::Unavailable => eprintln!(
                    "⚠ SIGHUP retaining nice adjustment pid={} — process identity unavailable",
                    pid
                ),
            },
            RuntimeAdjustment::OomBias {
                pid,
                orig_adj,
                identity,
            } => match process_identity_status(proc_root, pid, &identity) {
                ProcessIdentityStatus::Match => {
                    // Restore the root promptly even while child obligations
                    // remain; otherwise every new fork inherits its bias.
                    match oom_restore::restore(proc_root, pid, &identity, orig_adj) {
                        Ok(_) => remove_oom_bias(state, pid),
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
/// Process-name classifier for active-build detection.
///
/// Matches against the `ps` `comm=` value (kernel-truncated to 15 chars).
/// Substring matches cover toolchain-suffixed forms such as `cargo-build`
/// or `rustc-1.94`; exact matches pin named tools whose comm would
/// otherwise not match either substring.
///
/// CHANGED 2026-08-21 (active-build detection gap): `rust-analyzer` and
/// `cargo-watch` hold a target dir just as a build does — before this fix
/// only cargo/rustc/clippy-driver were recognized, so a disk-pressure
/// cleanup cycle could delete a target dir out from under a live
/// analysis/watch session (protected only by the mtime heuristic).
fn is_rust_build_process(comm: &str) -> bool {
    comm.contains("cargo")
        || comm.contains("rustc")
        || matches!(comm, "clippy-driver" | "rust-analyzer" | "cargo-watch")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PackageCacheKind {
    Cargo,
    Npm,
    Pip,
    Go,
}

impl PackageCacheKind {
    fn name(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Pip => "pip",
            Self::Go => "go",
        }
    }
}

fn package_cache_kind_for_command_name(name: &str) -> Option<PackageCacheKind> {
    let name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(name);

    if name == "cargo"
        || name.starts_with("cargo-")
        || name == "rustc"
        || name.starts_with("rustc-")
        || matches!(name, "clippy-driver" | "rust-analyzer")
    {
        return Some(PackageCacheKind::Cargo);
    }
    if matches!(
        name,
        "npm"
            | "npx"
            | "npm-cli.js"
            | "node"
            | "pnpm"
            | "yarn"
            | "bun"
            | "vite"
            | "webpack"
            | "esbuild"
            | "tsc"
    ) || name.starts_with("npm-")
    {
        return Some(PackageCacheKind::Npm);
    }
    if name == "pip" || name.starts_with("pip3") || name.starts_with("pip-") {
        return Some(PackageCacheKind::Pip);
    }
    if name == "go" {
        return Some(PackageCacheKind::Go);
    }
    None
}

/// Classify a process using both `comm` and the first few argv entries.
///
/// Most package-manager processes have a useful `comm` (`cargo`, `npm`,
/// `pip3`, or `go`).  The argv fallback covers interpreter/script wrappers
/// such as `python -m pip`, `node .../npm-cli.js`, and `/bin/sh npm ...`.
/// A false positive only defers cache cleanup, so unknown wrappers are
/// intentionally handled conservatively.
fn package_cache_kind_for_process(comm: &str, cmdline: &str) -> Option<PackageCacheKind> {
    package_cache_kind_for_command_name(comm).or_else(|| {
        cmdline
            .split_whitespace()
            .take(8)
            .find_map(package_cache_kind_for_command_name)
    })
}

fn is_package_process_wrapper(comm: &str) -> bool {
    let name = Path::new(comm)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(comm);
    name == "node"
        || name == "python"
        || name.starts_with("python3")
        || matches!(
            name,
            "sh" | "bash" | "dash" | "zsh" | "fish" | "env" | "sudo"
        )
}

fn detect_active_package_manager_operations_from(
    ps_output: &str,
    proc_root: &Path,
) -> Result<HashSet<PackageCacheKind>> {
    fs::metadata(proc_root.join("self")).with_context(|| {
        format!(
            "cannot inspect process metadata at {} for package-cache protection",
            proc_root.display()
        )
    })?;

    let mut active = HashSet::new();
    let mut saw_process = false;
    for line in ps_output.lines() {
        let mut parts = line.split_whitespace();
        let pid_text = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed ps output line: {line:?}"))?;
        let pid = pid_text.parse::<i32>().map_err(|error| {
            anyhow::anyhow!("malformed ps PID {pid_text:?} in output line {line:?}: {error}")
        })?;
        if pid <= 0 {
            anyhow::bail!("malformed non-positive ps PID {pid} in output line {line:?}");
        }
        let comm = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed ps output line: {line:?}"))?;
        saw_process = true;
        if let Some(kind) = package_cache_kind_for_command_name(comm) {
            active.insert(kind);
            continue;
        }

        let Some(cmdline) = read_package_process_cmdline(proc_root, pid)? else {
            continue;
        };
        if cmdline.is_empty() && is_package_process_wrapper(comm) {
            // A zombie cannot execute or hold a package cache open. Linux
            // deliberately exposes an empty cmdline for it; do not confuse
            // that affirmative terminal state with unreadable live argv.
            let status = fs::read_to_string(proc_root.join(pid.to_string()).join("status"));
            if status.as_ref().is_ok_and(|text| {
                text.lines().any(|line| {
                    line.strip_prefix("State:")
                        .and_then(|state| state.split_whitespace().next())
                        == Some("Z")
                })
            }) {
                continue;
            }
            anyhow::bail!(
                "empty command line for possible package-manager wrapper PID {} ({})",
                pid,
                comm
            );
        }
        if let Some(kind) = package_cache_kind_for_process(comm, &cmdline) {
            active.insert(kind);
        }
    }
    if !saw_process {
        anyhow::bail!("ps returned no process records for package-cache protection");
    }
    Ok(active)
}

/// Return the cache classes currently used by package-manager/build processes
/// for dry-run estimates and diagnostics. A failed process listing is an error:
/// inspection must fail closed rather than report an unprotected cache as safe.
async fn detect_active_package_manager_operations() -> Result<HashSet<PackageCacheKind>> {
    detect_active_package_manager_operations_with(Path::new("ps"), Path::new("/proc")).await
}

async fn detect_active_package_manager_operations_with(
    ps_bin: &Path,
    proc_root: &Path,
) -> Result<HashSet<PackageCacheKind>> {
    let out = Command::new(ps_bin)
        .args(["-eo", "pid=,comm="])
        .output()
        .await
        .context("failed to invoke ps for package-cache protection")?;

    if !out.status.success() {
        anyhow::bail!(
            "ps failed for package-cache protection (exit {})",
            out.status.code().unwrap_or(-1)
        );
    }

    detect_active_package_manager_operations_from(&String::from_utf8_lossy(&out.stdout), proc_root)
}

async fn detect_active_rust_builds() -> Result<HashSet<i32>> {
    detect_active_rust_builds_with(Path::new("ps")).await
}

async fn detect_active_rust_builds_with(ps_bin: &Path) -> Result<HashSet<i32>> {
    let out = Command::new(ps_bin)
        .args(["-eo", "pid=,comm="])
        .output()
        .await?;

    if !out.status.success() {
        anyhow::bail!("cannot inspect active Rust builds: ps failed");
    }

    let output = std::str::from_utf8(&out.stdout)?;
    anyhow::ensure!(!output.trim().is_empty(), "ps returned no process records");
    let mut build_pids = HashSet::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let pid: i32 = parts.next().context("missing ps PID")?.parse()?;
        anyhow::ensure!(pid > 0, "invalid ps PID");
        let comm = parts.next().context("missing ps command")?;

        if is_rust_build_process(comm) {
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
            // CHANGED 2026-09-20 (space audit): 5 -> 8. Nested game repos
            // sit at ~/Dev/dracon-platform/web/games/<wip|released>/<name>/
            // so their target dirs are at depth 6 — the old cap made every
            // nested-game build cache invisible to the guard.
            .max_depth(8)
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

            // Get directory size (in-process walk; unreadable trees measure 0
            // and fall out at the min-size gate below).
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
    // CHANGED 2026-09-20 (space audit): in-process WalkDir sum. The old
    // body shelled out to `du -sb` once PER CALL — with 580k top-level
    // /tmp entries that was 580k forks (~34/s under the service's 20%
    // CPUQuota), turning one tmp pass into a multi-hour window with the
    // whole daemon loop (memory mitigation included) blocked behind it.
    // File bytes only (directory entries themselves excluded): this is the
    // established contract — `active_package_operations_...` asserts a
    // 5-byte fixture measures exactly 5 — and the useful "reclaimable
    // bytes" semantic for cleanup accounting. Links not followed.
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// Action-level lingering gate for Rust target cleanup (2026-09-14).
///
/// A target dir touched within `min_age_days` is an active project, not a
/// cleanup candidate — even when disk pressure is at action/critical level.
/// `min_age_days == 0` disables the gate (the pre-fix delete-anything
/// posture). Pure so the guard loop and unit tests share one definition.
pub(crate) fn rust_target_old_enough_for_action_cleanup(
    mtime_secs_ago: u64,
    min_age_days: u64,
) -> bool {
    min_age_days == 0 || mtime_secs_ago >= min_age_days.saturating_mul(24).saturating_mul(3600)
}

/// Perform automatic cleanup of Rust target directories.
///
/// `bypass_age_gate` is true only on the daemon's critical tier (disk at or
/// above `disk_critical_percent`): at that point a full disk breaks the
/// machine, so rebuildable targets are candidates even when freshly
/// touched. Active-build, 60s-mtime and protected-path checks still apply —
/// only the lingering age gate is lifted.
async fn auto_cleanup_rust_targets(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    apply: bool,
    bypass_age_gate: bool,
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

    // Detect active builds — one of TWO protection mechanisms (the other is
    // the lingering age gate below). We protect target dirs where
    // cargo/rustc is actively running.
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

    // ADDED 2026-10-12 (stall guard): cap cross-device stash-move bytes
    // per pass. 2026-10-04 disabled this whole path after repeated 10-30 GiB
    // copies to /mnt/data/stash stalled the desktop. The budget is
    // reserved at gate time in BOTH dry-run and apply so preview == apply;
    // a failed move wasting budget only defers others — safe direction.
    // Direct deletes (stash-first off) are uncapped: no copy, no stall.
    let stash_budget = stash_budget_bytes(guard.stash_max_bytes_per_pass_mb);
    let mut stashed_bytes = 0u64;

    for target in targets {
        // Skip if too small
        if target.bytes < min_size_bytes {
            continue;
        }

        // CHANGED 2026-09-14: lingering gate — a target touched within
        // rust_target_action_min_age_days (default 7) belongs to an active
        // project and is never an action-level candidate, no matter how big
        // it is. The old code deleted anything above min_size (modulo the
        // 60s backstop), which thrashed daily drivers: each deleted target
        // was rebuilt within hours, pushing disk straight back over the
        // action line for the next cycle. Bonus: a just-rebuilt target is
        // fresh by definition, so the gate doubles as a re-clean cooldown.
        // CHANGED 2026-09-20 (space audit): at the critical tier a full
        // disk is worse than a rebuild — the gate is lifted (a 79 GiB
        // freshly-built workspace target held a 100% disk hostage because
        // every reclaim path deferred to freshness).
        if !bypass_age_gate
            && !rust_target_old_enough_for_action_cleanup(
                target.mtime_secs_ago,
                guard.rust_target_action_min_age_days,
            )
        {
            result.protected_paths.push(format!(
                "{} (touched {}d ago, under action min-age {}d)",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                guard.rust_target_action_min_age_days
            ));
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

        // FIXED 2026-10-02: the user-protected-path refusal used to live INSIDE
        // the `if apply` block. In a dry run that block is skipped entirely, so
        // the preview listed every protected tree as reclaimable and only
        // refused it at apply time — overstating reclaimable space by however
        // large the protected workspace is (~66 GiB on this host), which misled
        // the operator twice in one session.
        //
        // Hoisted so preview and apply go through ONE check and cannot
        // disagree, rather than adding a second filter that would drift. The
        // reason travels with the verdict, matching the convention the
        // active-build protections above already use.
        if let Err(e) = check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
            if apply {
                eprintln!("⚠️ skipping {}: {:#}", target.path.display(), e);
            }
            result.protected_paths.push(format!(
                "{} ({})",
                target.path.display(),
                one_line(&e.to_string())
            ));
            continue;
        }

        // ADDED 2026-09-27 (space tiers Phase 2): stash-first routing —
        // move to stash (TTL'd, restorable) instead of deleting.
        // ADDED 2026-10-12 (stall guard): over-budget entries defer with an
        // explicit operator note instead of stalling the desktop mid-pass.
        if guard.clean_stash_first
            && stash_over_budget(stashed_bytes, target.bytes, stash_budget)
        {
            result.protected_paths.push(format!(
                "{} ({}, over per-pass stash budget {}, deferred — needs operator)",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(stash_budget)
            ));
            continue;
        }
        if guard.clean_stash_first {
            stashed_bytes = stashed_bytes.saturating_add(target.bytes);
        }
        let mut stashed = false;
        let mut freed = target.bytes;
        if apply {
            if guard.clean_stash_first {
                let qdir = stash_root(guard);
                match stash_first_remove(&target.path, &qdir, &guard.protected_paths) {
                    Ok((manifest, f)) => {
                        stashed = true;
                        freed = f;
                        eprintln!(
                            "📦 Rust stashed: {} as {}",
                            target.path.display(),
                            manifest.name
                        );
                    }
                    Err(e) => {
                        eprintln!("⚠️ failed to stash {}: {}", target.path.display(), e);
                        continue;
                    }
                }
            } else {
                // `check_safe_to_delete_guard` already ran above for BOTH paths,
                // so the canonical path is re-derivable without a second
                // refusal branch that could drift from it.
                let safe_path = target.path.canonicalize().unwrap_or(target.path.clone());
                if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                    eprintln!("⚠️ failed to remove {}: {}", target.path.display(), e);
                    continue;
                }
            }
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += freed;
        if stashed {
            result.cleaned_paths.push(format!(
                "{} (stashed, {}, freed {})",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(freed)
            ));
        } else {
            result.cleaned_paths.push(format!(
                "{} ({})",
                target.path.display(),
                human_bytes(target.bytes)
            ));
        }
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

    // ADDED 2026-10-12 (stall guard): same per-pass stash-move budget
    // as the action path — a 14d-stale forgotten target is exactly the huge
    // cross-device copy that stalled the desktop in 2026-10-04.
    let stash_budget = stash_budget_bytes(guard.stash_max_bytes_per_pass_mb);
    let mut stashed_bytes = 0u64;

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

        // ADDED 2026-09-27 (space tiers Phase 2): stash-first routing.
        // ADDED 2026-10-12 (stall guard): over-budget entries defer with an
        // explicit operator note instead of stalling the desktop mid-pass.
        if guard.clean_stash_first
            && stash_over_budget(stashed_bytes, target.bytes, stash_budget)
        {
            result.protected_paths.push(format!(
                "{} ({}, over per-pass stash budget {}, deferred — needs operator)",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(stash_budget)
            ));
            continue;
        }
        if guard.clean_stash_first {
            stashed_bytes = stashed_bytes.saturating_add(target.bytes);
        }
        let mut stashed = false;
        let mut freed = target.bytes;
        if apply {
            if guard.clean_stash_first {
                let qdir = stash_root(guard);
                match stash_first_remove(&target.path, &qdir, &guard.protected_paths) {
                    Ok((manifest, f)) => {
                        stashed = true;
                        freed = f;
                        eprintln!(
                            "📦 proactive stashed: {} as {}",
                            target.path.display(),
                            manifest.name
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "⚠️ proactive: failed to stash {}: {}",
                            target.path.display(),
                            e
                        );
                        continue;
                    }
                }
            } else {
                let safe_path =
                    match check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
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
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += freed;
        if stashed {
            result.cleaned_paths.push(format!(
                "{} (stashed, {} days stale, {}, freed {})",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                human_bytes(target.bytes),
                human_bytes(freed)
            ));
        } else {
            result.cleaned_paths.push(format!(
                "{} ({} days stale, {})",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                human_bytes(target.bytes)
            ));
        }
    }

    Ok(result)
}

/// Parse the IUse% column out of `df -Pi` output.
///
/// FIXED 2026-10-09 (audit F131): extracted from `inode_use_percent` so the
/// column logic is testable, and made position-independent. The old
/// `lines().nth(1)` + `split_whitespace().nth(4)` assumed the data row is
/// line 1 and IUse% is its 5th field; `df` wraps a too-long Filesystem
/// column onto its own line, so the data row moves to line 3 with only 5
/// fields, and both assumptions break.
///
/// Anchor on the only unambiguous token in the row: the `NN%` value, with
/// the three integer fields immediately before it. That is stable whether
/// the Filesystem column wrapped (row has 5 fields) or the `Mounted on`
/// value itself contains spaces (row has 7+), neither of which a fixed
/// index or a pure end-index can express. The header row cannot match
/// because `Inodes`/`IUsed`/`IFree` are not integers.
fn parse_inode_use_percent(output: &str) -> Option<u8> {
    output
        .lines()
        .find_map(|line| inode_row_fields(line).map(|(_, _, _, pct)| pct))
}

/// Parse the (total, used, free) inode counts out of `df -Pi` output.
///
/// FIXED 2026-10-09 (audit F131): same anchor as `parse_inode_use_percent`,
/// plus an explicit Err instead of the old `.unwrap_or(0)` that turned any
/// layout change into a silent `(0, 0, 0)` — and therefore into
/// "0% inodes used, forever" with no error anywhere.
fn parse_inode_info(output: &str) -> Result<(u64, u64, u64)> {
    output
        .lines()
        .find_map(|line| inode_row_fields(line).map(|(total, used, free, _)| (total, used, free)))
        .ok_or_else(|| anyhow::anyhow!("no inode data line in df -Pi output"))
}

/// Locate the inode row's four numeric fields in one `df -Pi` line:
/// `(total, used, free, iuse_percent)`. Returns `None` when the line is
/// the header or otherwise not an inode data row.
fn inode_row_fields(line: &str) -> Option<(u64, u64, u64, u8)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let pct_idx = parts.iter().position(|p| {
        p.len() > 1 && p.ends_with('%') && p[..p.len() - 1].chars().all(|c| c.is_ascii_digit())
    })?;
    if pct_idx < 3 {
        return None;
    }
    let nums = &parts[pct_idx - 3..pct_idx];
    let total = nums[0].parse::<u64>().ok()?;
    let used = nums[1].parse::<u64>().ok()?;
    let free = nums[2].parse::<u64>().ok()?;
    let pct = parts[pct_idx].trim_end_matches('%').parse::<u8>().ok()?;
    Some((total, used, free, pct))
}

async fn inode_use_percent(path: &str) -> Result<u8> {
    let out = Command::new("df").args(["-Pi", path]).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("df -i command failed"));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    parse_inode_use_percent(&text).ok_or_else(|| anyhow::anyhow!("failed parsing df -i output"))
}

/// Get inode info for the configured filesystem.
async fn get_inode_info(path: &str) -> Result<(u64, u64, u64)> {
    let out = Command::new("df").args(["-Pi", path]).output().await?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("df -i command failed"));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    parse_inode_info(&text)
}

/// Clean Docker resources
async fn docker_prune(
    apply: bool,
    all: bool,
    volumes: bool,
    command_timeout_secs: u64,
) -> Result<u64> {
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

    // ADDED 2026-10-22: bounded, for the same reason as the Nix GC — a
    // prune that hangs must not hold the guard's loop.
    let mut prune = Command::new("docker");
    prune.args(&args);
    let out = match run_bounded(&mut prune, command_timeout_secs).await? {
        Some(out) => out,
        None => return Ok(0),
    };

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

/// There is no lock protocol shared by the guard and arbitrary package
/// managers.  Keep apply cache cleanup disabled until one exists; a process
/// snapshot cannot make a later `remove_dir_all` lifecycle-safe.
const PACKAGE_CACHE_APPLY_COORDINATION_AVAILABLE: bool = false;

fn package_cache_apply_is_disabled() -> bool {
    !PACKAGE_CACHE_APPLY_COORDINATION_AVAILABLE
}

fn storage_hotspot_apply_is_blocked(kind: &str, apply: bool) -> bool {
    apply && kind == "cache" && package_cache_apply_is_disabled()
}

/// A live process whose EXECUTABLE lives under `root`.
///
/// 2026-10-20 (disk-full recovery): the socket/pidfile probes miss a
/// service that runs a binary out of the target dir and binds a TCP port
/// instead of a unix socket (observed: `db-gateway`'s
/// `.cargo/target/debug/dg serve`). Deleting that target dir unlinks the
/// running binary's inode — the process keeps serving, but any restart
/// finds no executable. Read `/proc/<pid>/exe` for every process and
/// refuse the candidate if one points inside it.
#[cfg(target_os = "linux")]
fn live_process_binary_under(root: &Path) -> Option<(i32, PathBuf)> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
            continue;
        };
        // A deleted binary still resolves, with a " (deleted)" suffix.
        let exe_str = exe.to_string_lossy();
        let exe_str = exe_str.strip_suffix(" (deleted)").unwrap_or(&exe_str);
        let exe_path = Path::new(exe_str);
        if exe_path.starts_with(root) {
            return Some((pid, exe_path.to_path_buf()));
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn live_process_binary_under(_root: &Path) -> Option<(i32, PathBuf)> {
    None
}

/// Every live unix-socket LISTENER path, parsed from `/proc/net/unix`.
///
/// 2026-10-21 (codex app-server incident): the /tmp cleaner deleted
/// `/tmp/codex-daemon-1000/<hash>` — the control socket of the RUNNING codex
/// app-server daemon — because the directory was older than
/// `tmp_min_age_hours`. The daemon kept its socket BOUND, so every client
/// got ENOENT and codex refused to start ("app server did not become ready").
///
/// The open-fd scan could not save it: a bound unix socket is an ANONYMOUS
/// inode in `/proc/<pid>/fd` (`socket:[N]`), never a path, so no fd link ever
/// points at the socket FILE. The socket path in `/proc/net/unix` is the only
/// handle that exists, and it is exactly what the cleanup removes.
///
/// Collect the set ONCE per cleanup pass and keep any candidate that contains
/// one of these paths — see `path_holds_live_unix_socket`.
#[cfg(target_os = "linux")]
fn live_unix_socket_paths() -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let Ok(net_unix) = std::fs::read_to_string("/proc/net/unix") else {
        return out;
    };
    for line in net_unix.lines().skip(1) {
        // Num RefCount Protocol Flags Type St Inode Path
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue; // unnamed socket (no path)
        }
        let (flags, sock_type, state, sock_path) = (fields[3], fields[4], fields[5], fields[7]);
        // A listening stream socket: SO_ACCEPTCON in the flags word
        // (00010000) or the unconnected state (01) on a SOCK_STREAM.
        let is_listener = flags == "00010000" || (state == "01" && sock_type == "0001");
        if !is_listener {
            continue;
        }
        out.insert(PathBuf::from(sock_path));
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn live_unix_socket_paths() -> HashSet<PathBuf> {
    HashSet::new()
}

/// A live unix-socket LISTENER whose path lives under `root`.
///
/// 2026-10-20 (disk-full recovery): a `target/` dir can hold the runtime
/// state of a RUNNING service, not just build output. The dracon-platform
/// dev stack keeps its sockets under `target/dev-stack/sockets/`; wiping
/// `target/` unlinked the socket paths of five live API workers, so Caddy
/// could not open new connections even though the processes kept answering
/// on already-pooled ones. The build-activity check alone did not catch it
/// (no cargo/rustc was running), so the candidate needs its own probe.
#[cfg(target_os = "linux")]
fn live_unix_socket_under(root: &Path) -> Option<PathBuf> {
    live_unix_socket_paths()
        .into_iter()
        .find(|sock| sock.starts_with(root))
}

#[cfg(not(target_os = "linux"))]
fn live_unix_socket_under(_root: &Path) -> Option<PathBuf> {
    None
}

/// True when `candidate` CONTAINS the path of a live unix-socket listener.
///
/// 2026-10-21 (codex app-server incident): the /tmp cleaner deleted
/// `/tmp/codex-daemon-1000/<hash>` — the control socket of the RUNNING codex
/// app-server daemon — because the directory was older than
/// `tmp_min_age_hours` (24h) and the socket file's mtime never moves. The
/// daemon kept the socket BOUND, so every client got ENOENT and codex
/// refused to start ("app server did not become ready ... No such file or
/// directory").
///
/// The open-fd scan could not save it, and this is the reason it must be a
/// separate probe: a bound unix socket is an ANONYMOUS inode in
/// `/proc/<pid>/fd` (`socket:[N]`), never a path, so no fd link ever points
/// at the socket FILE. The socket path in `/proc/net/unix` is the only handle
/// that exists, and it is exactly what the cleanup removes.
///
/// Component-wise matching (`Path::starts_with`) keeps a sibling directory
/// that merely shares a textual prefix — `codex-daemon-1000x` — eligible for
/// removal.
fn path_holds_live_unix_socket(candidate: &Path, live_sockets: &HashSet<PathBuf>) -> bool {
    live_sockets.iter().any(|sock| sock.starts_with(candidate))
}

/// A pidfile under `root` that names a live process.
///
/// Complements the socket probe: a service may keep only a pidfile (no
/// socket), and the pidfile is what `make down`/status use to find it.
/// A stale pidfile whose PID is gone (or was reused by an unrelated
/// process) is NOT a reason to refuse the cleanup.
fn live_pidfile_under(root: &Path) -> Option<PathBuf> {
    // Cargo's own cache dirs never hold service state, and they hold the
    // bulk of the entries — skipping them keeps this scan cheap on a
    // multi-gigabyte target dir.
    const SKIP_DIRS: [&str; 6] = [
        "deps",
        "build",
        "incremental",
        "debug",
        "release",
        ".fingerprint",
    ];
    let entries = walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_entry(|e| {
            // Files always pass; only PRUNE directories. Cargo's cache dirs
            // never hold service state, and they hold the bulk of the
            // entries — skipping them keeps this scan cheap on a
            // multi-gigabyte target dir.
            !e.file_type().is_dir()
                || e.depth() == 0
                || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext == "pid")
        });
    for entry in entries {
        let Some(pid) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        else {
            continue;
        };
        if pid > 0 && Path::new(&format!("/proc/{pid}")).exists() {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

/// Recheck activity at the deletion boundary, not only when sizing hotspots.
/// Process inspection errors retain the candidate. The mtime backstop covers
/// builds whose current directory differs from their selected manifest.
///
/// 2026-10-20: also refuse a candidate that holds the runtime state of a
/// LIVE service (unix socket listener or live pidfile). A `target/` dir is
/// not always disposable build output — see `live_unix_socket_under`.
async fn validate_storage_cleanup_activity(kind: &str, path: &Path) -> Result<()> {
    if matches!(kind, "rust-build" | "build-output" | "node-deps") {
        if let Some((pid, exe)) = live_process_binary_under(path) {
            anyhow::bail!(
                "live process {pid} runs {} from inside this path (service runtime state, not build output)",
                exe.display()
            );
        }
        if let Some(sock) = live_unix_socket_under(path) {
            anyhow::bail!(
                "live service socket {} is inside this path (service runtime state, not build output)",
                sock.display()
            );
        }
        if let Some(pidfile) = live_pidfile_under(path) {
            anyhow::bail!(
                "live pidfile {} is inside this path (service runtime state, not build output)",
                pidfile.display()
            );
        }
    }
    if matches!(kind, "rust-build" | "build-output") {
        let project = path
            .parent()
            .context("cleanup candidate has no project")?
            .canonicalize()?;
        for pid in detect_active_rust_builds().await? {
            match get_process_cwd(pid).await {
                Some(cwd) => {
                    let cwd = cwd.canonicalize()?;
                    if cwd.starts_with(&project) || project.starts_with(&cwd) {
                        anyhow::bail!("active Rust build in {}", project.display());
                    }
                }
                None if Path::new(&format!("/proc/{pid}")).exists() => {
                    anyhow::bail!("cannot inspect active build PID {pid}");
                }
                None => {}
            }
        }
    }
    if matches!(kind, "node-deps" | "build-output")
        && detect_active_package_manager_operations()
            .await?
            .contains(&PackageCacheKind::Npm)
    {
        anyhow::bail!("active Node or package-manager operation");
    }
    if matches!(kind, "rust-build" | "node-deps" | "build-output") {
        let age = fs::metadata(path)?.modified()?.elapsed()?;
        if age.as_secs() < 60 {
            anyhow::bail!("modified <60s ago (active build)");
        }
    }
    Ok(())
}

/// Clean package manager caches using the real home directory.
async fn clean_package_caches(
    cargo: bool,
    npm: bool,
    pip: bool,
    go: bool,
    apply: bool,
) -> Result<(u64, Vec<String>)> {
    if apply && package_cache_apply_is_disabled() {
        eprintln!(
            "🛡️ keeping package caches: apply deletion is disabled because no shared package-manager lock is available"
        );
        return Ok((0, Vec::new()));
    }

    let active = detect_active_package_manager_operations().await?;
    let home = dirs::home_dir().context("cannot determine home directory for package caches")?;
    clean_package_caches_at(&home, cargo, npm, pip, go, apply, &active).await
}

/// Clean package manager caches below `home`.
///
/// `active` is supplied separately so dry-run behavior can be tested without
/// depending on the host process table. Every cache has its own protection
/// class: an active npm operation skips `.npm`, while an unrelated active Go
/// operation does not prevent cargo-cache estimates. Apply deletion is
/// intentionally refused until a lock shared with external package managers
/// can be acquired; a process snapshot cannot close the start-after-check race.
async fn clean_package_caches_at(
    home: &Path,
    cargo: bool,
    npm: bool,
    pip: bool,
    go: bool,
    apply: bool,
    active: &HashSet<PackageCacheKind>,
) -> Result<(u64, Vec<String>)> {
    if apply && package_cache_apply_is_disabled() {
        eprintln!(
            "🛡️ keeping package caches: apply deletion is disabled because no shared package-manager lock is available"
        );
        return Ok((0, Vec::new()));
    }

    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    let targets: Vec<(&str, bool, &str, PackageCacheKind)> = vec![
        (
            "cargo registry cache",
            cargo,
            ".cargo/registry/cache",
            PackageCacheKind::Cargo,
        ),
        ("npm cache", npm, ".npm", PackageCacheKind::Npm),
        ("pip cache", pip, ".cache/pip", PackageCacheKind::Pip),
        (
            "go build cache",
            go,
            ".cache/go-build",
            PackageCacheKind::Go,
        ),
    ];

    for (label, enabled, rel_path, kind) in targets {
        if !enabled {
            continue;
        }
        if active.contains(&kind) {
            eprintln!(
                "🛡️ keeping {label}: active {} operation detected",
                kind.name()
            );
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
        cleaned.push(format!("{label} ({})", human_bytes(size)));
        reclaimed += size;
    }

    Ok((reclaimed, cleaned))
}

/// The disk cost of the top-level trash entries the credential guard keeps.
///
/// ADDED 2026-10-24. The guard reported "keeping 55 flagged entries" and
/// nothing else — the entry COUNT looks like a rounding error, and it was
/// not: on this host those 55 entries held **11 GiB**, and they grew every
/// time the CI trashed a repository whose test files are named
/// `…-token-…test.ts` (the operator's own manuscript was in there too,
/// kept because a chapter is called "borrowed credentials").
///
/// A guard that keeps something must say what keeping it COSTS. This
/// measures it: the total, and the biggest few by name.
pub(crate) async fn kept_trash_entry_sizes(
    trash_files: &Path,
    names: &std::collections::HashSet<String>,
) -> (u64, Vec<(String, u64)>) {
    let mut total = 0u64;
    let mut sizes: Vec<(String, u64)> = Vec::with_capacity(names.len());
    for name in names {
        let bytes = get_dir_size(&trash_files.join(name)).await.unwrap_or(0);
        total = total.saturating_add(bytes);
        sizes.push((name.clone(), bytes));
    }
    sizes.sort_by(|a, b| b.1.cmp(&a.1));
    (total, sizes)
}

/// Empty trash. When `credential_guard` is set, the trash is first
/// scanned for credential-signal filenames (see
/// looks_credential_like and docs/design/disk-full-credentials-
/// 2026-08-10.md); flagged top-level entries are kept while the rest
/// purges (per-entry since 2026-09-20, not all-or-nothing). The
/// 2026-08-10 scan found 665 credential-pattern matches in the 56 GiB
/// trash, so blind emptying is unsafe by default.
///
/// Unobservable subtrees fail closed per-entry (audit L6): a directory
/// AT the walk depth cap (children unseen) or an unreadable subtree
/// (walk error) keeps its whole top-level entry instead of purging it.
async fn empty_trash(
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
    min_age_days: u64,
) -> Result<(u64, Vec<String>)> {
    let Some(home) = dirs::home_dir() else {
        return Ok((0, Vec::new()));
    };
    empty_trash_at(
        &home,
        apply,
        protected_paths,
        credential_guard,
        min_age_days,
    )
    .await
}

/// Map a trash path to its top-level entry name and add it to the
/// credential-guard keep set. Returns false when the path escapes the
/// trash root or has no nameable top component (nothing kept).
fn trash_keep_top_level(
    skip_names: &mut std::collections::HashSet<String>,
    trash_files: &Path,
    path: &Path,
) -> bool {
    if let Ok(rel) = path.strip_prefix(trash_files) {
        if let Some(top) = rel.components().next().and_then(|c| c.as_os_str().to_str()) {
            skip_names.insert(top.to_string());
            return true;
        }
    }
    false
}

async fn empty_trash_at(
    home: &Path,
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
    min_age_days: u64,
) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    {
        let trash_files = home.join(".local/share/Trash/files");
        let trash_info = home.join(".local/share/Trash/info");
        // Credential-flagged top-level entries to keep (2026-09-20 space
        // audit: per-entry skip replaced the all-or-nothing abort).
        let mut skip_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        if trash_files.exists() {
            let size = get_dir_size(&trash_files).await.unwrap_or(0);
            if size > 0 {
                // CHANGED 2026-09-20 (space audit): the old code aborted the
                // ENTIRE trash purge when a single credential-signal filename
                // matched anywhere inside — one trashed `target/` dir with
                // cargo fingerprint files named `*mint_dev_token*` blocked
                // 46 GiB of unrelated trash every cycle at 100% disk. The
                // guard now skips only the flagged top-level entries and
                // purges the rest. The scan must be exhaustive (no early
                // break): a capped match list would purge entries the cap
                // cut off.
                if credential_guard {
                    let mut samples = Vec::new();
                    let mut match_count = 0u64;
                    // FIXED 2026-10-02 (audit L6): the scan failed OPEN
                    // twice — credential files deeper than max_depth(8)
                    // were never seen, and walk errors (EPERM, I/O) were
                    // swallowed by `_ => {}`. Both now fail closed
                    // PER-ENTRY (keeping the 2026-09-20 design: skip the
                    // unobservable top-level entries, purge the rest —
                    // not the old all-or-nothing abort).
                    let mut depth_capped = 0u64;
                    let mut walk_errors = 0u64;
                    for entry in walkdir::WalkDir::new(&trash_files).max_depth(8) {
                        match entry {
                            Ok(e) if e.file_type().is_file() => {
                                if let Some(name) = e.file_name().to_str() {
                                    if looks_credential_like(name) {
                                        match_count += 1;
                                        trash_keep_top_level(
                                            &mut skip_names,
                                            &trash_files,
                                            e.path(),
                                        );
                                        if samples.len() < 3 {
                                            samples.push(e.path().display().to_string());
                                        }
                                    }
                                }
                            }
                            Ok(e) if e.file_type().is_dir() && e.depth() == 8 => {
                                // AT the cap: children unobservable (files
                                // AT depth 8 are still yielded and scanned
                                // above — only descent stops). Keep the
                                // whole top-level entry.
                                depth_capped += 1;
                                trash_keep_top_level(&mut skip_names, &trash_files, e.path());
                            }
                            Err(e) => {
                                // Unreadable subtree: purging it would
                                // destroy credential files the guard never
                                // saw. Keep the top-level entry; a pathless
                                // error (unreachable in practice — an
                                // unreadable trash root sizes to 0 and skips
                                // the block) is counted loud with no keep.
                                walk_errors += 1;
                                if let Some(path) = e.path() {
                                    trash_keep_top_level(&mut skip_names, &trash_files, path);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !skip_names.is_empty() {
                        // The guard must not hide the COST of what it keeps.
                        //
                        // "55 flagged entries" says nothing about how much
                        // disk that is, and the answer here was 11 GiB that
                        // grew every time the CI trashed a repository whose
                        // test files are named `…-token-…test.ts` — and the
                        // operator's own manuscript, kept because a chapter
                        // is called "borrowed credentials". The entry count
                        // looks like a rounding error; the size does not.
                        //
                        // Reporting only: no behaviour changes here.
                        let (kept_bytes, kept_sizes) =
                            kept_trash_entry_sizes(&trash_files, &skip_names).await;
                        let biggest: Vec<String> = kept_sizes
                            .iter()
                            .take(3)
                            .map(|(name, bytes)| format!("{} ({})", name, human_bytes(*bytes)))
                            .collect();
                        let mut why = format!(
                            "{} credential-like file(s), e.g. {}",
                            match_count,
                            if samples.is_empty() {
                                "(none sampled)".to_string()
                            } else {
                                samples.join(", ")
                            }
                        );
                        if depth_capped > 0 {
                            why.push_str(&format!(
                                "; {} unobservable capped subtrees",
                                depth_capped
                            ));
                        }
                        if walk_errors > 0 {
                            why.push_str(&format!("; {} unreadable subtrees", walk_errors));
                        }
                        if kept_bytes > 0 {
                            why.push_str(&format!(
                                "; they hold {} (biggest: {})",
                                human_bytes(kept_bytes),
                                if biggest.is_empty() {
                                    "(none)".to_string()
                                } else {
                                    biggest.join(", ")
                                }
                            ));
                        }
                        eprintln!(
                            "🛡️ trash: keeping {} flagged entr{} ({}), purging the rest",
                            skip_names.len(),
                            if skip_names.len() == 1 { "y" } else { "ies" },
                            why
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "trash/credential-guard",
                            format!(
                                "trash purge skipping {} flagged entries ({})",
                                skip_names.len(),
                                why
                            ),
                        ));
                    }
                }
                let mut succeeded = true;
                // Flagged entries force the per-entry path even when
                // min_age_days == 0: the whole-dir wipe below cannot spare
                // individual entries.
                let per_entry = min_age_days > 0 || !skip_names.is_empty();
                if per_entry {
                    // CHANGED 2026-08-25 (v0.112.39): age-based purge — only
                    // entries whose mtime is older than the cutoff are
                    // removed, preserving a recovery window instead of
                    // emptying everything at once. Handles both dry-run
                    // measurement and deletion, including paired .trashinfo
                    // metadata. min_age_days = 0 preserves the old behavior.
                    match purge_aged_trash_entries(
                        &trash_files,
                        &trash_info,
                        min_age_days,
                        protected_paths,
                        apply,
                        &skip_names,
                    )
                    .await
                    {
                        Ok((bytes, count)) if bytes > 0 => {
                            reclaimed += bytes;
                            cleaned.push(if min_age_days > 0 {
                                format!(
                                    "{} trash entries older than {}d ({})",
                                    count,
                                    min_age_days,
                                    human_bytes(bytes)
                                )
                            } else {
                                format!(
                                    "{} trash entries ({}, flagged entries kept)",
                                    count,
                                    human_bytes(bytes)
                                )
                            });
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("⚠️ aged trash purge failed: {}", e);
                            succeeded = false;
                        }
                    }
                } else if apply {
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
                // Skip-set runs already counted per-entry via purge_aged above;
                // the whole-size accounting below is only for the wipe path.
                if min_age_days == 0 && skip_names.is_empty() && (!apply || succeeded) {
                    cleaned.push(format!("trash files ({})", human_bytes(size)));
                    reclaimed += size;
                }
            }
        }

        // In aged mode (min_age_days > 0) the .trashinfo files are removed
        // alongside their entries by purge_aged_trash_entries; only the old
        // empty-everything path wipes the whole info directory. A non-empty
        // skip set also takes the per-entry path (whole-dir wipe would
        // orphan the kept entries' .trashinfo).
        if min_age_days == 0 && skip_names.is_empty() && trash_info.exists() {
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

/// Remove top-level trash entries whose mtime is older than
/// `min_age_days`, together with their paired `.trashinfo` metadata.
/// Works for both apply and dry-run (dry-run only measures). Protected
/// paths are skipped with a note; removal failures keep the entry and
/// do not abort the pass. `skip_names` holds top-level entry names the
/// credential guard flagged: they are kept unconditionally.
async fn purge_aged_trash_entries(
    trash_files: &Path,
    trash_info: &Path,
    min_age_days: u64,
    protected_paths: &[String],
    apply: bool,
    skip_names: &std::collections::HashSet<String>,
) -> Result<(u64, u64)> {
    // CHANGED 2026-09-09 (audit F41): `now - age` panicked (debug) on
    // absurd min_age configs. checked_sub falls back to the epoch, so an
    // older-than-epoch cutoff matches nothing (every real mtime is newer)
    // instead of crashing the pass — the safe direction.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let age = Duration::from_secs(min_age_days.saturating_mul(86_400));
    let cutoff = UNIX_EPOCH + now.checked_sub(age).unwrap_or(Duration::ZERO);
    let mut reclaimed = 0u64;
    let mut count = 0u64;
    let mut rd = tokio::fs::read_dir(trash_files).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if skip_names.contains(name) {
                eprintln!(
                    "🛡️ keeping credential-flagged trash entry {}",
                    path.display()
                );
                continue;
            }
        }
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = match meta.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if modified > cutoff {
            continue;
        }
        if let Err(e) = check_safe_to_delete_guard(&path, protected_paths) {
            eprintln!("🛡️ keeping protected trash entry {}: {}", path.display(), e);
            continue;
        }
        let entry_size = if meta.is_dir() {
            get_dir_size(&path).await.unwrap_or(0)
        } else {
            meta.len()
        };
        if !apply {
            reclaimed += entry_size;
            count += 1;
            continue;
        }
        let removed = if meta.is_dir() {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        if let Err(e) = removed {
            eprintln!("⚠️ failed to remove trash entry {}: {}", path.display(), e);
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let info = trash_info.join(format!("{}.trashinfo", name));
            let _ = tokio::fs::remove_file(info).await;
        }
        reclaimed += entry_size;
        count += 1;
    }
    Ok((reclaimed, count))
}

static RESOLVE_BIN_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();

fn resolve_bin(name: &str) -> String {
    resolve_bin_opt(name).unwrap_or_else(|| name.to_string())
}

/// Strict variant for privilege-adjacent execs (renice, systemctl):
/// refuse to run when no absolute store path resolves, instead of
/// falling back to a bare PATH-relative name that PATH poisoning could
/// redirect. Returns the absolute path.
/// Resolve a required binary to an absolute path, or explain why it cannot be
/// used. `pub(crate)` so `doctor` can distinguish "systemctl is not installed"
/// from "the service is not active" (audit 2026-10-01).
pub(crate) fn resolve_bin_strict(name: &str) -> Result<String> {
    resolve_bin_opt(name).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot resolve absolute path for `{name}` (not in NixOS store dirs) — refusing PATH-relative exec"
        )
    })
}

/// PATH-searching variant for READ-ONLY probes (doctor service checks,
/// status report). ADDED 2026-10-03 (audit R4-SYS-03): the strict
/// resolver only consults NixOS store dirs, so on non-NixOS systemd
/// hosts (systemctl at /usr/bin) every service check degraded to
/// Skipped. Store dirs first, then PATH for a regular executable
/// file; the returned path is always ABSOLUTE (never a bare name),
/// so the exec site stays immune to cwd-relative tricks. Residual
/// risk is a poisoned PATH entry lying to a diagnostic — which
/// mutates nothing. Mutating execs (renice, systemctl reload/restart)
/// keep using `resolve_bin_strict`; this fallback must never serve them.
pub(crate) fn resolve_bin_for_readonly_probe(name: &str) -> Result<String> {
    if let Some(store) = resolve_bin_opt(name) {
        return Ok(store);
    }
    let path_os = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_os).collect();
    search_path_for_executable(name, &dirs)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve `{name}` (not in NixOS store dirs or PATH)"))
}

/// Search explicit directories for a regular executable `name` (pure
/// helper for test — the PATH read lives in the caller so tests need
/// no env mutation). Skips empty entries, directories, dead links,
/// and files without any execute bit (a resolved-but-unrunnable path
/// would report Fail instead of Skipped downstream).
fn search_path_for_executable(name: &str, dirs: &[PathBuf]) -> Option<String> {
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        // metadata (not symlink_metadata): follow links — PATH
        // entries are routinely symlinks (alternatives, store
        // links). Dead links Err out and are skipped.
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return Some(candidate.to_string_lossy().to_string());
    }
    None
}

fn resolve_bin_opt(name: &str) -> Option<String> {
    let cache =
        RESOLVE_BIN_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        if let Some(cached) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(name) {
            return Some(cached.clone());
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
        });
    if let Some(ref resolved) = result {
        cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string(), resolved.clone());
    }
    result
}

/// Return the `nix-env --delete-generations` argument that retains the newest
/// `keep_generations` generations. Nix interprets a bare number as one
/// specific generation to delete; the leading `+` is what means "keep the
/// newest N generations".
fn nix_delete_generations_arg(keep_generations: u32) -> Option<String> {
    (keep_generations > 0).then(|| format!("+{keep_generations}"))
}

/// How many generations a Nix profile has.
///
/// ADDED 2026-10-22: the guard only attempts the root-owned system-profile
/// prune when there is genuinely something to prune, so the expected
/// permission failure stops being a line on every cycle. `None` when the
/// profile cannot be read — the caller then leaves the profile alone
/// rather than guessing.
fn nix_env_generation_count(nix_env: &Path, profile: &str) -> Option<usize> {
    let output = std::process::Command::new(nix_env)
        .args(["--list-generations", "-p", profile])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().filter(|line| !line.trim().is_empty()).count())
}

/// Run an external cleanup command with a hard bound.
///
/// ADDED 2026-10-22: the guard's loop calls the cleanup kinds inline, so a
/// slow `nix-collect-garbage` (minutes) or a docker prune blocks the whole
/// loop — and the memory monitoring that would notice a swap spike never
/// runs in the meantime. The tracked design note (the parent repository's
/// `docs/design/performance-swap-thrash-2026-10-21.md`) listed this as open
/// work: bound the external cleanup commands and keep memory monitoring
/// independent of slow storage maintenance.
///
/// The child is killed when the bound expires (`kill_on_drop`), so a wedged
/// cleanup cannot outlive its budget.
pub(crate) async fn run_bounded(
    command: &mut Command,
    timeout_secs: u64,
) -> Result<Option<std::process::Output>> {
    command.kill_on_drop(true);
    let bound = Duration::from_secs(timeout_secs.max(1));
    match tokio::time::timeout(bound, command.output()).await {
        Ok(result) => Ok(Some(result?)),
        Err(_) => {
            eprintln!(
                "⚠️ external cleanup command exceeded its {timeout_secs}s bound — skipped this pass"
            );
            Ok(None)
        }
    }
}

/// Run Nix profile and store cleanup with explicit binary paths.
///
/// The binary parameters keep the command construction testable without
/// depending on the host's Nix installation. Applying profile retention must
/// not pass `-d` to `nix-collect-garbage`: that flag deletes all old profile
/// generations, bypassing `nix_keep_generations`.
async fn clean_nix_garbage_with_bins(
    keep_generations: u32,
    apply: bool,
    nix_env: &Path,
    nix_gc: &Path,
    command_timeout_secs: u64,
) -> Result<(u64, Vec<String>)> {
    let reclaimed = 0u64;
    let mut cleaned = Vec::new();

    // CHANGED 2026-09-20 (space audit): generation-prune failures are
    // warnings, not fatal. The old code returned Err when either nix-env
    // call failed, which discarded the store-GC results collected below
    // and surfaced "Nix cleanup failed" every cycle — while the valuable
    // step (nix-collect-garbage) may have succeeded. A user-session guard
    // can also never prune /nix/var/nix/profiles/default (root-owned),
    // so that failure is expected, not exceptional; the per-profile note
    // keeps it visible without failing the pass.
    if let Some(gen_arg) = nix_delete_generations_arg(keep_generations).filter(|_| apply) {
        match Command::new(nix_env)
            .args(["--delete-generations", &gen_arg])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "⚠️ nix-env user-profile generation prune failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(e) => eprintln!("⚠️ nix-env user-profile generation prune failed: {}", e),
        }

        // The SYSTEM profile is root-owned: a user-session guard can never
        // prune it, and nix-env answers "Read-only file system (needs
        // root; user GC still runs)" every cycle. Attempt the prune only
        // when it would actually remove something — the generation count
        // is above the retention — so the expected permission failure is
        // not a line on every cycle, and name the documented privileged
        // path when it is genuinely needed.
        //
        // ADDED 2026-10-22: the 2026-10-21 disk incident left the guard
        // logging that failure every cycle while 144 GiB of scratch
        // accumulated; the noise belonged in the report, not on every pass.
        let system_profile = "/nix/var/nix/profiles/default";
        let system_profile_needs_prune = nix_env_generation_count(nix_env, system_profile)
            .map(|count| count > keep_generations as usize)
            .unwrap_or(false);
        if system_profile_needs_prune {
            match Command::new(nix_env)
                .args(["--delete-generations", &gen_arg])
                .arg("-p")
                .arg(system_profile)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {}
                Ok(output) => eprintln!(
                    "⚠️ nix-env system-profile generation prune failed ({}): {} (needs root — run scripts/prune-nix-system-profile.sh with sudo; user GC still runs)",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Err(e) => eprintln!(
                    "⚠️ nix-env system-profile generation prune failed: {} (needs root — run scripts/prune-nix-system-profile.sh with sudo; user GC still runs)",
                    e
                ),
            }
        }
    }

    // Do not use `-d` here. It deletes every old profile generation, which
    // would make nix_keep_generations ineffective. The explicit nix-env
    // commands above perform only the configured retention pruning; ordinary
    // garbage collection then removes store paths no longer rooted by the
    // retained generations.
    let args: &[&str] = if apply { &[] } else { &["--dry-run"] };
    // ADDED 2026-10-22: bounded. `nix-collect-garbage` can run for
    // minutes, and it ran INLINE in the guard's loop — so during a GC the
    // guard sampled no memory at all, which is exactly when a swap spike
    // must not go unnoticed.
    let mut gc = Command::new(nix_gc);
    gc.args(args);
    let out = match run_bounded(&mut gc, command_timeout_secs).await? {
        Some(out) => out,
        None => return Ok((reclaimed, cleaned)),
    };

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
        // nix-collect-garbage reports paths here, not their byte sizes. Do
        // not turn a path count into a made-up reclaim estimate; callers use
        // `reclaimed` for accounting and a false value is worse than zero.
    }

    Ok((reclaimed, cleaned))
}

/// Run nix-collect-garbage and retain the configured number of profile
/// generations.
async fn clean_nix_garbage(
    keep_generations: u32,
    apply: bool,
    command_timeout_secs: u64,
) -> Result<(u64, Vec<String>)> {
    let nix_env = resolve_bin("nix-env");
    let nix_gc = resolve_bin("nix-collect-garbage");
    clean_nix_garbage_with_bins(
        keep_generations,
        apply,
        Path::new(&nix_env),
        Path::new(&nix_gc),
        command_timeout_secs,
    )
    .await
}

/// Clean old node_modules directories
async fn clean_old_node_modules(
    roots: &[PathBuf],
    max_age_days: u64,
    apply: bool,
    protected_paths: &[String],
    stash_first: bool,
    stash_dir: &Path,
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
            // CHANGED 2026-09-20 (space audit): 5 -> 8, same nested-game
            // reasoning as the rust target scan (depth 6 node_modules were
            // invisible).
            .max_depth(8)
            .follow_links(false)
            .into_iter()
            // Once an outer node_modules directory is considered, its
            // descendants are included in the outer size and will be
            // removed with it. Do not visit nested node_modules trees or
            // count them a second time.
            .filter_entry(|entry| {
                entry.depth() == 0
                    || entry
                        .path()
                        .parent()
                        .and_then(|parent| parent.file_name())
                        .map(|name| name != "node_modules")
                        .unwrap_or(true)
            })
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
                // ADDED 2026-09-27 (space tiers Phase 2): stash-first.
                let mut stashed = false;
                let mut freed = size;
                if apply {
                    if stash_first {
                        match stash_first_remove(&path, stash_dir, protected_paths) {
                            Ok((manifest, f)) => {
                                stashed = true;
                                freed = f;
                                eprintln!(
                                    "📦 Node stashed: {} as {}",
                                    path.display(),
                                    manifest.name
                                );
                            }
                            Err(e) => {
                                eprintln!("⚠️ failed to stash {}: {}", path.display(), e);
                                succeeded = false;
                            }
                        }
                    } else {
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
                }
                if !apply || succeeded {
                    if stashed {
                        cleaned.push(format!(
                            "{} (stashed, {} days old, {}, freed {})",
                            path.display(),
                            modified_secs_ago / 86400,
                            human_bytes(size),
                            human_bytes(freed)
                        ));
                        reclaimed += freed;
                    } else {
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
        // Truncate the existing inode in place. Replacing the path with a
        // temporary file makes any process that already has the log open
        // continue writing to an unlinked inode, losing those later lines.
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        if file.metadata()?.len() != original_size {
            // A writer changed the file while it was being inspected. Leave
            // it alone and let the next cleanup pass retry safely.
            return Ok(0);
        }
        file.set_len(max_size_bytes)?;
        let new_size = file.metadata()?.len();
        return Ok(original_size.saturating_sub(new_size));
    }

    // Preserve header lines in memory, then write them back to the same inode.
    // FIXED 2026-10-02 (audit L8): two data-loss windows closed —
    //  1. Crash window: the old `set_len(0)` + `write_all` left an EMPTY
    //     log if the process died between the calls. The replacement
    //     (always shorter: capped at max_size < original) is now written
    //     at offset 0 FIRST, flushed, and only then is the stale tail cut
    //     with set_len. A crash mid-rewrite leaves stale-but-present
    //     content, never an empty file. Temp-and-rename stays rejected:
    //     open writers would keep appending to an unlinked inode (test
    //     truncate_log_preserves_open_writer_inode pins the inode).
    //  2. Non-UTF8 lines: `BufRead::lines()` yields Err on invalid UTF-8
    //     and both loops STOPPED there, silently dropping the bad line
    //     AND everything after it. Lines are now split as BYTES and
    //     preserved verbatim (including any `\r` — no line-ending
    //     rewriting); only a real I/O error aborts with Ok(0) for a
    //     later pass, never a partial rewrite of a half-seen file.
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut segments: Vec<Vec<u8>> = Vec::new();
    let mut kept_bytes = 0u64;
    let mut headers_kept = 0usize;
    let mut byte_lines = reader.split(b'\n').peekable();
    while let Some(seg_result) = byte_lines.next() {
        let seg = match seg_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "⚠️ truncate {}: I/O error mid-read ({}) — leaving file for next pass",
                    path.display(),
                    e
                );
                return Ok(0);
            }
        };
        // `split` yields a trailing empty segment when the file ends with
        // `\n` — the terminator, not a line. (The file is non-empty here:
        // original_size > max_size_bytes >= 0... and an empty file returns
        // Ok(0) above — so a final empty segment is always the artifact.)
        if byte_lines.peek().is_none() && seg.is_empty() {
            break;
        }
        if headers_kept < preserve_header_lines {
            headers_kept += 1;
        } else if kept_bytes + seg.len() as u64 + 1 > max_size_bytes {
            break;
        }
        kept_bytes += seg.len() as u64 + 1;
        segments.push(seg);
    }
    let mut replacement = Vec::with_capacity(kept_bytes as usize);
    for seg in &segments {
        replacement.extend_from_slice(seg);
        replacement.push(b'\n');
    }

    if std::fs::metadata(path)?.len() != original_size {
        // Do not overwrite content that was appended while we were reading.
        return Ok(0);
    }

    let mut output = std::fs::OpenOptions::new().write(true).open(path)?;
    if output.metadata()?.len() != original_size {
        return Ok(0);
    }
    output.write_all(&replacement)?;
    output.flush()?;
    output.set_len(replacement.len() as u64)?;
    output.sync_all()?;
    let new_size = output.metadata()?.len();
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
    let early = used >= guard.disk_early_warn_percent && used < guard.disk_warn_percent;
    let (previous, _) = report_state_transition(
        state,
        "disk-early-warning",
        if early { "early" } else { "ok" },
        guard.report_repeat_secs,
    );
    if early && previous.as_deref() != Some("early") {
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

/// Use space available to this user, excluding filesystem reserved blocks.
fn rapid_disk_fill_message(rate: f64, used_pct: u8, avail_bytes: u64) -> String {
    let available_gib = avail_bytes as f64 / 1024_f64.powi(3);
    let hours = available_gib / rate;
    format!(
        "Disk growing at {rate:.1} GiB/h ({used_pct}% used); {available_gib:.1} GiB available, \
         estimated {hours:.1} hours until full if growth continues — inspect recent writes with dracon-system storage"
    )
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
    avail_bytes: u64,
) -> Option<f64> {
    let now = Instant::now();
    state.disk_bytes_history.push((now, used_bytes));
    if state.disk_bytes_history.len() > 200 {
        let excess = state.disk_bytes_history.len() - 200;
        state.disk_bytes_history.drain(0..excess);
    }
    let Some(rate) = disk_fill_rate_gbph(&state.disk_bytes_history) else {
        // Re-arm the entry alert after reclamation. An old "rapid" verdict
        // must not suppress notification when a fresh growth window appears.
        report_state_transition(
            state,
            "disk-rapid-fill",
            "unmeasured",
            guard.report_repeat_secs,
        );
        return None;
    };
    let rapid = rate >= guard.disk_rapid_fill_gbph;
    let (previous, event_due) = report_state_transition(
        state,
        "disk-rapid-fill",
        if rapid { "rapid" } else { "ok" },
        guard.report_repeat_secs,
    );
    if rapid {
        let detail = rapid_disk_fill_message(rate, used_pct, avail_bytes);
        // One notification on entry; unchanged rapid growth is retained in
        // structured telemetry and summarized only at the repeat interval.
        if previous.as_deref() != Some("rapid") {
            send_notification(guard, "Dracon System Guard - Disk Filling Rapidly", &detail).await;
        }
        if event_due {
            emit_event(&DraconEvent::new(
                "system",
                EventSeverity::Warn,
                "disk/rapid-fill",
                detail,
            ));
        }
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
    all_processes: &[ProcSample],
) -> Option<MemoryReport> {
    if !guard.monitor_memory {
        return None;
    }
    let sample = memory_sample().await?;
    let mem_available_percent = sample.mem_available_percent();
    let swap_used_percent = sample.swap_used_percent();
    let psi_full_avg10 = psi_full_avg10().await;

    // Swap-in velocity (pages/s), measured on EVERY pass. This used to be
    // a fallback computed only when PSI was unavailable — but violent
    // swap-in with moderate PSI is exactly what thrashing looks like
    // (2026-10-04: si 100k+/s at PSI 3.9 read "ok" and no mitigation
    // fired). Swap-OUT is deliberately not a signal: paging out is
    // reclaim working, not pressure; faulting back IN is the stall.
    let mut pswpin_rate = None;
    if let Some((pin, pout)) = vmstat_swap_counters().await {
        if let Some((prev_at, prev_pin, _prev_pout)) = state.prev_swap_counters {
            let dt = prev_at.elapsed().as_secs_f64();
            if dt > 0.0 {
                pswpin_rate = Some(pin.saturating_sub(prev_pin) as f64 / dt);
            }
        }
        record_swap_counters(state, pin, pout);
    }

    let mem_low = mem_available_percent <= guard.mem_available_warn_percent;
    // Swap occupancy is useful context, but is not active pressure on its
    // own: Linux may keep cold pages in swap while RAM and PSI are healthy.
    let swap_high = swap_used_percent >= guard.swap_used_warn_percent;
    let psi_thrash = psi_full_avg10.is_some_and(|v| v >= guard.mem_psi_full_warn)
        || swapin_velocity_thrash(pswpin_rate, guard.mem_swapin_warn_pages_per_sec);
    let observed_pressure = classify_memory_pressure(mem_low, swap_high, psi_thrash);
    let (pressure, _previous_pressure, pressure_changed) = stabilize_memory_pressure_at(
        state,
        observed_pressure,
        guard.memory_pressure_sustain_secs,
        Instant::now(),
    );

    // A user service can often lower a process's priority but cannot raise it
    // again during recovery. Do not apply a reversible limiter unless the
    // service can complete both halves of that lifecycle.
    let can_restore_nice = if guard.auto_renice_on_memory || !state.memory_reniced_pids.is_empty() {
        nice_restore_capability_available(state)
    } else {
        true
    };

    // Top offenders (skipping kernel threads and exempt names).
    //
    // ADDED 2026-10-22: ranked by resident PLUS swapped, and the swapped
    // half is read for the candidates only (five `/proc/<pid>/status`
    // reads, not one per process). RSS alone misses the largest processes
    // once the kernel has evicted their pages.
    let exempt = parse_kinds(&guard.process_exempt_names);
    let mut top_rss: Vec<ProcSample> = all_processes
        .iter()
        .filter(|p| !exempt.contains(&p.command) && !is_kernel_process(&p.command))
        .cloned()
        .fold(Vec::new(), |mut acc, p| {
            if acc.len() < 5
                || memory_pressure_footprint_mb(&p)
                    > acc
                        .last()
                        .map(|x: &ProcSample| memory_pressure_footprint_mb(x))
                        .unwrap_or(0)
            {
                acc.push(p);
                acc.sort_by_key(|b| std::cmp::Reverse(memory_pressure_footprint_mb(b)));
                acc.truncate(5);
            }
            acc
        })
        .into_iter()
        .map(load_process_cmdline)
        .collect();
    // Fill in the swapped half for the candidates, then re-rank: a process
    // that is mostly swapped can overtake a bigger resident one.
    for sample in &mut top_rss {
        sample.swap_mb = read_proc_swap_mb(sample.pid).unwrap_or(0);
    }
    top_rss.sort_by_key(|b| std::cmp::Reverse(memory_pressure_footprint_mb(b)));

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
    if !state.mitigation_ledger_unavailable
        && pressure != "ok"
        && guard.auto_renice_on_memory
        && can_restore_nice
    {
        for p in &top_rss {
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            let current_identity = process_sample_identity(p);
            drop_stale_nice_adjustments(state, p.pid, &current_identity);
            let nice_val =
                graduated_nice_value(p.cpu_percent, p.rss_mb, guard.renice_value, p.nice);
            let applied_nice = state
                .memory_reniced_pids
                .get(&p.pid)
                .map(|entry| entry.applied_nice);
            if applied_nice != Some(nice_val) {
                let original_nice = state
                    .memory_reniced_pids
                    .get(&p.pid)
                    .map(|entry| entry.original_nice)
                    .or_else(|| {
                        state
                            .reniced_pids
                            .get(&p.pid)
                            .map(|entry| entry.original_nice)
                    })
                    .unwrap_or(p.nice);
                // ADDED 2026-10-24: never apply a priority the guard could
                // not take back. The old code checked the capability in the
                // guard's OWN user namespace only, and that check said yes
                // for processes the guard provably cannot restore — so the
                // mitigation was a one-way ratchet that left the operator's
                // long-lived processes stuck at a degraded priority.
                if !can_restore_nice_for(Path::new("/proc"), p.pid) {
                    if state.nice_restore_skipped_pids.insert(p.pid) {
                        eprintln!(
                            "⚠️ renice mitigation skipped for pid={} cmd={} — the guard cannot restore its priority in that process's user namespace, so it will not lower it",
                            p.pid, p.command
                        );
                    }
                    continue;
                }
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        state.memory_reniced_pids.insert(
                            p.pid,
                            MemoryReniceState {
                                original_nice,
                                applied_nice: nice_val,
                                identity: current_identity,
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
            // Registered managed jobs carry their own lifetime debt and an
            // independent recoverer. Ancestry-based bias must not be applied
            // on top of it, and unregistered jobs stay explicitly unsupported.
            if managed_oom::is_managed_job_process(p.pid) {
                match managed_oom::bias(p, &exempt).await {
                    Ok(Some((orig, target))) => {
                        eprintln!(
                            "🛡️ managed oom-bias pid={} cmd={} adj {} -> {} (critical pressure)",
                            p.pid, p.command, orig, target
                        );
                        limited.push(format!("oom-bias {}={}", p.command, target));
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("managed OOM bias not applied pid={}: {e}", p.pid),
                }
                continue;
            }
            match apply_oom_bias(p, all_processes, state) {
                Ok(Some((orig, target))) => {
                    eprintln!(
                        "🛡️ oom-bias pid={} cmd={} adj {} -> {} (critical pressure)",
                        p.pid, p.command, orig, target
                    );
                    limited.push(format!("oom-bias {}={}", p.command, target));
                }
                Ok(None) => {}
                Err(e) => eprintln!("OOM bias not applied pid={}: {e}", p.pid),
            }
        }
    }
    if !state.mitigation_ledger_unavailable
        && pressure == "critical"
        && guard.cap_offenders_cpu_percent > 0
    {
        for p in &top_rss {
            if state.capped_pids.contains_key(&p.pid) {
                continue;
            }
            let identity_ok = process_sample_is_current(p);
            if !identity_ok {
                continue;
            }
            match cap_cpu_process(p.pid, guard.cap_offenders_cpu_percent, &exempt).await {
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

    // Pressure-gated orphan reap (2026-10-04): runaways whose owners died
    // ignore the 24h auto-reap floor by design, so under warn/critical
    // pressure they are reaped by orphanhood instead of age. Opt-in,
    // default OFF. Blocking (SIGTERM grace sleeps), so it runs off the
    // tokio workers like the auto-reap pass.
    let mut pressure_reaped: Vec<ReapedProcess> = Vec::new();
    if (pressure == "warn" || pressure == "critical") && guard.reap_orphans_on_pressure {
        let policy = reap_policy_from_guard(guard);
        pressure_reaped = tokio::task::spawn_blocking(move || {
            let proc_root = Path::new("/proc");
            // No boot clock, no age evidence: fail closed like the
            // report-only scan rather than running half-blind.
            let boot_time = read_boot_time(proc_root)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(boot_time);
            let found =
                scan_pressure_orphans(proc_root, &policy, boot_time, now, PROC_TICKS_PER_SEC);
            Some(reap_pressure_orphans(
                proc_root,
                &policy,
                &found,
                PROC_TICKS_PER_SEC,
            ))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        for r in &pressure_reaped {
            limited.push(format!(
                "pressure-reap {}={} ({})",
                r.comm,
                r.pid,
                r.outcome
                    .as_ref()
                    .map(|o| format!("{o:?}"))
                    .unwrap_or_else(|| "skipped".to_string())
            ));
        }
    }

    // ── Forgotten heavy orphan TREES (2026-10-22; trees 2026-10-23) ──
    // The operator's shape: heavy, idle, and left behind by a test — "not
    // long running and working but truly heavy and forgotten, like some
    // chrome instances left behind during tests".
    //
    // The SCAN is unconditional: every qualifying tree is reported on
    // every pass, with its evidence, and so is every tree that was
    // considered and VETOED because it looks like a live desktop session.
    // ENDING one is pressure-gated, like the other orphan path, and
    // re-verifies every gate live before the signal.
    //
    // The weight is the AGGREGATE over the tree: the 2026-10-23 leak was
    // 8.6 GB across 792 orphaned browser processes of ~3 MiB each, so the
    // per-process floor was never crossed by any of them and the guard
    // logged critical memory pressure while reaping nothing.
    let forgotten = forgotten_scan(guard);
    for c in &forgotten.candidates {
        eprintln!(
            "🧹 forgotten orphan tree: {} pid={} {} processes {}MiB (root {}MiB) age={}s cpu={}s — {}",
            c.root_comm,
            c.root_pid,
            c.member_count,
            c.aggregate_footprint_mb,
            c.root_footprint_mb,
            c.age_secs,
            c.cpu_seconds,
            c.why
        );
    }
    for v in &forgotten.vetoed {
        eprintln!(
            "🛡️ heavy process tree vetoed: {} pid={} {} processes {}MiB — {}",
            v.root_comm, v.root_pid, v.member_count, v.aggregate_footprint_mb, v.reason
        );
    }
    // A heavy orphan that is deliberately left alone is REPORTED, not
    // skipped in silence: the operator needs to see the 1.5 GB tree the
    // guard is not ending, and why.
    for p in &forgotten.protected {
        eprintln!(
            "🛡️ heavy orphan tree left alone: {} pid={} {} processes {}MiB — {}",
            p.root_comm, p.root_pid, p.member_count, p.aggregate_footprint_mb, p.reason
        );
    }
    let mut forgotten_reaped: Vec<ReapedTree> = Vec::new();
    // The gate is a NAMED function, not an inline string comparison, and it
    // fails CLOSED: an unrecognised pressure level is not permission to
    // end a process. `forgotten::forgotten_reap_allowed_under_pressure`.
    if forgotten_reap_allowed_under_pressure(&pressure) {
        let policy = reap_policy_from_guard(guard);
        let trees = forgotten.candidates.clone();
        forgotten_reaped = tokio::task::spawn_blocking(move || {
            reap_forgotten_orphan_trees(
                Path::new("/proc"),
                &policy,
                &trees,
                PROC_TICKS_PER_SEC,
            )
        })
        .await
        .unwrap_or_default();
        for r in &forgotten_reaped {
            limited.push(format!(
                "forgotten-reap tree {}={} ({} processes, {}MiB, {} ended)",
                r.root_comm,
                r.root_pid,
                r.member_count,
                r.aggregate_footprint_mb,
                r.members_ended
            ));
        }
        // A leaked browser is two halves: the processes and the profile
        // directory they owned. The guard has just proved the browser is
        // dead, so the profile is reclaimed NOW rather than waiting out the
        // tmp age floor. Only a tree that was fully ended yields a profile.
        let profile_dirs: Vec<PathBuf> = forgotten_reaped
            .iter()
            .filter_map(|r| r.profile_dir.clone())
            .collect();
        let reclaimed_profiles =
            reclaim_reaped_browser_profiles(Path::new("/proc"), guard, &profile_dirs).await;
        for p in &reclaimed_profiles {
            limited.push(format!("reclaimed browser profile {p}"));
        }
    }

    // A child forked after its parent was biased inherits oom_score_adj=250,
    // but is not present in `oom_biased_pids`. Sweep those descendants on
    // every pass, including before a tracked parent is released or removed.
    let sweep = sweep_stranded_oom_descendants(Path::new("/proc"), all_processes, state, &exempt);
    if sweep.deferred > 0 {
        eprintln!(
            "⚠️ retaining {} pending descendant OOM restorations",
            sweep.deferred
        );
    }
    limited.extend(sweep.restored);

    // FIXED 2026-10-01 (audit): the oom_score_adj and CPUQuota RELEASES sat inside
    // `if pressure == "ok" && can_restore_nice`, but their APPLIES are not gated by
    // that flag (only the renice apply is). On a host without CAP_SYS_NICE — a
    // manual `guard daemon` run, or a unit predating v0.112.39 — the guard biased
    // oom to 250 and capped CPU under critical pressure and could then never
    // release either, while the only warning printed said "renice mitigation
    // disabled". The releases now run whenever pressure recovers, and only the
    // renice half stays gated, which is the flag that actually governs a
    // reversible renice.
    if pressure == "ok" {
        if !state.mitigation_ledger_unavailable {
            // Shared by all three release halves below: the renice, oom and CPU
            // cooldown stamps are all measured from the same release window.
            let now = Instant::now();
            let release_dur = Duration::from_secs(guard.release_after_secs);
            if can_restore_nice {
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
                    let restore_nice = state
                        .reniced_pids
                        .get(&pid)
                        .filter(|entry| same_process_incarnation(&entry.identity, &identity))
                        .map(|entry| entry.applied_nice)
                        .unwrap_or(original_nice);
                    match process_identity_status(Path::new("/proc"), pid, &identity) {
                        ProcessIdentityStatus::Match => {
                            drop_stale_nice_adjustments(state, pid, &identity);
                        }
                        ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                            remove_memory_renice(state, pid);
                            continue;
                        }
                        ProcessIdentityStatus::Unavailable => {
                            // Bounded defer: only retry the identity read a few times.
                            // Unbounded deferral is what caused the long-running memory
                            // leak when PIDs are in cgroups we cannot inspect.
                            let attempts = state
                                .memory_identity_unavailable_attempts
                                .entry(pid)
                                .or_insert(0);
                            *attempts += 1;
                            if *attempts >= IDENTITY_UNAVAILABLE_RETRY_LIMIT {
                                eprintln!(
                                "⚠️ mem-unrenice dropping pid={} after {} unavailable identity reads (will not retry)",
                                pid, *attempts
                            );
                                state.memory_identity_unavailable_attempts.remove(&pid);
                                remove_memory_renice(state, pid);
                            } else {
                                eprintln!(
                                "⚠️ mem-unrenice deferred for pid={} — process identity unavailable (attempt {}/{})",
                                pid, *attempts, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                            );
                            }
                            continue;
                        }
                    }
                    // Identity was Match (or fell through after dropping) — clear any
                    // prior unavailability counter for this pid.
                    state.memory_identity_unavailable_attempts.remove(&pid);
                    if let Err(e) = renice_process(pid, restore_nice).await {
                        // Bounded defer: a renice that fails with Permission denied will
                        // fail again every cycle. Retrying forever accumulates an entry
                        // per PID in `memory_reniced_pids` and leaks memory until the
                        // cgroup's MemoryMax trips.
                        let attempts = state.memory_unrenice_failures.entry(pid).or_insert(0);
                        *attempts += 1;
                        if *attempts >= UNRENICE_RETRY_LIMIT {
                            eprintln!(
                            "⚠️ mem-unrenice dropping pid={} after {} failed renices: {} (will not retry)",
                            pid, *attempts, e
                        );
                            state.memory_unrenice_failures.remove(&pid);
                            remove_memory_renice(state, pid);
                        } else {
                            eprintln!(
                                "⚠️ mem-unrenice failed for pid={} (attempt {}/{}): {}",
                                pid, *attempts, UNRENICE_RETRY_LIMIT, e
                            );
                        }
                        continue;
                    }
                    eprintln!(
                        "🛡️ mem-unrenice pid={} -> nice {} (pressure released)",
                        pid, restore_nice
                    );
                    state.memory_unrenice_failures.remove(&pid);
                    remove_memory_renice(state, pid);
                }
                state
                    .memory_cooled_since
                    .retain(|pid, _| state.memory_reniced_pids.contains_key(pid));
            }
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
                        if record_unavailable_attempt(
                            &mut state.oom_identity_unavailable_attempts,
                            pid,
                        ) {
                            eprintln!(
                            "⚠️ oom-restore retaining pid={} after {} unavailable identity reads",
                            pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                            // A retry limit bounds warning cadence, never proof
                            // that a live adjustment was restored.
                            state
                                .oom_identity_unavailable_attempts
                                .insert(pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT);
                        } else {
                            eprintln!(
                            "⚠️ oom-restore deferred for pid={} — process identity unavailable (attempt {}/{})",
                            pid,
                            state.oom_identity_unavailable_attempts.get(&pid).copied().unwrap_or(0),
                            IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                        }
                        continue;
                    }
                }
                // Identity was readable: clear any prior unavailability count.
                state.oom_identity_unavailable_attempts.remove(&pid);
                if let Err(e) = oom_restore::restore(Path::new("/proc"), pid, &identity, orig) {
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
                if cpu_limit::is_lease(&scope) {
                    match cpu_limit::release(&scope, &orig_cgroup).await {
                        Ok(()) => remove_cpu_cap(state, pid),
                        Err(e) => eprintln!("CPU lease release retained {scope}: {e}"),
                    }
                    continue;
                }
                let allow_pid_move = match process_identity_status(
                    Path::new("/proc"),
                    pid,
                    &identity,
                ) {
                    ProcessIdentityStatus::Match | ProcessIdentityStatus::Gone => true,
                    ProcessIdentityStatus::Mismatch => false,
                    ProcessIdentityStatus::Unavailable => {
                        if record_unavailable_attempt(
                            &mut state.cap_identity_unavailable_attempts,
                            pid,
                        ) {
                            eprintln!(
                                "⚠️ cpu-uncap retaining pid={} after {} unavailable identity reads",
                                pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                            );
                            // Unreadability never proves the legacy unit empty.
                            // Keep its durable recovery obligation.
                            state
                                .cap_identity_unavailable_attempts
                                .insert(pid, IDENTITY_UNAVAILABLE_RETRY_LIMIT);
                        } else {
                            eprintln!(
                            "⚠️ cpu-un cap deferred for pid={} — process identity unavailable (attempt {}/{})",
                            pid,
                            state.cap_identity_unavailable_attempts.get(&pid).copied().unwrap_or(0),
                            IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                        }
                        continue;
                    }
                };
                // Identity was readable: clear any prior unavailability count.
                state.cap_identity_unavailable_attempts.remove(&pid);
                let systemctl_bin = match resolve_bin_strict("systemctl") {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("cpu-uncap unavailable, skipping pid={pid}: {e:#}");
                        continue;
                    }
                };
                if let Err(e) = uncap_cpu_process_with_bin(
                    &PathBuf::from(systemctl_bin),
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
        }
    } else {
        // Pressure persists: restart any release timers from zero.
        state.memory_cooled_since.clear();
        state.oom_cooled_since.clear();
        state.cap_cooled_since.clear();
    }
    // Prune only known-gone or PID-reused entries. Identity read errors are
    // retained so a transient /proc failure cannot silently lose a limiter.
    // A recycled PID (same number, different starttime) must not keep
    // throttling an innocent new process.
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
    prune_oom_known_descendants(state, all_processes);
    state.capped_pids.retain(|pid, (_, _, identity)| {
        // Same rule as the memory and OOM prunes above: a recycled PID
        // (same number, different starttime) must not keep throttling an
        // innocent new process.
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });

    let (last_reported_pressure, event_due) = report_state_transition(
        state,
        "memory-pressure",
        &pressure,
        guard.report_repeat_secs,
    );

    // Desktop notifications are reserved for stabilized state transitions.
    // An unchanged warning may still produce an occasional structured event,
    // but it will not repeatedly interrupt the operator.
    if pressure_changed && pressure != "ok" {
        let offenders: String = top_rss
            .iter()
            .map(|p| {
                if p.swap_mb > 0 {
                    format!(
                        "{} pid={} {}MiB+{}MiB swap",
                        p.command, p.pid, p.rss_mb, p.swap_mb
                    )
                } else {
                    format!("{} pid={} {}MiB", p.command, p.pid, p.rss_mb)
                }
            })
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

    // Emit on entry/escalation/recovery, then only at the configured summary
    // interval while the same non-OK state persists.
    let recovery = pressure == "ok"
        && last_reported_pressure
            .as_deref()
            .is_some_and(|previous| previous != "ok");
    if event_due && (pressure != "ok" || recovery) {
        emit_event(&DraconEvent::new(
            "system",
            if pressure == "critical" {
                EventSeverity::Error
            } else if pressure == "warn" {
                EventSeverity::Warn
            } else {
                EventSeverity::Info
            },
            "memory/pressure",
            format!(
                "{}: observed={}, mem available {}%, swap used {}%, PSI full {:?}, top rss: {}",
                pressure,
                observed_pressure,
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
        observed_pressure: observed_pressure.to_string(),
        pressure,
        top_rss,
        limited,
        pressure_reaped,
        forgotten_trees: forgotten.candidates,
        vetoed_trees: forgotten.vetoed,
        forgotten_reaped,
        protected_trees: forgotten.protected,
    })
}

/// Enumerate zombies with parent/age context (ADDED 2026-08-10,
/// v0.112.35). Zombies can't be killed; the diagnostic value is the
/// count, their parents (a live parent that never wait()s), and how
/// long they have lingered.
/// Build the reporting policy from the operator's config.
///
/// Exempt names are shared with the pressure mitigation on purpose: if a
/// process is too important to renice under memory pressure, it is far too
/// important to appear on a list of things that look abandoned.
/// Record one unreadable-identity defer for `pid`. Returns true when the
/// entry has exhausted IDENTITY_UNAVAILABLE_RETRY_LIMIT and the caller
/// should drop it instead of retaining it forever.
fn record_unavailable_attempt(attempts: &mut HashMap<i32, u32>, pid: i32) -> bool {
    let n = attempts.entry(pid).or_insert(0);
    *n += 1;
    *n >= IDENTITY_UNAVAILABLE_RETRY_LIMIT
}

/// One applied, reversible mitigation, as persisted between daemon runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MitigationRecord {
    pid: i32,
    /// The reversible adjustment: the nice value to restore to, the
    /// `oom_score_adj` to restore, and the transient CPUQuota scope to drop.
    original_nice: Option<i32>,
    original_oom_adj: Option<i32>,
    cap_scope: Option<String>,
    /// The cgroup the capped pid was moved OUT of when the cap was applied, so
    /// a later run can put it back before stopping the transient unit. Absent
    /// only in a ledger written before this field existed (audit 2026-10-01).
    #[serde(default)]
    cap_orig_cgroup: Option<String>,
    identity: ProcessIdentity,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct MitigationLedger {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    boot_id: Option<String>,
    records: Vec<MitigationRecord>,
    #[serde(default)]
    oom_pending: Vec<(i32, OomPendingDescendant)>,
    #[serde(default)]
    oom_known: Vec<(i32, Vec<(i32, u64)>)>,
}

/// Where the ledger lives. `~/.dracon` is a REQUIRED ReadWritePaths entry in the
/// shipped unit, so a missing state root fails the unit loudly at start rather
/// than silently skipping persistence.
fn mitigation_ledger_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join(".dracon/guard-mitigations.json")
}

/// Write the currently-applied mitigations to disk, but only when they changed
/// (an unchanged pass must not rewrite the file every interval).
fn persist_mitigations(state: &mut GuardRuntimeState) -> bool {
    if !state.persist_mitigations {
        return true;
    }
    persist_mitigations_to(state, &mitigation_ledger_path())
}

/// Write the ledger to an explicit path. Split out so the round trip is
/// testable without touching the real `$HOME` (audit 2026-10-01).
fn persist_mitigations_to(state: &mut GuardRuntimeState, path: &Path) -> bool {
    if state.mitigation_ledger_unavailable {
        return false;
    }
    let mut records: Vec<MitigationRecord> = Vec::new();
    for (pid, entry) in &state.memory_reniced_pids {
        records.push(MitigationRecord {
            pid: *pid,
            original_nice: Some(entry.original_nice),
            original_oom_adj: None,
            cap_scope: None,
            cap_orig_cgroup: None,
            identity: entry.identity.clone(),
        });
    }
    for (pid, (original_adj, identity)) in &state.oom_biased_pids {
        records.push(MitigationRecord {
            pid: *pid,
            original_nice: None,
            original_oom_adj: Some(*original_adj),
            cap_scope: None,
            cap_orig_cgroup: None,
            identity: identity.clone(),
        });
    }
    for (pid, (scope, orig_cgroup, identity)) in &state.capped_pids {
        records.push(MitigationRecord {
            pid: *pid,
            original_nice: None,
            original_oom_adj: None,
            cap_scope: Some(scope.clone()),
            cap_orig_cgroup: Some(orig_cgroup.clone()),
            identity: identity.clone(),
        });
    }
    records.sort_by_key(|r| r.pid);
    let mut oom_pending: Vec<_> = state
        .oom_pending_descendants
        .iter()
        .map(|((pid, _), pending)| (*pid, pending.clone()))
        .collect();
    oom_pending.sort_by_key(|(pid, pending)| (*pid, pending.identity.starttime));
    let mut oom_known: Vec<_> = state
        .oom_known_descendants
        .iter()
        .map(|(pid, known)| {
            let mut members: Vec<_> = known.iter().copied().collect();
            members.sort_unstable();
            (*pid, members)
        })
        .collect();
    oom_known.sort_by_key(|(pid, _)| *pid);
    let boot_id = match fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_owned(),
        _ => {
            eprintln!("boot identity unavailable; refusing process mitigation persistence");
            return false;
        }
    };
    let ledger = MitigationLedger {
        version: 1,
        boot_id: Some(boot_id),
        records,
        oom_pending,
        oom_known,
    };
    let encoded = match serde_json::to_vec(&ledger) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("⚠️ could not serialise the mitigation ledger: {e}");
            return false;
        }
    };
    if state.last_persisted_mitigations.as_deref() == Some(encoded.as_slice()) {
        return true;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!(
                "⚠️ could not create {} for the mitigation ledger: {e}",
                parent.display()
            );
            return false;
        }
    }
    // Write-ahead means durable before mutation, not just atomic rename.
    let persist = || -> std::io::Result<()> {
        use std::io::Write;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(&encoded)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        fs::File::open(parent)?.sync_all()
    };
    if let Err(e) = persist() {
        eprintln!(
            "⚠️ could not persist the mitigation ledger to {}: {e}",
            path.display()
        );
        return false;
    }
    state.last_persisted_mitigations = Some(encoded);
    true
}

/// Reload mitigations applied by a previous run and put them back into the live
/// state maps, so the EXISTING restore path (identity checks, bounded defers,
/// SIGHUP drain) handles them with no new restore logic of its own.
///
/// Missing state is fresh state. Unreadable/corrupt state is NOT empty:
/// preserve its bytes and disable new mutations until it can safely be loaded.
fn hydrate_mitigations_from_disk(state: &mut GuardRuntimeState) {
    hydrate_mitigations_from(&mitigation_ledger_path(), state);
}

/// Adopt the mitigations recorded at `path`. Split out for the same reason as
/// `persist_mitigations_to`.
fn hydrate_mitigations_from(path: &Path, state: &mut GuardRuntimeState) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "could not read mitigation ledger {}: {e}; new process mitigations disabled",
                path.display()
            );
            state.mitigation_ledger_unavailable = true;
            return;
        }
    };
    if text.trim().is_empty() {
        state.mitigation_ledger_unavailable = true;
        eprintln!(
            "mitigation ledger {} is empty/truncated; refusing replacement",
            path.display()
        );
        return;
    }
    let ledger: MitigationLedger = match serde_json::from_str(&text) {
        Ok(ledger) => ledger,
        Err(e) => {
            eprintln!("mitigation ledger {} is unreadable ({e}); preserving it and disabling new process mitigations",path.display());
            state.mitigation_ledger_unavailable = true;
            return;
        }
    };
    if ledger.version > 1 {
        state.mitigation_ledger_unavailable = true;
        eprintln!(
            "unsupported mitigation ledger version {}; preserving original",
            ledger.version
        );
        return;
    }
    let current_boot = match fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_owned(),
        _ => {
            state.mitigation_ledger_unavailable = true;
            return;
        }
    };
    let has_obligations = !ledger.records.is_empty() || !ledger.oom_pending.is_empty();
    let legacy_unverified = has_obligations && ledger.boot_id.is_none();
    if legacy_unverified {
        eprintln!("legacy mitigation ledger has obligations without boot provenance; retaining compatibility records read-only for explicit recovery");
    }
    if ledger
        .boot_id
        .as_ref()
        .is_some_and(|id| id != &current_boot)
    {
        // Incarnations cannot cross boots. Preserve the historical bytes, but
        // never restore a previous boot's values into current PID matches.
        let archive = || -> std::io::Result<()> {
            use std::io::Write;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let mut copy = tempfile::Builder::new()
                .prefix("guard-mitigations-previous-boot-")
                .suffix(".json")
                .tempfile_in(parent)?;
            copy.write_all(text.as_bytes())?;
            copy.as_file().sync_all()?;
            copy.keep().map_err(|e| e.error)?;
            fs::File::open(parent)?.sync_all()
        };
        state.mitigation_ledger_unavailable = archive().is_err();
        return;
    }
    // Legacy records remain inspectable, but may neither be applied nor
    // persisted until their boot provenance is independently established.
    state.mitigation_ledger_unavailable = legacy_unverified;
    for (pid, pending) in ledger.oom_pending {
        if !matches!(
            process_identity_status(Path::new("/proc"), pid, &pending.identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        ) {
            state
                .oom_pending_descendants
                .insert((pid, pending.identity.starttime), pending);
        }
    }
    for (pid, known) in ledger.oom_known {
        state
            .oom_known_descendants
            .insert(pid, known.into_iter().collect());
    }
    let mut adopted = 0usize;
    for record in ledger.records {
        // Never adopt a recycled PID: the starttime is what proves it is the
        // same process the previous run adjusted.
        let status = process_identity_status(Path::new("/proc"), record.pid, &record.identity);
        // Unavailable is NOT gone. Retain for later identity-safe recovery.
        if matches!(
            status,
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        ) {
            continue;
        }
        if let Some(original) = record.original_nice {
            state.memory_reniced_pids.insert(
                record.pid,
                MemoryReniceState {
                    original_nice: original,
                    applied_nice: original + 5,
                    identity: record.identity.clone(),
                },
            );
        }
        if let Some(original) = record.original_oom_adj {
            state
                .oom_biased_pids
                .insert(record.pid, (original, record.identity.clone()));
        }
        if let Some(scope) = record.cap_scope {
            state.capped_pids.insert(
                record.pid,
                (
                    scope,
                    record.cap_orig_cgroup.unwrap_or_default(),
                    record.identity.clone(),
                ),
            );
        }
        adopted += 1;
    }
    if adopted > 0 {
        eprintln!(
            "🛡️ adopted {adopted} mitigation(s) left applied by a previous run — restoring them now"
        );
    }
}

fn reap_policy_from_guard(guard: &GuardPolicy) -> ReapPolicy {
    ReapPolicy {
        min_idle_hours: guard.reap_report_min_idle_hours,
        max_cpu_seconds: guard.reap_report_max_cpu_seconds,
        signatures: sorted_signatures(&guard.reap_report_signatures),
        exempt_names: sorted_signatures(&guard.process_exempt_names),
        forgotten_footprint_mb: guard.forgotten_orphan_footprint_mb,
        forgotten_tree_footprint_mb: guard.forgotten_orphan_tree_footprint_mb,
    }
}

/// Scan for forgotten heavy orphan TREES: heavy, idle, orphaned, and
/// provably a test job.
///
/// ADDED 2026-10-22 as a per-process scan; CHANGED 2026-10-23 to weigh a
/// whole tree, because the leak that cost 8.6 GB was 792 processes of
/// ~3 MiB each and no single one of them ever crossed a per-process
/// floor. Never signals anything: this is the report half. The ending
/// half is `reap_forgotten_orphan_trees`, and it runs only under memory
/// pressure.
fn forgotten_scan(guard: &GuardPolicy) -> ForgottenTreeScan {
    let proc_root = Path::new("/proc");
    let Some(boot_time) = read_boot_time(proc_root) else {
        return ForgottenTreeScan::default();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(boot_time);
    let policy = reap_policy_from_guard(guard);
    scan_forgotten_orphan_trees(
        proc_root,
        &policy,
        boot_time,
        now,
        PROC_TICKS_PER_SEC,
        guard.forgotten_orphan_footprint_mb,
        guard.forgotten_orphan_tree_footprint_mb,
    )
}

/// Report abandoned dev/test processes. Never signals, kills, or stops
/// anything -- see `reap.rs`. Returns empty on a host where procfs cannot
/// be read, so a missing clock degrades the report instead of failing the
/// pass.
fn reap_candidates(guard: &GuardPolicy) -> Vec<ReapCandidate> {
    let proc_root = Path::new("/proc");
    let Some(boot_time) = read_boot_time(proc_root) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(boot_time);
    scan_reap_candidates(
        proc_root,
        &reap_policy_from_guard(guard),
        boot_time,
        now,
        PROC_TICKS_PER_SEC,
    )
}

fn zombie_details(state: &mut GuardRuntimeState) -> Vec<ZombieInfo> {
    let now = Instant::now();
    let mut zombies = Vec::new();
    // PIDs recycle: key first-seen by (pid, starttime) so a new process
    // reusing a dead zombie's PID does not inherit its age.
    let mut seen: HashSet<(i32, u64)> = HashSet::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let Ok(pid) = pid_str.to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, comm, ppid, starttime)) = parse_proc_stat_zombie(&stat) else {
                continue;
            };
            let key = (pid, starttime);
            seen.insert(key);
            let first_seen = *state.zombies_since.entry(key).or_insert(now);
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
    state.zombies_since.retain(|key, _| seen.contains(key));
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
        if !marker.exists() {
            // The marker is already gone — the freeze watchdog auto-clears a
            // stuck marker, and an operator can run `dracon-sync resume`
            // directly. Without this resync the in-memory flag stayed true
            // forever, so every later pass re-entered this branch to log a
            // remove failure for a file that no longer exists.
            *sync_frozen = false;
            emit_event(&DraconEvent::new(
                "system",
                EventSeverity::Info,
                "disk/unfreeze",
                format!(
                    "sync freeze marker was already cleared; resynced at {}%",
                    used
                ),
            ));
        } else if !guard_owns_sync_freeze_marker(&marker) {
            // Present but not ours: an operator (or `dracon-sync pause`)
            // wrote it. Never remove or rewrite another owner's marker, and
            // do not claim the freeze was lifted either.
            eprintln!(
                "sync freeze marker {} is not ours; leaving it in place",
                marker.display()
            );
        } else if let Err(e) = fs::remove_file(&marker) {
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

/// Collect the set of paths under any of `roots` that are currently held
/// open by a running process (`/proc/*/fd` readlink targets,
/// `/proc/*/cwd`, and `/proc/*/exe`). Bounded by the process and
/// descriptor tables visible in procfs; entries in this set are skipped
/// by tmp cleanup even when old, because an open file, a process working
/// directory, or a running executable may still be in use.
async fn collect_open_paths_under_from(
    proc_root: &Path,
    roots: &[PathBuf],
) -> Option<std::collections::HashSet<PathBuf>> {
    // FIXED 2026-10-03 (audit R3-L31): `None` when the scan itself
    // fails (unreadable `proc_root`, pid-iteration error) — the caller
    // must skip the pass, because an empty set would silently disable
    // open-file protection. Per-process/per-fd read failures stay
    // skips (other users' fd tables are routinely unreadable to this
    // unprivileged service; failing the pass on those would disable
    // tmp cleanup forever).
    let mut open = std::collections::HashSet::new();
    let mut proc_rd = match tokio::fs::read_dir(proc_root).await {
        Ok(rd) => rd,
        Err(_) => return None,
    };
    loop {
        let pid_entry = match proc_rd.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => return None,
        };
        let name = pid_entry.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let process_dir = pid_entry.path();
        let fd_dir = process_dir.join("fd");
        if let Ok(mut fd_rd) = tokio::fs::read_dir(&fd_dir).await {
            loop {
                match fd_rd.next_entry().await {
                    Ok(Some(fd_entry)) => {
                        if let Ok(target) = tokio::fs::read_link(fd_entry.path()).await {
                            let target: PathBuf = target;
                            if roots.iter().any(|r| target.starts_with(r)) {
                                // Remember the deepest ancestor we saw so cleanup can
                                // check "is any prefix of this entry open" cheaply.
                                open.insert(target);
                            }
                        }
                    }
                    Ok(None) => break,
                    // FIXED 2026-10-03 (audit R4-SYS-11): one unreadable
                    // fd entry (an fd closed mid-iteration — TOCTOU on a
                    // live process) must skip THAT fd, not abandon the
                    // rest of the table. The old `while let Ok(Some(..))`
                    // exited the loop on Err, under-counting that
                    // process's open files — broader than the documented
                    // "per-fd read failures stay skips".
                    Err(_) => continue,
                }
            }
        }

        // A process cwd is a directory reference, not an open fd entry. It
        // must be collected even when the fd table cannot be read, otherwise
        // cleanup can recursively remove an old directory that a live process
        // is still using as its working directory.
        if let Ok(target) = tokio::fs::read_link(process_dir.join("cwd")).await {
            let target: PathBuf = target;
            if roots.iter().any(|r| target.starts_with(r)) {
                open.insert(target);
            }
        }

        // FIXED 2026-10-03 (audit R4-SYS-12): same for the running
        // executable — it is not an fd entry either, so without this
        // a stale binary executing from /tmp is unlinked while
        // running (the process survives on the unlinked inode but
        // the path is gone). One readlink per pid, same cost class
        // as cwd; failures stay per-process skips like cwd.
        if let Ok(target) = tokio::fs::read_link(process_dir.join("exe")).await {
            let target: PathBuf = target;
            if roots.iter().any(|r| target.starts_with(r)) {
                open.insert(target);
            }
        }
    }
    Some(open)
}

/// True when any entry at or under `dir` (including `dir` itself) is
/// newer than `cutoff`. Symlinks are never followed. Short-circuits on
/// the first fresh entry; a fully stale tree costs one walk — the same
/// order as the size accounting that follows for deletion candidates.
async fn tree_has_fresh_content(dir: &Path, cutoff: SystemTime) -> bool {
    // FIXED 2026-10-03 (audit R3-L22): fail CLOSED — any walk error
    // (metadata, readdir, entry iteration, mtime) answers "fresh"
    // (keep) instead of skipping toward a `false` (delete) verdict.
    // A transient I/O error or unreadable subtree must defer cleanup
    // to the next cycle, never authorize deletion of unknown content.
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = match tokio::fs::symlink_metadata(&current).await {
            Ok(m) => m,
            Err(_) => return true,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        match meta.modified() {
            Ok(mtime) if mtime > cutoff => return true,
            Ok(_) => {}
            Err(_) => return true,
        }
        if meta.is_dir() {
            let mut rd = match tokio::fs::read_dir(&current).await {
                Ok(rd) => rd,
                Err(_) => return true,
            };
            loop {
                match rd.next_entry().await {
                    Ok(Some(entry)) => stack.push(entry.path()),
                    Ok(None) => break,
                    Err(_) => return true,
                }
            }
        }
    }
    false
}

fn path_has_open_ancestor(path: &Path, open: &std::collections::HashSet<PathBuf>) -> bool {
    open.iter()
        .any(|o| o.starts_with(path) || path.starts_with(o))
}

fn resolve_safe_tmp_roots(roots: &[String]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for configured in roots {
        let configured = configured.trim();
        if configured.is_empty() {
            continue;
        }
        let requested = expand_tilde(configured);
        let safe_root = check_safe_tmp_root(&requested)
            .with_context(|| format!("invalid tmp_search_paths entry {:?}", configured))?;
        if !resolved.contains(&safe_root) {
            resolved.push(safe_root);
        }
    }
    Ok(resolved)
}

/// e2e-litter fast path for /tmp (ADDED 2026-10-12, obvious-fat increment).
///
/// Machine-generated test-copy namespaces (`reconcile-*`, `*-shield-*`,
/// `cw-browser-test-*`, ...) are never user documents, so they are judged
/// against a SHORTER age gate (`e2e_min_age_hours`, default 6) than the
/// general tmp gate. The 2026-10-11 incident left ~29 GiB of reconcile-e2e
/// copies 20h old — under the general 24h gate the guard would never touch
/// them before the disk went critical.
///
/// The fast path owns ONLY the age window the general pass skips: entries
/// whose whole tree is older than `general_cutoff` are left for the general
/// pass, so dry-run accounting can never double-count an entry. Every other
/// protection is identical to the general pass (open fd / cwd / exe scan,
/// symlink skip, protected paths, fail-closed tree walk). Deletion is DIRECT,
/// never stash: /tmp is already disposable, and copying tens of GiB to
/// stash just to delete it later defeats the purpose.
///
/// Returns `(reclaimed_bytes, cleaned, denied)` where `denied` holds entries
/// that could not be removed (typically root-owned leftovers needing sudo).
fn parse_tmp_e2e_patterns(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Minimal `*` glob over a tmp entry basename: exact, `prefix*`, `*suffix`,
/// `*infix*`, and bare `*`. Case-sensitive; tmp litter names are
/// machine-generated lowercase. No dependency, no regex — the pattern list
/// is operator config, not user input under test.
pub(crate) fn tmp_e2e_match(file_name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        let p = p.trim();
        if p.is_empty() {
            return false;
        }
        if p == "*" {
            return true;
        }
        let starts_star = p.starts_with('*');
        let ends_star = p.ends_with('*');
        let core = p.trim_matches('*');
        match (starts_star, ends_star) {
            (false, false) => file_name == p,
            (true, false) => file_name.ends_with(core),
            (false, true) => file_name.starts_with(core),
            (true, true) => file_name.contains(core),
        }
    })
}

/// Stash-move budget in bytes. 0 = unlimited (`u64::MAX`, never hit by
/// a saturating add). Pure so the boundary is unit-testable.
pub(crate) fn stash_budget_bytes(max_mb: u64) -> u64 {
    if max_mb == 0 {
        u64::MAX
    } else {
        max_mb.saturating_mul(1024).saturating_mul(1024)
    }
}

/// True when moving `entry_bytes` would exceed the per-pass stash
/// budget. Pure boundary helper: exactly-at-budget is allowed, one byte
/// over defers.
pub(crate) fn stash_over_budget(
    moved_bytes: u64,
    entry_bytes: u64,
    budget_bytes: u64,
) -> bool {
    moved_bytes.saturating_add(entry_bytes) > budget_bytes
}

/// Cutoff `SystemTime` for an age gate expressed in hours. Saturating: an
/// absurd value collapses to the epoch ("delete nothing") instead of
/// panicking — same hardening as the inline computation in
/// `clean_tmp_paths_with_proc`.
fn tmp_age_cutoff(min_age_hours: u64) -> Result<SystemTime> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let age = Duration::from_secs(min_age_hours.saturating_mul(3_600));
    Ok(UNIX_EPOCH + now.checked_sub(age).unwrap_or(Duration::ZERO))
}

async fn clean_tmp_e2e_fast_path(
    apply: bool,
    roots: &[String],
    general_cutoff: SystemTime,
    e2e_patterns: &[String],
    e2e_min_age_hours: u64,
    bypass_age_gate: bool,
    protected_paths: &[String],
) -> Result<(u64, Vec<String>, Vec<String>)> {
    let live_sockets = live_unix_socket_paths();
    clean_tmp_e2e_fast_path_with_proc(
        apply,
        roots,
        general_cutoff,
        e2e_patterns,
        e2e_min_age_hours,
        bypass_age_gate,
        protected_paths,
        Path::new("/proc"),
        &live_sockets,
    )
    .await
}

async fn clean_tmp_e2e_fast_path_with_proc(
    apply: bool,
    roots: &[String],
    general_cutoff: SystemTime,
    e2e_patterns: &[String],
    e2e_min_age_hours: u64,
    bypass_age_gate: bool,
    protected_paths: &[String],
    proc_root: &Path,
    live_sockets: &HashSet<PathBuf>,
) -> Result<(u64, Vec<String>, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();
    let mut denied: Vec<String> = Vec::new();
    if roots.is_empty() || e2e_patterns.is_empty() || e2e_min_age_hours == 0 {
        return Ok((0, cleaned, denied));
    }
    let root_paths = resolve_safe_tmp_roots(roots)?;
    if root_paths.is_empty() {
        return Ok((0, cleaned, denied));
    }
    let open_paths = match collect_open_paths_under_from(proc_root, &root_paths).await {
        Some(set) => set,
        None => anyhow::bail!(
            "open-file scan failed for {} — refusing tmp fast-path blind (cannot verify nothing is held open)",
            proc_root.display()
        ),
    };
    // At the critical tier the e2e age gate is lifted (mirrors the rust-target
    // critical bypass: test copies are regeneratable, a full disk is not).
    // Open/cwd/exe + symlink + protected checks below still apply, so a copy
    // a live test run is writing to is never touched.
    let fast_cutoff = if bypass_age_gate {
        SystemTime::now()
    } else {
        tmp_age_cutoff(e2e_min_age_hours)?
    };
    for root in &root_paths {
        let mut rd = match tokio::fs::read_dir(root).await {
            Ok(rd) => rd,
            Err(e) => {
                eprintln!(
                    "⚠️ Tmp fast-path: cannot list {} — skipping ({e})",
                    root.display()
                );
                continue;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            match entry.file_type().await {
                Ok(t) if t.is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !tmp_e2e_match(&name, e2e_patterns) {
                continue;
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                _ => continue,
            };
            // Exclusive window with the general pass: a tree entirely older
            // than the general cutoff is a general-pass candidate — leave it
            // so dry-run accounting never counts an entry twice.
            if meta.is_dir() {
                if !tree_has_fresh_content(&path, general_cutoff).await {
                    continue;
                }
                if tree_has_fresh_content(&path, fast_cutoff).await {
                    continue;
                }
            } else {
                let modified = match meta.modified() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if modified <= general_cutoff {
                    continue;
                }
                if modified > fast_cutoff {
                    continue;
                }
            }
            if path_has_open_ancestor(&path, &open_paths) {
                continue;
            }
            // A bound unix socket is an anonymous inode in /proc/<pid>/fd, so
            // the open-path scan above cannot see it. The socket PATH is the
            // only handle there is — deleting it strands a live daemon
            // (codex app-server, 2026-10-21).
            if path_holds_live_unix_socket(&path, live_sockets) {
                eprintln!(
                    "🛡️ keeping tmp entry {}: holds a live unix socket",
                    path.display()
                );
                continue;
            }
            if let Err(e) = check_safe_to_delete_tmp_entry(&path, root, protected_paths) {
                eprintln!("🛡️ keeping protected tmp entry {}: {}", path.display(), e);
                continue;
            }
            let entry_size = if meta.is_dir() {
                get_dir_size(&path).await.unwrap_or(0)
            } else {
                meta.len()
            };
            if !apply {
                reclaimed += entry_size;
                cleaned.push(format!(
                    "{} ({}, dry-run)",
                    path.display(),
                    human_bytes(entry_size)
                ));
                continue;
            }
            let removed = if meta.is_dir() {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            match removed {
                Ok(()) => {
                    reclaimed += entry_size;
                    cleaned.push(format!("{} ({})", path.display(), human_bytes(entry_size)));
                }
                Err(e) => {
                    // Typically root-owned test leftovers (e.g. docker-spawned
                    // build outputs) the unprivileged guard cannot unlink.
                    eprintln!("⚠️ failed to remove tmp entry {}: {}", path.display(), e);
                    denied.push(path.display().to_string());
                }
            }
        }
    }
    Ok((reclaimed, cleaned, denied))
}

/// The age a floor expresses, saturating instead of overflowing.
///
/// CHANGED 2026-10-22: shared by the general tmp floor and the size-aware
/// one, so both saturate the same way (audit F41's underflow hardening:
/// an absurd floor must mean "delete nothing", never a panic).
fn age_of(hours: u64) -> Duration {
    Duration::from_secs(hours.saturating_mul(3_600))
}

/// What one tmp cleanup pass found.
#[derive(Debug, Default)]
pub(crate) struct TmpCleanupOutcome {
    /// Bytes removed — or that would be removed, in dry-run.
    pub(crate) reclaimed: u64,
    /// Per-entry notes for the report.
    pub(crate) cleaned: Vec<String>,
    /// Entries that could not be removed: they belong to another user.
    /// ADDED 2026-10-22: aggregated. The per-entry failure line fired once
    /// per root-owned entry per cycle (a dozen lines a cycle on this host),
    /// which buried the lines that matter.
    pub(crate) denied: Vec<String>,
    /// Bytes held back ONLY by the age floor: stale by content, unheld and
    /// unprotected, but too young for the floor that applies to them.
    /// ADDED 2026-10-22: the disk alarms are percent-used and rapid fill;
    /// neither of them sees the scratch that is sitting there waiting for
    /// its floor to expire.
    pub(crate) age_blocked_bytes: u64,
    /// How many entries make up `age_blocked_bytes`.
    pub(crate) age_blocked_entries: usize,
}

/// Age-based top-level /tmp cleanup (ADDED 2026-08-25, v0.112.39).
/// Removes TOP-LEVEL entries of each root whose mtime is older than
/// `min_age_hours`, skipping symlinks, protected paths, and anything
/// currently held open by a process. Dry-run only measures.
///
/// ADDED 2026-10-22: a size-aware floor. An entry of at least
/// `large_entry_bytes` is judged against `large_min_age_hours` instead of the
/// general floor, so a bulk scratch clone does not have to wait the general
/// floor out. It is only consulted when `use_large_floor` is set (the
/// proactive/action tier), because deciding it costs a size measurement per
/// entry and that walk is only worth paying when the disk is filling.
/// Whether the scratch the age floor is holding back is worth a warning.
///
/// ADDED 2026-10-22: the disk alarms are percent-used and rapid fill;
/// neither of them sees the scratch that is sitting there waiting for its
/// floor. 0 disables the warning.
pub(crate) fn scratch_warning_threshold_exceeded(age_blocked_bytes: u64, warn_gib: u64) -> bool {
    warn_gib > 0 && age_blocked_bytes >= warn_gib.saturating_mul(1024 * 1024 * 1024)
}

/// The general-floor form: no size-aware floor.
///
/// Kept as the short form for the callers that want exactly the
/// pre-2026-10-22 behaviour (and for the tests, which exercise the general
/// floor on its own). The production path uses the full form below.
#[cfg(test)]
async fn clean_tmp_paths(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    protected_paths: &[String],
) -> Result<TmpCleanupOutcome> {
    clean_tmp_paths_with_floor(apply, roots, min_age_hours, 0, 0, false, protected_paths).await
}

/// The full form, with the size-aware floor.
///
/// ADDED 2026-10-22: see `clean_tmp_paths` for the size-aware floor's
/// rationale — a bulk scratch clone is judged against a short floor instead
/// of the general one, but only when the caller says the disk is filling.
async fn clean_tmp_paths_with_floor(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    large_entry_bytes: u64,
    large_min_age_hours: u64,
    use_large_floor: bool,
    protected_paths: &[String],
) -> Result<TmpCleanupOutcome> {
    let live_sockets = live_unix_socket_paths();
    clean_tmp_paths_with_proc(
        apply,
        roots,
        min_age_hours,
        large_entry_bytes,
        large_min_age_hours,
        use_large_floor,
        protected_paths,
        Path::new("/proc"),
        &live_sockets,
    )
    .await
}

async fn clean_tmp_paths_with_proc(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    large_entry_bytes: u64,
    large_min_age_hours: u64,
    use_large_floor: bool,
    protected_paths: &[String],
    proc_root: &Path,
    live_sockets: &HashSet<PathBuf>,
) -> Result<TmpCleanupOutcome> {
    let mut out = TmpCleanupOutcome::default();
    if roots.is_empty() || min_age_hours == 0 {
        return Ok(out);
    }
    let root_paths = resolve_safe_tmp_roots(roots)?;
    if root_paths.is_empty() {
        return Ok(out);
    }
    let open_paths = match collect_open_paths_under_from(proc_root, &root_paths).await {
        Some(set) => set,
        // R3-L31: without the open-file scan, cleanup would delete
        // files held open by live processes with no warning. Skip the
        // pass LOUD in both modes (dry-run would otherwise over-report
        // reclaimable); the caller prints the error.
        None => anyhow::bail!(
            "open-file scan failed for {} — refusing tmp cleanup blind (cannot verify nothing is held open)",
            proc_root.display()
        ),
    };
    // CHANGED 2026-09-09 (audit F41): same underflow hardening as the
    // trash cutoff above — absurd min_age_hours saturates to "delete
    // nothing" instead of panicking.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let cutoff = UNIX_EPOCH
        + now
            .checked_sub(age_of(min_age_hours))
            .unwrap_or(Duration::ZERO);
    // The size-aware floor. A SHORTER age means a LATER cutoff, and a later
    // cutoff judges MORE content old enough to delete — so the short floor
    // is the permissive one for deletion, and its freshness check is also
    // the cheaper walk (it stops at the first recent entry).
    let large_cutoff = UNIX_EPOCH
        + now
            .checked_sub(age_of(large_min_age_hours))
            .unwrap_or(Duration::ZERO);
    let large_floor_active = use_large_floor
        && large_min_age_hours > 0
        && large_entry_bytes > 0
        && large_cutoff > cutoff;
    for root in &root_paths {
        let mut rd = match tokio::fs::read_dir(root).await {
            Ok(rd) => rd,
            // FIXED 2026-10-03 (audit R4-SYS-13): skipping stays
            // deletion-safe, but silence under-reports dry-run —
            // name the skipped root like the proc-scan refusal does.
            Err(e) => {
                eprintln!("⚠️ Tmp: cannot list {} — skipping ({e})", root.display());
                continue;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            // Never follow or remove symlinks in tmp roots.
            // CHANGED 2026-09-09 (audit F40): `entry.metadata()`
            // FOLLOWS symlinks, so `is_symlink()` was never true and
            // every symlink fell through to the target-typed path.
            // `file_type()` does not follow — skip links up front.
            // (The downstream guard check also refuses symlinks, so this
            // was defense-in-depth theater plus scary log spam, not a
            // live deletion hole.)
            match entry.file_type().await {
                Ok(t) if t.is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                _ => continue,
            };
            let modified = match meta.modified() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Files judge by their own mtime. Directories cannot: dir
            // mtime only bumps on direct-child changes, so a stale-looking
            // top dir can hide freshly written nested files. Judge the
            // tree by its newest entry instead.
            let fresh = if meta.is_dir() {
                tree_has_fresh_content(&path, cutoff).await
            } else {
                modified > cutoff
            };
            if fresh {
                // Fresh content against the GENERAL floor. When the
                // size-aware floor is active, a large entry may still be
                // reclaimable: measure it and re-check against the short
                // floor. Measuring costs a walk, which is why this only
                // happens in the proactive/action tier.
                if large_floor_active {
                    let size = tmp_entry_size(&path, &meta).await;
                    if size >= large_entry_bytes {
                        let fresh_short = if meta.is_dir() {
                            tree_has_fresh_content(&path, large_cutoff).await
                        } else {
                            modified > large_cutoff
                        };
                        if !fresh_short {
                            remove_tmp_entry(&path, &meta, size, apply, &mut out).await;
                            continue;
                        }
                    }
                    // Held back by the age rule: its content is fresh.
                    // Counted whatever its size — the long tail of SMALL
                    // entries is where the incident's 62.7 GiB actually sat.
                    out.age_blocked_bytes += size;
                    out.age_blocked_entries += 1;
                }
                continue; // fresh content
            }
            if path_has_open_ancestor(&path, &open_paths) {
                continue; // held open by a live process
            }
            // A bound unix socket is an anonymous inode in /proc/<pid>/fd, so
            // the open-path scan above cannot see it. The socket PATH is the
            // only handle there is — deleting it strands a live daemon
            // (codex app-server, 2026-10-21).
            if path_holds_live_unix_socket(&path, live_sockets) {
                eprintln!(
                    "🛡️ keeping tmp entry {}: holds a live unix socket",
                    path.display()
                );
                continue;
            }
            if let Err(e) = check_safe_to_delete_tmp_entry(&path, root, protected_paths) {
                eprintln!("🛡️ keeping protected tmp entry {}: {}", path.display(), e);
                continue;
            }
            let entry_size = tmp_entry_size(&path, &meta).await;
            if !apply {
                out.reclaimed += entry_size;
                out.cleaned.push(format!(
                    "{} ({}, dry-run)",
                    path.display(),
                    human_bytes(entry_size)
                ));
                continue;
            }
            remove_tmp_entry(&path, &meta, entry_size, apply, &mut out).await;
        }
    }
    Ok(out)
}

/// The size of a tmp entry: a directory is measured by its tree, a file by
/// its own length.
async fn tmp_entry_size(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if meta.is_dir() {
        get_dir_size(path).await.unwrap_or(0)
    } else {
        meta.len()
    }
}

/// Remove one tmp entry, recording the outcome.
///
/// ADDED 2026-10-22: a removal that fails because the entry belongs to
/// another user is aggregated instead of logged per entry per cycle, and an
/// entry that is held back only by the age floor is counted so the guard can
/// report the scratch it is not reclaiming yet.
async fn remove_tmp_entry(
    path: &Path,
    meta: &std::fs::Metadata,
    entry_size: u64,
    apply: bool,
    out: &mut TmpCleanupOutcome,
) {
    if !apply {
        out.reclaimed += entry_size;
        out.cleaned.push(format!(
            "{} ({}, dry-run)",
            path.display(),
            human_bytes(entry_size)
        ));
        return;
    }
    let removed = if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    match removed {
        Ok(()) => {
            out.reclaimed += entry_size;
            out.cleaned
                .push(format!("{} ({})", path.display(), human_bytes(entry_size)));
        }
        Err(e) => {
            out.denied.push(format!("{}: {}", path.display(), e));
        }
    }
}

/// Reclaim the profile directories of browser trees the guard has just ended.
///
/// ADDED 2026-10-24. A leaked browser leaves TWO halves: the processes and
/// the profile directory they owned. On this host that profile is ~850 MiB —
/// seven of them held 5.9 GB after five hours, and the CI leaks one every
/// few minutes, so the residue grows at roughly 1.2 GB/hour while the disk
/// sits at 87%. Ending the processes promptly but leaving the profile to
/// wait out the tmp age floor (6 h here) reclaims only half of the leak.
///
/// The guard may reclaim it NOW because it has just PROVED the browser is
/// dead and abandoned. That proof is re-checked here, at the moment of
/// removal, because it was earned a moment ago and the world moves:
///
/// * the tree was ENDED and every member of it is gone — a tree that partly
///   survived is not proof that its profile is abandoned;
/// * the profile lives under one of the guard's VALIDATED TMP ROOTS. A
///   profile outside them — the operator's real `~/.config/google-chrome`,
///   say — is never touched, however the tree that named it ended;
/// * no live process names it in its command line: a browser that started
///   on the same path after the old one died;
/// * nothing holds a file open under it, and no live unix socket lives in
///   it (a bound socket is an anonymous inode, invisible to the open-file
///   scan — see the codex app-server incident, 2026-10-21);
/// * it passes the guard's normal protected-path and symlink checks.
///
/// Every check that CAN fail closed does. If the open-file scan cannot be
/// run, nothing is removed: an empty set would silently disable the
/// protection, which is the failure mode that cost the codex daemon its
/// control socket.
async fn reclaim_reaped_browser_profiles(
    proc_root: &Path,
    guard: &GuardPolicy,
    dirs: &[PathBuf],
) -> Vec<String> {
    let mut reclaimed = Vec::new();
    if !guard.reclaim_reaped_browser_profiles || dirs.is_empty() {
        return reclaimed;
    }
    let roots: Vec<String> = guard
        .tmp_search_paths
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let root_paths = match resolve_safe_tmp_roots(&roots) {
        Ok(r) if !r.is_empty() => r,
        _ => {
            eprintln!(
                "⚠️ browser-profile reclaim: no validated tmp root — reclaiming nothing"
            );
            return reclaimed;
        }
    };
    let open = match collect_open_paths_under_from(proc_root, &root_paths).await {
        Some(set) => set,
        None => {
            eprintln!(
                "⚠️ browser-profile reclaim: open-file scan failed — reclaiming nothing (cannot prove nothing is held open)"
            );
            return reclaimed;
        }
    };
    let live_sockets = live_unix_socket_paths();
    for dir in dirs {
        // It must be a real directory: a symlink is never followed, because
        // the target might be anywhere.
        let Ok(meta) = std::fs::symlink_metadata(dir) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        // It must be under a validated tmp root — this is what keeps the
        // operator's real browser profile out of reach.
        let Some(root) = reclaim_root_for(dir, &root_paths) else {
            eprintln!(
                "🛡️ keeping browser profile {}: outside the validated tmp roots",
                dir.display()
            );
            continue;
        };
        if path_has_open_ancestor(dir, &open) {
            eprintln!(
                "🛡️ keeping browser profile {}: a live process holds it open",
                dir.display()
            );
            continue;
        }
        if path_holds_live_unix_socket(dir, &live_sockets) {
            eprintln!(
                "🛡️ keeping browser profile {}: holds a live unix socket",
                dir.display()
            );
            continue;
        }
        if path_named_by_live_cmdline(proc_root, dir).await {
            eprintln!(
                "🛡️ keeping browser profile {}: a live process still names it",
                dir.display()
            );
            continue;
        }
        if let Err(e) = check_safe_to_delete_tmp_entry(dir, &root, &guard.protected_paths) {
            eprintln!(
                "🛡️ keeping browser profile {}: {}",
                dir.display(),
                e
            );
            continue;
        }
        let size = get_dir_size(dir).await.unwrap_or(0);
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => {
                eprintln!(
                    "🧹 reclaimed browser profile: {} ({})",
                    dir.display(),
                    human_bytes(size)
                );
                reclaimed.push(format!("{} ({})", dir.display(), human_bytes(size)));
            }
            Err(e) => {
                eprintln!(
                    "⚠️ failed to reclaim browser profile {}: {}",
                    dir.display(),
                    e
                );
            }
        }
    }
    reclaimed
}

/// The validated tmp root that contains `dir`, if any.
///
/// This is the check that keeps the operator's REAL browser profile out of
/// reach: `~/.config/google-chrome` is not under a tmp root, so a tree that
/// named it may be ended, but its profile is never removed.
///
/// The match is component-wise, so a sibling that merely shares a textual
/// prefix (`/tmp/profiles-old` against the root `/tmp/profiles`) does not
/// qualify.
fn reclaim_root_for(dir: &Path, root_paths: &[PathBuf]) -> Option<PathBuf> {
    root_paths
        .iter()
        .find(|root| dir.starts_with(root) && dir != *root)
        .cloned()
}

/// Whether any live process's COMMAND LINE names `path`.
///
/// The open-file scan cannot see this: a browser that has just started on a
/// profile path may not have a file open under it yet, and it is exactly
/// that moment when deleting the path would break it.
pub(crate) async fn path_named_by_live_cmdline(proc_root: &Path, path: &Path) -> bool {
    let needle = path.to_string_lossy().to_string();
    if needle.is_empty() {
        return false;
    }
    let mut rd = match tokio::fs::read_dir(proc_root).await {
        Ok(rd) => rd,
        Err(_) => return true,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = tokio::fs::read(proc_root.join(&*name).join("cmdline")).await else {
            continue;
        };
        // Chromium rewrites its argv into a NUL-free blob, so the text is
        // searched as-is rather than by field.
        let text = String::from_utf8_lossy(&raw);
        if text.contains(&needle) {
            return true;
        }
    }
    false
}

async fn run_auto_cleanup(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    used: u8,
    include_rust: bool,
) -> Result<()> {
    let apply = guard.auto_cleanup_apply;
    let mut total_reclaimed = 0u64;
    let mut all_cleaned: Vec<String> = Vec::new();

    // include_rust=false runs only the cheap non-rust kinds — used by the
    // proactive tier (>= proactive_cleanup_percent) where the expensive
    // ~/Dev-wide rust-target scan stays on its own cadence.
    if guard.auto_cleanup_rust && include_rust {
        // Critical tier lifts the lingering age gate (see
        // auto_cleanup_rust_targets): rebuildable beats unbootable.
        let bypass_age_gate = used >= guard.disk_critical_percent;
        if bypass_age_gate {
            eprintln!(
                "🚨 disk at {}% (critical) — rust target age gate lifted for this pass",
                used
            );
        }
        match auto_cleanup_rust_targets(guard, state, apply, bypass_age_gate).await {
            Ok(result) => {
                total_reclaimed += result.reclaimed_bytes;
                if apply {
                    for p in &result.cleaned_paths {
                        eprintln!("🧹 Rust: {}", p);
                    }
                }
                all_cleaned.extend(result.cleaned_paths);
            }
            Err(e) => eprintln!("⚠️ Rust target cleanup failed: {}", e),
        }
    }

    if guard.clean_trash {
        match empty_trash(
            apply,
            &guard.protected_paths,
            guard.trash_credential_guard,
            guard.trash_min_age_days,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Trash: {}", s)));
                if apply {
                    for c in &cleaned {
                        eprintln!("🗑️ {}", c);
                    }
                }
            }
            Err(e) => eprintln!("⚠️ Trash cleanup failed: {}", e),
        }
    }

    if guard.clean_tmp {
        let roots: Vec<String> = guard
            .tmp_search_paths
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // ADDED 2026-10-12 (obvious-fat fast path): e2e/test litter gets a
        // shorter age gate. Runs FIRST; it owns only the window the general
        // pass skips (newer than the general cutoff), so dry-run accounting
        // never double-counts. Honours the whole-pass disable (general age 0)
        // and its own 0 sentinel. Direct delete, never stash (/tmp is
        // already disposable).
        if guard.tmp_min_age_hours != 0 && guard.tmp_e2e_min_age_hours != 0 {
            let e2e_patterns = parse_tmp_e2e_patterns(&guard.tmp_e2e_patterns);
            // Critical tier lifts the e2e age gate (test copies are
            // regeneratable; a full disk is not) — mirrors the rust-target
            // critical bypass below.
            let bypass_e2e_gate = used >= guard.disk_critical_percent;
            if bypass_e2e_gate {
                eprintln!(
                    "🚨 disk at {}% (critical) — tmp e2e-litter age gate lifted for this pass",
                    used
                );
            }
            match tmp_age_cutoff(guard.tmp_min_age_hours) {
                Ok(general_cutoff) => {
                    match clean_tmp_e2e_fast_path(
                        apply,
                        &roots,
                        general_cutoff,
                        &e2e_patterns,
                        guard.tmp_e2e_min_age_hours,
                        bypass_e2e_gate,
                        &guard.protected_paths,
                    )
                    .await
                    {
                        Ok((bytes, cleaned, denied)) => {
                            total_reclaimed += bytes;
                            all_cleaned.extend(cleaned.iter().map(|s| format!("Tmp-e2e: {}", s)));
                            if apply && bytes > 0 {
                                eprintln!("🧹 Tmp-e2e: {}", human_bytes(bytes));
                            }
                            if !denied.is_empty() {
                                let sample: Vec<&str> =
                                    denied.iter().take(3).map(|s| s.as_str()).collect();
                                eprintln!(
                                    "🛡️ Tmp-e2e: {} entries need root to remove (e.g. {})",
                                    denied.len(),
                                    sample.join(", ")
                                );
                            }
                        }
                        Err(e) => eprintln!("⚠️ Tmp e2e fast-path failed: {}", e),
                    }
                }
                Err(e) => eprintln!("⚠️ Tmp e2e fast-path failed: {}", e),
            }
        }
        // ADDED 2026-10-22: the size-aware floor is a proactive/action-tier
        // behaviour. Deciding it costs a size measurement per entry, and
        // that walk is only worth paying when the disk is actually filling
        // — the general floor (and the e2e fast path) keep the cheap
        // normal cadence.
        let use_large_floor = used >= guard.proactive_cleanup_percent
            && guard.tmp_large_min_age_hours > 0
            && guard.tmp_large_entry_bytes > 0;
        match clean_tmp_paths_with_floor(
            apply,
            &roots,
            guard.tmp_min_age_hours,
            guard.tmp_large_entry_bytes,
            guard.tmp_large_min_age_hours,
            use_large_floor,
            &guard.protected_paths,
        )
        .await
        {
            Ok(outcome) => {
                total_reclaimed += outcome.reclaimed;
                all_cleaned.extend(outcome.cleaned.iter().map(|s| format!("Tmp: {}", s)));
                if apply && outcome.reclaimed > 0 {
                    eprintln!("🧹 Tmp: {}", human_bytes(outcome.reclaimed));
                }
                // ADDED 2026-10-22: one line per cycle, not one per entry.
                // On this host a dozen root-owned tmp entries failed every
                // cycle, and the per-entry lines buried the rest.
                if !outcome.denied.is_empty() {
                    let sample: Vec<&str> =
                        outcome.denied.iter().take(3).map(|s| s.as_str()).collect();
                    eprintln!(
                        "🛡️ Tmp: {} entries need another user to remove (e.g. {})",
                        outcome.denied.len(),
                        sample.join(", ")
                    );
                }
                // ADDED 2026-10-22: report the scratch the age floor is
                // holding back. The disk alarms are percent-used and rapid
                // fill; neither of them sees 144 GiB of deletable litter
                // waiting for its floor. The hours-until-full figure comes
                // from the same sustained fill rate the rapid-fill alarm
                // uses, so the two lines agree.
                if outcome.age_blocked_bytes > 0 {
                    // The hours-until-full figure comes from the same
                    // sustained fill rate the rapid-fill alarm uses, so the
                    // two lines agree. A disk that is not filling has no
                    // such figure — that is said plainly rather than
                    // omitted, because "the scratch is N GiB" without it
                    // reads like the disk is in trouble when it is not.
                    let eta = match disk_fill_rate_gbph(&state.disk_bytes_history) {
                        Some(gbph) if gbph > 0.0 => {
                            let hours = outcome.age_blocked_bytes as f64 / 1_073_741_824.0 / gbph;
                            format!("{hours:.1}")
                        }
                        _ => "never (the disk is not filling)".to_string(),
                    };
                    eprintln!(
                        "🧹 Tmp: {} held by the age floor across {} entries — hours until full: {}",
                        human_bytes(outcome.age_blocked_bytes),
                        outcome.age_blocked_entries,
                        eta
                    );
                    let warn_gib = guard.tmp_scratch_warn_gib;
                    if scratch_warning_threshold_exceeded(outcome.age_blocked_bytes, warn_gib) {
                        eprintln!(
                            "⚠️ Tmp: {} of scratch is held by the age floor (warn threshold {} GiB) — lower tmp_min_age_hours or enable tmp_large_min_age_hours",
                            human_bytes(outcome.age_blocked_bytes),
                            warn_gib
                        );
                    }
                }
            }
            Err(e) => eprintln!("⚠️ Tmp cleanup failed: {}", e),
        }
    }

    if guard.clean_nix_garbage {
        match clean_nix_garbage(
            guard.nix_keep_generations,
            apply,
            guard.cleanup_command_timeout_secs,
        )
        .await
        {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Nix: {}", s)));
                if apply {
                    for c in &cleaned {
                        eprintln!("📦 {}", c);
                    }
                }
            }
            Err(e) => eprintln!("⚠️ Nix cleanup failed: {}", e),
        }
    }

    // Audit M3 (2026-08-21): gated by clean_node_modules (default true)
    // for symmetry with every other cleanup kind; explicit `guard clean
    // --node-modules` remains available regardless of this flag.
    if guard.clean_node_modules {
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
            guard.clean_stash_first,
            &stash_root(guard),
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
        if apply {
            for c in &cleaned {
                eprintln!("📂 {}", c);
            }
        }
    }

    if guard.clean_package_caches {
        match clean_package_caches(true, true, true, true, apply).await {
            Ok((bytes, cleaned)) => {
                total_reclaimed += bytes;
                all_cleaned.extend(cleaned.iter().map(|s| format!("Cache: {}", s)));
                if apply {
                    for c in &cleaned {
                        eprintln!("💾 {}", c);
                    }
                }
            }
            Err(e) => eprintln!("⚠️ Package cache cleanup failed: {}", e),
        }
    }

    if guard.docker_prune && apply {
        // FIXED 2026-09-27 (audit F91): the `all` argument was a literal
        // `true`, so the daemon ran `docker system prune -f --all` — which
        // deletes EVERY unused image, not just dangling ones — on every
        // action/critical pass, with no knob to restrict it. F44 fixed the
        // two CLI call sites; this third one was missed. It now follows
        // the `docker_prune_all` policy knob (default false).
        match docker_prune(
            guard.auto_cleanup_apply,
            guard.docker_prune_all,
            guard.docker_prune_volumes,
            guard.cleanup_command_timeout_secs,
        )
        .await
        {
            Ok(bytes) => {
                total_reclaimed += bytes;
                if bytes > 0 {
                    eprintln!("🐳 Docker prune: {}", human_bytes(bytes));
                }
            }
            Err(e) => eprintln!("⚠️ Docker prune failed: {}", e),
        }
    }

    let cleanup_state = if all_cleaned.is_empty() {
        "ok"
    } else if apply {
        "applied"
    } else {
        "candidates"
    };
    let (previous, event_due) = report_state_transition(
        state,
        "auto-cleanup",
        cleanup_state,
        guard.report_repeat_secs,
    );
    if apply && total_reclaimed > 0 && previous.as_deref() != Some("applied") {
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
    } else if !apply && cleanup_state == "candidates" && event_due {
        eprintln!(
            "💡 disk at {}% — dry-run found {} cleanup candidate(s), estimated {} (no changes made)",
            used,
            all_cleaned.len(),
            human_bytes(total_reclaimed)
        );
    }

    Ok(())
}

async fn check_disk_state_change(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    used: u8,
    dstate: &str,
) {
    let previous = std::mem::replace(&mut state.last_disk_state, dstate.to_string());
    let transitioned = !previous.is_empty() && previous != dstate;
    // Do not notify merely because the service restarted while disk usage is
    // in the ordinary warning band. A critical initial state is actionable;
    // otherwise wait for a real transition.
    let initial_critical = previous.is_empty() && dstate == "critical";
    if transitioned || initial_critical {
        send_notification(
            guard,
            "Dracon System Guard",
            &format!("Disk pressure state changed to {} (used={}%)", dstate, used),
        )
        .await;
    }
}

async fn check_heavy_processes(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    samples: &[ProcSample],
) -> Result<Vec<GuardProcessAlert>> {
    let exempt = parse_kinds(&guard.process_exempt_names);
    let can_restore_nice = if guard.auto_renice || !state.reniced_pids.is_empty() {
        nice_restore_capability_available(state)
    } else {
        true
    };
    let mut current_heavy = HashSet::new();
    let mut current_report_keys = HashSet::new();
    let mut alerts = Vec::new();

    for p in samples {
        if exempt.contains(&p.command) {
            continue;
        }
        let heavy = p.cpu_percent >= guard.process_cpu_percent || p.rss_mb >= guard.process_rss_mb;
        if !heavy {
            continue;
        }
        let p = load_process_cmdline(p.clone());
        current_heavy.insert(p.pid);
        let report_key = format!("heavy-process-{}-{}", p.pid, p.starttime);
        current_report_keys.insert(report_key.clone());
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

        if !state.mitigation_ledger_unavailable && guard.auto_renice && can_restore_nice {
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
            let current_identity = process_sample_identity(&p);
            drop_stale_nice_adjustments(state, p.pid, &current_identity);
            let already_niced = state
                .reniced_pids
                .get(&p.pid)
                .map(|entry| entry.applied_nice);
            let nice_val =
                graduated_nice_value(p.cpu_percent, p.rss_mb, guard.renice_value, p.nice);
            if already_niced != Some(nice_val) {
                match renice_process(p.pid, nice_val).await {
                    Ok(()) => {
                        let original_nice = state
                            .reniced_pids
                            .get(&p.pid)
                            .map(|entry| entry.original_nice)
                            .or_else(|| {
                                state
                                    .memory_reniced_pids
                                    .get(&p.pid)
                                    .map(|entry| entry.original_nice)
                            })
                            .unwrap_or(p.nice);
                        state.reniced_pids.insert(
                            p.pid,
                            LegacyReniceState {
                                original_nice,
                                applied_nice: nice_val,
                                identity: current_identity,
                            },
                        );
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
            if state
                .reniced_pids
                .get(&p.pid)
                .map(|entry| entry.applied_nice)
                == Some(nice_val)
            {
                nice_applied = nice_val;
                action = format!("renice:{}", nice_val);
            }
        }

        let alert_state = if stuck { "stuck" } else { "heavy" };
        let (previous, _) =
            report_state_transition(state, &report_key, alert_state, guard.report_repeat_secs);
        if previous.as_deref() != Some(alert_state) {
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
    // Drop alert state when the process incarnation is no longer heavy so a
    // later recurrence can produce one fresh notification. Including
    // /proc starttime in the key prevents PID reuse from inheriting silence.
    state
        .report_states
        .retain(|key, _| !key.starts_with("heavy-process-") || current_report_keys.contains(key));

    // Un-renice recovery: processes that are no longer heavy. This is only
    // reached when the capability gate allowed the reversible action above;
    // otherwise retained state is deliberately left untouched for a future
    // run under a service with the required privilege.
    if can_restore_nice {
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
            let (original_nice, identity) = match state.reniced_pids.get(&pid) {
                Some(entry) => (entry.original_nice, entry.identity.clone()),
                None => continue,
            };
            let restore_nice = state
                .memory_reniced_pids
                .get(&pid)
                .filter(|entry| same_process_incarnation(&entry.identity, &identity))
                .map(|entry| entry.applied_nice)
                .unwrap_or(original_nice);
            match process_identity_status(Path::new("/proc"), pid, &identity) {
                ProcessIdentityStatus::Match => {
                    drop_stale_nice_adjustments(state, pid, &identity);
                }
                ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch => {
                    remove_legacy_renice(state, pid);
                    continue;
                }
                ProcessIdentityStatus::Unavailable => {
                    // Bounded defer; see the mem-unrenice path above.
                    let attempts = state
                        .legacy_identity_unavailable_attempts
                        .entry(pid)
                        .or_insert(0);
                    *attempts += 1;
                    if *attempts >= IDENTITY_UNAVAILABLE_RETRY_LIMIT {
                        eprintln!(
                            "⚠️ un-renice dropping pid={} after {} unavailable identity reads (will not retry)",
                            pid, *attempts
                        );
                        state.legacy_identity_unavailable_attempts.remove(&pid);
                        remove_legacy_renice(state, pid);
                    } else {
                        eprintln!(
                            "⚠️ un-renice deferred for pid={} — process identity unavailable (attempt {}/{})",
                            pid, *attempts, IDENTITY_UNAVAILABLE_RETRY_LIMIT
                        );
                    }
                    continue;
                }
            }
            state.legacy_identity_unavailable_attempts.remove(&pid);
            if let Err(e) = renice_process(pid, restore_nice).await {
                let attempts = state.legacy_unrenice_failures.entry(pid).or_insert(0);
                *attempts += 1;
                if *attempts >= UNRENICE_RETRY_LIMIT {
                    eprintln!(
                        "⚠️ un-renice dropping pid={} after {} failed renices: {} (will not retry)",
                        pid, *attempts, e
                    );
                    state.legacy_unrenice_failures.remove(&pid);
                    remove_legacy_renice(state, pid);
                } else {
                    eprintln!(
                        "⚠️ un-renice failed for pid={} (attempt {}/{}): {}",
                        pid, *attempts, UNRENICE_RETRY_LIMIT, e
                    );
                }
                continue;
            }
            eprintln!(
                "🔧 un-renice pid={} -> nice {} (pressure released)",
                pid, restore_nice
            );
            state.legacy_unrenice_failures.remove(&pid);
            remove_legacy_renice(state, pid);
        }
        state
            .cooled_since
            .retain(|pid, _| state.reniced_pids.contains_key(pid));
    }

    // Clean up only known-gone or PID-reused entries; retain identity read
    // failures for a later retry.
    state.reniced_pids.retain(|pid, entry| {
        !matches!(
            process_identity_status(Path::new("/proc"), *pid, &entry.identity),
            ProcessIdentityStatus::Gone | ProcessIdentityStatus::Mismatch
        )
    });

    // Summary feedback
    if !state.reniced_pids.is_empty() {
        let summary: Vec<String> = state
            .reniced_pids
            .iter()
            .map(|(pid, entry)| format!("pid={}:nice={}", pid, entry.applied_nice))
            .collect();
        eprintln!("🔧 reniced active: [{}]", summary.join(", "));
    }

    Ok(alerts)
}

/// Which tier of cleanup a background pass is running.
///
/// ADDED 2026-10-24: the cleanup moved off the guard's serial pass into
/// its own task, and the two tiers do different work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupMode {
    /// The disk is at action/critical: the full cleanup, rust included.
    Pressure,
    /// The proactive tier: the cheap kinds on their own cadence, plus the
    /// rust-target scan when it is armed.
    Proactive,
}

/// Start a disk cleanup in the BACKGROUND and return immediately.
///
/// ADDED 2026-10-24. The guard's protection loop — memory-pressure
/// mitigation, the heavy-process limiter, the forgotten-orphan tree reap
/// — used to wait behind this work, because it ran inline, ahead of them.
/// On a busy filesystem a single cleanup pass took eight minutes, and
/// during that time the guard applied no mitigation and reaped nothing
/// while the host was in critical memory pressure.
///
/// The task owns its own scratch runtime state: the live one belongs to
/// the protection loop, which is running concurrently, and the cleanup
/// only needs the disk-fill history for its ETA plus its own report-state
/// bookkeeping.
///
/// The cleanup's summary is handed back through `slot` and picked up by a
/// later pass.
fn spawn_background_cleanup(
    guard: &GuardPolicy,
    state: &GuardRuntimeState,
    used: u8,
    mode: CleanupMode,
    slot: Arc<tokio::sync::Mutex<CleanupSummary>>,
) {
    let guard = guard.clone();
    let mut scratch = GuardRuntimeState {
        disk_bytes_history: state.disk_bytes_history.clone(),
        ..GuardRuntimeState::default()
    };
    tokio::spawn(async move {
        let mut summary = CleanupSummary::default();
        // ADDED 2026-09-27 (space tiers Phase 2): relocate-first —
        // move cold bulk before deleting anything rebuildable.
        if guard.auto_relocate {
            match run_auto_relocate(&guard).await {
                Ok((moves, moved_bytes, candidates)) => {
                    summary.relocated_count = moves;
                    summary.relocated_bytes = moved_bytes;
                    summary.relocate_candidates = candidates;
                }
                Err(e) => eprintln!("⚠️ auto-relocate failed: {e:#}"),
            }
        }
        // Re-read pressure after moves so delete-based cleanup (and the
        // critical age-gate bypass) acts on post-move reality.
        let mut used = used;
        if summary.relocated_count > 0 {
            if let Ok(d) = disk_details_for(&guard.disk_mount_path).await {
                used = d.use_percent;
            }
        }
        let outcome = match mode {
            CleanupMode::Pressure => run_auto_cleanup(&guard, &mut scratch, used, true).await,
            CleanupMode::Proactive => async {
                if guard.auto_cleanup_rust {
                    run_proactive_cleanup(&guard, &mut scratch).await?;
                }
                run_auto_cleanup(&guard, &mut scratch, used, false).await
            }
            .await,
        };
        if let Err(e) = outcome {
            eprintln!("⚠️ background cleanup failed: {e:#}");
        }
        let mut slot = slot.lock().await;
        *slot = summary;
    });
}

fn auto_cleanup_due_at(state: &GuardRuntimeState, interval_secs: u64, now: Instant) -> bool {
    cooldown_due(state.last_auto_cleanup, interval_secs, now)
}

/// Whether a `interval_secs`-paced action last ran at `last` is due now.
///
/// The `.max(60)` floor keeps a policy of 0 (or a typo'd 1) from turning into
/// a retry loop every pass, and matches the floor applied to
/// `auto_cleanup_interval_secs` at load time. `checked_sub` is avoided
/// deliberately: `Instant - Duration` panics on underflow, and a policy large
/// enough to reach it must not kill the daemon.
fn cooldown_due(last: Option<Instant>, interval_secs: u64, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last).as_secs() >= interval_secs.max(60))
}

/// Enforce the stash TTL from inside the guard, on its own cadence.
///
/// Why this exists: `stash_expire` had exactly one non-test caller — the
/// CLI `Expire` arm. Nothing on any timer and no daemon pass ever called it,
/// so a 30-day TTL was a number nobody read. That was invisible while the
/// reclaim path could not write, but once stash actually worked the guard
/// started converting deletes into permanent second-disk usage: a stash
/// move FREES `/` at copy time (the origin is deleted) and only frees
/// `/mnt/data` when the entry expires. An expiry that never runs does not keep a
/// safety copy — it leaks.
///
/// Deliberately NOT inside the disk-pressure gate. The other reclaim paths run
/// at action/critical because they respond to pressure on `/`; this drains the
/// SECOND disk, so gating it on `/` pressure means a healthy `/` never drains
/// and the backlog only ever clears during a crisis.
///
/// Three gates beyond the cooldown, all about not deleting what the operator
/// did not ask to be deleted:
///
/// - `stash_expire_interval_secs = 0` opts out of the unattended path
///   entirely. The manual CLI still works.
/// - `clean_stash_first` must be armed. If it is not, any entries present
///   came from a human running `stash move` by hand, and those must not
///   become an unattended delete just because the directory is non-empty.
/// - `auto_cleanup_apply` must be true for actual deletion (audit M5,
///   2026-10-02 — previously hardcoded apply, arming unattended deletes
///   for operators who believed `auto_cleanup_apply = false` meant
///   dry-run). When false the pass still scans and reports, but removes
///   nothing.
fn maybe_expire_stash(guard: &GuardPolicy, state: &mut GuardRuntimeState, now: Instant) {
    if guard.stash_expire_interval_secs == 0 {
        return;
    }
    if !guard.clean_stash_first {
        return;
    }
    if !cooldown_due(
        state.last_stash_expire,
        guard.stash_expire_interval_secs,
        now,
    ) {
        return;
    }
    // Stamp before the work: a filesystem that errors on every pass must not
    // turn the interval into a per-pass retry.
    state.last_stash_expire = Some(now);

    let root = stash_root(guard);
    // Nothing to do, and no reason to spend a directory walk.
    if !root.is_dir() {
        return;
    }
    match stash_expire_detailed(&root, guard.stash_ttl_days, guard.auto_cleanup_apply) {
        Ok(outcome) => {
            // A pinned entry can never age out — no manifest, no timestamp — so
            // it needs a human (`stash purge`). Report it every pass, or it
            // quietly accumulates in a directory whose contract is bounded
            // growth.
            if !outcome.pinned.is_empty() {
                eprintln!(
                    "🕒 stash: {} pinned ({}), cannot age out — clear with `dracon-system stash purge <name> --apply`: {}",
                    outcome.pinned.len(),
                    human_bytes(outcome.pinned_bytes),
                    outcome.pinned.join(", ")
                );
            }
            for entry in outcome.removed {
                // Logged per deletion, not summarised: once an entry is gone the
                // journal is the only record that it ever existed, and the
                // operator-review signal cannot come from a directory listing.
                // In dry-run the entry is still on disk — say so explicitly.
                if guard.auto_cleanup_apply {
                    eprintln!(
                        "🕒 stash expired: {} (origin {}, freed {})",
                        entry.name,
                        entry.origin,
                        human_bytes(entry.bytes)
                    );
                } else {
                    eprintln!(
                        "🕒 stash would expire (dry-run, auto_cleanup_apply=false): {} (origin {}, {})",
                        entry.name,
                        entry.origin,
                        human_bytes(entry.bytes)
                    );
                }
            }
        }
        Err(e) => eprintln!("⚠️ stash expire failed: {e:#}"),
    }
}

/// Collapse a multi-line message into one line for a table cell.
///
/// anyhow's `{:#}` walks the whole error chain and newlines it. A table cell
/// that contains one would break the column alignment and, worse, let a
/// newline in an error string masquerade as a new row in a preview the
/// operator reads to decide whether to delete things.
fn one_line(msg: &str) -> String {
    msg.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cleanup_stale_cooldowns(state: &mut GuardRuntimeState, cooldown_secs: u64) {
    // checked_sub, not `-`: `Instant - Duration` PANICS when the duration
    // underflows the clock's representation ("overflow when subtracting
    // duration from instant"). A policy cooldown large enough to reach that
    // (>= 2^63 seconds, after the *2) would kill the daemon on its first
    // pass. normalize_guard_policy also caps the knob, but the subtraction
    // must not be able to panic for any value it is handed.
    let cutoff = Instant::now()
        .checked_sub(Duration::from_secs(cooldown_secs.saturating_mul(2)))
        .unwrap_or_else(Instant::now);
    state
        .notify_cooldowns
        .retain(|_, &mut since| since > cutoff);
}

async fn check_inode_usage(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    if !guard.monitor_inodes {
        return;
    }
    if let Ok(inode_percent) = inode_use_percent(&guard.disk_mount_path).await {
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
    let over_threshold = zombies.len() as u64 > guard.zombie_threshold;
    let (previous, event_due) = report_state_transition(
        state,
        "zombie-warning",
        if over_threshold {
            "over-threshold"
        } else {
            "ok"
        },
        guard.report_repeat_secs,
    );
    if over_threshold {
        if previous.as_deref() != Some("over-threshold") {
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
        if event_due {
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
    }
    zombies
}

/// Parse the `disk_extra_mounts` comma-separated list into mount points.
///
/// Extracted from the guard pass so it is testable: this list is
/// visibility-only and its only prior coverage was zero. Blank entries and
/// stray whitespace are dropped, because an empty entry would otherwise be
/// stat'd as "" and reported as a broken mount on every pass.
pub(crate) fn parse_extra_mounts(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

async fn check_large_logs(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    if !guard.monitor_logs {
        return;
    }
    // ADDED 2026-09-29 (DECIDE #4 follow-up): an ABSENT log_dirs now means
    // "scan the default" rather than "monitoring off", so log-size alerts
    // work without the operator naming a directory. An explicitly blank
    // value still disables the check.
    let Some(configured) = effective_log_dirs(&guard.log_dirs) else {
        return;
    };

    let mut log_dirs: Vec<PathBuf> = Vec::new();
    // A configured directory that does not exist used to be dropped without
    // a word, which made a wrong or not-yet-created default look identical
    // to "monitoring off". Report it once per pass instead: a silent skip
    // is exactly how a monitoring feature disappears without anyone noticing.
    let mut missing: Vec<String> = Vec::new();
    for entry in configured.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let path = expand_tilde(entry);
        if path.exists() {
            log_dirs.push(path);
        } else {
            missing.push(entry.to_string());
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "log monitoring: configured director{} not found, skipping: {}",
            if missing.len() == 1 { "y" } else { "ies" },
            missing.join(", ")
        );
    }

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
                    // CHANGED 2026-09-09 (audit F39): the strict variant
                    // rejects every descendant of /home and /var, so the
                    // example's own log_dirs (/var/log, ~/.local/share)
                    // could NEVER be truncated — the feature was dead.
                    // The guard variant still refuses exact system roots,
                    // user-protected paths, symlinks, and uncanonicalizable
                    // paths, which is the right posture for truncating
                    // known log files in standard locations.
                    let safe_path = match check_safe_to_delete_guard(path, &guard.protected_paths) {
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
    let mut total_reclaimed = 0u64;
    let mut all_cleaned: Vec<String> = Vec::new();

    if guard.auto_cleanup_rust {
        match proactive_cleanup_rust_targets(guard, state, apply).await {
            Ok(result) => {
                total_reclaimed += result.reclaimed_bytes;
                if apply {
                    for p in &result.cleaned_paths {
                        eprintln!("🧹 Proactive Rust: {}", p);
                    }
                }
                all_cleaned.extend(result.cleaned_paths);
            }
            Err(e) => eprintln!("⚠️ Proactive Rust target cleanup failed: {}", e),
        }
    }

    let cleanup_state = if all_cleaned.is_empty() {
        "ok"
    } else if apply {
        "applied"
    } else {
        "candidates"
    };
    let (previous, event_due) = report_state_transition(
        state,
        "proactive-cleanup",
        cleanup_state,
        guard.report_repeat_secs,
    );
    if apply && total_reclaimed > 0 && previous.as_deref() != Some("applied") {
        send_notification(
            guard,
            "Dracon System Guard - Proactive Cleanup",
            &format!(
                "Reclaimed {} ({} stale items)",
                human_bytes(total_reclaimed),
                all_cleaned.len()
            ),
        )
        .await;
    } else if !apply && cleanup_state == "candidates" && event_due {
        emit_event(&DraconEvent::new(
            "system",
            EventSeverity::Info,
            "guard/proactive-cleanup",
            format!(
                "dry-run identified {} stale items (estimated {})",
                all_cleaned.len(),
                human_bytes(total_reclaimed)
            ),
        ));
    }

    Ok(())
}

/// ADDED 2026-09-27 (space tiers Phase 2): relocate-first mitigation.
/// At action/critical, move cold candidates to the cold root until usage
/// drops below action% or candidates/moves run out. Report-only unless
/// `auto_relocate_apply`. Tracked content is never a candidate.
/// Returns (moves applied, bytes moved, candidates scanned).
async fn run_auto_relocate(guard: &GuardPolicy) -> Result<(usize, u64, Vec<ColdCandidate>)> {
    let cold_raw = guard.relocate_cold_root.trim();
    if cold_raw.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let cold_root = crate::resolve_policy_path(cold_raw);
    if !cold_root.is_dir() {
        eprintln!(
            "⚠️ auto-relocate: cold root {} missing — run `dracon-system setup`",
            cold_root.display()
        );
        return Ok((0, 0, Vec::new()));
    }
    let roots: Vec<PathBuf> = guard
        .relocate_candidate_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.is_dir() {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    if roots.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let min_bytes = guard
        .relocate_min_size_mb
        .saturating_mul(1024)
        .saturating_mul(1024);
    let candidates = find_cold_candidates(&roots, min_bytes, guard.relocate_min_age_days);
    if candidates.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let mut moves = 0usize;
    let mut moved_bytes = 0u64;
    let max_moves = guard.relocate_max_moves_per_pass.max(1) as usize;
    for cand in candidates.iter().take(max_moves) {
        // Re-check pressure before each move; stop once below action.
        // FIXED 2026-10-02 (audit L7): a df failure previously mapped
        // to u8::MAX, which reads as max pressure and KEPT MOVING trees
        // with no signal. Pause instead — relocating blind risks
        // churning disks for pressure that may not exist, and the next
        // pass retries once df is readable again.
        let used_now = match disk_details_for(&guard.disk_mount_path).await {
            Ok(d) => d.use_percent,
            Err(e) => {
                eprintln!(
                    "⚠️ auto-relocate: cannot read disk pressure for {} ({:#}) — pausing moves until pressure is readable",
                    guard.disk_mount_path, e
                );
                break;
            }
        };
        if used_now < guard.disk_action_percent {
            break;
        }
        let src = PathBuf::from(&cand.path);
        let plan = match plan_relocate(&src, &cold_root, &guard.protected_paths, false) {
            Ok(p) if p.ready => p,
            Ok(p) => {
                eprintln!(
                    "⚠️ auto-relocate: {} not ready: {}",
                    cand.path,
                    p.issues.join("; ")
                );
                continue;
            }
            Err(e) => {
                eprintln!("⚠️ auto-relocate: {}: {:#}", cand.path, e);
                continue;
            }
        };
        if !guard.auto_relocate_apply {
            eprintln!(
                "📦 auto-relocate would move {} ({}, {}d idle) → {} (report-only)",
                cand.path,
                human_bytes(cand.bytes),
                cand.age_days,
                plan.dest
            );
            continue;
        }
        match apply_relocate(&plan) {
            Ok(report) => {
                moves += 1;
                moved_bytes += report.bytes;
                eprintln!(
                    "📦 auto-relocated {} ({}) → {}",
                    report.link,
                    human_bytes(report.bytes),
                    report.target
                );
                if let Err(e) = record_relocation(&report.link, &report.target, report.bytes) {
                    eprintln!("⚠️ auto-relocate: moved but failed to record: {e:#}");
                }
            }
            Err(e) => eprintln!("⚠️ auto-relocate failed for {}: {:#}", cand.path, e),
        }
    }
    if moves > 0 {
        send_notification(
            guard,
            "Dracon System Guard",
            &format!(
                "Auto-relocated {} cold dir(s), {} → {}",
                moves,
                human_bytes(moved_bytes),
                cold_root.display()
            ),
        )
        .await;
    }
    Ok((moves, moved_bytes, candidates))
}

/// ADDED 2026-09-27 (space tiers Phase 2): monitor managed + auto-relocated
/// links every pass; notify once on drift, repeat per report cadence.
/// Loads link entries fresh (cheap TOML read) to avoid threading
/// SystemPolicy through the guard-only daemon state.
async fn check_link_drift(guard: &GuardPolicy, state: &mut GuardRuntimeState) {
    let entries = match load_system_policy() {
        Ok((_, policy)) => policy.links.entries,
        Err(_) => return,
    };
    let auto = relocation_link_entries();
    if entries.is_empty() && auto.is_empty() {
        return;
    }
    let mut policy = SystemPolicy::default();
    policy.links.entries = entries;
    let report = build_link_report_with(&policy, &auto);
    let value = if report.drifted == 0 {
        "ok".to_string()
    } else {
        format!("drifted={}", report.drifted)
    };
    let (_prev, should_emit) =
        report_state_transition(state, "links", &value, guard.report_repeat_secs);
    if should_emit && report.drifted > 0 {
        let detail = report
            .entries
            .iter()
            .filter(|e| e.issue != "ok")
            .take(5)
            .map(|e| format!("{} ({})", e.link, e.issue))
            .collect::<Vec<_>>()
            .join("; ");
        send_notification(
            guard,
            "Dracon System Guard",
            &format!("{} managed link(s) drifted: {}", report.drifted, detail),
        )
        .await;
        log_guard_event(
            guard,
            "links-drifted",
            &format!("drifted={} detail={}", report.drifted, detail),
        );
    }
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

    // ADDED 2026-09-26 (space tiers): extra mounts are visibility-only;
    // cleanup and freeze decisions still key off the primary mount.
    // Unreadable extras are skipped (verbose only) so one bad mount
    // cannot fail the whole guard pass.
    let mut extra_mounts = Vec::new();
    for mount in parse_extra_mounts(&guard.disk_extra_mounts) {
        match disk_details_for(&mount).await {
            Ok(d) => extra_mounts.push(MountStatus {
                mount: d.mount,
                use_percent: d.use_percent,
                state: disk_state(d.use_percent, guard).to_string(),
            }),
            Err(e) => veprintln!(1, "extra mount {mount}: {e:#}"),
        }
    }

    check_disk_trends(guard, state, used).await;
    check_disk_early_warning(guard, state, used).await;
    let fill_gbph =
        check_rapid_disk_fill(guard, state, details.used_bytes, used, details.avail_bytes).await;
    manage_sync_freeze(guard, used, &dstate, &mut sync_frozen);

    let mut relocate_candidates: Vec<ColdCandidate> = Vec::new();
    let mut relocated_count = 0usize;
    let mut relocated_bytes = 0u64;
    // ── The disk cleanup runs in the BACKGROUND (ADDED 2026-10-24) ────
    //
    // The cleanup is HOUSEKEEPING, and on a busy filesystem it can take
    // MINUTES: a /tmp with thousands of entries, a Trash with a
    // self-feeding .trashinfo loop, a ~/Dev-wide rust-target scan. It
    // used to run INLINE, ahead of the memory checks, so one slow pass
    // stalled the ENTIRE protection loop.
    //
    // Measured on 2026-10-24: a single pass held the loop for eight
    // minutes while the host sat in critical memory pressure. A
    // forgotten orphan tree had been eligible to be reaped for the whole
    // of that time and was only reaped when the cleanup finished and the
    // NEXT pass ran. The guard was doing housekeeping while the memory
    // it exists to protect drained away.
    //
    // The cleanup now runs in its own task. The pass never waits for it,
    // so the protection loop keeps its `interval_secs` cadence whatever
    // the filesystem is doing, and the cleanup's summary comes back
    // through a shared slot for the report.
    let cleanup_slot = state
        .cleanup_slot
        .get_or_insert_with(|| Arc::new(tokio::sync::Mutex::new(CleanupSummary::default())))
        .clone();
    if dstate == "action" || dstate == "critical" {
        let now = Instant::now();
        if auto_cleanup_due_at(state, guard.auto_cleanup_interval_secs, now) {
            // Set the timestamp before the scan so a persistent filesystem
            // error cannot turn into a 30-second retry loop.
            state.last_auto_cleanup = Some(now);
            spawn_background_cleanup(guard, state, used, CleanupMode::Pressure, cleanup_slot.clone());
        }
    } else if used >= guard.proactive_cleanup_percent {
        state.guard_cycle += 1;
        let interval = guard.proactive_cleanup_interval_cycles;
        let due = state.guard_cycle.is_multiple_of(interval);
        let cooldown_ok = state
            .last_proactive_cleanup
            .is_none_or(|t| t.elapsed().as_secs() >= interval.saturating_mul(guard.interval_secs));
        if due && cooldown_ok {
            // CHANGED 2026-08-25 (v0.112.39): the proactive tier previously
            // ran ONLY the rust-target scan, so /tmp, trash, nix and docker
            // all waited for action level (85%) — too late on a disk that
            // fills 20–60 GiB/h. The cheap non-rust kinds now run here too
            // (include_rust=false keeps the heavy ~/Dev scan on its own path).
            state.last_proactive_cleanup = Some(Instant::now());
            spawn_background_cleanup(guard, state, used, CleanupMode::Proactive, cleanup_slot.clone());
        }
    }

    // Pick up whatever the last background cleanup produced. Taking the
    // summary (rather than copying it) reports each cleanup once, on the
    // first pass after it finished.
    {
        let mut slot = cleanup_slot.lock().await;
        let summary = std::mem::take(&mut *slot);
        if summary.relocated_count > 0 || !summary.relocate_candidates.is_empty() {
            relocated_count = summary.relocated_count;
            relocated_bytes = summary.relocated_bytes;
            relocate_candidates = summary.relocate_candidates;
        }
    }

    // Stash expiry runs on every pass, outside the disk-pressure gate
    // above: it drains the SECOND disk rather than responding to pressure on
    // `/`, so putting it behind action/critical would mean the backlog only
    // ever clears during a crisis. Self-paced by its own cooldown.
    maybe_expire_stash(guard, state, Instant::now());

    check_disk_state_change(guard, state, used, &dstate).await;
    check_link_drift(guard, state).await;

    // One bounded metadata scan is shared by heavy-process and memory
    // pressure checks. The old implementation ran `ps ... args` twice per
    // pass, doubling both the untrusted argv payload and transient Vecs.
    let samples = process_samples().await?;
    let alerts = check_heavy_processes(guard, state, &samples).await?;
    cleanup_stale_cooldowns(state, guard.notify_cooldown_secs);

    check_inode_usage(guard, state).await;
    let zombies = check_zombie_processes(guard, state).await;
    let reap_candidates = reap_candidates(guard);
    // Opt-in auto-reap: terminate what still verifies, on a blocking
    // thread -- each kill can sleep up to ~7s in grace polls, and that
    // must never stall a tokio worker. Default OFF: no candidates are
    // ever signalled unless the operator sets `reap_stale_dev_servers`.
    let reaped = if guard.reap_stale_dev_servers && !reap_candidates.is_empty() {
        let policy = reap_policy_from_guard(guard);
        let for_reap = reap_candidates.clone();
        tokio::task::spawn_blocking(move || {
            auto_reap_stale_servers(Path::new("/proc"), &policy, &for_reap, PROC_TICKS_PER_SEC)
        })
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    check_large_logs(guard, state).await;
    let memory = check_memory_pressure(guard, state, &samples).await;

    let report = GuardReport {
        enabled: guard.enabled,
        disk_use_percent: used,
        disk_state: dstate,
        sync_frozen,
        extra_mounts,
        relocate_candidates,
        relocated_count,
        relocated_bytes,
        alerts,
        memory,
        zombies,
        reap_candidates,
        reaped,
        disk_fill_gbph: fill_gbph,
    };
    // AUDIT 2026-10-01: record what this pass left applied, so a panic or an
    // OOM kill during a critical-pressure episode cannot strand `nice` /
    // `oom_score_adj` / CPUQuota adjustments on live processes with no record
    // that they need restoring. Runs after the report fields are captured, so
    // the ledger always describes the state the pass ended in, and it skips the
    // write entirely when nothing changed.
    persist_mitigations(state);
    Ok(report)
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
    /// Per-entry failures `link apply` recovered from. ADDED 2026-10-01
    /// (audit): one bad entry used to abort the batch and hide every entry
    /// after it, so the operator never learned what had been changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) errors: Vec<String>,
}

fn resolve_system_policy_path() -> Result<Option<PathBuf>> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home"));
    resolve_system_policy_path_with(std::env::var_os("DRACON_SYSTEM_POLICY"), home)
}

fn resolve_system_policy_path_with(
    custom: Option<std::ffi::OsString>,
    home: PathBuf,
) -> Result<Option<PathBuf>> {
    if let Some(custom) = custom {
        // An explicit override is authoritative. Do not fall back to a
        // default policy when the override is missing or inaccessible: that
        // would turn a configuration error into an unexpected default run.
        // var_os is intentional so non-Unicode paths cannot be mistaken for
        // an unset override.
        return Ok(Some(PathBuf::from(custom)));
    }

    let candidates = [
        home.join(".dracon/utilities/system/dracon-system.toml"),
        home.join(".dracon/utilities/system/config.toml"),
        home.join(".dracon/system/dracon-system.toml"),
        home.join(".dracon/system/config.toml"),
    ];

    for path in candidates {
        match std::fs::metadata(&path) {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to inspect policy {}: {}",
                    path.display(),
                    error
                ));
            }
        }
    }

    Ok(None)
}

const CONFIG_ERROR_EXIT_STATUS: i32 = 78; // sysexits.h EX_CONFIG

#[derive(Debug)]
struct PolicyLoadError(anyhow::Error);

impl std::fmt::Display for PolicyLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for PolicyLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

fn exit_status_for_error(error: &anyhow::Error) -> i32 {
    if let Some(status) = error.downcast_ref::<oom_job::JobExit>() {
        status.0
    } else if error.downcast_ref::<PolicyLoadError>().is_some() {
        CONFIG_ERROR_EXIT_STATUS
    } else {
        1
    }
}

/// Load the operator policy, already normalized.
///
/// ADDED 2026-09-29 (queued audit follow-up, DECIDE #8): normalization runs
/// HERE, at the load boundary, rather than at each consumer. Nine of the
/// eleven `load_system_policy` call sites (setup.rs, links.rs, stash.rs,
/// relocate.rs, the link-drift check, the storage default-root lookup) never
/// called `normalize_guard_policy`, so an out-of-range threshold reached those
/// subsystems RAW — `stash_ttl_days`, `storage.min_size_mb` and the
/// relocate size/age/move-count knobs had no enforcement at all on that path.
/// Normalizing once here makes every consumer in-range by construction, so a
/// new call site cannot reintroduce the gap by forgetting to normalize.
pub(crate) fn load_system_policy() -> Result<(Option<PathBuf>, SystemPolicy)> {
    let Some(path) = resolve_system_policy_path()? else {
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
    let parsed = parse_system_policy(&content, &path)?;
    Ok((Some(path), parsed))
}

/// Parse policy TOML and normalize it in one step.
///
/// Normalization lives INSIDE this function, not at its call sites, so a
/// future caller cannot obtain a parsed-but-unnormalized policy by parsing
/// the file directly. `parse_system_policy_is_the_normalizing_entry_point`
/// pins that composition.
pub(crate) fn parse_system_policy(content: &str, path: &Path) -> Result<SystemPolicy> {
    let mut parsed: SystemPolicy = toml::from_str(content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))?;
    // Surface keys the parser dropped instead of letting a config look
    // applied when it is not. This WARNS rather than fails: a stray key must
    // not stop a monitoring daemon from starting, because the operator's
    // need is to be told, not locked out of their machine.
    if let Ok(doc) = content.parse::<toml::Value>() {
        let unknown = unknown_policy_keys(&doc);
        if !unknown.is_empty() {
            let detail: Vec<String> = unknown
                .iter()
                .map(|k| format!("{k}{}", hint_for(k)))
                .collect();
            eprintln!(
                "⚠ policy: {} unknown key(s) in {} ignored: {}",
                unknown.len(),
                path.display(),
                detail.join("; ")
            );
        }
    }
    parsed.normalize();
    Ok(parsed)
}

fn effective_system_policy_path() -> Result<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home"));
    let fallback = home.join(".dracon/utilities/system/dracon-system.toml");
    let resolved = resolve_system_policy_path_with(std::env::var_os("DRACON_SYSTEM_POLICY"), home)?;
    Ok(resolved.unwrap_or(fallback))
}

async fn is_user_service_active(service: &str) -> bool {
    // R4-SYS-03: read-only probe → PATH-aware resolution (strict
    // misses /usr/bin/systemctl on non-NixOS hosts). Mutating
    // systemctl execs elsewhere keep resolve_bin_strict.
    let bin = match resolve_bin_for_readonly_probe("systemctl") {
        Ok(b) => b,
        Err(_) => return false,
    };
    let output = Command::new(bin)
        .args(["--user", "is-active", service])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "active",
        _ => false,
    }
}

/// Probe a unit in the SYSTEM manager (no `--user`).
///
/// ADDED 2026-10-22: the daemon runs as a root-owned SYSTEM unit
/// (`dracon-sync-operator.service`), not as a user unit. The doctor's
/// user-scoped probe reported the daemon as inactive on a correctly
/// deployed host.
async fn is_system_service_active(service: &str) -> bool {
    let bin = match resolve_bin_for_readonly_probe("systemctl") {
        Ok(b) => b,
        Err(_) => return false,
    };
    let output = Command::new(bin)
        .args(["is-active", service])
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "active",
        _ => false,
    }
}

async fn build_status_report() -> Result<StatusReport> {
    let root = canonical_system_root();
    let system_policy_path = effective_system_policy_path()?;
    Ok(StatusReport {
        system_root: root.display().to_string(),
        nixos_root: root.join("nixos").display().to_string(),
        sync_policy: root
            .join("utilities/sync/dracon-sync.toml")
            .display()
            .to_string(),
        system_policy: system_policy_path.display().to_string(),
        system_policy_exists: system_policy_path.exists(),
        // FIXED 2026-10-22: the daemon runs as the root-owned SYSTEM unit
        // `dracon-sync-operator.service`; the old name was masked, so this
        // reported "inactive" on a correctly deployed host.
        sync_service_active: is_system_service_active(crate::doctor::SYNC_SERVICE).await,
    })
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
// `await_holding_lock`: the fixture tests below hold `FIXTURE_EXEC_LOCK`
// (see src/tests.rs) across their awaits on purpose. The exec of a
// freshly written fixture happens *inside* the awaited production call,
// so the lock has to span the await to cover the whole create->exec
// window. The guard is never held while acquiring another lock, and
// `#[tokio::test]` runs each test on its own current-thread runtime, so
// the holder always makes progress independently of a waiter and the
// serialization cannot deadlock.
#[allow(clippy::await_holding_lock)]
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
        let (policy_icon, policy_color) = if report.system_policy_exists {
            ("\u{2705}", Color::Green)
        } else {
            ("\u{274c}", Color::Red)
        };
        table.add_row(vec![
            Cell::new(policy_icon).fg(policy_color),
            Cell::new("system policy exists"),
            Cell::new(if report.system_policy_exists {
                "yes"
            } else {
                "no"
            }),
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
    let requested_kinds = kinds.unwrap_or_else(|| policy.storage.kinds.clone());
    // Audit M2 (2026-08-21): report-only hotspot kinds (git-db) must never
    // reach the deletion path — filter them out of every selection source
    // (CLI flag and policy default alike) before building CleanupConfig.
    let (effective_kinds, non_cleanup_requested) =
        filter_selectable_cleanup_kinds(parse_kinds(&requested_kinds));

    // ADDED 2026-09-09 (audit F37): `--json --cleanup` used to return
    // here, silently ignoring --cleanup/--apply with exit 0. Refuse the
    // combo up front instead of pretending to clean.
    if json && cleanup {
        anyhow::bail!("storage --json cannot be combined with --cleanup (JSON mode is report-only); run without --json to clean");
    }

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
            kinds: effective_kinds,
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
        if !non_cleanup_requested.is_empty() {
            println!(
                "⚠️  Ignored report-only kinds (never deletable): {}",
                non_cleanup_requested.join(", ")
            );
        }
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
        let mut package_cache_skipped = false;
        // Per-kind reclaim tracking
        let mut reclaim_by_kind: HashMap<String, u64> = HashMap::new();
        for item in &selected {
            if storage_hotspot_apply_is_blocked(&item.kind, cfg.apply) {
                package_cache_skipped = true;
                table.add_row(vec![
                    Cell::new(human_bytes(item.bytes)),
                    Cell::new(&item.kind),
                    Cell::new(item.path.display().to_string()),
                    Cell::new("SKIP package-cache coordination").fg(Color::Yellow),
                ]);
                continue;
            }
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
            actionable.push((item.kind.clone(), item.path.clone()));
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
        if package_cache_skipped {
            eprintln!(
                "🛡️ storage: refusing cache-hotspot deletion because no shared package-manager lock is available"
            );
        }

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
        let mut cleanup_failures = Vec::new();
        if cfg.apply {
            for (kind, path) in actionable {
                if let Err(e) = validate_storage_cleanup_activity(&kind, &path).await {
                    eprintln!("🛡️ keeping {}: {}", path.display(), e);
                    continue;
                }
                match validate_storage_cleanup_path(&path, &user_protected) {
                    Ok(safe_path) if safe_path.exists() => {
                        println!("🗑️  Deleting {}", path.display());
                        if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                            cleanup_failures.push(format!("{}: {}", path.display(), e));
                        }
                    }
                    Ok(_) => {}
                    Err(e) => cleanup_failures.push(format!("{}: {}", path.display(), e)),
                }
            }
            if cleanup_failures.is_empty() {
                if package_cache_skipped {
                    println!(
                        "✅ Cleanup complete; cache hotspots were retained for lifecycle safety."
                    );
                } else {
                    println!("✅ Cleanup complete.");
                }
            } else {
                eprintln!("⚠️ {} cleanup path(s) failed:", cleanup_failures.len());
                for failure in &cleanup_failures {
                    eprintln!("  • {}", failure);
                }
                println!("Cleanup completed with failures; successful paths were removed.");
            }
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

/// Validate one `storage --cleanup --apply` target for deletion.
///
/// CHANGED 2026-08-21 (protected-path inconsistency fix): the interactive
/// cleanup previously used the strict `check_safe_to_delete`, whose
/// SYSTEM_PROTECTED ancestor check rejects EVERY path under `/home` — on a
/// normal workstation that is all of them (live incident 2026-08-21:
/// node_modules/build candidates were refused with "under system root
/// /home" while the guard's own auto-cleanup path deleted the same class
/// of paths without issue). The apply path now uses
/// `check_safe_to_delete_guard`, the same rule set as the guard: known
/// artifact/cache dirs under /home are deletable, while exact system
/// roots, user-protected paths, symlinks, and canonicalization failures
/// stay refused.
fn validate_storage_cleanup_path(path: &Path, user_protected: &[String]) -> Result<PathBuf> {
    // Backstop for audit M2 (2026-08-21): `.git` is project history, not a
    // regenerable artifact, and the tracked-dir gate cannot protect it (git
    // never tracks its own database). Kind-level filtering keeps it out of
    // the candidate list; this refuses it even if a future caller passes it
    // through directly.
    // CHANGED 2026-09-09 (audit F38): the old check matched only
    // `file_name() == ".git"`, so `/repo/.git/objects` sailed through.
    // Refuse any path with a `.git` component.
    if path.components().any(|c| c.as_os_str() == ".git") {
        anyhow::bail!(
            "refusing to delete git database (project history, not an artifact): {}",
            path.display()
        );
    }
    check_safe_to_delete_guard(path, user_protected)
}

/// One-line audit summary for the `guard once` table (audit F118,
/// 2026-10-08). Without this, the kills existed only in JSON/stderr.
/// Names each PID with its matched signature and outcome, capped at 5
/// entries like the candidate rows above.
///
/// REWORKED 2026-10-09 (audit F124): the `outcome: None` arm used to
/// render "skipped" for BOTH skip reasons. A budget-deferred candidate is
/// re-scanned next pass and may still be signalled, which is materially
/// different from a candidate that no longer verifies, so the table now
/// names the disposition instead of collapsing the two.
fn summarize_reaped(reaped: &[ReapedProcess]) -> String {
    let detail = |r: &ReapedProcess| -> String {
        let outcome = match (&r.outcome, r.disposition) {
            (
                Some(TerminateOutcome::Signalled {
                    escalated_to_sigkill,
                }),
                _,
            ) => {
                if *escalated_to_sigkill {
                    "SIGTERM→SIGKILL"
                } else {
                    "SIGTERM"
                }
            }
            (Some(TerminateOutcome::AlreadyGone), _) => "already gone",
            (Some(TerminateOutcome::Refused { reason }), _) => reason,
            (Some(TerminateOutcome::Failed { reason }), _) => reason,
            (None, ReapDisposition::Deferred) => "deferred to next pass",
            (None, ReapDisposition::NotVerified) => "no longer verifies",
            (None, ReapDisposition::Signalled) => "skipped",
        };
        format!("pid={} {} [{}]", r.pid, r.signature, outcome)
    };
    if reaped.len() <= 5 {
        format!(
            "{}: {}",
            reaped.len(),
            reaped.iter().map(detail).collect::<Vec<_>>().join(", ")
        )
    } else {
        format!(
            "{}: {}, …and {} more",
            reaped.len(),
            reaped[..5]
                .iter()
                .map(detail)
                .collect::<Vec<_>>()
                .join(", "),
            reaped.len() - 5
        )
    }
}

async fn cmd_guard_once(guard: &GuardPolicy, json: bool) -> Result<()> {
    // One process owns hydrate/mutate/persist for the entire lifetime, just
    // like the daemon. A diagnostic may not drain the running guard's state.
    let _owner = acquire_daemon_lock("dracon-system-guard").with_context(|| {
        "guard already owns process mitigation state; refusing concurrent guard once"
    })?;
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    // Only the daemon persists: a one-shot run restores everything before it
    // exits, and nothing else should write the operator's state dir.
    let mut runtime = GuardRuntimeState {
        persist_mitigations: true,
        ..GuardRuntimeState::default()
    };
    // AUDIT 2026-10-01: a previous run may have died (panic, OOM kill) with
    // mitigations applied. Adopt them into the live state and drain them through
    // the EXISTING restore path before the first pass, so no process is left
    // reniced/biased/capped forever. Identity is re-verified on adoption, so a
    // recycled PID is never touched.
    cpu_limit::recover().await;
    hydrate_mitigations_from_disk(&mut runtime);
    if !runtime.memory_reniced_pids.is_empty()
        || !runtime.oom_biased_pids.is_empty()
        || !runtime.capped_pids.is_empty()
    {
        restore_runtime_adjustments(&mut runtime).await;
    }
    let report_result = run_guard_once(guard, &mut runtime).await;
    // A one-shot invocation has no daemon runtime to carry these entries into
    // a later retry. Restore every limiter before handling either a report or
    // an error, including the JSON early-return path.
    let adjustments_restored = restore_runtime_adjustments(&mut runtime).await;
    if !adjustments_restored {
        eprintln!("⚠ guard once: some process adjustments could not be restored");
    }
    let report = report_result?;
    if !adjustments_restored {
        anyhow::bail!("guard once could not restore every process adjustment");
    }
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

    for m in &report.extra_mounts {
        let icon = match m.state.as_str() {
            "ok" => "✅",
            "warn" => "⚠️",
            "action" => "🟠",
            "critical" => "🔴",
            _ => "",
        };
        table.add_row(vec![
            Cell::new(icon),
            Cell::new(format!("Disk {}", m.mount)),
            Cell::new(format!("{}% ({})", m.use_percent, m.state)),
        ]);
    }

    if report.relocated_count > 0 {
        table.add_row(vec![
            Cell::new("📦"),
            Cell::new("Auto-relocated"),
            Cell::new(format!(
                "{} dir(s), {}",
                report.relocated_count,
                human_bytes(report.relocated_bytes)
            )),
        ]);
    }
    for (i, c) in report.relocate_candidates.iter().enumerate() {
        if i >= 5 {
            table.add_row(vec![
                Cell::new(""),
                Cell::new("Cold candidates"),
                Cell::new(format!(
                    "…and {} more",
                    report.relocate_candidates.len() - 5
                )),
            ]);
            break;
        }
        table.add_row(vec![
            Cell::new("📦"),
            Cell::new(if i == 0 { "Cold candidates" } else { "" }),
            Cell::new(format!(
                "{} ({} in {} files, {}d idle)",
                c.path,
                human_bytes(c.bytes),
                c.files,
                c.age_days
            )),
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
                if m.observed_pressure != m.pressure {
                    format!("{} (observed {})", m.pressure, m.observed_pressure)
                } else {
                    m.pressure.clone()
                },
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

    // ADDED 2026-10-08 (audit F118): the reaper's audit trail must be
    // visible to a human running `guard once` without `--json`. `reaped` /
    // `pressure_reaped` were serialized to JSON and stderr only, so the
    // operator saw zero evidence that processes were killed. Same class as
    // the relocation/stash visibility fixes: a destructive action with
    // no table row is an action nobody can review.
    if !guard.reap_stale_dev_servers {
        table.add_row(vec![
            Cell::new(""),
            Cell::new("Reap candidates"),
            Cell::new(if report.reap_candidates.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "{} (reap_stale_dev_servers is OFF — report only)",
                    report.reap_candidates.len()
                )
            }),
        ]);
    }
    if !report.reaped.is_empty() {
        table.add_row(vec![
            Cell::new("🪓"),
            Cell::new("Reaped (stale servers)"),
            Cell::new(summarize_reaped(&report.reaped)),
        ]);
    }
    if let Some(memory) = &report.memory {
        if !memory.pressure_reaped.is_empty() {
            table.add_row(vec![
                Cell::new("🪓"),
                Cell::new("Reaped (pressure orphans)"),
                Cell::new(summarize_reaped(&memory.pressure_reaped)),
            ]);
        }
        if !memory.forgotten_trees.is_empty() {
            let mut parts: Vec<String> = memory
                .forgotten_trees
                .iter()
                .map(|t| {
                    format!(
                        "pid={} {} proc {}MiB",
                        t.root_pid, t.member_count, t.aggregate_footprint_mb
                    )
                })
                .collect();
            parts.sort();
            table.add_row(vec![
                Cell::new("🧹"),
                Cell::new("Forgotten orphan trees"),
                Cell::new(format!("{} — {}", memory.forgotten_trees.len(), parts.join(", "))),
            ]);
        }
        if !memory.forgotten_reaped.is_empty() {
            let ended: usize = memory.forgotten_reaped.iter().map(|r| r.members_ended).sum();
            table.add_row(vec![
                Cell::new("🪓"),
                Cell::new("Reaped (forgotten trees)"),
                Cell::new(format!(
                    "{} tree(s), {ended} process(es) ended",
                    memory.forgotten_reaped.len()
                )),
            ]);
        }
        if !memory.vetoed_trees.is_empty() {
            table.add_row(vec![
                Cell::new("🛡️"),
                Cell::new("Heavy trees vetoed"),
                Cell::new(format!(
                    "{} (left alone — a live desktop session)",
                    memory.vetoed_trees.len()
                )),
            ]);
        }
        if !memory.protected_trees.is_empty() {
            let mut parts: Vec<String> = memory
                .protected_trees
                .iter()
                .map(|t| {
                    format!(
                        "pid={} {} proc {}MiB — {}",
                        t.root_pid, t.member_count, t.aggregate_footprint_mb, t.reason
                    )
                })
                .collect();
            parts.sort();
            table.add_row(vec![
                Cell::new("🛡️"),
                Cell::new("Heavy trees left alone"),
                Cell::new(format!(
                    "{} — {}",
                    memory.protected_trees.len(),
                    parts.join(", ")
                )),
            ]);
        }
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

fn rotate_guard_log_for_policy(guard: &GuardPolicy, startup: bool) {
    if let Some(log_path) = resolve_guard_log_path(&guard.guard_log_file) {
        let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
        rotate_guard_log_if_oversized(&log_path, max_bytes, startup);
    }
}

#[cfg(test)]
fn rotate_guard_log_for_policy_with_home(guard: &GuardPolicy, startup: bool, home: Option<&Path>) {
    if let Some(log_path) = resolve_guard_log_path_with_home(&guard.guard_log_file, home) {
        let max_bytes = guard.guard_log_max_mb.saturating_mul(1024 * 1024);
        rotate_guard_log_if_oversized(&log_path, max_bytes, startup);
    }
}

/// What a SIGHUP reload attempt did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReloadOutcome {
    /// The new policy is live. Runtime state was cleanly reset, so nothing
    /// from the previous policy is still in force.
    Applied,
    /// The new policy was NOT adopted. The previous policy stays live so its
    /// mitigations remain armed and the leftover process adjustments are
    /// retried on the next pass instead of being stranded.
    Deferred,
}

/// Adopt a freshly loaded policy, or refuse to.
///
/// Bounded by design: this touches the policy and the in-memory runtime
/// state, and nothing else. It performs no I/O, spawns nothing, and does not
/// read the policy file, so the whole reload decision is testable in-process.
///
/// ADDED 2026-09-29 (DECIDE #2 follow-up): the reload previously swapped the
/// policy UNCONDITIONALLY, before checking whether the pre-swap restore of
/// process adjustments had fully succeeded. On a partial restore the leftover
/// `runtime` entries describe adjustments made under the OLD policy, and
/// pairing them with the new one can strand them permanently: if the new
/// policy disables the mitigation that produced them — `auto_renice = false`
/// or `auto_renice_on_memory = false` — nothing will ever retry the restore,
/// and the process stays reniced for the lifetime of the daemon. Keeping the
/// old policy is the safe failure mode: behaviour is exactly what it was
/// before the SIGHUP, and the still-armed mitigation retries next pass.
///
/// `enabled = false` is always applied even after a partial restore: stopping
/// is the safe direction, and the retained state is retried by the shutdown
/// restore in the daemon loop before it returns.
pub(crate) fn apply_policy_reload(
    guard: &mut GuardPolicy,
    runtime: &mut GuardRuntimeState,
    new_policy: GuardPolicy,
    adjustments_restored: bool,
) -> ReloadOutcome {
    let disabling = !new_policy.enabled;
    if (!adjustments_restored || runtime.mitigation_ledger_unavailable) && !disabling {
        return ReloadOutcome::Deferred;
    }
    *guard = new_policy;
    normalize_guard_policy(guard);
    if adjustments_restored && !runtime.mitigation_ledger_unavailable {
        let persist_mitigations = runtime.persist_mitigations;
        *runtime = GuardRuntimeState {
            persist_mitigations,
            ..GuardRuntimeState::default()
        };
    }
    ReloadOutcome::Applied
}

async fn cmd_guard_daemon(guard: &mut GuardPolicy) -> Result<()> {
    if !guard.enabled {
        println!("guard disabled in policy");
        return Ok(());
    }
    let _lock = acquire_daemon_lock("dracon-system-guard")
        .with_context(|| "failed to acquire guard daemon lock")?;

    // ── Startup cleanup: rotate the configured guard log if oversized ──
    // Use the same resolver as event logging; a blank path disables both
    // logging and rotation rather than silently targeting /tmp.
    rotate_guard_log_for_policy(guard, true);

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
    let mut runtime = GuardRuntimeState {
        persist_mitigations: true,
        ..GuardRuntimeState::default()
    };
    // Recovery belongs to the daemon, not merely the one-shot CLI path.
    cpu_limit::recover().await;
    hydrate_mitigations_from_disk(&mut runtime);
    restore_runtime_adjustments(&mut runtime).await;
    persist_mitigations(&mut runtime);
    while !shutdown.load(Ordering::SeqCst) {
        if runtime.mitigation_ledger_unavailable {
            hydrate_mitigations_from_disk(&mut runtime);
        }
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
                    let outcome = apply_policy_reload(
                        guard,
                        &mut runtime,
                        new_policy.guard,
                        adjustments_restored,
                    );
                    if outcome == ReloadOutcome::Deferred {
                        // The previous policy stays live on purpose; see
                        // apply_policy_reload for why adopting the new one
                        // here could strand a process adjustment.
                        eprintln!(
                            "system: SIGHUP reload deferred: keeping previous policy so pending process adjustments are retried"
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "guard/policy-reload",
                            "SIGHUP reload deferred: process adjustments could not all be restored, previous policy kept".to_string(),
                        ));
                        continue;
                    }
                    interval = guard.interval_secs;
                    veprintln!(
                        2,
                        "system: policy reloaded on SIGHUP (disk_warn={}%, disk_critical={}%)",
                        guard.disk_warn_percent,
                        guard.disk_critical_percent
                    );
                    // FIXED 2026-09-27 (audit F93): `enabled` was only ever
                    // checked once, at daemon startup (main.rs:6996), so a
                    // SIGHUP that loaded `enabled = false` assigned it to
                    // the LIVE policy and the loop kept calling
                    // `run_guard_once` forever — the documented way to stop
                    // the guard did not stop cleanup, and re-enabling
                    // required a restart. Honour it on reload, mirroring
                    // the startup path. Process adjustments were already
                    // restored above, so exiting here is clean.
                    if !guard.enabled {
                        eprintln!(
                            "guard disabled in policy — stopping after SIGHUP reload (restart the service to re-enable)"
                        );
                        break;
                    }
                }
                Err(e) => {
                    // CHANGED 2026-09-09 (audit F42): the old message
                    // claimed "using defaults" but this arm assigns
                    // nothing — the previous (known-good) policy stays
                    // live, which is the safer behavior. Say so.
                    eprintln!(
                        "system: SIGHUP reload warning: corrupted policy file, keeping previous policy: {}",
                        e
                    );
                    emit_event(&DraconEvent::new(
                        "system",
                        EventSeverity::Error,
                        "guard/policy-reload",
                        format!(
                            "SIGHUP reload: policy corrupted, keeping previous policy: {}",
                            e
                        ),
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
        // Registered managed jobs carry their own lifetime OOM debt. The
        // independent recoverer owns it; the daemon only observes progress so
        // a stuck recovery stays visible instead of silently drifting.
        match managed_oom::drain(false, &parse_kinds(&guard.process_exempt_names)).await {
            Ok(0) => {}
            Ok(pending) => {
                eprintln!("managed OOM recovery: {pending} registered job debt(s) outstanding")
            }
            Err(e) => eprintln!("managed OOM recovery check failed: {e:#}"),
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
        eprintln!("⚠ shutdown: some process adjustments could not be restored; durable obligations retained");
    }
    persist_mitigations(&mut runtime);
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
            match docker_prune(
                apply,
                docker,
                docker_volumes,
                guard.cleanup_command_timeout_secs,
            )
            .await
            {
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
        match clean_package_caches(true, true, true, true, apply).await {
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
        // ADDED 2026-09-09 (audit F45): this branch printed human-readable
        // disk text even with --json, and execution then fell through to
        // the JSON report below — mixed output. Emit a zeroed JSON report
        // instead.
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "reclaimed_bytes": 0,
                    "reclaimed_human": human_bytes(0),
                    "actions": Vec::<String>::new(),
                })
            );
            return Ok(());
        }
        let disk = disk_use_percent_for(&guard.disk_mount_path).await?;
        println!("Disk usage: {}% (mount: {})", disk, guard.disk_mount_path);

        if let Ok((total, used, _free)) = get_inode_info(&guard.disk_mount_path).await {
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
    fn all() -> Self {
        Self {
            rust: true,
            trash: true,
            nix: true,
            caches: true,
            node_modules: true,
            docker: true,
        }
    }

    /// Returns true if no targets are enabled.
    fn is_empty(&self) -> bool {
        !self.rust && !self.trash && !self.nix && !self.caches && !self.node_modules && !self.docker
    }
}

/// Resolve target selection, defaulting a bare `guard clean` to all targets.
fn resolve_clean_targets(all: bool, targets: &CleanTargets) -> CleanTargets {
    if all || targets.is_empty() {
        CleanTargets::all()
    } else {
        targets.clone()
    }
}

async fn cmd_guard_clean(
    guard: &GuardPolicy,
    json: bool,
    apply: bool,
    all: bool,
    targets: CleanTargets,
    min_size_mb: Option<u64>,
) -> Result<()> {
    let targets = resolve_clean_targets(all, &targets);
    let do_rust = targets.rust;
    let do_trash = targets.trash;
    let do_nix = targets.nix;
    let do_caches = targets.caches;
    let do_node = targets.node_modules;
    let do_docker = targets.docker;

    let mut guard_clone = guard.clone();
    if let Some(mb) = min_size_mb {
        guard_clone.cleanup_min_size_mb = mb;
    }

    let mut total_reclaimed = 0u64;
    let mut actions: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    if do_rust {
        let mut runtime = GuardRuntimeState::default();
        // FIXED 2026-09-27 (audit F92): this passed
        // `bypass_age_gate = false`, so the `rust_target_action_min_age_days`
        // (default 7) freshness gate also applied to the operator's EXPLICIT
        // `guard clean --rust --apply`. On a full disk the freshly-built
        // `target/` that is actually consuming the space is exactly the one
        // the gate protects, so the documented "execute cleanup" flow printed
        // `Protected: … (touched 0d ago, under action min-age 7d)` and
        // reclaimed nothing — the escape hatch silently did nothing.
        //
        // The gate exists to stop AUTONOMOUS daily thrash (a deleted target
        // gets rebuilt within hours, pushing the disk back over the action
        // line), not as a safety invariant: the live-build protections
        // (mtime<60s backstop, active cargo/rustc detection, ancestor-aware
        // protected-project matching) all still apply here, exactly as they
        // do when the daemon lifts the gate at the critical tier.
        let result = auto_cleanup_rust_targets(&guard_clone, &mut runtime, apply, true).await?;
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
            guard_clone.trash_min_age_days,
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
        match clean_nix_garbage(
            guard_clone.nix_keep_generations,
            apply,
            guard_clone.cleanup_command_timeout_secs,
        )
        .await
        {
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
        // Explicit `guard clean` always deletes (no stash-first).
        match clean_old_node_modules(
            &roots,
            guard_clone.node_modules_max_age_days,
            apply,
            &guard_clone.protected_paths,
            false,
            Path::new(""),
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
        match clean_package_caches(true, true, true, true, apply).await {
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
        // CHANGED 2026-09-09 (audit F44): the old call passed `apply`
        // as docker's `--all`, so EVERY `--docker --apply` deleted ALL
        // unused images with no opt-out, while the prune path tied
        // `--all` to its own flag. Tie docker `--all` to clean's `--all`
        // targets flag instead: `clean --all --apply` prunes aggressively,
        // `clean --docker --apply` prunes dangling-only.
        match docker_prune(
            apply,
            all,
            guard_clone.docker_prune_volumes,
            guard_clone.cleanup_command_timeout_secs,
        )
        .await
        {
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
    // FIXED 2026-09-27 (audit rework round 4, F84): the two nested
    // `else { if .. }` wrappers are `collapsible_else_if` on the pinned
    // MSRV toolchain. Both collapsed; behaviour is identical.
    } else if actions.is_empty() && failures.is_empty() {
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

    Ok(())
}

async fn cmd_guard(cmd: GuardCommands) -> Result<()> {
    let (_, policy) =
        load_system_policy().map_err(|error| anyhow::Error::new(PolicyLoadError(error)))?;
    let mut guard = policy.guard;
    normalize_guard_policy(&mut guard);
    // ONCE per process, before any subcommand runs: a configured stash or
    // cold root that the shipped unit leaves read-only would otherwise surface
    // only as a per-candidate EROFS, once per pass, forever.
    crate::warn_uncovered_storage_roots(&guard);
    match cmd {
        GuardCommands::Once { json } => cmd_guard_once(&guard, json).await,
        GuardCommands::Daemon => cmd_guard_daemon(&mut guard).await,
        GuardCommands::NotifyTest { title, body } => {
            let title = title.unwrap_or_else(|| "dracon-system test".to_string());
            let body = body.unwrap_or_else(|| {
                "notify_command works — flags are passed as argv, no shell involved".to_string()
            });
            // Report what will run, so a misconfiguration is visible here
            // rather than at the next real alert.
            match NotifyCommand::parse(&guard.notify_command) {
                Ok(cmd) => {
                    let mut rendered = cmd.program.clone();
                    for arg in &cmd.args {
                        rendered.push(' ');
                        rendered.push_str(arg);
                    }
                    println!("notify_command: {rendered}");
                    println!("title: {title}");
                    println!("body:  {body}");
                }
                Err(e) => anyhow::bail!("notify_command rejected: {e}"),
            }
            // Force the send regardless of `notify`, so an operator can test
            // a command before enabling it.
            let mut test_guard = guard.clone();
            test_guard.notify = true;
            send_notification(&test_guard, &title, &body).await;
            Ok(())
        }
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
            all,
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
            cmd_guard_clean(&guard, json, apply, all, targets, min_size_mb).await
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    VERBOSITY.store(cli.verbose, Ordering::SeqCst);

    match cli.cmd {
        Commands::ManagedJob { command } => oom_job::launch(&command),
        Commands::OomJobRecover => managed_oom::recover_loop().await,
        Commands::OomJobReconcile { id, confirm } => {
            managed_oom::reconcile_orphan(&id, confirm).await
        }
        Commands::ManagedJobEnter { .. } => {
            anyhow::bail!("job entry requires the single-threaded startup path")
        }
        Commands::ProtectionProbe { root, phase } => protection_probe::run(&root, &phase).await,
        Commands::ProtectionWorker { root, child } => protection_probe::worker(&root, child),
        Commands::CpuLeaseRelease { key, generation } => cpu_limit::release(&key, &generation)
            .await
            .map_err(anyhow::Error::msg),
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
        Commands::Relocate {
            path,
            to,
            apply,
            allow_tracked,
            json,
        } => cmd_relocate(path, to, apply, allow_tracked, json),
        Commands::Stash { cmd } => cmd_stash(cmd),
        Commands::Setup { apply, json } => cmd_setup(apply, json),
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

fn main() {
    let cli = Cli::parse();
    // The job sentinel must never start a runtime or background threads: its
    // terminal proof relies on being the only remaining task in the boundary.
    let result = if let Commands::ManagedJobEnter { id, command } = &cli.cmd {
        oom_job::enter(id, command)
    } else if let Commands::ManagedJob { command } = &cli.cmd {
        oom_job::launch(command)
    } else {
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(run(cli)),
            Err(error) => Err(error.into()),
        }
    };
    if let Err(error) = result {
        eprintln!("Error: {error:#}");
        std::process::exit(exit_status_for_error(&error));
    }
}
