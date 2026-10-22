//! Forgotten heavy orphans — the shape the operator described as
//! "not long running and working but truly heavy and forgotten, like some
//! chrome instances left behind during tests".
//!
//! This is the opposite of the existing orphan pass in `reap.rs`. That one
//! is deliberately ignorant of age, CPU and weight: it exists to catch a
//! HOT YOUNG orphan, the thing that is still burning CPU after its test
//! runner died. This one is heavy and idle, and it only ever considers a
//! process it can show is a TEST job.
//!
//! Two rules carry the whole design:
//!
//! - **A live desktop session is never a candidate.** Not headless and no
//!   test profile means the process belongs to the operator's own session,
//!   whatever its weight. The operator's Chrome is a normal, long-running,
//!   working session; it is not "forgotten" and must never be touched.
//! - **Reporting is unconditional; ending is pressure-gated.** The guard
//!   reports every qualifying process with its evidence on every pass. It
//!   terminates one only under memory pressure, through the existing
//!   `reap_orphans_on_pressure` path, and only after re-verifying every
//!   gate live (including the starttime nonce, so a recycled PID is never
//!   signalled).
//!
//! The module lives beside `reap.rs` rather than inside it because it ends
//! processes: a diagnostic that can end a process is a different tool with
//! a different blast radius, and that discipline is worth keeping visible.

use crate::reap::{parent_comm_is_systemd, parse_stat, ReapPolicy};
use std::path::Path;

/// What a browser-ish command line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopVerdict {
    /// A test job: headless, or running from a temporary/test profile.
    Test { why: String },
    /// A live desktop session — never a candidate.
    Desktop { reason: String },
    /// Not a browser at all.
    NotABrowser,
}

/// The command-line flags that mean "this browser is a test job".
const HEADLESS_MARKERS: &[&str] = &["--headless", "headless_shell"];
/// Path fragments that mean "this profile belongs to a test harness".
const TEST_PROFILE_MARKERS: &[&str] = &[
    "playwright",
    "puppeteer",
    "chromium-",
    "chrome-",
    "headless",
    ".cache",
    "tmp",
];

/// Classify a command line: a test job, a live desktop session, or neither.
///
/// The rule is deliberately one-directional: anything that is NOT headless
/// and does NOT carry a test profile is a desktop session, whatever else it
/// looks like. A heavier, newer or stranger browser that the rules do not
/// recognise is therefore vetoed rather than risked.
pub(crate) fn classify_desktop_session(args: &str) -> DesktopVerdict {
    let lower = args.to_ascii_lowercase();
    if !lower.contains("chrom") {
        return DesktopVerdict::NotABrowser;
    }
    if HEADLESS_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return DesktopVerdict::Test {
            why: "headless".to_string(),
        };
    }
    // A browser with no profile flag at all, or one pointing at the
    // operator's own profile directory, is a desktop session.
    let profile = extract_user_data_dir(&lower);
    match profile {
        None => DesktopVerdict::Desktop {
            reason: "no --user-data-dir: a normal browser session".to_string(),
        },
        Some(path) => {
            if TEST_PROFILE_MARKERS.iter().any(|m| path.contains(m)) {
                DesktopVerdict::Test {
                    why: format!("test profile {path}"),
                }
            } else {
                DesktopVerdict::Desktop {
                    reason: format!("profile {path} is not a test profile"),
                }
            }
        }
    }
}

/// The `--user-data-dir` value from a command line, if it has one.
fn extract_user_data_dir(lower_args: &str) -> Option<String> {
    let idx = lower_args.find("--user-data-dir=")?;
    let rest = &lower_args[idx + "--user-data-dir=".len()..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// A heavy, idle, orphaned process that can be shown to be a test job.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ForgottenOrphan {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    pub(crate) args: String,
    /// Resident plus swapped footprint in MiB.
    pub(crate) footprint_mb: u64,
    pub(crate) rss_mb: u64,
    pub(crate) swap_mb: u64,
    pub(crate) age_secs: u64,
    pub(crate) cpu_seconds: u64,
    /// Why it qualifies: "headless" or "test profile …".
    pub(crate) why: String,
    /// `/proc/<pid>/stat` field 22 at scan time — the kill-time nonce.
    #[serde(skip_serializing)]
    pub(crate) starttime: u64,
}

/// A process that looked heavy and orphaned but was vetoed, with the reason.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct VetoedOrphan {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    pub(crate) footprint_mb: u64,
    pub(crate) reason: String,
}

