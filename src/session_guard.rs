//! Fleet-aware agent-session lifecycle: exemption narrowing and idle
//! retirement.
//!
//! # Why this exists
//!
//! The guard's `process_exempt_names` list names whole APPLICATIONS. When
//! the operator's agent fleet ran under the name `pi`, that single token
//! exempted EVERY session from memory-pressure mitigation — so a fleet of
//! 31 sessions holding 12.3 GiB was invisible to the mitigation path. The
//! guard could report "critical memory pressure" and then correctly refuse
//! to act on the only thing causing it. The exemption was not wrong about
//! an individual interactive session; it was wrong as a BLANKET over an
//! unbounded fleet.
//!
//! This module separates two questions that the blanket list conflated:
//!
//! 1. **Is this session safe to leave alone?** An interactive session the
//!    operator is watching must never be reniced, OOM-biased, or killed.
//! 2. **Is this fleet still bounded?** A fleet that grows without limit
//!    turns every session into a hostage: the guard may not touch any of
//!    them, so the pressure it reports is unresolvable.
//!
//! The answer to (1) is per-session and is answered by *activity*, not by
//! name. The answer to (2) is fleet-wide and is answered by a count.
//!
//! # The idle-retirement rule
//!
//! A session is RETIRABLE only when BOTH of these hold:
//!
//! - **No CPU accrual**: the session's cumulative CPU time has not
//!   advanced for at least `session_idle_window_secs`. A session that is
//!   thinking, waiting on a model, or running a tool is accruing CPU and
//!   is therefore NOT retirable.
//! - **No ledger/terminal activity**: neither the session's transcript
//!   (named by its registry marker) nor its working directory's GLLA
//!   ledger has been written within `session_idle_window_secs`.
//!
//! Neither condition is ever satisfied by raw age alone. A session used
//! for three days and still in use has a recent transcript write, so it
//! survives. A three-day-old session nobody has touched has neither CPU
//! accrual nor a recent write, so it does not.
//!
//! # The fleet rule
//!
//! `max_active_sessions` bounds how many agent sessions may hold the
//! blanket exemption at once. When the live fleet exceeds it, the
//! exemption narrows from "every session of this name" to "the sessions
//! that are demonstrably active". The narrowing is what gives the guard
//! authority again; the idle-retirement rule is what actually reclaims
//! the memory.
//!
//! Both rules fail CLOSED toward safety: an unmeasurable session (no
//! activity file, unreadable state, or an observation window too short
//! to prove anything) is `Unknown`, and `Unknown` is treated exactly
//! like `Active` everywhere — the guard never acts on what it cannot
//! prove.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The default idle window, in seconds, used for BOTH the CPU-accrual
/// window and the ledger/transcript staleness window.
///
/// 6 hours is long enough that a session parked overnight survives, and
/// short enough that a fleet cannot hide 12 GiB behind a week-old
/// timestamp.
pub(crate) const DEFAULT_SESSION_IDLE_WINDOW_SECS: u64 = 6 * 3600;

/// How many agent sessions may hold the blanket exemption at once before
/// the exemption narrows to the demonstrably active ones.
pub(crate) const DEFAULT_MAX_ACTIVE_SESSIONS: usize = 8;

/// How a session is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    /// The session is demonstrably doing work: it accrued CPU recently,
    /// or its ledger/transcript moved recently. It keeps the exemption.
    Active,
    /// The session is demonstrably idle: no CPU accrual across the idle
    /// window AND no ledger/transcript write within the idle window.
    Idle,
    /// The session could not be measured. Treated as ACTIVE everywhere:
    /// the guard never acts on what it cannot prove.
    Unknown,
}

/// One observed agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionObservation {
    pub(crate) pid: i32,
    /// The executable name (comm), e.g. `pi`.
    pub(crate) comm: String,
    /// Cumulative CPU time in clock ticks (utime + stime).
    pub(crate) cpu_ticks: u64,
    /// `/proc/<pid>/stat` starttime, used to detect PID reuse.
    pub(crate) starttime: u64,
    /// Newest mtime among the session's activity files (its transcript and
    /// its working directory's GLLA ledger). `None` means no activity file
    /// could be located — which is UNKNOWN, not inactive.
    pub(crate) last_activity: Option<SystemTime>,
    /// The activity files this observation was derived from.
    pub(crate) activity_files: Vec<PathBuf>,
}

