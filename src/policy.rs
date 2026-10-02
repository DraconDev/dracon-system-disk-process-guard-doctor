use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Policy structs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub(crate) struct SystemPolicy {
    #[serde(default)]
    pub(crate) storage: StoragePolicy,
    #[serde(default)]
    pub(crate) links: LinkPolicy,
    #[serde(default)]
    pub(crate) guard: GuardPolicy,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct StoragePolicy {
    #[serde(default)]
    pub(crate) default_root: String,
    #[serde(default = "default_min_size_mb")]
    pub(crate) min_size_mb: u64,
    #[serde(default = "default_kinds")]
    pub(crate) kinds: String,
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            default_root: String::new(),
            min_size_mb: default_min_size_mb(),
            kinds: default_kinds(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub(crate) struct LinkPolicy {
    #[serde(default)]
    pub(crate) entries: Vec<LinkEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LinkEntry {
    pub(crate) link: String,
    pub(crate) target: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct GuardPolicy {
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    #[serde(default = "default_disk_mount_path")]
    pub(crate) disk_mount_path: String,
    #[serde(default = "default_guard_interval_secs")]
    pub(crate) interval_secs: u64,
    #[serde(default = "default_disk_early_warn_percent")]
    pub(crate) disk_early_warn_percent: u8,
    #[serde(default = "default_disk_warn_percent")]
    pub(crate) disk_warn_percent: u8,
    #[serde(default = "default_disk_action_percent")]
    pub(crate) disk_action_percent: u8,
    #[serde(default = "default_disk_critical_percent")]
    pub(crate) disk_critical_percent: u8,
    // Disk pressure is reported by default; pausing synchronization is an
    // explicit operator choice, not an automatic guard action.
    #[serde(default = "default_false")]
    pub(crate) freeze_sync_at_action: bool,
    #[serde(default = "default_sync_freeze_marker")]
    pub(crate) sync_freeze_marker: String,
    #[serde(default = "default_unfreeze_below_percent")]
    pub(crate) unfreeze_below_percent: u8,
    #[serde(default = "default_process_cpu_percent")]
    pub(crate) process_cpu_percent: f32,
    #[serde(default = "default_process_rss_mb")]
    pub(crate) process_rss_mb: u64,
    #[serde(default = "default_process_sustain_secs")]
    pub(crate) process_sustain_secs: u64,
    #[serde(default = "default_process_exempt_names")]
    pub(crate) process_exempt_names: String,
    // --- Abandoned dev/test process reporting (report-only) -------------
    // Test runs leave orphaned dev servers behind when they are killed
    // mid-flight. These three fields only change what the guard *prints*;
    // nothing here signals, kills, or stops a process. Lowering
    // min_idle_hours makes the report noisier, never more destructive.
    /// Minimum age before an idle, terminal-less process is reported.
    #[serde(default = "default_reap_report_min_idle_hours")]
    pub(crate) reap_report_min_idle_hours: u64,
    /// Lifetime CPU ceiling, in seconds, below which a process is idle.
    #[serde(default = "default_reap_report_max_cpu_seconds")]
    pub(crate) reap_report_max_cpu_seconds: u64,
    /// Comma-separated allowlist of command-line signatures that mark a
    /// process as disposable test/dev infrastructure.
    #[serde(default = "default_reap_report_signatures")]
    pub(crate) reap_report_signatures: String,
    #[serde(default = "default_true")]
    pub(crate) notify: bool,
    #[serde(default = "default_notify_command")]
    pub(crate) notify_command: String,
    #[serde(default = "default_notify_cooldown_secs")]
    pub(crate) notify_cooldown_secs: u64,
    // Repeat structured warning events at most this often while a
    // condition remains unchanged. State transitions are still immediate.
    #[serde(default = "default_report_repeat_secs")]
    pub(crate) report_repeat_secs: u64,
    #[serde(default)]
    pub(crate) auto_renice: bool,
    #[serde(default = "default_renice_value")]
    pub(crate) renice_value: i32,
    #[serde(default = "default_release_after_secs")]
    pub(crate) release_after_secs: u64,
    #[serde(default = "default_guard_log_file")]
    pub(crate) guard_log_file: String,
    #[serde(default = "default_guard_log_max_mb")]
    pub(crate) guard_log_max_mb: u64,
    #[serde(default = "default_auto_cleanup_rust")]
    pub(crate) auto_cleanup_rust: bool,
    #[serde(default)]
    pub(crate) auto_cleanup_apply: bool,
    // Keep the action-level dry-run/cleanup scan from running every guard
    // cycle while disk pressure persists. This is deliberately independent
    // from report_repeat_secs: scanning the tree is work, not just reporting.
    #[serde(default = "default_auto_cleanup_interval_secs")]
    pub(crate) auto_cleanup_interval_secs: u64,
    #[serde(default = "default_cleanup_min_size_mb")]
    pub(crate) cleanup_min_size_mb: u64,
    #[serde(default = "default_rust_search_roots")]
    pub(crate) rust_search_roots: String,
    #[serde(default = "default_node_modules_search_roots")]
    pub(crate) node_modules_search_roots: String,
    #[serde(default = "default_true")]
    pub(crate) track_trends: bool,
    #[serde(default = "default_trend_warn_hours")]
    pub(crate) trend_warn_hours: u64,
    #[serde(default = "default_true")]
    pub(crate) monitor_inodes: bool,
    #[serde(default = "default_inode_warn_percent")]
    pub(crate) inode_warn_percent: u8,
    #[serde(default = "default_true")]
    pub(crate) monitor_zombies: bool,
    #[serde(default = "default_zombie_threshold")]
    pub(crate) zombie_threshold: u64,
    // ADDED 2026-08-10 (v0.112.35): memory/swap pressure guard —
    // the 2026-08-09/10 incidents (swap thrash, kswapd 86% CPU,
    // everything crawling on 19 GiB used zram swap) had NO guard:
    // the daemon only watched disk and per-process CPU/RSS.
    #[serde(default = "default_true")]
    pub(crate) monitor_memory: bool,
    #[serde(default = "default_mem_available_warn_percent")]
    pub(crate) mem_available_warn_percent: u8,
    #[serde(default = "default_swap_used_warn_percent")]
    pub(crate) swap_used_warn_percent: u8,
    #[serde(default = "default_mem_psi_full_warn")]
    pub(crate) mem_psi_full_warn: f64,
    // Require memory pressure to persist before notifying or applying
    // reversible pressure mitigation. This prevents transient samples
    // and swap occupancy alone from disturbing active processes.
    #[serde(default = "default_memory_pressure_sustain_secs")]
    pub(crate) memory_pressure_sustain_secs: u64,
    // ADDED 2026-08-10 (v0.112.36): during memory pressure, deprioritize
    // the top RSS offenders (graduated nice, reversible when pressure
    // drops). The runtime requires CAP_SYS_NICE so a user service cannot
    // leave a process permanently deprioritized when recovery needs to
    // raise its priority. Whitelist via process_exempt_names. Never a cap.
    #[serde(default = "default_true")]
    pub(crate) auto_renice_on_memory: bool,
    // ADDED 2026-08-10 (v0.112.36): during CRITICAL memory pressure,
    // raise oom_score_adj on the top offenders so the kernel's
    // last-resort OOM kill picks them instead of an innocent process.
    // Writing oom_score_adj never triggers a kill — it only steers
    // the victim choice IF the kernel kills anyway. Restored on
    // recovery. Whitelist via process_exempt_names.
    #[serde(default = "default_true")]
    pub(crate) bias_oom_on_pressure: bool,
    // ADDED 2026-08-10 (v0.112.36): during CRITICAL pressure, hard-
    // throttle top offenders to N% CPU via a transient systemd scope
    // (CPUQuota). Unlike memory caps, CPU throttling never kills and
    // never frees-then-crashes: it only limits scheduling. Tames a
    // stuck busy-loop that nice 19 still lets burn a core. 0 = off.
    // Requires a user systemd manager (the guard already runs under
    // one). Whitelist via process_exempt_names.
    #[serde(default)]
    pub(crate) cap_offenders_cpu_percent: u32,
    // ADDED 2026-08-10 (v0.112.35): sustained-heavy escalation —
    // a process still heavy after this many seconds is reported as
    // a "stuck candidate" (e.g. the 4 svelte-check at 285% CPU
    // holding 6 GiB that never finished during the incident).
    #[serde(default = "default_process_stuck_after_secs")]
    pub(crate) process_stuck_after_secs: u64,
    // ADDED 2026-08-10 (v0.112.35): rapid disk-fill alert in
    // GiB/hour, computed from df byte deltas (percent deltas are
    // too coarse on large disks).
    #[serde(default = "default_disk_rapid_fill_gbph")]
    pub(crate) disk_rapid_fill_gbph: f64,
    // ADDED 2026-08-10 (v0.112.35): refuse to empty the trash
    // when top-level entries carry credential signals (the
    // 2026-08-10 Trash scan found 665 credential-pattern matches;
    // see docs/design/disk-full-credentials-2026-08-10.md).
    #[serde(default = "default_true")]
    pub(crate) trash_credential_guard: bool,
    #[serde(default = "default_true")]
    pub(crate) monitor_logs: bool,
    #[serde(default = "default_log_size_mb")]
    pub(crate) log_size_mb: u64,
    /// Directories scanned for oversized log files.
    ///
    /// ADDED 2026-09-29 (DECIDE #4 follow-up): this was a plain `String`
    /// defaulting to "", so "the operator never mentioned it" and "the
    /// operator wrote `log_dirs = \"\"`" were indistinguishable — which is
    /// why no default could be added without silently taking away the only
    /// way to switch the check off. `Option<String>` separates them: `None`
    /// (key absent) means "use the default", `Some(blank)` means "off".
    #[serde(default)]
    pub(crate) log_dirs: Option<String>,
    #[serde(default)]
    pub(crate) auto_truncate_logs: bool,
    #[serde(default = "default_log_max_truncate_mb")]
    pub(crate) log_max_truncate_mb: u64,
    #[serde(default)]
    pub(crate) log_preserve_header_lines: usize,
    #[serde(default = "default_true")]
    pub(crate) docker_prune: bool,
    #[serde(default)]
    pub(crate) docker_prune_volumes: bool,
    // ADDED 2026-09-27 (audit F91). `docker system prune --all` deletes
    // every unused image, not just dangling ones — far more destructive
    // than the plain prune. The two CLI call sites tie it to an explicit
    // `--all` flag, but `run_auto_cleanup` (the DAEMON path) passed a
    // literal `true` with no knob at all, so any policy with
    // `docker_prune = true` + `auto_cleanup_apply = true` reaped all
    // unused images on every action/critical pass. Default false: the
    // daemon prunes dangling layers only unless the operator opts in.
    #[serde(default)]
    pub(crate) docker_prune_all: bool,
    #[serde(default = "default_true")]
    pub(crate) clean_package_caches: bool,
    #[serde(default = "default_true")]
    pub(crate) clean_trash: bool,
    #[serde(default = "default_true")]
    pub(crate) clean_nix_garbage: bool,
    // ADDED 2026-08-21 (audit M3): node_modules cleanup previously ran
    // unconditionally whenever auto_cleanup_apply was on — the only
    // cleanup kind with no feature flag. Default true preserves existing
    // behavior; set false to disable (e.g. projects resumed after long
    // pauses lose deps mid-session since node_modules mtimes are idle).
    #[serde(default = "default_true")]
    pub(crate) clean_node_modules: bool,
    // Keep the newest N generations in each Nix profile when cleanup is
    // applied. Zero leaves profile generations untouched.
    #[serde(default = "default_nix_keep_generations")]
    pub(crate) nix_keep_generations: u32,
    #[serde(default = "default_node_modules_max_age_days")]
    pub(crate) node_modules_max_age_days: u64,
    // ADDED 2026-08-25 (v0.112.39): /tmp hygiene — the 2026-08-25 disk
    // incident found 207 GiB accumulated in /tmp (418 stale
    // puppeteer/playwright profiles, pi-bash logs, stale audit clones)
    // that no existing cleanup kind covers; rust-target cleanup never
    // looks outside ~/Dev. Age-based TOP-LEVEL entry cleanup with
    // open-path protection (entries held open by any process, or used as a
    // process cwd, are skipped). Configured roots are restricted to explicit
    // temporary namespaces by check_safe_tmp_root before scanning.
    #[serde(default = "default_true")]
    pub(crate) clean_tmp: bool,
    #[serde(default = "default_tmp_search_paths")]
    pub(crate) tmp_search_paths: String,
    #[serde(default = "default_tmp_min_age_hours")]
    pub(crate) tmp_min_age_hours: u64,
    // ADDED 2026-08-25 (v0.112.39): only purge trash entries older than
    // this many days, so clean_trash keeps a recovery window instead of
    // emptying everything at once at the next action-level event.
    // 0 preserves the old empty-everything behavior.
    #[serde(default = "default_trash_min_age_days")]
    pub(crate) trash_min_age_days: u64,
    #[serde(default)]
    pub(crate) protected_paths: Vec<String>,
    #[serde(default = "default_proactive_cleanup_percent")]
    pub(crate) proactive_cleanup_percent: u8,
    #[serde(default = "default_rust_target_max_age_days")]
    pub(crate) rust_target_max_age_days: u64,
    // CHANGED 2026-09-14: action-level Rust target cleanup now has its own
    // lingering gate (default 7 days). Previously the action tier deleted ANY
    // target above cleanup_min_size_mb (only a 60s mtime backstop), which
    // thrashed daily-driver projects (dracon-platform/target deleted ~20x in
    // 2 days, each rebuild regrowing 4-8 GiB straight back over the action
    // line). 0 disables the gate and restores the old delete-anything posture.
    #[serde(default = "default_rust_target_action_min_age_days")]
    pub(crate) rust_target_action_min_age_days: u64,
    #[serde(default = "default_proactive_cleanup_interval_cycles")]
    pub(crate) proactive_cleanup_interval_cycles: u64,
    // ADDED 2026-09-26 (space tiers): extra mounts reported alongside the
    // primary mount (comma-separated). Visibility only: cleanup and freeze
    // decisions still key off the primary mount.
    #[serde(default)]
    pub(crate) disk_extra_mounts: String,
    // ADDED 2026-09-26 (space tiers): quarantine root for hold-then-delete.
    #[serde(default = "default_quarantine_dir")]
    pub(crate) quarantine_dir: String,
    // ADDED 2026-09-26 (space tiers): quarantine TTL in days; 0 disables expiry.
    #[serde(default = "default_quarantine_ttl_days")]
    pub(crate) quarantine_ttl_days: u64,
    // ADDED 2026-10-02: how often the daemon runs quarantine expiry ITSELF.
    //
    // The TTL above was never enforced by anything. `quarantine_expire` had
    // exactly one non-test caller — the CLI `Expire` arm — so the only way an
    // entry ever aged out was a human running it by hand. No systemd timer, no
    // cron, no daemon pass. That was harmless while the reclaim path could not
    // write (every move failed with EROFS, so nothing was ever quarantined),
    // but once the ReadWritePaths fix made quarantine work, the guard began
    // converting deletes into permanent second-disk usage: a move frees `/` at
    // copy time and only frees `/mnt/data` when the entry expires, so a TTL
    // that never fires means the system pays for every reclaimed tree twice,
    // forever. See docs/design/guard-namespace-contract-2026-10-01.md.
    //
    // 0 = never expire automatically. The manual CLI path still works; only the
    // unattended one is disabled.
    #[serde(default = "default_quarantine_expire_interval_secs")]
    pub(crate) quarantine_expire_interval_secs: u64,
    // ADDED 2026-09-27 (space tiers Phase 2): daemon rust-target and
    // node_modules cleanup moves candidates to quarantine instead of
    // deleting them. Explicit `guard clean` always deletes.
    #[serde(default)]
    pub(crate) clean_quarantine_first: bool,
    // ADDED 2026-09-27 (space tiers Phase 2): scan for cold relocation
    // candidates at action/critical. Needs relocate_cold_root set; that
    // default is empty (disabled) so no machine path is ever assumed —
    // multi-user setups must configure it explicitly.
    #[serde(default = "default_true")]
    pub(crate) auto_relocate: bool,
    #[serde(default)]
    pub(crate) auto_relocate_apply: bool,
    #[serde(default = "default_relocate_candidate_roots")]
    pub(crate) relocate_candidate_roots: String,
    #[serde(default)]
    pub(crate) relocate_cold_root: String,
    #[serde(default = "default_relocate_min_size_mb")]
    pub(crate) relocate_min_size_mb: u64,
    #[serde(default = "default_relocate_min_age_days")]
    pub(crate) relocate_min_age_days: u64,
    #[serde(default = "default_relocate_max_moves_per_pass")]
    pub(crate) relocate_max_moves_per_pass: u64,
}

impl Default for GuardPolicy {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            disk_mount_path: default_disk_mount_path(),
            interval_secs: default_guard_interval_secs(),
            disk_early_warn_percent: default_disk_early_warn_percent(),
            disk_warn_percent: default_disk_warn_percent(),
            disk_action_percent: default_disk_action_percent(),
            disk_critical_percent: default_disk_critical_percent(),
            freeze_sync_at_action: default_false(),
            sync_freeze_marker: default_sync_freeze_marker(),
            unfreeze_below_percent: default_unfreeze_below_percent(),
            process_cpu_percent: default_process_cpu_percent(),
            process_rss_mb: default_process_rss_mb(),
            process_sustain_secs: default_process_sustain_secs(),
            process_exempt_names: default_process_exempt_names(),
            reap_report_min_idle_hours: default_reap_report_min_idle_hours(),
            reap_report_max_cpu_seconds: default_reap_report_max_cpu_seconds(),
            reap_report_signatures: default_reap_report_signatures(),
            notify: default_true(),
            notify_command: default_notify_command(),
            notify_cooldown_secs: default_notify_cooldown_secs(),
            report_repeat_secs: default_report_repeat_secs(),
            auto_renice: false,
            renice_value: default_renice_value(),
            release_after_secs: default_release_after_secs(),
            guard_log_file: default_guard_log_file(),
            guard_log_max_mb: default_guard_log_max_mb(),
            auto_cleanup_rust: default_auto_cleanup_rust(),
            auto_cleanup_apply: false,
            auto_cleanup_interval_secs: default_auto_cleanup_interval_secs(),
            cleanup_min_size_mb: default_cleanup_min_size_mb(),
            rust_search_roots: default_rust_search_roots(),
            node_modules_search_roots: default_node_modules_search_roots(),
            track_trends: default_true(),
            trend_warn_hours: default_trend_warn_hours(),
            monitor_inodes: default_true(),
            inode_warn_percent: default_inode_warn_percent(),
            monitor_zombies: default_true(),
            zombie_threshold: default_zombie_threshold(),
            monitor_memory: default_true(),
            mem_available_warn_percent: default_mem_available_warn_percent(),
            swap_used_warn_percent: default_swap_used_warn_percent(),
            mem_psi_full_warn: default_mem_psi_full_warn(),
            memory_pressure_sustain_secs: default_memory_pressure_sustain_secs(),
            auto_renice_on_memory: default_true(),
            bias_oom_on_pressure: default_true(),
            cap_offenders_cpu_percent: 0,
            process_stuck_after_secs: default_process_stuck_after_secs(),
            disk_rapid_fill_gbph: default_disk_rapid_fill_gbph(),
            trash_credential_guard: default_true(),
            monitor_logs: default_true(),
            log_size_mb: default_log_size_mb(),
            log_dirs: None,
            auto_truncate_logs: false,
            log_max_truncate_mb: default_log_max_truncate_mb(),
            log_preserve_header_lines: 0,
            docker_prune: default_true(),
            docker_prune_volumes: false,
            docker_prune_all: false,
            clean_package_caches: default_true(),
            clean_trash: default_true(),
            clean_nix_garbage: default_true(),
            clean_node_modules: default_true(),
            nix_keep_generations: 5,
            node_modules_max_age_days: default_node_modules_max_age_days(),
            clean_tmp: default_true(),
            tmp_search_paths: default_tmp_search_paths(),
            tmp_min_age_hours: default_tmp_min_age_hours(),
            trash_min_age_days: default_trash_min_age_days(),
            protected_paths: Vec::new(),
            proactive_cleanup_percent: default_proactive_cleanup_percent(),
            rust_target_max_age_days: default_rust_target_max_age_days(),
            rust_target_action_min_age_days: default_rust_target_action_min_age_days(),
            proactive_cleanup_interval_cycles: default_proactive_cleanup_interval_cycles(),
            disk_extra_mounts: String::new(),
            quarantine_dir: default_quarantine_dir(),
            quarantine_ttl_days: default_quarantine_ttl_days(),
            quarantine_expire_interval_secs: default_quarantine_expire_interval_secs(),
            clean_quarantine_first: false,
            auto_relocate: default_true(),
            auto_relocate_apply: false,
            relocate_candidate_roots: default_relocate_candidate_roots(),
            relocate_cold_root: String::new(),
            relocate_min_size_mb: default_relocate_min_size_mb(),
            relocate_min_age_days: default_relocate_min_age_days(),
            relocate_max_moves_per_pass: default_relocate_max_moves_per_pass(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default value functions for serde
// ---------------------------------------------------------------------------

pub(crate) fn default_min_size_mb() -> u64 {
    512
}

pub(crate) fn default_kinds() -> String {
    "rust-build,node-deps,build-output,cache".to_string()
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_tmp_search_paths() -> String {
    "/tmp".to_string()
}

pub(crate) fn default_tmp_min_age_hours() -> u64 {
    24
}

pub(crate) fn default_trash_min_age_days() -> u64 {
    7
}

pub(crate) fn default_false() -> bool {
    false
}

pub(crate) fn default_enabled() -> bool {
    true
}

fn default_disk_mount_path() -> String {
    if PathBuf::from("/nix").exists() {
        "/nix".to_string()
    } else {
        "/".to_string()
    }
}

fn default_guard_interval_secs() -> u64 {
    30
}

fn default_disk_early_warn_percent() -> u8 {
    70
}

pub(crate) fn default_disk_warn_percent() -> u8 {
    80
}

pub(crate) fn default_disk_action_percent() -> u8 {
    90
}

pub(crate) fn default_disk_critical_percent() -> u8 {
    95
}

pub(crate) fn default_sync_freeze_marker() -> String {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("⚠️ could not determine home directory, using /var/tmp fallback");
            PathBuf::from("/var/tmp")
        }
    };
    home.join(".dracon")
        .join("dracon-sync.freeze")
        .display()
        .to_string()
}

fn default_unfreeze_below_percent() -> u8 {
    88
}

pub(crate) fn default_process_cpu_percent() -> f32 {
    50.0
}

pub(crate) fn default_process_rss_mb() -> u64 {
    4096
}

pub(crate) fn default_process_sustain_secs() -> u64 {
    30
}

pub(crate) fn default_reap_report_min_idle_hours() -> u64 {
    crate::reap::DEFAULT_REAP_REPORT_MIN_IDLE_HOURS
}

pub(crate) fn default_reap_report_max_cpu_seconds() -> u64 {
    crate::reap::DEFAULT_REAP_REPORT_MAX_CPU_SECONDS
}

pub(crate) fn default_reap_report_signatures() -> String {
    crate::reap::DEFAULT_REAP_REPORT_SIGNATURES.to_string()
}

pub(crate) fn default_process_exempt_names() -> String {
    // dracon-code (agent sessions) is exempt out of the box: the fleet
    // runs dozens of them, and renicing or biasing an interactive agent
    // session degrades the work the operator is watching. Decided
    // 2026-09-29 (audit DECIDE): match the example template.
    "systemd,dbus-daemon,Xorg,kwin_wayland,plasmashell,dracon-code".to_string()
}

pub(crate) fn default_notify_command() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "dracon".to_string());
    let candidates = [
        format!("/etc/profiles/per-user/{}/bin/notify-send", user),
        "/run/current-system/sw/bin/notify-send".to_string(),
        "/usr/bin/notify-send".to_string(),
    ];
    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return path.clone();
        }
    }
    "/usr/bin/notify-send".to_string()
}

// ---------------------------------------------------------------------------
// notify_command parsing
// ---------------------------------------------------------------------------
//
// `notify_command` is a policy value, so it is interpreted here rather than
// in the notification call site.
//
// Security model: the string is split into a program and an argv array and
// handed to execve. NO SHELL IS EVER INVOKED, so `;`, `|`, `&&`, `$(...)`,
// backticks and globs are inert characters, not syntax. There is no variable,
// command, or arithmetic expansion either. The notification title and body
// are appended as ordinary argv entries, so a message that happens to
// contain shell metacharacters cannot become code.
//
// The one thing argv-splitting alone does NOT stop is the operator naming an
// arbitrary program — `/bin/sh -c '...'` is an absolute path and would
// otherwise run. Before argument support existed that was blocked only
// ACCIDENTALLY, because the whole string was used as one path and
// `/bin/sh -c foo` does not exist as a file. Splitting the string would have
// quietly turned notify_command into a general-purpose command-execution
// knob, so NOTIFY_FORBIDDEN_PROGRAMS makes that refusal explicit instead of
// incidental.

/// Programs `notify_command` may not name, matched on the file name
/// component so `/bin/sh` and `/usr/bin/bash` are both caught.
pub(crate) const NOTIFY_FORBIDDEN_PROGRAMS: &[&str] = &[
    // Shells: the classic path from "run a notifier" to "run anything".
    "sh",
    "bash",
    "dash",
    "zsh",
    "ksh",
    "mksh",
    "csh",
    "tcsh",
    "fish",
    "ash",
    "busybox",
    // Privilege escalation: the guard already runs unprivileged; a notifier
    // has no business changing that.
    "sudo",
    "su",
    "doas",
    "pkexec",
    "setuidgid",
    // Argument pass-through wrappers: these re-interpret argv as a command,
    // which is precisely the capability being denied.
    "env",
    "xargs",
    "nohup",
    "setsid",
    "nice",
    "ionice",
    "timeout",
    "watch",
    "stdbuf",
    "systemd-run",
    "flatpak-run",
    "chroot",
    "unshare",
];

/// A validated `notify_command`: an absolute program path plus its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotifyCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

/// Split a configured command into words using shell-like QUOTING only.
///
/// This deliberately implements the tokenizer half of shell word splitting
/// and none of the evaluation half: no `$VAR`, no `$(...)`, no backticks, no
/// globs, no operator parsing. Inside double quotes a backslash escapes only
/// `" \ $` and a backslash (POSIX); elsewhere it escapes the next character
/// literally, and `\<newline>` is a line continuation that vanishes.
///
/// A quoted empty string yields an empty argument (`''` is one empty word,
/// not zero words) — that distinction is why `has_word` is tracked
/// separately from whether the buffer is empty.
pub(crate) fn split_command_words(input: &str) -> Result<Vec<String>, String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut has_word = false;
    let mut chars = input.chars();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            '\'' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated single quote".to_string()),
                    }
                }
            }
            '"' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            // A backslash-newline inside double quotes is a
                            // line continuation and produces nothing.
                            Some('\n') => {}
                            // POSIX keeps the backslash for every other char
                            // inside double quotes, so "\n" stays two chars.
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => {
                                return Err("trailing backslash inside double quotes".to_string())
                            }
                        },
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated double quote".to_string()),
                    }
                }
            }
            '\\' => {
                has_word = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(escaped) => current.push(escaped),
                    None => return Err("trailing backslash".to_string()),
                }
            }
            plain => {
                has_word = true;
                current.push(plain);
            }
        }
    }
    if has_word {
        words.push(current);
    }
    Ok(words)
}

