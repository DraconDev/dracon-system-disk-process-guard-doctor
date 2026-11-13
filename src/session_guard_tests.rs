//! Tests for session_guard.rs -- the fleet-aware agent-session lifecycle.
//!
//! The rules under test are destructive, so the assertions that matter
//! most are the NEGATIVE ones: a session that accrued CPU, a session with
//! a recent transcript write, a session that cannot be measured, and a
//! session that woke up between the scan and the signal must ALL survive.
//! A false positive here kills somebody's working session, so the tests
//! lean on the exclusions.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The window the tests use. Short enough to compute, long enough that a
/// "recent" write is unambiguous.
const WINDOW: u64 = 3600;

/// The reference "now" for the tests. It is the REAL clock, because the
/// fixture backdates file mtimes against the real clock; a synthetic
/// epoch would make every activity file look like it was written in the
/// future, which would turn "stale" into "not yet stale".
fn now() -> SystemTime {
    SystemTime::now()
}

/// A time far enough in the past that any file written "now" is stale
/// relative to it.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(10 * WINDOW)
}

struct Fixture {
    root: PathBuf,
    registry: PathBuf,
    // The tempdir owns the directory; holding it here keeps the fixture
    // alive for the whole test.
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new(_name: &str) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let registry = root.join("registry");
        fs::create_dir_all(&registry).expect("registry");
        Fixture {
            root,
            registry,
            _dir: dir,
        }
    }

    fn proc(&self, pid: i32, comm: &str, cpu_ticks: u64) -> PathBuf {
        self.proc_with_tty(pid, comm, cpu_ticks, 0)
    }

    /// A process entry with an explicit controlling terminal. `tty_nr` 0
    /// means "detached"; anything else is an interactive session.
    fn proc_with_tty(&self, pid: i32, comm: &str, cpu_ticks: u64, tty_nr: i32) -> PathBuf {
        let dir = self.root.join(pid.to_string());
        fs::create_dir_all(&dir).expect("proc dir");
        fs::write(dir.join("comm"), format!("{comm}\n")).expect("comm");
        // The stat fixture carries the CPU total in utime and zero in
        // stime, because parse_stat SUMS the two fields: splitting the
        // total across both would double every value the tracker reads.
        //
        // The field order after the comm is the kernel's: state, ppid,
        // pgrp, session, tty_nr, tpgid. The session slot must be present
        // even when it is 0, or tty_nr would be read from the wrong
        // column.
        fs::write(
            dir.join("stat"),
            format!(
                "42 ({comm}) S 1 {} 0 {tty_nr} -1 4194560 100 0 0 0 {cpu_ticks} 0 0 0 20 0 1 0 100 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
                pid + 1
            ),
        )
        .expect("stat");
        dir
    }

    /// A registry marker naming the session's transcript.
    fn marker(&self, pid: i32, transcript: &Path) {
        fs::write(
            self.registry.join(format!("{pid}.json")),
            format!(
                "{{\"pid\":{pid},\"sessionFile\":{},\"startedAt\":\"2026-01-01T00:00:00Z\"}}",
                serde_json::to_string(&transcript.to_string_lossy()).unwrap()
            ),
        )
        .expect("marker");
    }

    fn transcript(&self, name: &str, age_secs: u64) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, b"session\n").expect("transcript");
        set_age(&path, age_secs);
        path
    }

    fn policy(&self) -> SessionPolicy {
        SessionPolicy {
            session_names: vec!["pi".to_string()],
            idle_window_secs: WINDOW,
            max_active_sessions: 8,
            registry_dir: self.registry.clone(),
        }
    }
}

/// Backdate a file's mtime by rewriting it with an old timestamp.
fn set_age(path: &Path, age_secs: u64) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for mtime");
    file.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .expect("set mtime");
}

