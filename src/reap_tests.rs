//! Tests for reap.rs -- abandoned dev/test process reporting.
//!
//! Every test runs against a fixture proc tree written to a temp dir, not
//! the live `/proc`. The module is report-only by construction, so the
//! assertions that matter most are the *negative* ones: a process that is
//! busy, young, attached to a terminal, or not on the allowlist must never
//! reach the report. A false positive here points a human at a live
//! session, so the tests lean on the exclusions.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};

const TICKS: u64 = 100;

/// Seconds of uptime a process must have to look abandoned at 24h idle.
const DAY_SECS: u64 = 24 * 3600;

struct Fixture {
    root: PathBuf,
    boot_time: u64,
    now: u64,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("dracon-reap-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture proc root");
        // A fixed epoch so elapsed arithmetic is exact, not wall-clock.
        let boot_time = 1_700_000_000;
        Fixture {
            root,
            boot_time,
            now: boot_time + 10 * DAY_SECS,
        }
    }

    /// Write one process into the fixture tree.
    ///
    /// `age_secs` is how old the process is *at scan time*, so the
    /// starttime is derived from `now` rather than from boot; otherwise
    /// every test would measure `now - age` and assert the wrong number.
    /// Going through the scanner's own elapsed calculation is the point:
    /// the stat fixture is real enough to exercise it.
    #[allow(clippy::too_many_arguments)]
    fn proc(
        &self,
        pid: i32,
        comm: &str,
        args: &[&str],
        state: char,
        tty_nr: i32,
        cpu_secs: u64,
        age_secs: u64,
        rss_kb: u64,
    ) {
        let dir = self.root.join(pid.to_string());
        fs::create_dir_all(&dir).expect("pid dir");
        let uptime_secs = self.now - self.boot_time;
        let starttime = uptime_secs.saturating_sub(age_secs) * TICKS;
        let utime = cpu_secs * TICKS;
        // utime and stime are summed by the parser (lifetime CPU), so all
        // of the fixture's budget goes in utime to keep `cpu_secs` meaning
        // the total rather than each half of it.
        let stime = 0;
        // Fields are the kernel's 1-indexed ones; comm sits at 2 and may
        // contain spaces, exactly as on a real host. After comm the order
        // is state ppid pgrp session tty_nr tpgid flags minflt cminflt
        // majflt cmajflt utime stime cutime cstime priority nice
        // num_threads itrealvalue starttime.
        let stat = format!(
            "{pid} ({comm}) {state} {ppid} {pgrp} {session} {tty_nr} 0 -1 4194304 0 0 0 \
             {utime} {stime} 0 0 20 0 1 0 {starttime} 0 0 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0",
            ppid = 1224,
            pgrp = pid,
            session = pid,
        );
        fs::write(dir.join("stat"), stat).expect("stat");
        fs::write(dir.join("comm"), format!("{comm}\n")).expect("comm");
        let mut cmdline: Vec<u8> = Vec::new();
        for arg in args {
            cmdline.extend_from_slice(arg.as_bytes());
            cmdline.push(0);
        }
        fs::write(dir.join("cmdline"), cmdline).expect("cmdline");
        fs::write(
            dir.join("status"),
            format!("Name:\t{comm}\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t{rss_kb} kB\n"),
        )
        .expect("status");
    }

    /// A realistic abandoned dev server: sleeping, no tty, 5s of lifetime
    /// CPU, three days old, 64 MiB.
    fn abandoned_server(&self, pid: i32, comm: &str, args: &[&str]) {
        self.proc(pid, comm, args, 'S', 0, 5, 3 * DAY_SECS, 65_536);
    }

    fn scan(&self, policy: &ReapPolicy) -> Vec<ReapCandidate> {
        scan_reap_candidates(&self.root, policy, self.boot_time, self.now, TICKS)
    }

    fn default_scan(&self) -> Vec<ReapCandidate> {
        self.scan(&ReapPolicy::default())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn policy() -> ReapPolicy {
    ReapPolicy::default()
}

/// One abandoned server in a fresh tree, scanned exactly once. Every
/// re-verification test below mutates the tree (or the candidate) between
/// this scan and `verify_candidate_for_reap`: the scan result is stale by
/// construction, and the kill decision must not trust it.
fn scanned_candidate(fx: &Fixture) -> ReapCandidate {
    fx.abandoned_server(
        4242,
        "node",
        &["node", "scripts/e2e-harness.mjs", "serve", "1491"],
    );
    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "fixture must scan as exactly one candidate");
    found.into_iter().next().unwrap()
}

