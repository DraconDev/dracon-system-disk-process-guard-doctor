//! Report-only detection of abandoned dev/test processes.
//!
//! Test runs launch dev servers (`vite`, `eve web`, Playwright/Chromium,
//! `bun run <script>`) as children. When the run is killed rather than
//! completing -- a `timeout` deadline, a SIGKILL, a crashed agent -- those
//! children are reparented to `systemd --user` and keep listening forever.
//! Nothing reaps them, so every interrupted run adds another layer.
//!
//! This module finds them and *reports* them. By default the report is
//! the whole story: a diagnostic that can end a process is a different
//! tool with a different blast radius, and the guard never ends anything
//! unless the operator explicitly opts in with `reap_stale_dev_servers`
//! (age-gated) or `reap_orphans_on_pressure` (memory-pressure orphanhood
//! proof, no age/CPU/state gates). With an opt-in set, the same candidates
//! are re-verified live at kill time (every scan criterion plus a
//! starttime check against PID reuse) and then SIGTERMed, escalating to
//! SIGKILL. The output is a worklist a human decides on -- or, under an
//! opt-in, the audit trail of what the guard decided.
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
    /// `/proc/<pid>/stat` field 22 at scan time. The kill-time
    /// re-verification compares this against the live value so a recycled
    /// PID is never signalled. Skipped in JSON: it is a kill-time nonce,
    /// not report evidence (the report already carries `idle_hours`).
    #[serde(skip_serializing)]
    pub(crate) starttime: u64,
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
    /// 4: parent PID; compared against init/systemd for orphanhood.
    pub(crate) ppid: i32,
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
        ppid: get(1)?.parse().ok()?,
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
            starttime: fields.starttime,
        });
    }

    out.sort_by(|a, b| b.rss_mb.cmp(&a.rss_mb).then(a.pid.cmp(&b.pid)));
    out
}

// --- Opt-in auto-reap ------------------------------------------------------
// Everything below only runs when the operator sets
// `reap_stale_dev_servers = true`. The scan result is stale the moment it
// is collected, so `verify_candidate_for_reap` re-checks every criterion
// against the LIVE tree immediately before `terminate_process` signals
// anything. Any doubt -- an unreadable file, a changed field, a recycled
// PID -- fails closed: the candidate is recorded as unverified and no
// signal is sent.

/// Linux USER_HZ is fixed at 100 on every arch. Shared by the scan and
/// the kill-time re-verification so the two can never disagree on ticks.
pub(crate) const PROC_TICKS_PER_SEC: u64 = 100;

/// How long SIGTERM gets to work before the SIGKILL escalation: 50 polls
/// 100ms apart. A dev server that traps TERM for cleanup finishes in
/// milliseconds; five seconds is already generous.
const TERM_GRACE_POLLS: u32 = 50;