/// A session selected for retirement, with the evidence that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetirementCandidate {
    pub(crate) pid: i32,
    pub(crate) comm: String,
    pub(crate) starttime: u64,
    pub(crate) cpu_ticks: u64,
    pub(crate) idle_secs: u64,
    pub(crate) activity_files: Vec<PathBuf>,
}

/// Policy for the fleet-aware session lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPolicy {
    /// The executable names that count as agent sessions.
    pub(crate) session_names: Vec<String>,
    /// The idle window in seconds (CPU accrual AND activity-file staleness).
    pub(crate) idle_window_secs: u64,
    /// The maximum number of sessions that may hold the blanket exemption.
    pub(crate) max_active_sessions: usize,
    /// Directory holding the session registry markers (`<pid>.json` files
    /// naming each session's transcript).
    pub(crate) registry_dir: PathBuf,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            session_names: Vec::new(),
            idle_window_secs: DEFAULT_SESSION_IDLE_WINDOW_SECS,
            max_active_sessions: DEFAULT_MAX_ACTIVE_SESSIONS,
            registry_dir: PathBuf::new(),
        }
    }
}

/// True when the executable name is one of the configured agent-session
/// names. Matching is by exact name (or a `-` extension), the same rule
/// the exemption list uses, so `pi` does not accidentally cover
/// `pipewatch`.
pub(crate) fn is_session_name(comm: &str, names: &[String]) -> bool {
    names.iter().any(|name| {
        if name.is_empty() {
            return false;
        }
        comm == name.as_str()
            || (comm.len() > name.len()
                && comm.starts_with(name.as_str())
                && comm.as_bytes()[name.len()] == b'-')
    })
}

/// Read the newest mtime among the given files. Missing files are skipped;
/// an empty list yields `None`.
fn newest_mtime(paths: &[PathBuf]) -> Option<SystemTime> {
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter_map(|m| m.modified().ok())
        .max()
}

/// The activity files that prove a session was alive.
///
/// Two sources, both scoped to THIS session:
///
/// - the session transcript, named by the registry marker for this pid;
/// - the GLLA ledger of the session's own working directory
///   (`<cwd>/.pi-glla/active.jsonl`).
///
/// The ledger is per-working-directory, so sessions that share a
/// directory also share that signal. That errs toward "active", which is
/// the safe direction: a shared recent write can only ever spare a
/// session from retirement, never condemn one.
fn activity_files_for(policy: &SessionPolicy, pid: i32, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();

    // The registry marker names the session's own transcript file.
    let marker = policy.registry_dir.join(format!("{pid}.json"));
    if let Ok(text) = std::fs::read_to_string(&marker) {
        // The marker is a small JSON object written by the session
        // registry. Parse it defensively: a malformed marker must not
        // create a false "active" verdict, and it must not crash the
        // guard either.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(path) = value.get("sessionFile").and_then(|v| v.as_str()) {
                files.push(PathBuf::from(path));
            }
        }
    }

    if let Some(cwd) = cwd {
        files.push(cwd.join(".pi-glla").join("active.jsonl"));
    }
    files
}

/// The classification inputs, decoupled from the /proc reading so the
/// decision can be tested directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionEvidence {
    /// How long the session has been observed without accruing CPU.
    pub(crate) cpu_idle_for: Duration,
    /// Newest activity-file mtime, or `None` when none could be read.
    pub(crate) last_activity: Option<SystemTime>,
}