#[test]
fn reverification_accepts_an_unchanged_candidate() {
    let fx = Fixture::new("reverify-ok");
    let cand = scanned_candidate(&fx);
    assert!(verify_candidate_for_reap(&fx.root, &cand, &policy(), TICKS));
}

#[test]
fn reverification_rejects_a_recycled_pid() {
    let fx = Fixture::new("reverify-reuse");
    let cand = scanned_candidate(&fx);
    // Same PID, new process: the kernel reused 4242 after the old server
    // exited. A starttime mismatch must fail closed, never signal.
    fx.proc(
        4242,
        "node",
        &["node", "scripts/e2e-harness.mjs", "serve", "1491"],
        'S',
        0,
        1,
        60,
        65_536,
    );
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_a_process_that_woke_up() {
    let fx = Fixture::new("reverify-woke");
    let cand = scanned_candidate(&fx);
    fx.proc(
        4242,
        "node",
        &["node", "scripts/e2e-harness.mjs", "serve", "1491"],
        'R',
        0,
        5,
        3 * DAY_SECS,
        65_536,
    );
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_a_process_that_gained_a_terminal() {
    let fx = Fixture::new("reverify-tty");
    let cand = scanned_candidate(&fx);
    fx.proc(
        4242,
        "node",
        &["node", "scripts/e2e-harness.mjs", "serve", "1491"],
        'S',
        34816,
        5,
        3 * DAY_SECS,
        65_536,
    );
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_a_cmdline_that_left_the_allowlist() {
    let fx = Fixture::new("reverify-exec");
    let cand = scanned_candidate(&fx);
    // Same PID/starttime, but the process exec'd into something the
    // operator never allowlisted. `node` alone matches no signature.
    fx.proc(
        4242,
        "node",
        &["node", "scripts/real-work.js"],
        'S',
        0,
        5,
        3 * DAY_SECS,
        65_536,
    );
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_a_process_that_burned_cpu() {
    let fx = Fixture::new("reverify-cpu");
    let cand = scanned_candidate(&fx);
    fx.proc(
        4242,
        "node",
        &["node", "scripts/e2e-harness.mjs", "serve", "1491"],
        'S',
        0,
        3600,
        3 * DAY_SECS,
        65_536,
    );
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_a_vanished_process() {
    let fx = Fixture::new("reverify-gone");
    let cand = scanned_candidate(&fx);
    fs::remove_dir_all(fx.root.join("4242")).expect("remove pid dir");
    assert!(!verify_candidate_for_reap(
        &fx.root,
        &cand,
        &policy(),
        TICKS
    ));
}

#[test]
fn reverification_rejects_pids_the_guard_must_never_signal() {
    let fx = Fixture::new("reverify-reserved");
    let mut cand = scanned_candidate(&fx);
    // PID 1 and the guard's own PID are refused even if every other
    // criterion matches: defence in depth against a crafted allowlist.
    for pid in [0, -7, 1, std::process::id() as i32] {
        cand.pid = pid;
        assert!(
            !verify_candidate_for_reap(&fx.root, &cand, &policy(), TICKS),
            "pid {pid} must never verify"
        );
    }
}

// --- Signal execution ----------------------------------------------------
// These spawn real short-lived children of the test process and terminate
// only those children (or PIDs that cannot exist). Nothing else on the
// host is signalled.

#[test]
fn terminate_refuses_reserved_and_own_pids() {
    for pid in [0, -1, 1, std::process::id() as i32] {
        assert!(
            matches!(terminate_process(pid), TerminateOutcome::Refused { .. }),
            "pid {pid} must be refused"
        );
    }
}

#[test]
fn terminate_reports_a_pid_that_cannot_exist() {
    // No kernel hands out PID i32::MAX, so this exercises the ESRCH path
    // with zero chance of signalling a stranger (no exit-and-reuse race).
    assert!(matches!(
        terminate_process(i32::MAX),
        TerminateOutcome::AlreadyGone
    ));
}

#[test]
fn terminate_ends_a_live_child_with_sigterm() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id() as i32;
    let outcome = terminate_process(pid);
    assert!(
        matches!(
            outcome,
            TerminateOutcome::Signalled {
                escalated_to_sigkill: false
            }
        ),
        "unexpected {outcome:?}"
    );
    let status = child.wait().expect("reap child");
    assert!(!status.success());
    // `sleep` installs no handler: it dies FROM the signal, it does not
    // exit after it.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(15));
    }
}

#[test]
fn terminate_escalates_to_sigkill_when_term_is_ignored() {
    // A single process that ignores SIGTERM must still die, via SIGKILL.
    // python3 is the only portable-enough TERM-ignorer; hosts without it
    // (minimal build sandboxes) skip instead of failing.
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: no python3 to build a TERM-ignoring child");
        return;
    }
    // Readiness handshake: the child prints `ready` AFTER installing the
    // SIGTERM ignore, so the signal can never land during interpreter
    // startup (when the default disposition would still kill it).
    let mut child = std::process::Command::new("python3")
        .args([
            "-c",
            "import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print('ready',flush=True);time.sleep(60)",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn ignorer");
    let pid = child.id() as i32;
    {
        use std::io::BufRead;
        let stdout = child.stdout.as_mut().expect("piped stdout");
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read ready");
        assert_eq!(line.trim(), "ready");
    }
    let outcome = terminate_process(pid);
    assert!(
        matches!(
            outcome,
            TerminateOutcome::Signalled {
                escalated_to_sigkill: true
            }
        ),
        "unexpected {outcome:?}"
    );
    let _ = child.wait();
}

#[test]
fn auto_reap_kills_a_verified_candidate_and_records_the_kill() {
    // End to end across the seam: the fixture tree carries the *evidence*
    // (old, idle, allowlisted) while the PID is a real live child of this
    // test. Verify reads the fixture; terminate signals the child.
    let fx = Fixture::new("auto-reap-e2e");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id() as i32;
    fx.proc(
        pid,
        "sleep",
        &["sleep", "vite-fixture"],
        'S',
        0,
        5,
        3 * DAY_SECS,
        1024,
    );
    let found = fx.default_scan();
    assert_eq!(found.len(), 1);
    let reaped = auto_reap_stale_servers(&fx.root, &policy(), &found, TICKS);
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].pid, pid);
    assert!(
        matches!(reaped[0].outcome, Some(TerminateOutcome::Signalled { .. })),
        "unexpected {:?}",
        reaped[0].outcome
    );
    let status = child.wait().expect("reap child");
    assert!(!status.success());
}

#[test]
fn auto_reap_skips_a_candidate_that_vanished_after_the_scan() {
    let fx = Fixture::new("auto-reap-stale");
    let cand = scanned_candidate(&fx);
    fs::remove_dir_all(fx.root.join("4242")).expect("remove pid dir");
    let reaped = auto_reap_stale_servers(&fx.root, &policy(), &[cand], TICKS);
    assert_eq!(reaped.len(), 1);
    // Recorded, not silently dropped -- but nothing was signalled.
    assert_eq!(reaped[0].outcome, None);
    assert!(!reaped[0].verified);
}

#[test]
fn an_abandoned_dev_server_is_reported() {
    let fx = Fixture::new("reports-abandoned");
    fx.abandoned_server(
        4242,
        "node",
        &["node", "node_modules/.bin/vite", "dev", "--port", "5173"],
    );
    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "the server is reported: {found:?}");
    assert_eq!(found[0].pid, 4242);
    assert_eq!(found[0].idle_hours, 72, "three days idle");
    assert_eq!(found[0].cpu_seconds, 5);
    assert_eq!(found[0].rss_mb, 64);
    assert!(found[0].args.contains("vite"), "args retained: {found:?}");
    assert!(!found[0].args.contains('\0'), "cmdline is display-ready");
}

#[test]
fn a_process_with_a_controlling_terminal_is_never_reported() {
    // The decisive filter. A parked shell, an editor, or a `pi` session in a
    // tmux pane has a tty and must stay invisible no matter how old it is.
    let fx = Fixture::new("tty-protected");
    fx.proc(
        900,
        "zsh",
        &["zsh", "-c", "npm run test"],
        'S',
        34816,
        5,
        30 * DAY_SECS,
        9_000,
    );
    assert!(
        fx.default_scan().is_empty(),
        "a tty-attached process is never a reap candidate"
    );
}

#[test]
fn a_busy_process_is_never_reported() {
    // Ten days old and terminal-less, but it has done real work.
    let fx = Fixture::new("busy-exempt");
    fx.proc(
        901,
        "node",
        &["node", "vite", "dev"],
        'S',
        0,
        600,
        10 * DAY_SECS,
        65_536,
    );
    assert!(
        fx.default_scan().is_empty(),
        "high lifetime CPU means in use, not abandoned"
    );
}

#[test]
fn a_young_process_is_never_reported() {
    let fx = Fixture::new("too-young");
    // Idle and terminal-less, but only 20 minutes old.
    fx.proc(
        902,
        "node",
        &["node", "vite", "dev"],
        'S',
        0,
        1,
        20 * 60,
        65_536,
    );
    assert!(fx.default_scan().is_empty(), "a fresh server is not dead");
}

#[test]
fn a_non_allowlisted_process_is_never_reported() {
    // Old, idle, no tty -- but it is not on the signature allowlist, so
    // there is no evidence it is disposable infrastructure.
    let fx = Fixture::new("not-allowlisted");
    fx.proc(
        903,
        "rsync",
        &["rsync", "-a", "/home/dracon/backup/"],
        'S',
        0,
        2,
        20 * DAY_SECS,
        1_024,
    );
    assert!(
        fx.default_scan().is_empty(),
        "the allowlist is what makes this safe"
    );
}

#[test]
fn an_exempt_name_is_never_reported() {
    let fx = Fixture::new("exempt");
    fx.abandoned_server(904, "chrome", &["/usr/bin/chromium", "--headless"]);
    let with_exempt = ReapPolicy {
        exempt_names: vec!["chromium".to_string()],
        ..ReapPolicy::default()
    };
    assert!(
        fx.scan(&with_exempt).is_empty(),
        "the shared exempt list keeps named processes out"
    );
}

#[test]
fn a_running_process_is_never_reported() {
    // Idle-looking CPU history but currently on-CPU: state R is work.
    let fx = Fixture::new("on-cpu");
    fx.proc(
        905,
        "node",
        &["node", "vite", "dev"],
        'R',
        0,
        1,
        5 * DAY_SECS,
        65_536,
    );
    assert!(fx.default_scan().is_empty(), "an R process is busy");
}

#[test]
fn non_process_procfs_entries_are_ignored() {
    let fx = Fixture::new("procfs-noise");
    for dir in ["self", "net", "sys", "fs", "meminfo:broken"] {
        fs::create_dir_all(fx.root.join(dir)).expect("noise dir");
    }
    fx.abandoned_server(906, "node", &["node", "vite", "dev"]);
    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "only the real pid dir is classified");
    assert_eq!(found[0].pid, 906);
}

