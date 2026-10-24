//! Tests for the forgotten heavy orphans — the shape the operator
//! described as "not long running and working but truly heavy and
//! forgotten, like some chrome instances left behind during tests".
//!
//! CHANGED 2026-10-23: the pass weighs a whole TREE, not one process.
//! The tests keep the old per-process cases (a lone heavy browser is
//! still found) and add the cases that cost 8.6 GB on 2026-10-23: a tree
//! of many small processes, the exempt list that swallowed the whole
//! browser family, and the pressure gate that the documentation claimed
//! existed but did not.

use super::forgotten::*;
use crate::reap::{
    cmdline_executable, matches_exempt, verify_forgotten_tree_for_reap, ReapPolicy,
};
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

    fn scan(&self, policy: &ReapPolicy, floor_mb: u64) -> ForgottenTreeScan {
        scan_forgotten_orphan_trees(
            &self.root,
            policy,
            self.boot_time,
            self.now,
            TICKS,
            floor_mb,
            floor_mb,
        )
    }

    /// The scan with two independent floors: the per-process one and the
    /// tree-aggregate one.
    fn scan_with_floors(
        &self,
        policy: &ReapPolicy,
        per_process_mb: u64,
        tree_mb: u64,
    ) -> ForgottenTreeScan {
        scan_forgotten_orphan_trees(
            &self.root,
            policy,
            self.boot_time,
            self.now,
            TICKS,
            per_process_mb,
            tree_mb,
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
        forgotten_tree_footprint_mb: 1024,
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
    assert_eq!(c.root_pid, 4242);
    assert_eq!(c.member_count, 1);
    assert_eq!(c.aggregate_footprint_mb, 4096);
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
        scan.vetoed[0].aggregate_footprint_mb > 0,
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

/// THE 2026-10-23 REGRESSION: a tree of many SMALL processes.
///
/// The leak that cost 8.6 GB was 792 orphaned browser processes whose
/// median single-process weight was 3 MiB. Under the old per-process
/// floor of 2 GiB, not one of them ever qualified, so the guard logged
/// critical memory pressure and reaped nothing. The weight that decides
/// is the tree's AGGREGATE.
#[test]
fn a_tree_of_many_small_processes_is_a_candidate_by_its_aggregate_weight() {
    let fx = Fixture::new("aggregate");
    fx.fake_parent(1, "systemd");
    // The root: a headless test browser whose owner died.
    fx.proc(
        3000,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--no-sandbox",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        3 * 3600,
        3,
    );
    // Its renderers: 40 of them, 3 MiB each — 120 MiB, well under the
    // 2 GiB per-process floor, none of them an orphan on its own.
    for i in 0..40 {
        fx.proc(
            3100 + i,
            3000,
            "chromium",
            &[
                "/usr/bin/chromium",
                "--type=renderer",
                "--headless=new",
                "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
            ],
            'S',
            0,
            1,
            3 * 3600,
            3,
        );
    }

    // The OLD behaviour: nothing qualifies, because no single process
    // crosses the 2 GiB per-process floor.
    let per_process_only = fx.scan_with_floors(&policy(), 2048, u64::MAX);
    assert!(
        per_process_only.candidates.is_empty(),
        "no single process is 2 GiB: {:?}",
        per_process_only.candidates
    );

    // The NEW behaviour: the tree's aggregate is 123 MiB, which clears
    // the 64 MiB aggregate floor used here.
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(
        scan.candidates.len(),
        1,
        "the tree must be found by its aggregate weight: {:?}",
        scan.candidates
    );
    let tree = &scan.candidates[0];
    assert_eq!(tree.root_pid, 3000);
    assert_eq!(tree.member_count, 41, "root plus forty renderers");
    assert_eq!(tree.aggregate_footprint_mb, 123);
    assert_eq!(tree.root_footprint_mb, 3, "the root alone is tiny");
}

/// A tree that is heavy in aggregate but whose ROOT is a desktop session
/// is vetoed as a whole — the veto protects the tree, not just the root.
#[test]
fn a_heavy_desktop_tree_is_vetoed_even_though_its_root_is_light() {
    let fx = Fixture::new("veto_tree");
    fx.fake_parent(1, "systemd");
    fx.proc(
        4000,
        1,
        "chrome",
        &["/nix/store/x/google-chrome/chrome", "--profile-directory=Default"],
        'S',
        0,
        1,
        10 * 3600,
        10,
    );
    for i in 0..40 {
        fx.proc(
            4100 + i,
            4000,
            "chrome",
            &["/nix/store/x/google-chrome/chrome", "--type=renderer"],
            'S',
            0,
            1,
            10 * 3600,
            100,
        );
    }
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert!(
        scan.candidates.is_empty(),
        "a desktop tree must never be a candidate: {:?}",
        scan.candidates
    );
    assert_eq!(
        scan.vetoed.len(),
        1,
        "the heavy desktop tree must be reported as vetoed: {:?}",
        scan.vetoed
    );
    assert_eq!(scan.vetoed[0].member_count, 41);
    assert_eq!(scan.vetoed[0].aggregate_footprint_mb, 4010);
}

/// The detached crashpad handler belongs to the tree: Chromium's handler
/// calls `setsid` and is reparented to init, so it survives the browser
/// as a lone orphan — but the browser's own command lines name it.
#[test]
fn the_detached_crashpad_handler_is_part_of_the_tree() {
    let fx = Fixture::new("crashpad");
    fx.fake_parent(1, "systemd");
    fx.proc(
        5000,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        3 * 3600,
        100,
    );
    fx.proc(
        5001,
        5000,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--type=zygote",
            "--crashpad-handler-pid=5900",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        1,
        3 * 3600,
        100,
    );
    // The handler detached out of the tree and was reparented to init.
    fx.fake_parent(1, "systemd");
    fx.proc(
        5900,
        1,
        "chrome_crashpad_handler",
        &["/nix/store/x/chromium/chrome_crashpad_handler", "--monitor-self"],
        'S',
        0,
        1,
        3 * 3600,
        50,
    );

    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];
    assert_eq!(tree.member_count, 3, "root, zygote, and the handler");
    assert!(
        tree.members.iter().any(|m| m.pid == 5900),
        "the crashpad handler must be in the tree: {:?}",
        tree.members
    );
    assert_eq!(tree.aggregate_footprint_mb, 250);
}

/// A tree that has only just started cannot be a leak anybody had a
/// chance to notice — and a freshly detached browser may still have a
/// client attached to it. The lifetime-CPU gate does NOT tell the two
/// apart (a live measurement found an orphaned Chromium root with only
/// FOUR SECONDS of CPU), so time is the gate.
#[test]
fn a_tree_that_has_only_just_started_is_not_a_candidate() {
    let fx = Fixture::new("young");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9100,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        30, // thirty seconds old
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert!(
        scan.candidates.is_empty(),
        "a 30-second-old tree must not be a candidate: {:?}",
        scan.candidates
    );

    // The same tree, old enough, is.
    let fx = Fixture::new("old-enough");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9200,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        FORGOTTEN_MIN_TREE_AGE_SECS + 60,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(
        scan.candidates.len(),
        1,
        "a tree past the age floor must be a candidate: {:?}",
        scan.candidates
    );
}

/// THE 2026-10-23 OPERATOR DECISION: for a BROWSER, the exempt list does
/// not decide — the desktop-session veto does.
///
/// `process_exempt_names` names whole APPLICATIONS. The token `chrome`
/// protects the operator's live session AND every headless test browser,
/// and the headless test browsers are the leak: two orphaned google-chrome
/// test trees on this host hold 2.1 GB that the guard could never see.
/// The veto tells them apart exactly, so a headless test browser is a
/// candidate even with `chrome` on the exempt list, and the operator's own
/// desktop Chrome is vetoed even though the name would have exempted it.
#[test]
fn for_a_browser_the_desktop_veto_decides_not_the_exempt_list() {
    let mut exempt = policy();
    exempt.exempt_names = vec!["chrome".to_string()];

    // A headless google-chrome test tree: a candidate, despite the name.
    let fx = Fixture::new("veto-not-exempt");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9300,
        1,
        "chrome",
        &[
            "/nix/store/x/google-chrome/share/google/chrome/chrome",
            "--headless=new",
            "--user-data-dir=/tmp/cw-real-profile-standard-1792777061299",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&exempt, 2048, 64);
    assert_eq!(
        scan.candidates.len(),
        1,
        "a headless test browser must be a candidate even when 'chrome' is exempt: {:?}",
        scan.candidates
    );

    // The operator's own desktop Chrome: VETOED, never a candidate.
    let fx = Fixture::new("veto-not-exempt-desktop");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9400,
        1,
        "chrome",
        &[
            "/nix/store/x/google-chrome/share/google/chrome/chrome",
            "--ozone-platform=x11",
            "--no-first-run",
        ],
        'S',
        0,
        1,
        10 * 3600,
        8192,
    );
    let scan = fx.scan_with_floors(&exempt, 2048, 64);
    assert!(
        scan.candidates.is_empty(),
        "the operator's desktop Chrome must never be a candidate: {:?}",
        scan.candidates
    );
}