impl NotifyCommand {
    /// Parse and validate a configured `notify_command`.
    ///
    /// The first word is the program and must be an absolute path and must
    /// not be a shell, privilege escalator, or argument pass-through wrapper
    /// (see `NOTIFY_FORBIDDEN_PROGRAMS`). The remaining words are its
    /// arguments, passed through verbatim.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("notify_command is empty".to_string());
        }
        let mut words = split_command_words(trimmed)?;
        let program = words.remove(0);
        if !program.starts_with('/') {
            return Err(format!(
                "notify_command must start with an absolute path, got: {program}"
            ));
        }
        let file_name = std::path::Path::new(&program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&program);
        if NOTIFY_FORBIDDEN_PROGRAMS.contains(&file_name) {
            return Err(format!(
                "notify_command may not use `{file_name}` (shell, privilege escalator, \
                 or argument pass-through wrapper) — point it at a notifier binary"
            ));
        }
        Ok(Self {
            program,
            args: words,
        })
    }
}

pub(crate) fn default_notify_cooldown_secs() -> u64 {
    300
}

pub(crate) fn default_report_repeat_secs() -> u64 {
    1800
}

pub(crate) fn default_renice_value() -> i32 {
    5
}

pub(crate) fn default_release_after_secs() -> u64 {
    120
}

