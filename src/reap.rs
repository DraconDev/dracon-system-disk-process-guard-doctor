//! Report-only detection of abandoned dev/test processes.
//!
//! Test runs launch dev servers (`vite`, `eve web`, Playwright/Chromium,
//! `bun run <script>`) as children. When the run is killed rather than
//! completing -- a `timeout` deadline, a SIGKILL, a crashed agent -- those
//! children are reparented to `systemd --user` and keep listening forever.
//! Nothing reaps them, so every interrupted run adds another layer.
//!
//! This module finds them and *reports* them. It deliberately contains no
//! signal-sending, no killing, and no child-management: a diagnostic that
//! can end a process is a different tool with a different blast radius,
//! and the guard's contract is that it never ends anything itself. The
//! output is a worklist a human decides on.
//!
//! Every criterion below is a precondition for reporting, and each one
//! exists to rule out a live interactive session:
//!
//! - state `S` (sleeping) -- a process on-CPU is doing work, not abandoned
//! - `tty_nr == 0` -- no controlling terminal, so no shell or tmux pane
//!   owns it. This is the single most important filter: it is what keeps
//!   idle-but-real work (a parked editor, a background `pi` session) out
//!   of the report.
//! - near-zero lifetime CPU -- idle by measurement, not by assumption
//! - long uptime -- abandoned, not young
//! - an explicit allowlist signature -- never a generic "old process"
//!
//! The allowlist is the operator's: a process is only ever a candidate
//! because its command line matches a signature they configured.

use serde::Serialize;
use std::path::Path;

/// Defaults for the reporting thresholds. `min_idle_hours` is the floor
/// for "abandoned" -- a dev server that has been idle for a day is not
/// something anybody is about to look at.
pub(crate) const DEFAULT_REAP_REPORT_MIN_IDLE_HOURS: u64 = 24;

/// Lifetime CPU, in seconds, at or below which a process counts as idle.
/// A day-old server that burned 5s of CPU at startup is idle; one that
/// served requests all day is not.
pub(crate) const DEFAULT_REAP_REPORT_MAX_CPU_SECONDS: u64 = 60;

/// Signatures that mark a process as disposable test/dev infrastructure.
/// Comma-separated in the TOML. Deliberately specific: the default is
/// narrow enough that a shell, an editor, or an agent session can never
/// match by accident.
pub(crate) const DEFAULT_REAP_REPORT_SIGNATURES: &str =
    "eve web,vite,playwright,chromium,bun run,bun test,npm run test,.mjs,litestream,stub-auth-api";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReapPolicy {
    pub(crate) min_idle_hours: u64,
    pub(crate) max_cpu_seconds: u64,
    pub(crate) signatures: Vec<String>,
    pub(crate) exempt_names: Vec<String>,
}

/// Split a comma-separated list into a deterministic, sorted Vec.
///
/// `parse_kinds` returns a HashSet, which would make the *reported*
/// signature depend on hash order when two entries both match. Sorting
/// keeps consecutive guard reports comparable.
pub(crate) fn sorted_signatures(csv: &str) -> Vec<String> {
    let mut out: Vec<String> = crate::parse_kinds(csv).into_iter().collect();
    out.sort();
    out
}

impl Default for ReapPolicy {
    fn default() -> Self {
        ReapPolicy {
            min_idle_hours: DEFAULT_REAP_REPORT_MIN_IDLE_HOURS,
            max_cpu_seconds: DEFAULT_REAP_REPORT_MAX_CPU_SECONDS,
            signatures: sorted_signatures(DEFAULT_REAP_REPORT_SIGNATURES),
            exempt_names: Vec::new(),
        }
    }
}

/// One process that looks abandoned, with the evidence that got it here.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ReapCandidate {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    /// Full command line, so the operator can tell a stale audit server
    /// from the one they are actively using.
    pub(crate) args: String,
    pub(crate) idle_hours: u64,
    /// Lifetime CPU seconds. Small, by construction.
    pub(crate) cpu_seconds: u64,
    pub(crate) rss_mb: u64,
    /// The allowlist entry that matched.
    pub(crate) signature: String,
}