/// The kill-time gate applies the same rule: a browser that no longer
/// looks like a test job is not ours to end.
#[test]
fn the_kill_time_gate_uses_the_veto_for_a_browser_too() {
    let mut exempt = policy();
    exempt.exempt_names = vec!["chrome".to_string()];
    let fx = Fixture::new("veto-not-exempt-kill");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9500,
        1,
        "chrome",
        &[
            "/nix/store/x/google-chrome/share/google/chrome/chrome",
            "--headless=new",
            "--user-data-dir=/tmp/cw-real-profile-standard-1792777061299",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&exempt, 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, tree, &exempt, TICKS).is_some(),
        "a headless test browser must pass the kill-time gate"
    );

    // The root turns into the operator's desktop session: refused.
    let dir = fx.root.join("9500");
    fs::write(
        dir.join("cmdline"),
        b"/nix/store/x/google-chrome/share/google/chrome/chrome\0--ozone-platform=x11\0",
    )
    .expect("cmdline");
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, tree, &exempt, TICKS).is_none(),
        "a browser that became a desktop session must not be signalled"
    );
}

/// A non-browser exempt process keeps its name exemption: the veto only
/// speaks for browsers.
#[test]
fn a_non_browser_exempt_process_keeps_its_name_exemption() {
    let mut exempt = policy();
    exempt.exempt_names = vec!["bun".to_string()];
    let fx = Fixture::new("veto-not-exempt-other");
    fx.fake_parent(1, "systemd");
    fx.proc(
        9600,
        1,
        "bun",
        &["bun", "test", "src/lib"],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&exempt, 2048, 64);
    assert!(
        scan.candidates.is_empty(),
        "a non-browser exempt process keeps its exemption: {:?}",
        scan.candidates
    );
}