#[test]
fn a_vanished_process_does_not_break_the_scan() {
    // A pid directory with no stat is a process that exited mid-scan.
    let fx = Fixture::new("vanished");
    fs::create_dir_all(fx.root.join("7000")).expect("empty pid dir");
    fx.abandoned_server(7001, "node", &["node", "vite", "dev"]);
    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "the survivor is still reported");
    assert_eq!(found[0].pid, 7001);
}

#[test]
fn malformed_stat_is_skipped_rather_than_panicking() {
    let fx = Fixture::new("malformed");
    let dir = fx.root.join("8000");
    fs::create_dir_all(&dir).expect("pid dir");
    fs::write(dir.join("stat"), "garbage without a comm field").expect("stat");
    fx.abandoned_server(8001, "node", &["node", "vite", "dev"]);
    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "garbage is skipped, the scan continues");
}

#[test]
fn a_comm_containing_spaces_and_parens_still_parses() {
    // The kernel allows spaces and parens in comm, which is why the parser
    // anchors on the last ')'. A naive whitespace split gets this wrong.
    let fx = Fixture::new("odd-comm");
    let dir = fx.root.join("8100");
    fs::create_dir_all(&dir).expect("pid dir");
    let stat = format!(
        "8100 (weird (name) here) S 1224 8100 8100 0 0 -1 4194304 0 0 0 {u} 0 0 0 20 0 1 0 \
         {st} 0 0 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0",
        u = 5 * TICKS,
        st = (10 * DAY_SECS - 3 * DAY_SECS) * TICKS,
    );
    fs::write(dir.join("stat"), stat).expect("stat");
    fs::write(dir.join("comm"), "weird (name) here\n").expect("comm");
    fs::write(dir.join("cmdline"), b"node\0vite\0dev\0").expect("cmdline");
    fs::write(dir.join("status"), "Name:\tweird\nVmRSS:\t1024 kB\n").expect("status");

    let found = fx.default_scan();
    assert_eq!(found.len(), 1, "an odd comm still classifies: {found:?}");
    assert_eq!(found[0].comm, "weird (name) here");
    assert_eq!(found[0].idle_hours, 72);
}