/// Classify a session from its evidence.
///
/// The two conditions are conjunctive, exactly as the retirement rule
/// states: no CPU accrual across the idle window AND no ledger/transcript
/// write within the idle window. Either one alone is not enough.
pub(crate) fn classify_evidence(
    evidence: &SessionEvidence,
    idle_window_secs: u64,
    now: SystemTime,
) -> SessionState {
    // No CPU baseline yet, or the CPU moved recently: the session is
    // working (or the window has not elapsed, so nothing is proven).
    if evidence.cpu_idle_for.as_secs() < idle_window_secs {
        return SessionState::Active;
    }
    // A missing activity file is UNKNOWN, never idle: the guard may not
    // retire a session whose activity it could not read.
    let Some(last_activity) = evidence.last_activity else {
        return SessionState::Unknown;
    };
    let age = now
        .duration_since(last_activity)
        .unwrap_or(Duration::ZERO);
    if age.as_secs() < idle_window_secs {
        return SessionState::Active;
    }
    SessionState::Idle
}

/// The per-session tracking state the guard carries between passes.
#[derive(Debug, Clone)]
pub(crate) struct TrackedSession {
    /// The PID incarnation this entry belongs to.
    starttime: u64,
    /// The CPU total observed when the current CPU-idle streak began.
    baseline_cpu_ticks: u64,
    /// When the current CPU-idle streak began.
    idle_since: Instant,
    /// The most recent observation.
    observation: SessionObservation,
}

/// Tracks agent sessions across guard passes so CPU accrual can be
/// measured over a window rather than over the process lifetime.
#[derive(Debug, Default)]
pub(crate) struct SessionTracker {
    pub(crate) sessions: HashMap<i32, TrackedSession>,
}

impl SessionTracker {
    /// Observe every configured agent session under `proc_root`.
    ///
    /// `processes` carries the already-read per-process facts
    /// `(pid, comm, cpu_ticks, starttime)`; this function adds the
    /// activity-file evidence, which is the part `/proc` cannot provide.
    pub(crate) fn observe(
        &mut self,
        policy: &SessionPolicy,
        proc_root: &Path,
        processes: &[(i32, String, u64, u64)],
        now: SystemTime,
        now_instant: Instant,
    ) -> Vec<SessionObservation> {
        let mut out = Vec::new();
        for (pid, comm, cpu_ticks, starttime) in processes {
            if !is_session_name(comm, &policy.session_names) {
                continue;
            }
            let cwd = std::fs::read_link(proc_root.join(pid.to_string()).join("cwd")).ok();
            let files = activity_files_for(policy, *pid, cwd.as_deref());
            let last_activity = newest_mtime(&files);
            let observation = SessionObservation {
                pid: *pid,
                comm: comm.clone(),
                cpu_ticks: *cpu_ticks,
                starttime: *starttime,
                last_activity,
                activity_files: files,
            };

            let tracked = self.sessions.get_mut(pid);
            match tracked {
                // A recycled PID is a different process: the old streak
                // says nothing about it.
                Some(entry) if entry.starttime != *starttime => {
                    entry.starttime = *starttime;
                    entry.baseline_cpu_ticks = *cpu_ticks;
                    entry.idle_since = now_instant;
                    entry.observation = observation.clone();
                }
                Some(entry) => {
                    if *cpu_ticks > entry.baseline_cpu_ticks {
                        // The session did work: the idle streak restarts.
                        entry.baseline_cpu_ticks = *cpu_ticks;
                        entry.idle_since = now_instant;
                    }
                    entry.observation = observation.clone();
                }
                None => {
                    self.sessions.insert(
                        *pid,
                        TrackedSession {
                            starttime: *starttime,
                            baseline_cpu_ticks: *cpu_ticks,
                            idle_since: now_instant,
                            observation: observation.clone(),
                        },
                    );
                }
            }

            let entry = self.sessions.get(pid).expect("inserted above");
            let evidence = SessionEvidence {
                cpu_idle_for: now_instant.saturating_duration_since(entry.idle_since),
                last_activity: observation.last_activity,
            };
            let state = classify_evidence(&evidence, policy.idle_window_secs, now);
            out.push(observation);
            let _ = state;
        }
        out
    }

    /// Classify every tracked session as of now.
    pub(crate) fn states(
        &self,
        policy: &SessionPolicy,
        now: SystemTime,
        now_instant: Instant,
    ) -> HashMap<i32, SessionState> {
        let mut out = HashMap::new();
        for (pid, entry) in &self.sessions {
            let evidence = SessionEvidence {
                cpu_idle_for: now_instant.saturating_duration_since(entry.idle_since),
                last_activity: entry.observation.last_activity,
            };
            out.insert(
                *pid,
                classify_evidence(&evidence, policy.idle_window_secs, now),
            );
        }
        out
    }