/// THE 2026-10-23 EXEMPT REGRESSION: the exempt list used to be matched
/// as a substring of the WHOLE command line, so the token `chrome`
/// matched `--user-data-dir=/tmp/puppeteer_dev_chrome_profile-*` and every
/// Puppeteer test browser was exempt — invisible to the guard before any
/// other gate ran.
#[test]
fn a_test_profile_path_does_not_exempt_a_test_browser() {
    // The RAW cmdline, NUL-separated as /proc gives it: argv[0] is the
    // executable, the rest are its arguments.
    let test_browser = "/usr/bin/chromium\0--headless=new\0--no-sandbox\0--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc\0";
    let exempt = vec!["chrome".to_string()];
    assert!(
        !matches_exempt(&cmdline_executable(test_browser), &exempt),
        "a puppeteer test browser must NOT be exempt just because its profile path contains 'chrome'"
    );
    // The operator's own browser IS protected: its executable is `chrome`.
    let own = "/nix/store/x/google-chrome-149.0.7827.200/share/google/chrome/chrome\0--ozone-platform=x11\0";
    assert!(
        matches_exempt(&cmdline_executable(own), &exempt),
        "the operator's own chrome must stay exempt"
    );
    // The playwright family was never caught by the substring either, and
    // must stay visible now.
    let playwright = "/nix/store/x/chromium/chromium\0--headless=new\0--user-data-dir=/tmp/playwright_chromiumdev_profile-Zz\0";
    assert!(
        !matches_exempt(&cmdline_executable(playwright), &exempt),
        "a playwright test browser must not be exempt"
    );
}