pub(crate) fn default_guard_log_file() -> String {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return String::new(),
    };
    home.join(".local")
        .join("state")
        .join("dracon")
        .join("dracon-system-guard.log")
        .display()
        .to_string()
}

pub(crate) fn default_guard_log_max_mb() -> u64 {
    1
}

pub(crate) fn default_auto_cleanup_rust() -> bool {
    true
}

pub(crate) fn default_cleanup_min_size_mb() -> u64 {
    256
}

pub(crate) fn default_rust_search_roots() -> String {
    "~/Dev".to_string()
}

pub(crate) fn default_node_modules_search_roots() -> String {
    "~/Dev".to_string()
}

fn default_trend_warn_hours() -> u64 {
    24
}

fn default_inode_warn_percent() -> u8 {
    85
}

fn default_zombie_threshold() -> u64 {
    20
}

fn default_mem_available_warn_percent() -> u8 {
    10
}

fn default_swap_used_warn_percent() -> u8 {
    50
}

fn default_mem_psi_full_warn() -> f64 {
    10.0
}

fn default_memory_pressure_sustain_secs() -> u64 {
    120
}

fn default_process_stuck_after_secs() -> u64 {
    600
}

fn default_disk_rapid_fill_gbph() -> f64 {
    20.0
}

fn default_log_size_mb() -> u64 {
    100
}