/// `/proc/stat`'s `btime`: seconds since the epoch at which the clock
/// started counting, which is what `starttime` in `/proc/<pid>/stat` is
/// relative to.
pub(crate) fn read_boot_time(proc_root: &Path) -> Option<u64> {
    let stat = std::fs::read_to_string(proc_root.join("stat")).ok()?;
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The subset of `/proc/<pid>/stat` this module needs. Field numbers are
/// the kernel's 1-indexed ones, so they read the same as the man page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatFields {
    /// 3: R/S/D/Z/T/...
    pub(crate) state: char,
    /// 7: controlling terminal; 0 means none.
    pub(crate) tty_nr: i32,
    /// 14 + 15: user + system CPU, in clock ticks, for the whole life of
    /// the process.
    pub(crate) cpu_ticks: u64,
    /// 22: start time, in clock ticks since boot.
    pub(crate) starttime: u64,
}

/// Parse the fields after the final `)` in `/proc/<pid>/stat`.
///
/// The comm field (2) may contain spaces and parentheses, so the split
/// anchors on the last `)` rather than whitespace. Offsets below are
/// 0-indexed within that tail: field 3 is index 0, so field N is N-3.
pub(crate) fn parse_stat(stat: &str) -> Option<StatFields> {
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let get = |idx: usize| -> Option<&str> { fields.get(idx).copied() };
    Some(StatFields {
        state: get(0)?.chars().next()?,
        tty_nr: get(4)?.parse().ok()?,
        cpu_ticks: get(11)?
            .parse::<u64>()
            .ok()?
            .checked_add(get(12)?.parse::<u64>().ok()?)?,
        starttime: get(19)?.parse().ok()?,
    })
}

/// Seconds a process has existed, given its `starttime` in clock ticks.
pub(crate) fn elapsed_secs(starttime: u64, ticks_per_sec: u64, boot_time: u64, now: u64) -> u64 {
    if ticks_per_sec == 0 {
        return 0;
    }
    let started = boot_time.saturating_add(starttime / ticks_per_sec);
    now.saturating_sub(started)
}

fn matches_signature(args: &str, signatures: &[String]) -> Option<String> {
    signatures
        .iter()
        .find(|needle| !needle.is_empty() && args.contains(needle.as_str()))
        .cloned()
}

fn rss_mb_from_status(pid_dir: &Path) -> u64 {
    let Ok(status) = std::fs::read_to_string(pid_dir.join("status")) else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

/// Scan `proc_root` and return every process that looks abandoned.
///
/// `now` and `ticks_per_sec` are parameters rather than reads so the whole
/// classifier is testable against a fixture proc tree. Results are sorted
/// by RSS descending, then pid, so consecutive reports are comparable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_reap_candidates(
    proc_root: &Path,
    policy: &ReapPolicy,
    boot_time: u64,
    now: u64,
    ticks_per_sec: u64,
) -> Vec<ReapCandidate> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out: Vec<ReapCandidate> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // procfs mixes PIDs with non-numeric control entries (self, net,
        // sys/fs). A PID directory is numeric.
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let pid_dir = entry.path();

        // An unreadable stat is a process that exited during the scan. Skip
        // it; it is not evidence of anything.
        let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
            continue;
        };
        let Some(fields) = parse_stat(&raw) else {
            continue;
        };

        // Only sleeping processes. R is working, D is blocked on IO, Z is
        // already dead, T is stopped: none of those is "abandoned".
        if fields.state != 'S' {
            continue;
        }
        // The decisive filter: no controlling terminal means no shell, tmux
        // pane, or agent session is holding this process.
        if fields.tty_nr != 0 {
            continue;
        }
        // Kernel threads have no cmdline to match and no business here.
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
        let Some(signature) = matches_signature(&args, &policy.signatures) else {
            continue;
        };

        let cpu_seconds = fields.cpu_ticks / ticks_per_sec.max(1);
        if cpu_seconds > policy.max_cpu_seconds {
            continue;
        }
        let age_secs = elapsed_secs(fields.starttime, ticks_per_sec, boot_time, now);
        if age_secs < policy.min_idle_hours.saturating_mul(3600) {
            continue;
        }

        let comm = std::fs::read_to_string(pid_dir.join("comm"))
            .map(|c| c.trim().to_string())
            .unwrap_or_default();
        out.push(ReapCandidate {
            pid,
            comm,
            args,
            idle_hours: age_secs / 3600,
            cpu_seconds,
            rss_mb: rss_mb_from_status(&pid_dir),
            signature,
        });
    }

    out.sort_by(|a, b| b.rss_mb.cmp(&a.rss_mb).then(a.pid.cmp(&b.pid)));
    out
}