/// The exempt list is a list of PROCESS NAMES, and it extends a name
/// with a `-` (so `wezterm` covers `wezterm-gui`) but not with a `_`
/// (so `chrome` does not swallow `chrome_crashpad_handler`).
#[test]
fn the_exempt_list_matches_process_names() {
    let exempt = vec!["wezterm".to_string(), "chrome".to_string()];
    assert!(matches_exempt("wezterm", &exempt));
    assert!(matches_exempt("wezterm-gui", &exempt));
    assert!(matches_exempt("chrome", &exempt));
    assert!(!matches_exempt("chrome_crashpad_handler", &exempt));
    assert!(!matches_exempt("chromium", &exempt));
    assert!(!matches_exempt("", &exempt));
}

/// THE DOCUMENTED INVARIANT: the pressure gate is a named function that
/// fails CLOSED. The design doc and the CHANGELOG both claimed it
/// existed and that a test pinned its closed side; neither did.
#[test]
fn the_forgotten_reap_is_skipped_without_memory_pressure() {
    assert!(
        !forgotten_reap_allowed_under_pressure("ok"),
        "no pressure is not permission to end a process"
    );
    assert!(!forgotten_reap_allowed_under_pressure(""));
    assert!(
        !forgotten_reap_allowed_under_pressure("unknown"),
        "an unrecognised pressure level must fail closed"
    );
    assert!(
        !forgotten_reap_allowed_under_pressure("action"),
        "an unrecognised pressure level must fail closed"
    );
    assert!(forgotten_reap_allowed_under_pressure("warn"));
    assert!(forgotten_reap_allowed_under_pressure("critical"));
}

/// The kill-time re-verification applies the desktop veto AGAIN: a tree
/// whose root no longer looks like a test job is not ours to end.
#[test]
fn the_kill_time_gate_rejects_a_tree_that_is_now_a_desktop_session() {
    let fx = Fixture::new("kill_gate");
    fx.fake_parent(1, "systemd");
    fx.proc(
        6000,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];

    // Unchanged, the gate accepts it.
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, tree, &policy(), TICKS).is_some(),
        "an unchanged test tree must pass the kill-time gate"
    );

    // Now the root is a desktop session: the gate must refuse.
    let dir = fx.root.join("6000");
    fs::write(
        dir.join("cmdline"),
        b"/usr/bin/google-chrome\0--profile-directory=Default\0",
    )
    .expect("cmdline");
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, tree, &policy(), TICKS).is_none(),
        "a tree that turned into a desktop session must not be signalled"
    );
}

/// The kill-time gate refuses a recycled PID, and refuses a tree that is
/// no longer heavy.
#[test]
fn the_kill_time_gate_refuses_a_recycled_pid_or_a_light_tree() {
    let fx = Fixture::new("kill_gate_2");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7000,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];

    // A recycled PID (the starttime no longer matches) is never signalled.
    let mut recycled = tree.clone();
    recycled.root_starttime += 1;
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, &recycled, &policy(), TICKS).is_none(),
        "a recycled PID must never be signalled"
    );

    // No longer heavy: the footprint is below the floor.
    let dir = fx.root.join("7000");
    fs::write(
        dir.join("status"),
        "Name:\tchromium\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t1024 kB\nVmSwap:\t0 kB\n",
    )
    .expect("status");
    assert!(
        verify_forgotten_tree_for_reap(&fx.root, tree, &policy(), TICKS).is_none(),
        "a tree that is no longer heavy must not be signalled"
    );
}