fn default_log_max_truncate_mb() -> u64 {
    50
}

/// Default directories scanned for oversized logs.
///
/// The guard's own state directory. Chosen over candidates like `/var/log`
/// or the service's journal because:
///   - it always exists on a machine running the daemon, so the check is not
///     inert on a fresh install;
///   - `dracon-system-guard.service` lists `%h/.local/state/dracon` in
///     `ReadWritePaths`, so `auto_truncate_logs` can actually reclaim there,
///     whereas `/var/log` fails EROFS under the shipped hardening;
///   - it holds the guard's own event log, the one log whose growth the
///     guard most needs to notice.
pub(crate) fn default_log_dirs() -> String {
    "~/.local/state/dracon".to_string()
}

/// Resolve the configured log directory list.
///
/// Returns the comma-separated list to scan, or `None` when log-size
/// monitoring is explicitly disabled. `None` in (key absent) resolves to
/// the default; `Some` holding only whitespace is the operator's explicit
/// "off" and is honoured as such, so adding a default did not take away the
/// ability to disable the check.
pub(crate) fn effective_log_dirs(raw: &Option<String>) -> Option<String> {
    match raw {
        None => Some(default_log_dirs()),
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value.clone()),
    }
}

pub(crate) fn default_node_modules_max_age_days() -> u64 {
    30
}

pub(crate) fn default_nix_keep_generations() -> u32 {
    5
}

pub(crate) fn default_proactive_cleanup_percent() -> u8 {
    80
}

pub(crate) fn default_auto_cleanup_interval_secs() -> u64 {
    1800
}

pub(crate) fn default_rust_target_max_age_days() -> u64 {
    14
}

pub(crate) fn default_rust_target_action_min_age_days() -> u64 {
    7
}

pub(crate) fn default_proactive_cleanup_interval_cycles() -> u64 {
    120
}

pub(crate) fn default_quarantine_dir() -> String {
    "~/.local/share/dracon/quarantine".to_string()
}

pub(crate) fn default_quarantine_ttl_days() -> u64 {
    30
}

pub(crate) fn default_quarantine_expire_interval_secs() -> u64 {
    // A day is the natural unit for a 30-day TTL: the scan is one
    // metadata walk of the quarantine root (one manifest per entry, not one
    // per file), and a day is far finer than the resolution any operator
    // cares about for "when does my 30-day hold end".
    86_400
}

pub(crate) fn default_relocate_candidate_roots() -> String {
    "~/Dev".to_string()
}

pub(crate) fn default_relocate_min_size_mb() -> u64 {
    2048
}

pub(crate) fn default_relocate_min_age_days() -> u64 {
    14
}