/// The outcome of one scan: what qualifies, and what was vetoed.
#[derive(Debug, Clone, Default)]
pub(crate) struct ForgottenScan {
    pub(crate) candidates: Vec<ForgottenOrphan>,
    pub(crate) vetoed: Vec<VetoedOrphan>,
}

/// Find the heavy, idle, orphaned test jobs.
///
/// Every criterion below is a precondition, and each one exists to rule
/// out a live interactive session:
///
/// - the owner is provably dead (reparented to init/systemd),
/// - no controlling terminal (no shell, tmux pane or agent session owns it),
/// - sleeping, not working,
/// - idle by measurement: near-zero lifetime CPU,
/// - heavy: resident PLUS swapped footprint above the floor,
/// - and, the decisive one, a command line that classifies as a TEST job.
pub(crate) fn scan_forgotten_heavy_orphans(
    proc_root: &Path,
    policy: &ReapPolicy,
    boot_time: u64,
    now: u64,
    ticks_per_sec: u64,
    footprint_floor_mb: u64,
) -> ForgottenScan {
    let mut out = ForgottenScan::default();
    let self_pid = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        if pid <= 1 || pid == self_pid {
            continue;
        }
        let pid_dir = entry.path();
        let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
            continue;
        };
        let Some(fields) = parse_stat(&raw) else {
            continue;
        };
        // Owner provably dead: reparented to init/systemd.
        if !parent_comm_is_systemd(proc_root, fields.ppid) {
            continue;
        }
        // Detached from any terminal.
        if fields.tty_nr != 0 {
            continue;
        }
        // Sleeping, not working.
        if fields.state != 'S' {
            continue;
        }
        let Ok(cmdline_raw) = std::fs::read_to_string(pid_dir.join("cmdline")) else {
            continue;
        };
        let args = cmdline_raw.replace('\0', " ").trim().to_string();
        if args.is_empty() {
            continue;
        }
        if policy
            .exempt_names
            .iter()
            .any(|exempt| args.contains(exempt.as_str()))
        {
            continue;
        }
        // Idle by measurement, not by assumption.
        let cpu_seconds = fields.cpu_ticks / ticks_per_sec.max(1);
        if cpu_seconds > policy.max_cpu_seconds {
            continue;
        }
        let comm = std::fs::read_to_string(pid_dir.join("comm"))
            .map(|c| c.trim().to_string())
            .unwrap_or_default();
        let rss_mb = crate::reap::rss_mb_from_status(&pid_dir);
        let swap_mb = crate::read_proc_swap_mb(pid).unwrap_or(0);
        let footprint_mb = rss_mb.saturating_add(swap_mb);
        if footprint_mb < footprint_floor_mb {
            continue;
        }
        let age_secs = crate::reap::elapsed_secs(fields.starttime, ticks_per_sec, boot_time, now);
        match classify_desktop_session(&args) {
            DesktopVerdict::Test { why } => out.candidates.push(ForgottenOrphan {
                pid,
                comm,
                args,
                footprint_mb,
                rss_mb,
                swap_mb,
                age_secs,
                cpu_seconds,
                why,
                starttime: fields.starttime,
            }),
            DesktopVerdict::Desktop { reason } => out.vetoed.push(VetoedOrphan {
                pid,
                comm,
                footprint_mb,
                reason,
            }),
            DesktopVerdict::NotABrowser => {}
        }
    }
    out.candidates
        .sort_by_key(|c| std::cmp::Reverse(c.footprint_mb));
    out.vetoed
        .sort_by_key(|c| std::cmp::Reverse(c.footprint_mb));
    out
}