/// A member whose PID was recycled is left out of the kill set rather
/// than ending the whole tree: the rest of the tree is still reclaimable.
#[test]
fn a_recycled_member_is_left_out_of_the_kill_set_not_the_whole_tree() {
    let fx = Fixture::new("kill_gate_3");
    fx.fake_parent(1, "systemd");
    fx.proc(
        8000,
        1,
        "chromium",
        &[
            "/usr/bin/chromium",
            "--headless=new",
            "--user-data-dir=/tmp/puppeteer_dev_chrome_profile-abc",
        ],
        'S',
        0,
        2,
        3 * 3600,
        4096,
    );
    fx.proc(
        8001,
        8000,
        "chromium",
        &["/usr/bin/chromium", "--type=renderer"],
        'S',
        0,
        1,
        3 * 3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];
    assert_eq!(tree.member_count, 2);

    // The renderer is recycled: it is no longer the process the scan saw.
    let uptime = fx.now - fx.boot_time;
    let dir = fx.root.join("8001");
    fs::write(
        dir.join("stat"),
        format!(
            "8001 (chromium) S 8000 8001 8001 0 0 -1 4194304 0 0 0 100 0 0 0 20 0 1 0 {} 0 0 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0",
            uptime * TICKS
        ),
    )
    .expect("stat");

    let verified = verify_forgotten_tree_for_reap(&fx.root, tree, &policy(), TICKS)
        .expect("the tree must still be reapable");
    assert!(
        verified.members.iter().all(|m| m.pid != 8001),
        "the recycled member must be left out: {:?}",
        verified.members
    );
    assert_eq!(verified.skipped, 1);
}

// ---------------------------------------------------------------------------
// "Is anyone still driving it?" (ADDED 2026-10-24, audit)
// ---------------------------------------------------------------------------
//
// The pass used to decide that with LIFETIME CPU: a tree that had burned
// more than `max_cpu_seconds` was skipped — not reported, not reaped,
// forever. That is the wrong measurement, and it fails in the worst
// direction: the leak that mattered is a leak that DID real work, and the
// more work it did the more certain the guard was to ignore it. On this
// host it hid an orphaned 1.5 GB, 20-process headless browser that nothing
// was driving.
//
// The precise question is "is anyone still driving it", and a browser
// driven over the DevTools protocol holds an ESTABLISHED connection to its
// debugging port. A browser nobody drives holds none.

#[test]
fn the_devtools_port_is_read_from_the_command_line() {
    assert_eq!(
        devtools_port("chrome --remote-debugging-port=9222 --headless=new"),
        Some(9222)
    );
    // Port 0 means "the OS picks one" — the real port is in the profile.
    assert_eq!(devtools_port("chrome --remote-debugging-port=0"), None);
    assert_eq!(devtools_port("chrome --headless=new"), None);
    assert_eq!(devtools_port("chrome --remote-debugging-port=abc"), None);
    assert_eq!(devtools_port(""), None);
}

#[test]
fn the_devtools_port_is_read_from_the_profile_when_the_flag_is_zero() {
    let profile = std::env::temp_dir().join(format!("dracon-dtport-{}", std::process::id()));
    let _ = fs::remove_dir_all(&profile);
    fs::create_dir_all(&profile).expect("profile dir");
    fs::write(
        profile.join("DevToolsActivePort"),
        "59999\n/devtools/browser/abc-def\n",
    )
    .expect("DevToolsActivePort");
    let args = format!(
        "chrome --remote-debugging-port=0 --user-data-dir={}",
        profile.display()
    );
    assert_eq!(devtools_port(&args), None);
    assert_eq!(devtools_port_from_profile(&args), Some(59999));
    let _ = fs::remove_dir_all(&profile);
}