fn observation(pid: i32, cpu_ticks: u64, last_activity: Option<SystemTime>) -> SessionObservation {
    SessionObservation {
        pid,
        comm: "pi".to_string(),
        cpu_ticks,
        starttime: 1000 + pid as u64,
        last_activity,
        activity_files: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// classify_evidence -- the core idle rule
// ---------------------------------------------------------------------------

/// A session that accrued no CPU and wrote nothing recently IS idle.
#[test]
fn no_cpu_accrual_and_stale_activity_is_idle() {
    let evidence = SessionEvidence {
        cpu_accrued: 0,
        observed_for: Duration::from_secs(2 * WINDOW),
        last_activity: Some(long_ago()),
    };
    assert_eq!(classify_evidence(&evidence, WINDOW, now()), SessionState::Idle);
}

/// A session that accrued CPU recently is ACTIVE, even when its files are
/// old. This is the "it is working right now" case.
#[test]
fn recent_cpu_accrual_is_active_even_with_stale_files() {
    let evidence = SessionEvidence {
        cpu_accrued: 42,
        observed_for: Duration::from_secs(60),
        last_activity: Some(long_ago()),
    };
    assert_eq!(
        classify_evidence(&evidence, WINDOW, now()),
        SessionState::Active
    );
}

/// A session whose transcript was written recently is ACTIVE, even when it
/// has not accrued CPU. This is the "thinking / waiting on the operator"
/// case, and it is the reason age is never enough.
#[test]
fn recent_activity_write_is_active_even_with_no_cpu() {
    let evidence = SessionEvidence {
        cpu_accrued: 0,
        observed_for: Duration::from_secs(2 * WINDOW),
        last_activity: Some(now() - Duration::from_secs(60)),
    };
    assert_eq!(
        classify_evidence(&evidence, WINDOW, now()),
        SessionState::Active
    );
}

/// A session whose activity file cannot be read is UNKNOWN, not idle. The
/// guard may not retire what it cannot measure.
#[test]
fn unmeasurable_activity_is_unknown_not_idle() {
    let evidence = SessionEvidence {
        cpu_accrued: 0,
        observed_for: Duration::from_secs(2 * WINDOW),
        last_activity: None,
    };
    assert_eq!(
        classify_evidence(&evidence, WINDOW, now()),
        SessionState::Unknown
    );
}

/// A window that has not elapsed proves nothing: the session is UNKNOWN
/// until the window has actually passed. It is NOT reported as active —
/// the guard has not proved it is working either.
#[test]
fn window_must_actually_elapse_before_idle_is_possible() {
    let evidence = SessionEvidence {
        cpu_accrued: 0,
        observed_for: Duration::from_secs(WINDOW - 1),
        last_activity: Some(long_ago()),
    };
    assert_eq!(
        classify_evidence(&evidence, WINDOW, now()),
        SessionState::Unknown
    );
}

// ---------------------------------------------------------------------------
// exemption_pids -- the fleet rule
// ---------------------------------------------------------------------------

/// Within the fleet bound, every session keeps the exemption.
#[test]
fn small_fleet_keeps_the_blanket_exemption() {
    let policy = SessionPolicy {
        session_names: vec!["pi".to_string()],
        idle_window_secs: WINDOW,
        max_active_sessions: 8,
        registry_dir: PathBuf::new(),
    };
    let observations: Vec<SessionObservation> = (0..4)
        .map(|i| observation(100 + i, 10, Some(long_ago())))
        .collect();
    let states: HashMap<i32, SessionState> = observations
        .iter()
        .map(|o| (o.pid, SessionState::Idle))
        .collect();
    let keep = exemption_pids(&policy, &["pi".to_string()], &observations, &states);
    assert_eq!(keep.len(), 4, "a fleet within bounds keeps every pass");
}

/// Above the fleet bound, the idle sessions lose the exemption and the
/// active ones keep it. This is the fleet-aware rule that gives the guard
/// its authority back.
#[test]
fn oversized_fleet_narrows_the_exemption_to_active_sessions() {
    let policy = SessionPolicy {
        session_names: vec!["pi".to_string()],
        idle_window_secs: WINDOW,
        max_active_sessions: 3,
        registry_dir: PathBuf::new(),
    };
    let observations: Vec<SessionObservation> = (0..5)
        .map(|i| observation(100 + i, 10, Some(long_ago())))
        .collect();
    let mut states: HashMap<i32, SessionState> = HashMap::new();
    states.insert(100, SessionState::Active);
    states.insert(101, SessionState::Active);
    states.insert(102, SessionState::Unknown);
    states.insert(103, SessionState::Idle);
    states.insert(104, SessionState::Idle);
    let keep = exemption_pids(&policy, &["pi".to_string()], &observations, &states);
    assert_eq!(
        keep,
        vec![100, 101, 102],
        "active and unmeasurable sessions keep the pass; idle ones give it up"
    );
}

/// A session whose state was never computed is treated as unknown, and
/// unknown keeps the exemption.
#[test]
fn unclassified_session_keeps_the_exemption() {
    let policy = SessionPolicy {
        session_names: vec!["pi".to_string()],
        idle_window_secs: WINDOW,
        max_active_sessions: 0,
        registry_dir: PathBuf::new(),
    };
    let observations = vec![observation(100, 10, Some(long_ago()))];
    let keep = exemption_pids(&policy, &["pi".to_string()], &observations, &HashMap::new());
    assert_eq!(keep, vec![100]);
}

// ---------------------------------------------------------------------------
// retirement_candidates -- the selection
// ---------------------------------------------------------------------------

/// Only idle sessions are retirement candidates.
#[test]
fn only_idle_sessions_are_retirement_candidates() {
    let observations = vec![
        observation(100, 10, Some(long_ago())),
        observation(101, 10, Some(now() - Duration::from_secs(30))),
        observation(102, 10, None),
    ];
    let mut states = HashMap::new();
    states.insert(100, SessionState::Idle);
    states.insert(101, SessionState::Active);
    states.insert(102, SessionState::Unknown);
    let candidates = retirement_candidates(&observations, &states, WINDOW, now());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].pid, 100);
}