pub(crate) fn default_relocate_max_moves_per_pass() -> u64 {
    3
}

// ---------------------------------------------------------------------------
// Legal-range enforcement (normalization)
// ---------------------------------------------------------------------------
//
// `dracon-system.example.toml` documents a legal range for most numeric
// knobs. Before this module, those ranges existed only as PROSE: the daemon
// accepted any value the type allowed, and a nonsense threshold silently did
// nothing (a NaN comparison is false everywhere; a percent above 100 never
// trips; a 0 log cap disables rotation without saying so). The audit DECIDE
// #8 recorded that gap; this is the enforcement half.
//
// `normalize_guard_policy` is the SINGLE place the ranges are applied, and
// `SystemPolicy::normalize` runs it from `load_system_policy`, so every
// consumer — daemon, doctor, setup, links, relocate, quarantine, storage —
// gets in-range values by construction.
//
// Two rules decide clamp vs. leave-alone:
//
// 1. A value that makes the daemon MALFUNCTION is clamped.
// 2. A value where 0 is a DOCUMENTED "off" sentinel is left alone. A floor
//    there would silently re-arm a feature the operator deliberately turned
//    off, which is a worse failure than the one the clamp prevents. Those
//    knobs are listed in SENTINEL_ZERO_KNOBS so a range test can fail if a
//    later change clamps one by accident.
//
// Normalization is idempotent: every rule is min/max/clamp against a
// constant or an already-normalized neighbour, so applying it twice is a
// no-op. That is what lets the load boundary and the CLI entry points both
// call it.

/// Knobs where `0` is a documented "disabled" sentinel rather than a nonsense
/// value. These are deliberately NOT given a floor: see rule 2 above. This
/// list exists so the range tests can fail if a later "just add a floor"
/// change clamps one by accident — it is a test contract, not runtime data.
#[cfg(test)]
pub(crate) const SENTINEL_ZERO_KNOBS: &[&str] = &[
    // 0 = no trend alert within any horizon.
    "trend_warn_hours", // 0 = never alert on zombie count.
    "zombie_threshold",
    // 0 = never alert on log size.
    "log_size_mb",
    // 0 = treat every node_modules as a candidate.
    "node_modules_max_age_days",
    // 0 = sweep /tmp regardless of age.
    "tmp_min_age_hours",
    // 0 = treat fresh directories as relocation candidates.
    "relocate_min_age_days",
    // 0 = purge trash immediately (no recovery window).
    "trash_min_age_days",
    // 0 = disable the action-tier age gate (old delete-anything posture).
    "rust_target_action_min_age_days",
    // 0 = never expire quarantine entries.
    "quarantine_ttl_days",
    // 0 = never run quarantine expiry automatically (CLI still works).
    "quarantine_expire_interval_secs",
    // 0 = CPU throttling off.
    "cap_offenders_cpu_percent",
    // 0 = report every idle process / report only never-ran processes.
    "reap_report_min_idle_hours",
    "reap_report_max_cpu_seconds",
    // 0 = keep no Nix generations (delete everything prunable).
    "nix_keep_generations",
];

/// Largest header `log_preserve_header_lines` will preserve when truncating.
/// The preserve step reads that many lines into memory, so an unbounded
/// value is a memory-exhaustion vector on a multi-gigabyte log.
pub(crate) const LOG_PRESERVE_HEADER_LINES_MAX: usize = 10_000;

/// Inclusive bounds of the POSIX nice range. `graduated_nice_value` already
/// clamps its result to this, but `renice_value` is the operator's *floor*
/// input: a negative value would let the guard RAISE an offender's priority,
/// which the tier-floor contract explicitly promises never happens.
pub(crate) const RENICE_VALUE_MIN: i32 = 0;
pub(crate) const RENICE_VALUE_MAX: i32 = 19;

/// The clamped-field set from the most recent `SystemPolicy::normalize` call
/// in this process, or `None` before the first one. Used to report each
/// distinct state exactly once.
static LAST_REPORTED_CLAMPS: std::sync::OnceLock<std::sync::Mutex<Option<Vec<&'static str>>>> =
    std::sync::OnceLock::new();

impl SystemPolicy {
    /// Normalize every sub-policy and report what was clamped.
    ///
    /// FIXED 2026-09-29 (auditor, first review round): the report used to
    /// print on EVERY load, and the guard re-loads the policy once per pass
    /// (`check_link_drift` reads it fresh each cycle to pick up link
    /// entries). `guard once` therefore emitted the line twice and
    /// `guard daemon` once per interval — about 5,760 lines a day at the
    /// default 30 s interval, which buries everything else in the journal
    /// and is exactly the "recurring noise" the docs promised would not
    /// happen. The clamped set is now reported only when it CHANGES.
    pub(crate) fn normalize(&mut self) {
        let mut adjusted = normalize_storage_policy(&mut self.storage);
        adjusted.append(&mut normalize_guard_policy(&mut self.guard));
        // Sorted so the comparison is order-independent.
        adjusted.sort_unstable();
        report_clamps(adjusted);
    }
}