/// ADDED 2026-10-08 (audit F120): wall-clock budget for one reap pass on the
/// blocking thread. The pass is serial and a single kill can consume ~7s of
/// grace polls, so without a budget the pass duration is unbounded in the
/// candidate count (a 97-candidate pass could hold the blocking pool for
/// ~11 minutes). Past the budget, remaining candidates are deferred to the
/// next pass, which re-scans and re-verifies them — nothing is skipped
/// permanently, and the destructive certainty bar is unchanged.
const REAP_PASS_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// How long SIGKILL gets before the kill is declared failed: 20 polls
/// 100ms apart. Only uninterruptible sleep survives SIGKILL, and no
/// amount of waiting fixes that -- the bound just keeps the pass moving.
const KILL_GRACE_POLLS: u32 = 20;
const GRACE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Re-check a scanned candidate against the live `proc_root` immediately
/// before signalling. Every check that admitted the candidate is repeated
/// -- sleeping state, no tty, allowlist signature, exemptions, CPU
/// ceiling -- plus two the scan cannot do: the live `starttime` must
/// equal the scanned one (PID-reuse guard), and reserved PIDs are refused
/// outright. Age is NOT re-checked: a process only gets older, so a pass
/// that cleared the idle floor at scan time still clears it now.
pub(crate) fn verify_candidate_for_reap(
    proc_root: &Path,
    candidate: &ReapCandidate,
    policy: &ReapPolicy,
    ticks_per_sec: u64,
) -> bool {
    // Defence in depth: reserved PIDs and the guard itself are refused
    // even if a crafted allowlist somehow matched them.
    if candidate.pid <= 1 || candidate.pid == std::process::id() as i32 {
        return false;
    }
    let pid_dir = proc_root.join(candidate.pid.to_string());
    let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
        return false;
    };
    let Some(fields) = parse_stat(&raw) else {
        return false;
    };
    // PID reuse: same number, different process. Never signal it.
    if fields.starttime != candidate.starttime {
        return false;
    }
    if fields.state != 'S' || fields.tty_nr != 0 {
        return false;
    }
    if fields.cpu_ticks / ticks_per_sec.max(1) > policy.max_cpu_seconds {
        return false;
    }
    let Ok(cmdline_raw) = std::fs::read_to_string(pid_dir.join("cmdline")) else {
        return false;
    };
    let args = cmdline_raw.replace('\0', " ").trim().to_string();
    if args.is_empty() {
        return false;
    }
    if policy
        .exempt_names
        .iter()
        .any(|exempt| args.contains(exempt.as_str()))
    {
        return false;
    }
    matches_signature(&args, &policy.signatures).is_some()
}

/// What happened when the guard tried to end one process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TerminateOutcome {
    /// The process is gone. `escalated_to_sigkill` tells whether SIGTERM
    /// alone did it or the SIGKILL fallback was needed.
    Signalled { escalated_to_sigkill: bool },
    /// `kill(pid, 0)` reported ESRCH before any signal was sent: the
    /// process exited on its own between verification and termination.
    AlreadyGone,
    /// Deliberately not signalled: a reserved PID, the guard itself, or a
    /// process the guard has no permission to signal.
    Refused { reason: &'static str },
    /// Signalled but still alive afterwards (SIGKILL does not reach
    /// uninterruptible sleep), or the `kill` syscall itself failed.
    Failed { reason: &'static str },
}

/// One candidate the auto-reap pass considered, with what it did about
/// it. `outcome` is `None` when re-verification failed or the pass was
/// cut short: recorded, not silently dropped, but nothing was signalled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReapedProcess {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    pub(crate) signature: String,
    pub(crate) verified: bool,
    pub(crate) outcome: Option<TerminateOutcome>,
    /// FIXED 2026-10-09 (audit F124): before this, a budget-deferred
    /// candidate and a candidate that no longer verified were
    /// byte-identical records (`verified: false`, `outcome: None`), so
    /// the JSON/table trail could not tell "deferred, re-scanned next
    /// pass" from "permanently dropped" — and a PID that WAS signalled
    /// in a later pass had no completed row naming it. Additive field:
    /// `verified`/`outcome` keep their meaning and existing consumers.
    pub(crate) disposition: ReapDisposition,
}

/// Why a candidate ended the pass the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReapDisposition {
    /// A signal was sent, or the process was already gone.
    Signalled,
    /// The pass budget was reached before this candidate was reached; it
    /// is re-scanned and re-verified on the next pass.
    Deferred,
    /// The candidate was re-verified at kill time and no longer met the
    /// gate, so nothing was signalled and nothing further is planned.
    NotVerified,
}

/// True when `pid` names a live process. EPERM means the process exists
/// but belongs to another user; only ESRCH (and its absence from the
/// tree) means gone. A zombie still answers `kill(pid, 0)` but is already
/// dead -- its parent just has not reaped it, and the port, memory, and
/// CPU are all freed -- so zombies count as gone. An unreadable stat
/// falls back to the kill probe: a live process keeps polling rather
/// than being wrongly declared dead.
fn pid_is_alive(pid: i32) -> bool {
    // SAFETY: kill with sig 0 performs no action; it only reports
    // whether the process exists and is signallable.
    let rc = unsafe { libc::kill(pid, 0) };
    let exists = if rc == 0 {
        true
    } else {
        // SAFETY: reading errno immediately after the failed call, same thread.
        unsafe { *libc::__errno_location() != libc::ESRCH }
    };
    if !exists {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(raw) => !matches!(parse_stat(&raw).map(|f| f.state), Some('Z') | Some('X')),
        Err(_) => exists,
    }
}