#[test]
fn results_are_ordered_by_rss_then_pid() {
    // Stable ordering makes consecutive guard reports comparable.
    let fx = Fixture::new("ordering");
    fx.proc(
        500,
        "node",
        &["node", "vite"],
        'S',
        0,
        1,
        3 * DAY_SECS,
        1_000,
    );
    fx.proc(
        501,
        "node",
        &["node", "vite"],
        'S',
        0,
        1,
        3 * DAY_SECS,
        9_000,
    );
    fx.proc(
        502,
        "node",
        &["node", "vite"],
        'S',
        0,
        1,
        3 * DAY_SECS,
        9_000,
    );
    let found = fx.default_scan();
    let pids: Vec<i32> = found.iter().map(|c| c.pid).collect();
    assert_eq!(
        pids,
        vec![501, 502, 500],
        "rss desc, then pid asc: {pids:?}"
    );
}

#[test]
fn the_cpu_ceiling_is_inclusive() {
    // Exactly at the limit is idle; one second over is not. An off-by-one
    // here would either hide real candidates or admit a busy one.
    let fx = Fixture::new("cpu-boundary");
    fx.proc(
        601,
        "node",
        &["node", "vite"],
        'S',
        0,
        60,
        3 * DAY_SECS,
        1_000,
    );
    fx.proc(
        602,
        "node",
        &["node", "vite"],
        'S',
        0,
        61,
        3 * DAY_SECS,
        1_000,
    );
    let found = fx.default_scan();
    let pids: Vec<i32> = found.iter().map(|c| c.pid).collect();
    assert_eq!(pids, vec![601], "60s is idle, 61s is not: {pids:?}");
}