/// What the clamp reporter should do for one transition.
///
/// Modelled as a pure decision rather than read out of the process-global
/// inside the production wrapper: FIXED 2026-09-29 (auditor, second review
/// round) the four cadence tests drove `LAST_REPORTED_CLAMPS` directly while
/// libtest ran them on separate threads, so they raced each other and the
/// full suite failed roughly 7% of runs. The logic is now a total function
/// of (previous, current) with no shared state, so the tests are
/// order-independent and need no mutex.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClampReport {
    /// In range, and nothing was ever clamped in this process: stay silent
    /// and record nothing. Writing an empty set here would make the first
    /// real clamp look like a transition from a known state.
    SilentNoState,
    /// Identical to what was already reported: stay silent.
    Unchanged,
    /// Out of range: report these clamped fields.
    Clamped(Vec<&'static str>),
    /// Was clamped, now in range: report the recovery so a fix is confirmed
    /// rather than inferred from silence.
    Recovered(Vec<&'static str>),
}

/// Decide the clamp report for one transition. Pure: no globals, no I/O.
///
/// A fresh process reports its first non-empty set, so a daemon restart
/// always re-tells the operator their file is out of range.
pub(crate) fn clamp_report_decision(
    previous: Option<&[&'static str]>,
    current: &[&'static str],
) -> ClampReport {
    match (previous, current) {
        (None, []) => ClampReport::SilentNoState,
        (None, cur) => ClampReport::Clamped(cur.to_vec()),
        (Some(prev), cur) if prev == cur => ClampReport::Unchanged,
        (Some(prev), []) => ClampReport::Recovered(prev.to_vec()),
        (Some(_), cur) => ClampReport::Clamped(cur.to_vec()),
    }
}

/// Report a clamped-field set once per distinct state, per process.
fn report_clamps(adjusted: Vec<&'static str>) {
    let cell = LAST_REPORTED_CLAMPS.get_or_init(|| std::sync::Mutex::new(None));
    let mut last = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let decision = clamp_report_decision(last.as_deref(), &adjusted);
    let fields = match &decision {
        ClampReport::SilentNoState | ClampReport::Unchanged => return,
        ClampReport::Clamped(fields) | ClampReport::Recovered(fields) => fields.clone(),
    };
    *last = Some(adjusted);
    if matches!(decision, ClampReport::Recovered(_)) {
        eprintln!(
            "✓ policy: all values now in range (previously clamped: {})",
            fields.join(", ")
        );
    } else {
        // Silent clamping is how DECIDE #8 was allowed to look like a
        // decision; say what moved so an operator can fix the file.
        eprintln!(
            "⚠ policy: out-of-range value(s) clamped to their legal range: {}",
            fields.join(", ")
        );
    }
}

pub(crate) fn normalize_storage_policy(storage: &mut StoragePolicy) -> Vec<&'static str> {
    let mut adjusted = Vec::new();
    // 0 would make every file — including empty ones — a hotspot candidate,
    // and the resulting report is unreadable rather than wrong.
    if storage.min_size_mb < 1 {
        adjusted.push("storage.min_size_mb");
        storage.min_size_mb = 1;
    }
    adjusted
}

/// Apply every documented legal range to a guard policy, returning the names
/// of the fields that were clamped (empty when the policy was already legal).
pub(crate) fn normalize_guard_policy(policy: &mut GuardPolicy) -> Vec<&'static str> {
    normalize_guard_policy_with_home(policy, dirs::home_dir().as_deref())
}

/// `normalize_guard_policy` with an explicit home, so the `~` expansion can be
/// tested without mutating the process-wide `$HOME` (which would race every
/// other test in the binary). Mirrors `expand_tilde_with_home`,
/// `resolve_policy_path_with_home` and `resolve_guard_log_path_with_home`.
pub(crate) fn normalize_guard_policy_with_home(
    policy: &mut GuardPolicy,
    home: Option<&Path>,
) -> Vec<&'static str> {
    let mut adjusted: Vec<&'static str> = Vec::new();

    // FIXED 2026-10-02 (audit HIGH): `protected_paths` was the ONLY path-valued
    // guard knob that never had `~` expanded. Every sibling — quarantine_dir,
    // relocate_cold_root, relocate_candidate_roots, rust_search_roots, the guard
    // log path — expands at point of use, so `~/Dev/...` worked for all of them.
    // Here the raw string went straight to `Path::canonicalize`, which resolves
    // `~` against the process CWD, failed with NotFound, and was silently
    // skipped — so a `~` entry parsed, reported as configured, and protected
    // NOTHING. The config was inert while the absolute-path test stayed green,
    // which is precisely the failure the design doc warns about.
    //
    // Expanding here rather than at point of use keeps this the single
    // normalization boundary and means the value is correct for every consumer,
    // not just the one that happens to call expand_tilde. Idempotent: an entry
    // that is already absolute is left byte-identical.
    let expanded_protected: Vec<String> = policy
        .protected_paths
        .iter()
        .map(|p| expand_tilde_with_home(p.trim(), home).display().to_string())
        .collect();
    if expanded_protected != policy.protected_paths {
        adjusted.push("protected_paths");
        policy.protected_paths = expanded_protected;
    }

    // Each knob takes exactly one of four shapes:
    //   floor!  — below this the daemon misbehaves; raise it.
    //   ceil!   — above this the knob is meaningless or unbounded; cap it.
    //   band!   — both ends are meaningful.
    //   fband!  — the float form. Deliberately unconditional, see below.
    macro_rules! floor {
        ($field:ident, $min:expr) => {{
            let min = $min;
            if policy.$field < min {
                adjusted.push(stringify!($field));
                policy.$field = min;
            }
        }};
    }
    macro_rules! ceil {
        ($field:ident, $max:expr) => {{
            let max = $max;
            if policy.$field > max {
                adjusted.push(stringify!($field));
                policy.$field = max;
            }
        }};
    }
    macro_rules! band {
        ($field:ident, $min:expr, $max:expr) => {{
            let (min, max) = ($min, $max);
            if policy.$field < min || policy.$field > max {
                adjusted.push(stringify!($field));
                policy.$field = policy.$field.max(min).min(max);
            }
        }};
    }
    // TOML accepts `nan` and `inf` float literals and serde maps them
    // straight through. A NaN fails EVERY comparison, so a `>`/`max` guard
    // would let it through and silently disable the threshold it guards.
    // f32/f64::max and ::min return the non-NaN operand, so assigning
    // unconditionally is what actually scrubs NaN and ±inf.
    macro_rules! fband {
        ($field:ident, $min:expr, $max:expr) => {{
            let (min, max) = ($min, $max);
            let before = policy.$field;
            let after = before.max(min).min(max);
            if after != before {
                adjusted.push(stringify!($field));
                policy.$field = after;
            }
        }};
    }

    // --- cadence ---------------------------------------------------------
    // 0 would busy-loop the guard with no sleep between passes.
    floor!(interval_secs, 5);
    floor!(auto_cleanup_interval_secs, 60);
    floor!(report_repeat_secs, 60);
    floor!(proactive_cleanup_interval_cycles, 1);

    // --- disk bands ------------------------------------------------------
    band!(disk_warn_percent, 1, 100);
    // ADDED 2026-10-01 (audit): every other percent threshold is banded 1..100
    // precisely because 0 means "match on every sample". Here 0 made the
    // early band (`used >= early && used < warn`) cover the whole filesystem,
    // so the guard emitted its early warning on every single pass.
    band!(disk_early_warn_percent, 1, 100);
    // ADDED 2026-09-09 (audit F43): an early-warn above warn made the
    // early band (`used >= early && used < warn`) permanently empty —
    // the operator's early-warning config silently did nothing.
    if policy.disk_early_warn_percent > policy.disk_warn_percent {
        adjusted.push("disk_early_warn_percent");
        policy.disk_early_warn_percent = policy.disk_warn_percent;
    }
    if policy.disk_action_percent < policy.disk_warn_percent {
        adjusted.push("disk_action_percent");
        policy.disk_action_percent = policy.disk_warn_percent;
    }
    if policy.disk_critical_percent < policy.disk_action_percent {
        adjusted.push("disk_critical_percent");
        policy.disk_critical_percent = policy.disk_action_percent;
    }
    if policy.disk_action_percent > 100 {
        adjusted.push("disk_action_percent");
        policy.disk_action_percent = 100;
    }
    if policy.disk_critical_percent > 100 {
        adjusted.push("disk_critical_percent");
        policy.disk_critical_percent = 100;
    }
    // Both of these are "start acting before action" gates; landing on or
    // above disk_action_percent would make them fire at the same level and
    // leave the band they exist to cover empty.
    if policy.proactive_cleanup_percent >= policy.disk_action_percent {
        adjusted.push("proactive_cleanup_percent");
        // max(1) for the same reason as unfreeze_below_percent: a 0 band here
        // would run the proactive cleanup on every pass (2026-10-01 audit).
        policy.proactive_cleanup_percent = policy.disk_action_percent.saturating_sub(1).max(1);
    }
    // A 0 here would mean "unfreeze only at 0% used", which never happens on
    // a real filesystem, so the freeze marker would only ever be cleared by
    // the external freeze watchdog — a foot-gun the 2026-10-01 audit found
    // had no lower bound at all.
    band!(unfreeze_below_percent, 1, 100);
    if policy.unfreeze_below_percent >= policy.disk_action_percent {
        adjusted.push("unfreeze_below_percent");
        // max(1) keeps the floor even for a pathological action threshold of 1.
        policy.unfreeze_below_percent = policy.disk_action_percent.saturating_sub(1).max(1);
    }
    // Percent thresholds whose top end is meaningful (100 = "warn only when
    // full") but whose 0 would mean "alert on every sample".
    band!(inode_warn_percent, 1, 100);
    band!(mem_available_warn_percent, 1, 100);
    band!(swap_used_warn_percent, 1, 100);

    // --- process thresholds ----------------------------------------------
    // Upper bound is deliberately generous: per-process CPU is a percentage
    // of ONE core, so a 32-core process legitimately reads 3200%.
    fband!(process_cpu_percent, 1.0, 100_000.0);
    floor!(process_rss_mb, 64);
    floor!(process_sustain_secs, 5);
    // A stuck threshold below the sustain threshold makes "stuck" fire on
    // every process that merely became heavy.
    if policy.process_stuck_after_secs < policy.process_sustain_secs {
        adjusted.push("process_stuck_after_secs");
        policy.process_stuck_after_secs = policy.process_sustain_secs;
    }
    // Outside 0..=19 the renice exec fails outright; negative would try to
    // raise priority, inverting the tier-floor contract.
    band!(renice_value, RENICE_VALUE_MIN, RENICE_VALUE_MAX);
    // 0 would un-renice on the very next pass after a renice, so a heavy
    // process would be reniced and restored every cycle.
    floor!(release_after_secs, 5);
    floor!(notify_cooldown_secs, 5);
    // ADDED 2026-10-01 (audit): no ceiling meant a huge value reached
    // `cleanup_stale_cooldowns`, which computes a cutoff at 2x the cooldown
    // with `Instant - Duration`. At >= 2^63 seconds that subtraction panics
    // and kills the daemon on its first pass; below that it silently disables
    // cooldown pruning and notification suppression in practice (nothing is
    // ever "stale"). One day is far beyond any useful notify cooldown.
    ceil!(notify_cooldown_secs, 86_400);

    // --- memory pressure -------------------------------------------------
    // A NaN mem_psi_full_warn fails every comparison, so the pressure state
    // machine would never leave "clear" and no mitigation would ever run.
    fband!(mem_psi_full_warn, 0.0, 100.0);
    floor!(memory_pressure_sustain_secs, 30);
    // systemd CPUQuota accepts values above 100%, but this knob is a cap
    // expressed as a percentage of one CPU. Keep invalid values from
    // reaching the per-pass cap loop, where they would fail and retry for
    // every offender on every interval.
    ceil!(cap_offenders_cpu_percent, 100);

    // --- disk trends -----------------------------------------------------
    fband!(disk_rapid_fill_gbph, 0.5, 100_000.0);
    // trend_warn_hours: 0 is a sentinel (SENTINEL_ZERO_KNOBS), no clamp.

    // --- logging ---------------------------------------------------------
    // 0 is NOT "unlimited" here: rotate_guard_log_if_oversized returns early
    // on a 0 cap, so it silently disables rotation and the event log grows
    // without bound until the disk guard notices it.
    floor!(guard_log_max_mb, 1);
    floor!(log_max_truncate_mb, 1);
    ceil!(log_preserve_header_lines, LOG_PRESERVE_HEADER_LINES_MAX);
    // log_size_mb: 0 is a sentinel (SENTINEL_ZERO_KNOBS), no clamp.

    // --- cleanup thresholds ----------------------------------------------
    // 0 would make every Rust target — including a 0-byte one — a candidate.
    floor!(cleanup_min_size_mb, 1);
    floor!(rust_target_max_age_days, 1);
    // node_modules_max_age_days, tmp_min_age_hours, trash_min_age_days and
    // rust_target_action_min_age_days: 0 is a sentinel, no clamp.
    // nix_keep_generations: 0 is a sentinel; no upper bound is meaningful
    // (nix treats "+N" as keep N, and a large N is merely conservative).

    // --- quarantine / relocation -----------------------------------------
    // quarantine_ttl_days: 0 is a sentinel (never expire), no clamp.
    // quarantine_expire_interval_secs: 0 is a sentinel (never expire
    // automatically), no clamp. Flooring either would silently RE-ENABLE
    // unattended deletion for an operator who explicitly opted out, which is
    // the opposite failure from the one a floor guards against.
    floor!(relocate_min_size_mb, 1);
    // relocate_min_age_days: 0 is a sentinel (fresh dirs are candidates).
    floor!(relocate_max_moves_per_pass, 1);
    // reap_report_*: both 0s are sentinels, no clamp.

    // --- empty string fallbacks ------------------------------------------
    if policy.sync_freeze_marker.trim().is_empty() {
        adjusted.push("sync_freeze_marker");
        policy.sync_freeze_marker = default_sync_freeze_marker();
    }
    if policy.quarantine_dir.trim().is_empty() {
        adjusted.push("quarantine_dir");
        policy.quarantine_dir = default_quarantine_dir();
    }
    if policy.notify_command.trim().is_empty() {
        adjusted.push("notify_command");
        policy.notify_command = default_notify_command();
    }

    adjusted
}