/// SIGTERM `pid`, escalating to SIGKILL after the grace period. Blocking:
/// the caller runs the whole auto-reap pass on a blocking thread, never
/// on a tokio worker.
pub(crate) fn terminate_process(pid: i32) -> TerminateOutcome {
    if pid <= 1 {
        return TerminateOutcome::Refused {
            reason: "reserved pid",
        };
    }
    if pid == std::process::id() as i32 {
        return TerminateOutcome::Refused {
            reason: "guard will not signal itself",
        };
    }
    if !pid_is_alive(pid) {
        return TerminateOutcome::AlreadyGone;
    }
    // SAFETY: pid is a positive, non-self PID that existed a moment ago.
    // The residual exit-and-reuse race (microseconds between the ESRCH
    // probe and this call) is inherent to kill-by-PID and is why the
    // caller re-verified starttime immediately before; the window cannot
    // be closed further from userspace.
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    if rc != 0 {
        // SAFETY: errno read immediately after the failed call.
        let errno = unsafe { *libc::__errno_location() };
        if errno == libc::ESRCH {
            return TerminateOutcome::AlreadyGone;
        }
        if errno == libc::EPERM {
            return TerminateOutcome::Refused {
                reason: "permission denied",
            };
        }
        return TerminateOutcome::Failed {
            reason: "SIGTERM syscall failed",
        };
    }
    for _ in 0..TERM_GRACE_POLLS {
        if !pid_is_alive(pid) {
            return TerminateOutcome::Signalled {
                escalated_to_sigkill: false,
            };
        }
        std::thread::sleep(GRACE_POLL_INTERVAL);
    }
    // SAFETY: same PID as above; SIGKILL cannot be caught or ignored.
    let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
    if rc != 0 {
        // SAFETY: errno read immediately after the failed call.
        let errno = unsafe { *libc::__errno_location() };
        if errno == libc::ESRCH {
            return TerminateOutcome::Signalled {
                escalated_to_sigkill: false,
            };
        }
        return TerminateOutcome::Failed {
            reason: "SIGKILL syscall failed",
        };
    }
    for _ in 0..KILL_GRACE_POLLS {
        if !pid_is_alive(pid) {
            return TerminateOutcome::Signalled {
                escalated_to_sigkill: true,
            };
        }
        std::thread::sleep(GRACE_POLL_INTERVAL);
    }
    TerminateOutcome::Failed {
        reason: "process survived SIGKILL (uninterruptible sleep?)",
    }
}

/// Run the opt-in auto-reap pass over already-scanned `candidates`:
/// re-verify each one against the live `proc_root`, terminate what still
/// verifies, and record everything -- kills AND skips. Every action is
/// also echoed to stderr so the journal shows it without opening JSON.
/// Blocking (see `terminate_process`): the caller must run this on a
/// blocking thread.
pub(crate) fn auto_reap_stale_servers(
    proc_root: &Path,
    policy: &ReapPolicy,
    candidates: &[ReapCandidate],
    ticks_per_sec: u64,
) -> Vec<ReapedProcess> {
    reap_verified_candidates(
        proc_root,
        policy,
        candidates,
        ticks_per_sec,
        verify_candidate_for_reap,
        "reap",
        REAP_PASS_BUDGET,
    )
}