    /// Forget the sessions that are gone, so the tracker cannot grow
    /// without bound either.
    pub(crate) fn retain_live(&mut self, live_pids: &std::collections::HashSet<i32>) {
        self.sessions.retain(|pid, _| live_pids.contains(pid));
    }
}

/// The pids that keep the blanket exemption.
///
/// An exempt process keeps its exemption when the session is demonstrably
/// ACTIVE or UNKNOWN. An IDLE session keeps it only while the fleet is
/// within `max_active_sessions` — above that bound the idle sessions give
/// up the blanket exemption and the guard can act on them.
pub(crate) fn exemption_pids(
    policy: &SessionPolicy,
    exempt_names: &[String],
    observations: &[SessionObservation],
    states: &HashMap<i32, SessionState>,
) -> Vec<i32> {
    let fleet_size = observations.len();
    let mut out = Vec::new();
    for obs in observations {
        if !exempt_names.iter().any(|n| n == &obs.comm) {
            continue;
        }
        let state = states.get(&obs.pid).copied().unwrap_or(SessionState::Unknown);
        let keep = match state {
            SessionState::Active | SessionState::Unknown => true,
            SessionState::Idle => fleet_size <= policy.max_active_sessions,
        };
        if keep {
            out.push(obs.pid);
        }
    }
    out
}

/// Select the sessions that may be retired.
///
/// A session qualifies only when it is `SessionState::Idle` — which
/// already encodes "no CPU accrual across the window AND no activity-file
/// write within the window". The window is re-applied here so a caller
/// cannot widen it by passing a stale classification.
pub(crate) fn retirement_candidates(
    observations: &[SessionObservation],
    states: &HashMap<i32, SessionState>,
    idle_window_secs: u64,
    now: SystemTime,
) -> Vec<RetirementCandidate> {
    let mut out = Vec::new();
    for obs in observations {
        if states.get(&obs.pid).copied() != Some(SessionState::Idle) {
            continue;
        }
        let Some(last_activity) = obs.last_activity else {
            continue;
        };
        let age = now
            .duration_since(last_activity)
            .unwrap_or(Duration::ZERO);
        if age.as_secs() < idle_window_secs {
            continue;
        }
        out.push(RetirementCandidate {
            pid: obs.pid,
            comm: obs.comm.clone(),
            starttime: obs.starttime,
            cpu_ticks: obs.cpu_ticks,
            idle_secs: age.as_secs(),
            activity_files: obs.activity_files.clone(),
        });
    }
    out
}

/// Re-verify a candidate immediately before signalling it.
///
/// The verification is deliberately paranoid: the PID incarnation must be
/// unchanged, the session must STILL not have accrued CPU, and the
/// activity files must still be as stale as they were. A session that
/// woke up between the scan and the signal is left alone.
pub(crate) fn verify_retirement(
    proc_root: &Path,
    candidate: &RetirementCandidate,
    idle_window_secs: u64,
    now: SystemTime,
) -> bool {
    if candidate.pid <= 1 || candidate.pid == std::process::id() as i32 {
        return false;
    }
    let pid_dir = proc_root.join(candidate.pid.to_string());
    let Ok(raw) = std::fs::read_to_string(pid_dir.join("stat")) else {
        return false;
    };
    let Some(fields) = crate::reap::parse_stat(&raw) else {
        return false;
    };
    // Same PID incarnation.
    if fields.starttime != candidate.starttime {
        return false;
    }
    // The session must not have accrued CPU since the scan.
    if fields.cpu_ticks > candidate.cpu_ticks {
        return false;
    }
    // The activity files must still be stale. A write that happened after
    // the scan means the session woke up.
    match newest_mtime(&candidate.activity_files) {
        Some(newest) => {
            let age = now
                .duration_since(newest)
                .unwrap_or(Duration::ZERO);
            age.as_secs() >= idle_window_secs
        }
        // The activity files vanished between the scan and the signal.
        // That is not proof of idleness; refuse.
        None => false,
    }
}