// ---------------------------------------------------------------------------
// Utility / formatting helpers
// ---------------------------------------------------------------------------

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut idx = 0usize;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    format!("{value:.1} {}", UNITS[idx])
}

pub(crate) fn canonical_system_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join(".dracon")
}

fn expand_tilde_with_home(raw: &str, home: Option<&Path>) -> PathBuf {
    if raw == "~" {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(".").join(rest));
    }
    PathBuf::from(raw)
}

pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    expand_tilde_with_home(raw, dirs::home_dir().as_deref())
}

/// Expand `~` AND make the result absolute, resolving a relative policy path
/// against the user's home rather than the process's current directory.
///
/// ADDED 2026-10-01 (audit). The shipped unit sets `WorkingDirectory=%h` with
/// the explicit comment "Keep any intentionally relative policy paths stable
/// for the user service", so a relative path in the policy means "under $HOME".
/// `setup --apply` used the plain lexical expansion, so `relocate_cold_root =
/// "cold"` created `./cold` under the invoking shell's CWD and reported ready,
/// while the daemon — running with CWD=$HOME — looked for `~/cold`, warned
/// "cold root missing — run dracon-system setup" on every pass, and any future
/// canonicalised nesting check disagreed with the lexical one. Both call sites
/// now resolve through here, so the two agree by construction.
pub(crate) fn resolve_policy_path(raw: &str) -> PathBuf {
    let expanded = expand_tilde(raw);
    if expanded.is_absolute() {
        return expanded;
    }
    match dirs::home_dir() {
        Some(home) => home.join(expanded),
        // No home to resolve against: keep the lexical path rather than
        // inventing a root-relative one.
        None => expanded,
    }
}

/// Resolve the optional guard event-log path using the same config-path
/// semantics at every call site. A blank value disables persistent logging;
/// `~` and `~/...` refer to the service user's home directory. Non-empty
/// relative paths remain relative to the process working directory; the
/// shipped user service pins that directory to the user's home.
pub(crate) fn resolve_guard_log_path(raw: &str) -> Option<PathBuf> {
    resolve_guard_log_path_with_home(raw, dirs::home_dir().as_deref())
}

pub(crate) fn resolve_guard_log_path_with_home(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(expand_tilde_with_home(raw, home))
}

/// Keys the policy parser accepts, derived from the structs THEMSELVES.
///
/// Serializing a default emits every field, because every field carries a
/// serde default and none carries `skip_serializing_if`. That makes this
/// set exact and self-maintaining: adding a knob to `GuardPolicy` adds it
/// here automatically, so a hand-kept list of key names — the obvious
/// implementation, and one that silently rots — is not needed.
///
/// One gap needs closing. TOML cannot represent a null, so serializing a
/// default OMITS every `Option` field (there is no "absent" distinct from
/// "null" to write). Left unhandled, a perfectly valid `log_dirs = "..."`
/// would be reported as an unknown key on every load — a false positive that
/// would train the operator to ignore the warning. `option_field_keys`
/// supplies those; it is the one place that needs a human, and
/// `option_field_keys_cover_every_option_field` fails if a new `Option` is
/// added without adding it there.
fn known_keys_of<T: Serialize + Default>(extra: &[&str]) -> HashSet<String> {
    let mut keys: HashSet<String> = toml::to_string(&T::default())
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|v| v.as_table().cloned())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    keys.extend(extra.iter().map(|k| k.to_string()));
    keys
}

/// `Option` fields, which TOML serialization omits when they are `None`.
pub(crate) const GUARD_OPTION_FIELDS: &[&str] = &["log_dirs"];

/// Every key the policy file accepts, per top-level section.
pub(crate) fn known_policy_keys() -> HashMap<&'static str, HashSet<String>> {
    let mut map = HashMap::new();
    map.insert("guard", known_keys_of::<GuardPolicy>(GUARD_OPTION_FIELDS));
    map.insert("storage", known_keys_of::<StoragePolicy>(&[]));
    map.insert("links", known_keys_of::<LinkPolicy>(&[]));
    map
}

/// Unknown keys in a parsed policy document, as `section.key` strings.
///
/// A misspelled knob, a key appended under the wrong table, or a section
/// that does not exist are all silently dropped by serde today: the operator
/// sees a config that looks applied and is not. This surfaces them WITHOUT
/// failing, because a stray key must not stop a monitoring daemon from
/// starting — the operator needs to be told, not locked out of their machine.
pub(crate) fn unknown_policy_keys(doc: &toml::Value) -> Vec<String> {
    let Some(table) = doc.as_table() else {
        return Vec::new();
    };
    let known = known_policy_keys();
    let mut unknown = Vec::new();
    for (section, value) in table {
        match known.get(section.as_str()) {
            None => unknown.push(section.clone()),
            Some(accepted) => {
                // A section given as a bare key rather than a table is a
                // shape error, not an unknown key; report it as unknown so
                // it is not silently accepted as a no-op.
                let Some(inner) = value.as_table() else {
                    unknown.push(section.clone());
                    continue;
                };
                for key in inner.keys() {
                    if !accepted.contains(key) {
                        unknown.push(format!("{section}.{key}"));
                        continue;
                    }
                    // ADDED 2026-10-01 (audit): one more level, for the only
                    // section with a nested table. A typo inside a
                    // `[[links.entries]]` body was accepted silently while the
                    // same typo one level up was reported, which contradicts
                    // the template's promise that an unrecognised key is at
                    // least NAMED. `entries` is the single known nested key, so
                    // the check is explicit rather than recursive.
                    if section == "links" && key == "entries" {
                        unknown.extend(unknown_link_entry_keys(value));
                    }
                }
            }
        }
    }
    unknown.sort();
    unknown
}