/// Re-verify pressure orphans at kill time: same liveness discipline as
/// the age-gated path (same PID incarnation, still allowlisted, still
/// detached) but WITHOUT its state/CPU/age gates — a hot young orphan
/// is the exact shape this exists for. The orphanhood proof (parent
/// still init/systemd) replaces the idle proof.
pub(crate) fn verify_orphan_for_reap(
    proc_root: &Path,
    candidate: &ReapCandidate,
    policy: &ReapPolicy,
    ticks_per_sec: u64,
) -> bool {
    let _ = ticks_per_sec;
    if candidate.pid <= 1 || candidate.pid == std::process::id() as i32 {
        return false;
    }
    let pid_dir = proc_root.join(candidate.pid.to_string());
    let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
        return false;
    };
    let Some(fields) = parse_stat(&raw) else {
        return false;
    };
    if fields.starttime != candidate.starttime {
        return false;
    }
    // Owner must STILL be dead: a PID reused under a live parent, or a
    // process reparented by anything but init, is not our kill.
    if !parent_comm_is_systemd(proc_root, fields.ppid) {
        return false;
    }
    if fields.tty_nr != 0 {
        return false;
    }
    let Ok(cmdline_raw) = std::fs::read_to_string(pid_dir.join("cmdline")) else {
        return false;
    };
    let args = cmdline_raw.replace('\0', " ").trim().to_string();
    if args.is_empty() {
        return false;
    }
    if policy
        .exempt_names
        .iter()
        .any(|exempt| args.contains(exempt.as_str()))
    {
        return false;
    }
    matches_signature(&args, &policy.signatures).is_some()
}

/// The pressure-gated orphan pass: same kill-and-record discipline as
/// auto-reap, with orphan verification instead of idle verification.
pub(crate) fn reap_pressure_orphans(
    proc_root: &Path,
    policy: &ReapPolicy,
    candidates: &[ReapCandidate],
    ticks_per_sec: u64,
) -> Vec<ReapedProcess> {
    reap_verified_candidates(
        proc_root,
        policy,
        candidates,
        ticks_per_sec,
        verify_orphan_for_reap,
        "pressure-reap",
        REAP_PASS_BUDGET,
    )
}

pub(crate) fn reap_verified_candidates(
    proc_root: &Path,
    policy: &ReapPolicy,
    candidates: &[ReapCandidate],
    ticks_per_sec: u64,
    verify: fn(&Path, &ReapCandidate, &ReapPolicy, u64) -> bool,
    log_tag: &str,
    budget: std::time::Duration,
) -> Vec<ReapedProcess> {
    let mut out = Vec::with_capacity(candidates.len());
    // ADDED 2026-10-08 (audit F120): the pass runs SERIALLY on a blocking
    // thread and each kill can sleep up to ~7s in grace polls, so an
    // unbounded pass over the 97-process population the design doc
    // describes could hold the blocking pool ~11 minutes and starve every
    // other blocking task (relocation, quarantine, process collection).
    // Cap the pass: candidates past the budget are left for the next
    // pass, which re-scans and re-verifies them anyway. The ~7s worst
    // case of the one terminate in flight is the only permitted overshoot.
    // `budget` is a parameter (not the const directly) so the deferral
    // path is testable in milliseconds instead of a real minute.
    let pass_started = std::time::Instant::now();
    let mut deferred = 0usize;
    for candidate in candidates {
        if pass_started.elapsed() >= budget {
            // Recorded, not silently dropped: the operator's `guard once`
            // table shows these as skipped entries, and the eprintln says
            // why. They were NOT acted on and NOT re-verified.
            eprintln!(
                "🛡️ {log_tag}: pid {} ({}) deferred -- pass budget reached, next pass re-scans it",
                candidate.pid, candidate.comm,
            );
            out.push(ReapedProcess {
                pid: candidate.pid,
                comm: candidate.comm.clone(),
                signature: candidate.signature.clone(),
                verified: false,
                outcome: None,
                disposition: ReapDisposition::Deferred,
            });
            deferred += 1;
            continue;
        }
        let verified = verify(proc_root, candidate, policy, ticks_per_sec);
        if !verified {
            eprintln!(
                "🛡️ {log_tag}: pid {} ({}) no longer verifies -- skipped, nothing signalled",
                candidate.pid, candidate.comm,
            );
            out.push(ReapedProcess {
                pid: candidate.pid,
                comm: candidate.comm.clone(),
                signature: candidate.signature.clone(),
                verified: false,
                outcome: None,
                disposition: ReapDisposition::NotVerified,
            });
            continue;
        }
        let outcome = terminate_process(candidate.pid);
        eprintln!(
            "🛡️ {log_tag}: pid {} ({}, {}) -> {outcome:?}",
            candidate.pid, candidate.comm, candidate.signature,
        );
        out.push(ReapedProcess {
            pid: candidate.pid,
            comm: candidate.comm.clone(),
            signature: candidate.signature.clone(),
            verified: true,
            outcome: Some(outcome),
            disposition: ReapDisposition::Signalled,
        });
    }
    if deferred > 0 {
        eprintln!(
            "🛡️ {log_tag}: pass budget ({:?}) reached — {} candidate(s) deferred to the next pass (re-scanned and re-verified there)",
            REAP_PASS_BUDGET, deferred
        );
    }
    out
}

