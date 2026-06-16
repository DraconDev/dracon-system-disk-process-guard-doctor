use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Policy structs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct SystemPolicy {
    #[serde(default)]
    pub(crate) storage: StoragePolicy,
    #[serde(default)]
    pub(crate) links: LinkPolicy,
    #[serde(default)]
    pub(crate) guard: GuardPolicy,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LinkPolicy {
    #[serde(default)]
    pub(crate) entries: Vec<LinkEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LinkEntry {
    pub(crate) link: String,
    pub(crate) target: String,
}

#[derive(Debug, Clone, Deserialize)]
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
    #[serde(default = "default_true")]
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
    #[serde(default = "default_true")]
    pub(crate) notify: bool,
    #[serde(default = "default_notify_command")]
    pub(crate) notify_command: String,
    #[serde(default = "default_notify_cooldown_secs")]
    pub(crate) notify_cooldown_secs: u64,
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
    #[serde(default = "default_true")]
    pub(crate) monitor_logs: bool,
    #[serde(default = "default_log_size_mb")]
    pub(crate) log_size_mb: u64,
    #[serde(default = "default_log_dirs")]
    pub(crate) log_dirs: String,
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
    #[serde(default = "default_true")]
    pub(crate) clean_package_caches: bool,
    #[serde(default = "default_true")]
    pub(crate) clean_trash: bool,
    #[serde(default = "default_true")]
    pub(crate) clean_nix_garbage: bool,
    #[serde(default = "default_nix_keep_generations")]
    pub(crate) nix_keep_generations: u32,
    #[serde(default = "default_node_modules_max_age_days")]
    pub(crate) node_modules_max_age_days: u64,
    #[serde(default)]
    pub(crate) protected_paths: Vec<String>,
    #[serde(default = "default_proactive_cleanup_percent")]
    pub(crate) proactive_cleanup_percent: u8,
    #[serde(default = "default_rust_target_max_age_days")]
    pub(crate) rust_target_max_age_days: u64,
    #[serde(default = "default_proactive_cleanup_interval_cycles")]
    pub(crate) proactive_cleanup_interval_cycles: u64,
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
            freeze_sync_at_action: default_true(),
            sync_freeze_marker: default_sync_freeze_marker(),
            unfreeze_below_percent: default_unfreeze_below_percent(),
            process_cpu_percent: default_process_cpu_percent(),
            process_rss_mb: default_process_rss_mb(),
            process_sustain_secs: default_process_sustain_secs(),
            process_exempt_names: default_process_exempt_names(),
            notify: default_true(),
            notify_command: default_notify_command(),
            notify_cooldown_secs: default_notify_cooldown_secs(),
            auto_renice: false,
            renice_value: default_renice_value(),
            release_after_secs: default_release_after_secs(),
            guard_log_file: default_guard_log_file(),
            guard_log_max_mb: default_guard_log_max_mb(),
            auto_cleanup_rust: default_auto_cleanup_rust(),
            auto_cleanup_apply: false,
            cleanup_min_size_mb: default_cleanup_min_size_mb(),
            rust_search_roots: default_rust_search_roots(),
            node_modules_search_roots: default_node_modules_search_roots(),
            track_trends: default_true(),
            trend_warn_hours: default_trend_warn_hours(),
            monitor_inodes: default_true(),
            inode_warn_percent: default_inode_warn_percent(),
            monitor_zombies: default_true(),
            zombie_threshold: default_zombie_threshold(),
            monitor_logs: default_true(),
            log_size_mb: default_log_size_mb(),
            log_dirs: default_log_dirs(),
            auto_truncate_logs: false,
            log_max_truncate_mb: default_log_max_truncate_mb(),
            log_preserve_header_lines: 0,
            docker_prune: default_true(),
            docker_prune_volumes: false,
            clean_package_caches: default_true(),
            clean_trash: default_true(),
            clean_nix_garbage: default_true(),
            nix_keep_generations: 5,
            node_modules_max_age_days: default_node_modules_max_age_days(),
            protected_paths: Vec::new(),
            proactive_cleanup_percent: default_proactive_cleanup_percent(),
            rust_target_max_age_days: default_rust_target_max_age_days(),
            proactive_cleanup_interval_cycles: default_proactive_cleanup_interval_cycles(),
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

pub(crate) fn default_process_exempt_names() -> String {
    "systemd,dbus-daemon,Xorg,kwin_wayland,plasmashell".to_string()
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

pub(crate) fn default_notify_cooldown_secs() -> u64 {
    300
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

fn default_log_size_mb() -> u64 {
    100
}

fn default_log_max_truncate_mb() -> u64 {
    50
}

fn default_log_dirs() -> String {
    String::new()
}

pub(crate) fn default_node_modules_max_age_days() -> u64 {
    30
}

pub(crate) fn default_nix_keep_generations() -> u32 {
    5
}

pub(crate) fn default_proactive_cleanup_percent() -> u8 {
    50
}

pub(crate) fn default_rust_target_max_age_days() -> u64 {
    14
}

pub(crate) fn default_proactive_cleanup_interval_cycles() -> u64 {
    120
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

pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    PathBuf::from(raw)
}

pub(crate) fn parse_kinds(csv: &str) -> HashSet<String> {
    csv.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}