#[test]
fn the_idle_floor_is_inclusive() {
    let fx = Fixture::new("idle-boundary");
    fx.proc(
        701,
        "node",
        &["node", "vite"],
        'S',
        0,
        1,
        DAY_SECS - 60,
        1_000,
    );
    fx.proc(
        702,
        "node",
        &["node", "vite"],
        'S',
        0,
        1,
        DAY_SECS + 60,
        1_000,
    );
    let found = fx.default_scan();
    let pids: Vec<i32> = found.iter().map(|c| c.pid).collect();
    assert_eq!(pids, vec![702], "23h59m is too young, 24h01m qualifies");
}

#[test]
fn a_widened_allowlist_matches_a_new_signature() {
    // The allowlist is operator-owned; adding a signature widens the report
    // and nothing else.
    let fx = Fixture::new("custom-signature");
    fx.proc(
        801,
        "mytool",
        &["mytool", "--serve"],
        'S',
        0,
        2,
        4 * DAY_SECS,
        2_048,
    );
    assert!(fx.default_scan().is_empty(), "not on the default allowlist");
    let custom = ReapPolicy {
        signatures: vec!["mytool --serve".to_string()],
        ..ReapPolicy::default()
    };
    let found = fx.scan(&custom);
    assert_eq!(found.len(), 1, "the custom signature matches");
    assert_eq!(found[0].signature, "mytool --serve");
}

#[test]
fn a_missing_proc_root_reports_nothing() {
    let missing = std::env::temp_dir().join("dracon-reap-test-does-not-exist");
    let _ = fs::remove_dir_all(&missing);
    assert!(
        scan_reap_candidates(&missing, &policy(), 0, 0, TICKS).is_empty(),
        "an unreadable proc root degrades to an empty report"
    );
}

#[test]
fn boot_time_is_read_from_the_proc_stat_marker() {
    let fx = Fixture::new("btime");
    fs::write(fx.root.join("stat"), "cpu  1 2 3\nbtime 1700000000\n").expect("stat");
    assert_eq!(read_boot_time(&fx.root), Some(1_700_000_000));
    fs::write(fx.root.join("stat"), "cpu  1 2 3\n").expect("stat");
    assert_eq!(read_boot_time(&fx.root), None, "no btime, no clock");
    assert_eq!(read_boot_time(Path::new("/nonexistent-proc")), None);
}

#[test]
fn elapsed_never_underflows_or_divides_by_zero() {
    // A zero clock divisor or a starttime in the future must not panic;
    // both just mean "not old enough".
    assert_eq!(elapsed_secs(0, 0, 100, 200), 0, "zero ticks divisor");
    assert_eq!(
        elapsed_secs(1_000_000, TICKS, 100, 200),
        0,
        "start in the future"
    );
    assert_eq!(
        elapsed_secs(0, TICKS, 100, 200),
        100,
        "starttime 0 means it started at boot, so age is now - boot"
    );
}
