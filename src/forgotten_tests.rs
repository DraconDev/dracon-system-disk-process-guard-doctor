//! Tests for the forgotten heavy orphans — the shape the operator
//! described as "not long running and working but truly heavy and
//! forgotten, like some chrome instances left behind during tests".

use super::forgotten::*;
use crate::reap::{verify_forgotten_heavy_for_reap, ReapCandidate, ReapPolicy};
use std::fs;
use std::path::PathBuf;

const TICKS: u64 = 100;
const DAY_SECS: u64 = 86_400;

/// A fake /proc tree, built the same way the reap tests build one: the
/// scanner's own arithmetic is exercised, not a mock of it.
struct Fixture {
    root: PathBuf,
    boot_time: u64,
    now: u64,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "dracon-forgotten-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture proc root");
        let boot_time = 1_700_000_000;
        Fixture {
            root,
            boot_time,
            now: boot_time + 10 * DAY_SECS,
        }
    }

    /// One process in the tree. `ppid` defaults to a systemd parent, which
    /// is what "reparented to init" looks like.
    #[allow(clippy::too_many_arguments)]
    fn proc(
        &self,
        pid: i32,
        ppid: i32,
        comm: &str,
        args: &[&str],
        state: char,
        tty_nr: i32,
        cpu_secs: u64,
        age_secs: u64,
        rss_mb: u64,
    ) {
        let dir = self.root.join(pid.to_string());
        fs::create_dir_all(&dir).expect("pid dir");
        let uptime_secs = self.now - self.boot_time;
        let starttime = uptime_secs.saturating_sub(age_secs) * TICKS;
        let utime = cpu_secs * TICKS;
        let stime = 0;
        let stat = format!(
            "{pid} ({comm}) {state} {ppid} {pid} {pid} {tty_nr} 0 -1 4194304 0 0 0 \
             {utime} {stime} 0 0 20 0 1 0 {starttime} 0 0 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0",
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
            format!(
                "Name:\t{comm}\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t{} kB\nVmSwap:\t0 kB\n",
                rss_mb * 1024
            ),
        )
        .expect("status");
    }

    /// The parent whose command decides orphanhood.
    fn fake_parent(&self, pid: i32, comm: &str) {
        let dir = self.root.join(pid.to_string());
        fs::create_dir_all(&dir).expect("parent dir");
        fs::write(dir.join("comm"), format!("{comm}\n")).expect("parent comm");
    }

    fn scan(&self, policy: &ReapPolicy, floor_mb: u64) -> ForgottenScan {
        scan_forgotten_heavy_orphans(
            &self.root,
            policy,
            self.boot_time,
            self.now,
            TICKS,
            floor_mb,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn policy() -> ReapPolicy {
    ReapPolicy {
        min_idle_hours: 0,
        max_cpu_seconds: 30,
        signatures: Vec::new(),
        exempt_names: Vec::new(),
        forgotten_footprint_mb: 2048,
    }
}

/// The classification is the whole safety story: a browser that is not
/// headless and does not carry a test profile is the operator's own
/// session, whatever its weight.
#[test]
fn desktop_session_is_never_classified_as_a_test_job() {
    for args in [
        "/usr/bin/google-chrome --profile-directory=Default",
        "/usr/bin/chromium --no-sandbox",
        "google-chrome-stable",
        "/usr/bin/chromium --user-data-dir=/home/dracon/.config/google-chrome",
    ] {
        let verdict = classify_desktop_session(args);
        assert!(
            matches!(verdict, DesktopVerdict::Desktop { .. }),
            "a normal browser session must be vetoed, got {verdict:?} for {args}"
        );
    }
}

#[test]
fn headless_and_test_profiles_are_test_jobs() {
    let headless = classify_desktop_session("/usr/bin/chromium --headless=new --no-sandbox");
    assert!(
        matches!(headless, DesktopVerdict::Test { .. }),
        "{headless:?}"
    );

    let profile = classify_desktop_session(
        "/usr/bin/chromium --user-data-dir=/tmp/playwright_chromiumdev-1234/profile",
    );
    assert!(
        matches!(profile, DesktopVerdict::Test { .. }),
        "{profile:?}"
    );

    let cache = classify_desktop_session(
        "/usr/bin/chromium --user-data-dir=/home/dracon/.cache/puppeteer/chrome",
    );
    assert!(matches!(cache, DesktopVerdict::Test { .. }), "{cache:?}");
}

#[test]
fn a_non_browser_is_not_a_browser() {
    let verdict = classify_desktop_session("/usr/bin/node /home/dracon/Dev/app/server.js");
    assert_eq!(verdict, DesktopVerdict::NotABrowser);
}

/// A heavy, idle, orphaned headless browser IS a candidate, with its
/// evidence attached.
#[test]
fn a_forgotten_heavy_test_browser_is_a_candidate() {
    let fx = Fixture::new("candidate");
    fx.fake_parent(1, "systemd");
    fx.proc(
        4242,
        1,
        "chrome",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--no-sandbox",
            "--user-data-dir=/tmp/playwright_chromiumdev-99/profile",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan(&policy(), 2048);
    assert_eq!(
        scan.candidates.len(),
        1,
        "the forgotten browser must be found"
    );
    let c = &scan.candidates[0];
    assert_eq!(c.pid, 4242);
    assert_eq!(c.rss_mb, 4096);
    assert!(
        c.why.contains("headless"),
        "the reason must be recorded: {}",
        c.why
    );
    assert!(scan.vetoed.is_empty());
}

/// The operator's own browser, however heavy, is VETOED — and the veto is
/// reported, so the operator can see it was weighed and left alone.
#[test]
fn a_heavy_desktop_browser_is_vetoed_and_never_a_candidate() {
    let fx = Fixture::new("veto");
    fx.fake_parent(1, "systemd");
    fx.proc(
        5151,
        1,
        "chrome",
        &["/usr/bin/google-chrome", "--profile-directory=Default"],
        'S',
        0,
        1,
        10 * 3600,
        8192,
    );
    let scan = fx.scan(&policy(), 2048);
    assert!(
        scan.candidates.is_empty(),
        "a desktop session must never be a candidate: {:?}",
        scan.candidates
    );
    assert_eq!(scan.vetoed.len(), 1, "the veto must be reported");
    assert!(
        !scan.vetoed[0].reason.is_empty(),
        "the veto must say why it was vetoed"
    );
    assert!(
        scan.vetoed[0].footprint_mb > 0,
        "the veto must carry the weight that made it worth considering"
    );
}

/// A light process is not "heavy", whatever else it is.
#[test]
fn a_light_orphan_is_not_a_candidate() {
    let fx = Fixture::new("light");
    fx.fake_parent(1, "systemd");
    fx.proc(
        6161,
        1,
        "chrome",
        &["/usr/bin/chromium", "--headless=new"],
        'S',
        0,
        1,
        3 * 3600,
        64,
    );
    let scan = fx.scan(&policy(), 2048);
    assert!(
        scan.candidates.is_empty(),
        "a 64 MiB process is not heavy: {:?}",
        scan.candidates
    );
}

/// A WORKING process is not forgotten: the criteria require idleness, and
/// a busy process is doing something.
#[test]
fn a_busy_orphan_is_not_a_candidate() {
    let fx = Fixture::new("busy");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7171,
        1,
        "chrome",
        &["/usr/bin/chromium", "--headless=new"],
        'S',
        0,
        900,
        3 * 3600,
        4096,
    );
    let scan = fx.scan(&policy(), 2048);
    assert!(
        scan.candidates.is_empty(),
        "a process burning CPU is working, not forgotten: {:?}",
        scan.candidates
    );
}

/// The kill-time re-verification applies the desktop veto AGAIN: a process
/// that no longer looks like a test job is not ours to end, whatever the
/// scan said.
#[test]
fn the_kill_time_gate_rejects_a_process_that_is_now_a_desktop_session() {
    let fx = Fixture::new("kill_gate");
    fx.fake_parent(1, "systemd");
    fx.proc(
        8181,
        1,
        "chrome",
        &["/usr/bin/chromium", "--headless=new"],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let candidate = ReapCandidate {
        pid: 8181,
        comm: "chrome".to_string(),
        args: "/usr/bin/chromium --headless=new".to_string(),
        idle_hours: 3,
        cpu_seconds: 2,
        rss_mb: 4096,
        signature: "headless".to_string(),
        starttime: {
            let uptime = fx.now - fx.boot_time;
            (uptime - 3 * 3600) * TICKS
        },
    };
    // The scan's own verdict: it is a test job, so the gate accepts it.
    assert!(
        verify_forgotten_heavy_for_reap(&fx.root, &candidate, &policy(), TICKS),
        "an unchanged test job must pass the kill-time gate"
    );

    // Now the same process is a desktop session: the gate must refuse.
    let dir = fx.root.join("8181");
    fs::write(
        dir.join("cmdline"),
        b"/usr/bin/google-chrome\0--profile-directory=Default\0",
    )
    .expect("cmdline");
    assert!(
        !verify_forgotten_heavy_for_reap(&fx.root, &candidate, &policy(), TICKS),
        "a process that turned into a desktop session must not be signalled"
    );
}

/// The kill-time gate also refuses a process that is no longer heavy, and
/// one whose PID was recycled.
#[test]
fn the_kill_time_gate_refuses_a_light_or_recycled_process() {
    let fx = Fixture::new("kill_gate_2");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9191,
        1,
        "chrome",
        &["/usr/bin/chromium", "--headless=new"],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let uptime = fx.now - fx.boot_time;
    let candidate = ReapCandidate {
        pid: 9191,
        comm: "chrome".to_string(),
        args: "/usr/bin/chromium --headless=new".to_string(),
        idle_hours: 3,
        cpu_seconds: 2,
        rss_mb: 4096,
        signature: "headless".to_string(),
        starttime: (uptime - 3 * 3600) * TICKS,
    };
    // A recycled PID (the starttime no longer matches) is never signalled.
    let mut recycled = candidate.clone();
    recycled.starttime += 1;
    assert!(
        !verify_forgotten_heavy_for_reap(&fx.root, &recycled, &policy(), TICKS),
        "a recycled PID must never be signalled"
    );

    // No longer heavy: the footprint is below the floor.
    let dir = fx.root.join("9191");
    fs::write(
        dir.join("status"),
        "Name:\tchrome\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t1024 kB\nVmSwap:\t0 kB\n",
    )
    .expect("status");
    assert!(
        !verify_forgotten_heavy_for_reap(&fx.root, &candidate, &policy(), TICKS),
        "a process that is no longer heavy must not be signalled"
    );
}