#[test]
fn an_established_connection_is_what_means_a_client_is_attached() {
    let path = std::env::temp_dir().join(format!("dracon-tcp-{}", std::process::id()));
    fs::write(
        &path,
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
         0: 0100007F:1F90 0100007F:A24D 01 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000 100 0 0 10 0\n\
         1: 0100007F:C350 0100007F:2328 0A 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0000 100 0 0 10 0\n",
    )
    .expect("tcp table");
    // A24D == 41549, established. The row also proves that the SLOT column
    // ("0:") is not mistaken for the local address.
    assert!(tcp_table_has_established(&path, 41549));
    // 2328 == 9000, in TIME_WAIT (0A), not established.
    assert!(!tcp_table_has_established(&path, 9000));
    // Nothing at all is listening on this one.
    assert!(!tcp_table_has_established(&path, 1234));
    let _ = fs::remove_file(&path);
}

/// The port column of `/proc/net/tcp` is HEX, and getting it wrong turns a
/// live client into an invisible one.
///
/// This test pins the conversion: the fixture above uses `A24D`, and the
/// port it means is 41549. An earlier fixture used `A2A5` and asserted the
/// same port — A2A5 is 41637, so the assertion could only ever have been
/// satisfied by a bug. The live case is the same one: the leaked browser's
/// DevTools port 41549 appears in `/proc/net/tcp` as `A24D`.
#[test]
fn the_tcp_port_column_is_read_as_hexadecimal() {
    assert_eq!(41549u16, u16::from_str_radix("A24D", 16).unwrap());
    assert_eq!(u16::from_str_radix("A2A5", 16).unwrap(), 41637);
    // The shape of a real row: the slot column is "42:", not an address.
    let (local, remote, state) =
        parse_proc_tcp_line("  42: 0100007F:A24D 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 458732102 1 0000 100 0 0 10 0").unwrap();
    assert_eq!(local, 41549);
    assert_eq!(remote, 0);
    assert_eq!(state, "0A");
}

/// THE REGRESSION: a heavy, orphaned, headless tree that did a lot of work
/// in its life but that NOBODY is driving is a LEAK, and it must be a
/// candidate. Before this change the lifetime-CPU gate hid it completely.
#[test]
fn a_busy_tree_that_nobody_is_driving_is_a_candidate() {
    let fx = Fixture::new("undriven");
    fx.fake_parent(1, "systemd");
    // A profile whose DevToolsActivePort names a port nothing is using.
    let profile = std::env::temp_dir().join(format!("dracon-undriven-{}", std::process::id()));
    let _ = fs::remove_dir_all(&profile);
    fs::create_dir_all(&profile).expect("profile dir");
    fs::write(profile.join("DevToolsActivePort"), "59997\n").expect("port file");
    let user_data = format!("--user-data-dir={}", profile.display());
    fx.proc(
        7000,
        1,
        "chrome",
        &[
            "/usr/bin/chrome",
            "--headless=new",
            &user_data,
            "--remote-debugging-port=0",
        ],
        'S',
        0,
        // 900 s of lifetime CPU: far past the policy's 30 s limit.
        900,
        3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(
        scan.candidates.len(),
        1,
        "a heavy orphan nobody is driving must be a candidate: {:?}",
        scan.candidates
    );
    assert_eq!(scan.candidates[0].root_pid, 7000);
    assert!(scan.protected.is_empty(), "{:?}", scan.protected);
    let _ = fs::remove_dir_all(&profile);
}

/// A tree with no debugging port cannot be driven over one, so the
/// conservative CPU proxy still decides — but the tree is REPORTED, not
/// skipped in silence.
#[test]
fn a_busy_tree_with_no_debug_port_is_left_alone_but_reported() {
    let fx = Fixture::new("busy-noport");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7100,
        1,
        "chrome",
        &["/usr/bin/chrome", "--headless=new"],
        'S',
        0,
        900,
        3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    assert_eq!(scan.protected.len(), 1, "{:?}", scan.protected);
    assert!(
        scan.protected[0].reason.contains("busy"),
        "{}",
        scan.protected[0].reason
    );
    let _ = fs::remove_dir_all(&fx.root);
}

// ---------------------------------------------------------------------------
// The profile a leaked browser leaves behind (ADDED 2026-10-24)
// ---------------------------------------------------------------------------

/// A leaked browser is TWO halves: the processes and the profile directory
/// they owned. The profile is read from the ROOT's command line, with its
/// ORIGINAL case — a directory whose name contains an uppercase letter does
/// not exist at the lowercased path.
#[test]
fn the_profile_directory_is_read_from_the_root_command_line() {
    let fx = Fixture::new("profile");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7200,
        1,
        "chrome",
        &[
            "/usr/bin/chrome",
            "--headless=new",
            "--user-data-dir=/tmp/CW-Real-Profile-1234",
        ],
        'S',
        0,
        0,
        3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.candidates);
    let tree = &scan.candidates[0];
    assert_eq!(
        tree_user_data_dir(tree),
        Some(PathBuf::from("/tmp/CW-Real-Profile-1234"))
    );
    // A tree that named no profile yields none.
    let fx2 = Fixture::new("noprofile");
    fx2.fake_parent(1, "systemd");
    fx2.proc(
        7201,
        1,
        "chrome",
        &["/usr/bin/chrome", "--headless=new"],
        'S',
        0,
        0,
        3600,
        4096,
    );
    let scan2 = fx2.scan_with_floors(&policy(), 2048, 64);
    assert_eq!(
        tree_user_data_dir(&scan2.candidates[0]),
        None,
        "a tree with no --user-data-dir yields no profile"
    );
    let _ = fs::remove_dir_all(&fx.root);
    let _ = fs::remove_dir_all(&fx2.root);
}