// --- Pressure-gated orphan scan --------------------------------------------
// The 24h auto-reap floor cannot catch a runaway whose owner just died:
// on 2026-10-04 an 8G orphaned `bun test`, 10 min old and reparented to
// systemd --user, thrashed the box while every age-gated scan refused it.
// Under memory pressure (warn/critical) the guard may kill orphans with
// NO age, CPU, or state gates — but only when ALL of these hold:
//
// - allowlisted dev-server signature (never a generic "old process"),
// - parent is init/systemd, i.e. the owner is provably dead,
// - no controlling terminal (a disowned tmux/shell job keeps its tty,
//   which is the operator's remaining handle — those are kept),
// - not in the shared exempt list,
// - not a reserved PID and not the guard itself.
//
// The caller gates on stabilized pressure AND the `reap_orphans_on_
// pressure` opt-in (default OFF); this scan only finds candidates.

/// True when `ppid`'s command name is the init system — i.e. the process
/// was reparented because its real parent died. Covers PID 1 and the
/// systemd user manager alike (both are comm `systemd`). Anything
/// unreadable fails closed: an unprovable parent is not an orphan.
pub(crate) fn parent_comm_is_systemd(proc_root: &Path, ppid: i32) -> bool {
    if ppid <= 0 {
        return false;
    }
    std::fs::read_to_string(proc_root.join(ppid.to_string()).join("comm"))
        .map(|comm| comm.trim() == "systemd")
        .unwrap_or(false)
}

/// Find parent-dead dev-server processes. Deliberately IGNORANT of age,
/// CPU, and run state — a hot young orphan is the exact shape this
/// exists for. Results carry the same evidence fields as reap candidates
/// (age/cpu are measured, not gated) and sort the same way.
pub(crate) fn scan_pressure_orphans(
    proc_root: &Path,
    policy: &ReapPolicy,
    boot_time: u64,
    now: u64,
    ticks_per_sec: u64,
) -> Vec<ReapCandidate> {
    let self_pid = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out: Vec<ReapCandidate> = Vec::new();

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
        // Owner must be provably dead: reparented to init/systemd.
        if !parent_comm_is_systemd(proc_root, fields.ppid) {
            continue;
        }
        // ... and detached from any terminal: a disowned shell job is
        // the operator's, not the guard's.
        if fields.tty_nr != 0 {
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
        let Some(signature) = matches_signature(&args, &policy.signatures) else {
            continue;
        };

        // Measured, not gated: evidence for the audit trail.
        let cpu_seconds = fields.cpu_ticks / ticks_per_sec.max(1);
        let age_secs = elapsed_secs(fields.starttime, ticks_per_sec, boot_time, now);
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
            starttime: fields.starttime,
        });
    }

    out.sort_by(|a, b| b.rss_mb.cmp(&a.rss_mb).then(a.pid.cmp(&b.pid)));
    out
}