/// Keys accepted inside one `[[links.entries]]` table. Derived from the struct
/// so it cannot drift from the real shape (audit 2026-10-01).
fn known_link_entry_keys() -> HashSet<String> {
    // LinkEntry has no Default (both fields are required), so serialise a
    // concrete instance rather than `T::default()`.
    toml::to_string(&LinkEntry {
        link: String::new(),
        target: String::new(),
    })
    .ok()
    .and_then(|text| text.parse::<toml::Value>().ok())
    .and_then(|v| v.as_table().cloned())
    .map(|t| t.keys().cloned().collect())
    .unwrap_or_default()
}

/// Unknown keys inside `[[links.entries]]` bodies, as `links.entries.<key>`.
fn unknown_link_entry_keys(links_value: &toml::Value) -> Vec<String> {
    let Some(entries) = links_value
        .as_table()
        .and_then(|t| t.get("entries"))
        .and_then(|e| e.as_array())
    else {
        return Vec::new();
    };
    let accepted = known_link_entry_keys();
    let mut unknown = Vec::new();
    for entry in entries {
        let Some(table) = entry.as_table() else {
            // A non-table entry is a shape error; report the key itself so it is
            // not silently dropped.
            unknown.push("links.entries".to_string());
            continue;
        };
        for key in table.keys() {
            if !accepted.contains(key) {
                unknown.push(format!("links.entries.{key}"));
            }
        }
    }
    unknown
}

/// Human-readable guidance for a rejected key, used in the warning.
///
/// Reports BOTH a near-miss spelling and, when the key is spelled correctly
/// but filed under the wrong table, the section it actually belongs to —
/// that misplacement is the exact confusion this check exists to surface.
pub(crate) fn hint_for(key: &str) -> String {
    let (section, base) = match key.split_once('.') {
        Some((s, b)) => (Some(s), b),
        None => (None, key),
    };
    let known = known_policy_keys();

    // Exact name, different table: the key is real but misplaced.
    for (name, candidates) in &known {
        if Some(*name) == section {
            continue;
        }
        if candidates.contains(base) {
            return format!(" — `{base}` belongs under [{name}]");
        }
    }

    // ADDED 2026-10-01 (audit): a mistyped SECTION name gets no hint at all,
    // because only key names were ever considered. `guards` for `[guard]` is
    // the single most common config typo and the nearest key is 4 edits away,
    // so the operator got a bare "unknown key: guards" with nothing to act on.
    // Section names are now candidates, but only when the key was NOT found
    // under a dotted path (i.e. it is a bare section name).
    if section.is_none() {
        let mut best = usize::MAX;
        let mut close_sections: Vec<&str> = Vec::new();
        for name in known.keys() {
            let d = edit_distance(base, name);
            if d < best {
                best = d;
                close_sections.clear();
            }
            if d == best && d <= 2 {
                close_sections.push(name);
            }
        }
        if !close_sections.is_empty() {
            return format!(" — did you mean [{}]?", close_sections.join("] or ["));
        }
    }

    // Otherwise a near-miss spelling is the useful hint.
    //
    // FAMILY-AWARE (audit 2026-10-01): some knobs come in families whose
    // members are DIFFERENT policy decisions with confusable names — the
    // rust-target gates are `rust_target_max_age_days` (what counts as a
    // target) and `rust_target_action_min_age_days` (what to clean), and a raw
    // edit distance pointed a mistyped action-tier key at the max gate, which
    // silently changes what the guard does. So: prefer a same-tier near miss;
    // otherwise never suggest a DIFFERENT tier of the SAME family; otherwise
    // list the family's real members rather than picking one; and only fall back
    // to plain edit distance across families.
    let typed_tier = policy_tier(base);
    let typed_family = policy_family(base);
    let mut best = usize::MAX;
    let mut best_same_tier = usize::MAX;
    let mut close: Vec<&str> = Vec::new();
    let mut close_same_tier: Vec<&str> = Vec::new();
    let mut family_members: Vec<&str> = Vec::new();
    for candidates in known.values() {
        for candidate in candidates {
            let d = edit_distance(base, candidate);
            if typed_family.is_some() && policy_family(candidate) == typed_family {
                family_members.push(candidate);
            }
            if typed_tier.is_some() && policy_tier(candidate) == typed_tier {
                if d < best_same_tier {
                    best_same_tier = d;
                    close_same_tier.clear();
                }
                if d == best_same_tier && d <= 2 {
                    close_same_tier.push(candidate);
                }
                continue;
            }
            // A sibling tier of the same family is never a safe suggestion.
            if typed_family.is_some() && policy_family(candidate) == typed_family {
                continue;
            }
            if d < best {
                best = d;
                close.clear();
            }
            if d == best && d <= 2 {
                close.push(candidate);
            }
        }
    }
    if !close_same_tier.is_empty() {
        return format!(" — did you mean {}?", close_same_tier.join(" or "));
    }
    family_members.sort_unstable();
    family_members.dedup();
    if !family_members.is_empty() && family_members.len() <= 4 {
        return format!(
            " — did you mean one of {}? (they are different settings)",
            family_members.join(", ")
        );
    }
    if close.is_empty() {
        String::new()
    } else {
        format!(" — did you mean {}?", close.join(" or "))
    }
}

/// The specific knob a name refers to, inside its family. Two keys in the same
/// family with different tiers are different policy decisions, so a suggestion
/// must not cross from one to the other.
fn policy_tier(key: &str) -> Option<&str> {
    if let Some(rest) = key.strip_prefix("rust_target") {
        return Some(match rest {
            "_max_age_days" => "rust_target_max",
            "_action_min_age_days" => "rust_target_action",
            _ => "rust_target_other",
        });
    }
    // `trash_min_age_days` and `trash_max_age_days` bound the same decision
    // (how old is too old for this kind of file), so they share a tier.
    for suffix in ["_min_age_days", "_max_age_days"] {
        if key.ends_with(suffix) {
            return Some(key);
        }
    }
    None
}

/// The family a name belongs to, used to refuse cross-tier suggestions and to
/// list the real members when no single one can be picked.
fn policy_family(key: &str) -> Option<&str> {
    if let Some(rest) = key.strip_prefix("rust_target") {
        if rest.ends_with("_age_days") {
            return Some("rust_target");
        }
        return None;
    }
    for suffix in ["_min_age_days", "_max_age_days"] {
        if let Some(prefix) = key.strip_suffix(suffix) {
            return Some(prefix);
        }
    }
    None
}

/// Levenshtein distance, bounded by the lengths involved. Only used to
/// suggest a near-miss spelling, so a full table is unnecessary.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

pub(crate) fn parse_kinds(csv: &str) -> HashSet<String> {
    csv.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Hotspot kinds that are REPORT-ONLY and can never be selected for
/// deletion by `storage --cleanup --apply` (audit M2, 2026-08-21).
///
/// `git-db` (.git directories) is project history, not a regenerable
/// artifact: deleting it loses every commit, stash, and ref of a repo.
/// The normal safety nets do not cover it — `is_git_tracked_dir(".git")`
/// is always false (git never tracks its own database), so the
/// `allow_tracked` gate would happily pass it through, and the guard
/// classifier only knows about system roots / user-protected paths /
/// symlinks. Exclusion therefore happens at kind-selection level AND as
/// a path-level backstop in `validate_storage_cleanup_path`.
pub(crate) const NON_CLEANUP_KINDS: &[&str] = &["git-db"];

/// Partition requested cleanup kinds into selectable vs report-only.
/// Returns `(kept, excluded)` where `excluded` lists the rejected kinds
/// in `NON_CLEANUP_KINDS`, deduplicated and sorted.
pub(crate) fn filter_selectable_cleanup_kinds(
    requested: HashSet<String>,
) -> (HashSet<String>, Vec<String>) {
    let excluded: Vec<String> = NON_CLEANUP_KINDS
        .iter()
        .filter(|k| requested.contains(**k))
        .map(|k| (*k).to_string())
        .collect();
    let kept = requested
        .into_iter()
        .filter(|k| !NON_CLEANUP_KINDS.contains(&k.as_str()))
        .collect();
    (kept, excluded)
}