/// A tree yields its profile ONLY when it is provably gone. One surviving
/// member is enough to make the reclaim wrong.
#[test]
fn a_tree_that_partly_survived_yields_no_profile() {
    let fx = Fixture::new("survivor");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7300,
        1,
        "chrome",
        &[
            "/usr/bin/chrome",
            "--headless=new",
            "--user-data-dir=/tmp/profile-survivor",
        ],
        'S',
        0,
        0,
        3600,
        4096,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 64);
    let tree = &scan.candidates[0];
    assert_eq!(
        tree_yields_profile(tree, 0),
        Some(PathBuf::from("/tmp/profile-survivor")),
        "a fully ended tree yields its profile"
    );
    assert_eq!(
        tree_yields_profile(tree, 1),
        None,
        "a tree with a surviving member yields nothing"
    );
    let _ = fs::remove_dir_all(&fx.root);
}

/// THE REGRESSION: a browser's own helpers must not be carved out of its
/// tree by the exempt list.
///
/// The exempt list names APPLICATIONS — `chrome` is on the operator's list
/// — and every process of a chrome tree is named `chrome`. The ROOT was
/// already admitted despite the name exemption (the desktop veto decides
/// for a browser), but the exemption was still applied to the tree's
/// DESCENDANTS while the tree was built. Two things followed, and both were
/// wrong:
///
///   * the tree's AGGREGATE weight was measured as the ROOT'S alone, so a
///     379 MiB, 9-process tree weighed 122 MiB — below the floor, and
///     invisible to the very pass that exists to see it;
///   * when such a tree WAS ended, its helpers were not in the tree, so
///     they were never signalled and survived it.
#[test]
fn a_browsers_own_helpers_stay_in_its_tree() {
    let mut pol = policy();
    // The operator's real exempt list names chrome.
    pol.exempt_names = vec!["chrome".to_string()];
    let fx = Fixture::new("family");
    fx.fake_parent(1, "systemd");
    // The root: 40 MiB.
    fx.proc(
        7400,
        1,
        "chrome",
        &[
            "/usr/bin/chrome",
            "--headless=new",
            "--user-data-dir=/tmp/profile-family",
        ],
        'S',
        0,
        0,
        3600,
        40,
    );
    // Three helpers, each 120 MiB, each named chrome too.
    for (i, pid) in [7401i32, 7402, 7403].iter().enumerate() {
        fx.proc(
            *pid,
            7400,
            "chrome",
            &["/usr/bin/chrome", "--type=renderer"],
            'S',
            0,
            0,
            3600,
            120,
        );
        let _ = i;
    }
    // The tree's real weight: 40 + 120*3 = 400 MiB. The root alone is 40.
    let scan = fx.scan_with_floors(&pol, 2048, 256);
    assert_eq!(
        scan.candidates.len(),
        1,
        "a 400 MiB chrome tree must be a candidate: {:?}",
        scan.candidates
    );
    let tree = &scan.candidates[0];
    assert_eq!(
        tree.member_count, 4,
        "the browser's own helpers must be IN the tree: {:?}",
        tree.members
    );
    assert!(
        tree.aggregate_footprint_mb >= 400,
        "the tree must be weighed as a whole, not as its root: {}",
        tree.aggregate_footprint_mb
    );
    let _ = fs::remove_dir_all(&fx.root);
}