/// The activity window is re-applied at selection time, so a caller
/// cannot widen it by passing a stale classification.
#[test]
fn selection_reapplies_the_activity_window() {
    let observations = vec![observation(100, 10, Some(now() - Duration::from_secs(30)))];
    let mut states = HashMap::new();
    states.insert(100, SessionState::Idle);
    let candidates = retirement_candidates(&observations, &states, WINDOW, now());
    assert!(
        candidates.is_empty(),
        "a recently-written session is not retirable even if the state says idle"
    );
}

// ---------------------------------------------------------------------------
// verify_retirement -- the last gate before a signal
// ---------------------------------------------------------------------------

/// A candidate that still verifies is accepted.
#[test]
fn a_still_idle_candidate_verifies() {
    let fixture = Fixture::new("still-idle");
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    let dir = fixture.proc(4242, "pi", 500);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// A candidate whose CPU advanced since the scan does NOT verify: it woke
/// up.
#[test]
fn a_candidate_that_accrued_cpu_does_not_verify() {
    let fixture = Fixture::new("woke-up-cpu");
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    let dir = fixture.proc(4242, "pi", 900);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// A candidate whose transcript was written since the scan does NOT
/// verify.
#[test]
fn a_candidate_with_a_fresh_write_does_not_verify() {
    let fixture = Fixture::new("woke-up-write");
    let transcript = fixture.transcript("transcript.jsonl", 10);
    let dir = fixture.proc(4242, "pi", 500);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// A candidate whose activity files vanished does NOT verify: a vanished
/// file is not proof of idleness.
#[test]
fn a_candidate_with_missing_activity_files_does_not_verify() {
    let fixture = Fixture::new("vanished");
    let dir = fixture.proc(4242, "pi", 500);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![fixture.root.join("gone.jsonl")],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// A recycled PID does not verify: the starttime no longer matches.
#[test]
fn a_recycled_pid_does_not_verify() {
    let fixture = Fixture::new("recycled");
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    let dir = fixture.proc(4242, "pi", 500);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        // A starttime that does not belong to this incarnation.
        starttime: parse_starttime(&dir) + 1,
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// The guard never signals itself.
#[test]
fn the_guard_never_signals_itself() {
    let fixture = Fixture::new("self");
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    let dir = fixture.proc(std::process::id() as i32, "pi", 500);
    let candidate = RetirementCandidate {
        pid: std::process::id() as i32,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

/// A session with a controlling terminal is INTERACTIVE — the operator can
/// see it, so it is theirs to close. Every other reap path in the guard
/// requires tty == 0 for the same reason, and the retirement path must
/// not be the one that forgets it.
#[test]
fn a_tty_attached_session_is_never_retired() {
    let fixture = Fixture::new("tty");
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    // tty_nr = 34842, as a real interactive pane reports.
    let dir = fixture.proc_with_tty(4242, "pi", 500, 34842);
    let candidate = RetirementCandidate {
        pid: 4242,
        comm: "pi".to_string(),
        starttime: parse_starttime(&dir),
        cpu_ticks: 500,
        idle_secs: 10 * WINDOW,
        activity_files: vec![transcript],
    };
    assert!(!verify_retirement(
        &fixture.root,
        &candidate,
        WINDOW,
        now()
    ));
}

// ---------------------------------------------------------------------------
// The tracker across passes
// ---------------------------------------------------------------------------

/// A session that accrues CPU between passes is never classified idle.
#[test]
fn a_session_that_works_between_passes_stays_active() {
    let fixture = Fixture::new("working");
    let policy = fixture.policy();
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    fixture.marker(4242, &transcript);
    let dir = fixture.proc(4242, "pi", 100);

    let mut tracker = SessionTracker::default();
    let t0 = Instant::now();
    let facts = vec![(4242, "pi".to_string(), 100, parse_starttime(&dir))];
    tracker.observe(
        &policy,
        &fixture.root,
        &facts,
        long_ago(),
        t0,
    );
    // Second pass: the session did work.
    let t1 = t0 + Duration::from_secs(2 * WINDOW);
    let facts = vec![(4242, "pi".to_string(), 900, parse_starttime(&dir))];
    tracker.observe(&policy, &fixture.root, &facts, now(), t1);
    let states = tracker.states(&policy, now(), t1);
    assert_eq!(states.get(&4242), Some(&SessionState::Active));
}

/// A session that neither works nor writes is idle once the window has
/// elapsed.
#[test]
fn a_session_that_neither_works_nor_writes_becomes_idle() {
    let fixture = Fixture::new("idle");
    let policy = fixture.policy();
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    fixture.marker(4242, &transcript);
    let dir = fixture.proc(4242, "pi", 100);

    let mut tracker = SessionTracker::default();
    let t0 = Instant::now();
    let facts = vec![(4242, "pi".to_string(), 100, parse_starttime(&dir))];
    tracker.observe(&policy, &fixture.root, &facts, long_ago(), t0);
    let t1 = t0 + Duration::from_secs(2 * WINDOW);
    tracker.observe(&policy, &fixture.root, &facts, now(), t1);
    let states = tracker.states(&policy, now(), t1);
    assert_eq!(states.get(&4242), Some(&SessionState::Idle));
}

/// A session with no registry marker and no ledger has no activity file,
/// so it is UNKNOWN -- never idle, never retired.
#[test]
fn a_session_with_no_activity_file_is_never_idle() {
    let fixture = Fixture::new("no-marker");
    let policy = fixture.policy();
    let dir = fixture.proc(4242, "pi", 100);

    let mut tracker = SessionTracker::default();
    let t0 = Instant::now();
    let facts = vec![(4242, "pi".to_string(), 100, parse_starttime(&dir))];
    tracker.observe(&policy, &fixture.root, &facts, long_ago(), t0);
    let t1 = t0 + Duration::from_secs(2 * WINDOW);
    tracker.observe(&policy, &fixture.root, &facts, now(), t1);
    let states = tracker.states(&policy, now(), t1);
    assert_eq!(states.get(&4242), Some(&SessionState::Unknown));
}

/// The tracker forgets sessions that are gone, so it cannot become the
/// unbounded thing it exists to prevent.
#[test]
fn the_tracker_forgets_dead_sessions() {
    let fixture = Fixture::new("dead");
    let policy = fixture.policy();
    let dir = fixture.proc(4242, "pi", 100);
    let mut tracker = SessionTracker::default();
    let facts = vec![(4242, "pi".to_string(), 100, parse_starttime(&dir))];
    tracker.observe(&policy, &fixture.root, &facts, long_ago(), Instant::now());
    assert_eq!(tracker.sessions.len(), 1);
    tracker.retain_live(&HashSet::new());
    assert!(tracker.sessions.is_empty());
}

/// A recycled PID resets the CPU baseline: the old streak says nothing
/// about the new process.
#[test]
fn a_recycled_pid_resets_the_cpu_baseline() {
    let fixture = Fixture::new("recycled-baseline");
    let policy = fixture.policy();
    let transcript = fixture.transcript("transcript.jsonl", 10 * WINDOW);
    fixture.marker(4242, &transcript);
    let dir = fixture.proc(4242, "pi", 100);
    let starttime = parse_starttime(&dir);

    let mut tracker = SessionTracker::default();
    let t0 = Instant::now();
    let facts = vec![(4242, "pi".to_string(), 100, starttime)];
    tracker.observe(&policy, &fixture.root, &facts, long_ago(), t0);
    // A different incarnation: the CPU total is far higher, but it is a
    // DIFFERENT process, so it must not inherit the idle streak.
    let t1 = t0 + Duration::from_secs(2 * WINDOW);
    let facts = vec![(4242, "pi".to_string(), 900, starttime + 1)];
    tracker.observe(&policy, &fixture.root, &facts, now(), t1);
    let states = tracker.states(&policy, now(), t1);
    assert_eq!(states.get(&4242), Some(&SessionState::Active));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_starttime(dir: &Path) -> u64 {
    let raw = fs::read_to_string(dir.join("stat")).expect("stat");
    crate::reap::parse_stat(&raw).expect("parse").starttime
}