/// A NON-browser tree keeps the exemption: an exempt daemon inside a
/// dev-server tree is still the operator's, and must neither be ended nor
/// counted as reclaimable.
#[test]
fn a_non_browser_tree_still_honours_the_exempt_list() {
    let mut pol = policy();
    pol.exempt_names = vec!["dbus-daemon".to_string()];
    let fx = Fixture::new("nonbrowser");
    fx.fake_parent(1, "systemd");
    fx.proc(
        7500,
        1,
        "node",
        &["/usr/bin/node", "server.js"],
        'S',
        0,
        0,
        3600,
        100,
    );
    fx.proc(
        7501,
        7500,
        "dbus-daemon",
        &["/usr/bin/dbus-daemon"],
        'S',
        0,
        0,
        3600,
        900,
    );
    let scan = fx.scan_with_floors(&pol, 2048, 256);
    // The exempt daemon is left out of the tree and out of the weight, so
    // the tree weighs 100 MiB and does not cross the floor.
    assert!(
        scan.candidates.is_empty(),
        "an exempt member must not be counted as reclaimable: {:?}",
        scan.candidates
    );
    let _ = fs::remove_dir_all(&fx.root);
}

/// A heavy DESKTOP session must be REPORTED, even when it is busy.
///
/// THE REGRESSION: the veto path skipped any tree whose root had burned
/// more than `reap_report_max_cpu_seconds` of lifetime CPU, so the
/// operator's own Chrome — 8.3 hours of CPU, 10.2 GiB, 65 processes — was
/// never reported by the guard, not once, because it was skipped before
/// it was ever weighed. That is the same lifetime-CPU-as-proxy error this
/// work removed from the REAP path, applied to the REPORT path, and it
/// failed in the same direction: the busier and heavier a tree was, the
/// more certain the guard was to hide it.
#[test]
fn a_busy_desktop_session_is_still_reported_as_vetoed() {
    let fx = Fixture::new("busy-desktop");
    fx.fake_parent(1, "systemd");
    // A live desktop browser: no --headless, no --user-data-dir, so the
    // veto classifies it as the operator's own session. 3 GiB.
    fx.proc(
        7600,
        1,
        "chrome",
        &["/usr/bin/google-chrome", "--profile-directory=Default"],
        'S',
        0,
        // 4000 s of lifetime CPU — far past the policy's 30 s limit.
        4000,
        3600,
        3072,
    );
    // A heavy helper, so the tree's aggregate is over the floor.
    fx.proc(
        7601,
        7600,
        "chrome",
        &["/usr/bin/google-chrome", "--type=renderer"],
        'S',
        0,
        4000,
        3600,
        1024,
    );
    let scan = fx.scan_with_floors(&policy(), 2048, 256);
    assert!(
        scan.candidates.is_empty(),
        "a desktop session is never a candidate: {:?}",
        scan.candidates
    );
    assert_eq!(
        scan.vetoed.len(),
        1,
        "a heavy desktop session must be REPORTED, busy or not: {:?}",
        scan.vetoed
    );
    assert_eq!(scan.vetoed[0].root_pid, 7600);
    assert!(
        scan.vetoed[0].aggregate_footprint_mb >= 4096,
        "the vetoed tree must be weighed as a whole: {}",
        scan.vetoed[0].aggregate_footprint_mb
    );
    let _ = fs::remove_dir_all(&fx.root);
}
