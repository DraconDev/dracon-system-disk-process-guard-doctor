use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn run_guard_with_policy(contents: &str) -> Output {
    let path = unique_policy_path();
    fs::write(&path, contents).expect("write policy fixture");
    let output = run_guard_at(&path);
    let _ = fs::remove_file(&path);
    output
}

fn run_guard_at(path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dracon-system"))
        .env("DRACON_SYSTEM_POLICY", path)
        .args(["guard", "daemon"])
        .output()
        .expect("run guard daemon")
}

fn run_status_at(path: &Path, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dracon-system"));
    command.env("DRACON_SYSTEM_POLICY", path).arg("status");
    if json {
        command.arg("--json");
    }
    command.output().expect("run status")
}

fn run_status_with_home(home: &Path, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dracon-system"));
    command
        .env_remove("DRACON_SYSTEM_POLICY")
        .env("HOME", home)
        .arg("status");
    if json {
        command.arg("--json");
    }
    command.output().expect("run status")
}

fn assert_human_policy_existence(output: &Output, expected: bool) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_value = if expected { "yes" } else { "no" };
    assert!(
        stdout
            .lines()
            .any(|line| { line.contains("system policy exists") && line.contains(expected_value) }),
        "human status should report system policy exists={expected}:\n{stdout}"
    );
}

fn unique_policy_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dracon-system-f61-policy-{}-{nanos}.toml",
        std::process::id()
    ))
}

fn unique_home_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dracon-system-f67-home-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn disabled_policy_exits_cleanly_without_restart_trigger() {
    let output = run_guard_with_policy("[guard]\nenabled = false\n");

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("guard disabled in policy"));
}

#[test]
fn malformed_policy_exits_with_config_status() {
    let output = run_guard_with_policy("[guard\n");

    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to parse"));
}

#[test]
fn status_reports_explicit_policy_override_in_human_and_json_modes() {
    let path = unique_policy_path();
    fs::write(&path, "[guard]\nenabled = false\n").expect("write policy fixture");
    let expected = path.display().to_string();

    let human = run_status_at(&path, false);
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).contains(&expected),
        "human status should report the explicit policy path"
    );
    assert_human_policy_existence(&human, true);

    let json = run_status_at(&path, true);
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("status --json should be valid JSON");
    assert_eq!(report["system_policy"], expected);
    assert_eq!(report["system_policy_exists"], true);

    let _ = fs::remove_file(&path);
}

#[test]
fn status_reports_missing_explicit_policy_override() {
    let path = unique_policy_path();
    let expected = path.display().to_string();

    let human = run_status_at(&path, false);
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).contains(&expected),
        "human status should report a missing explicit policy path"
    );
    assert_human_policy_existence(&human, false);

    let json = run_status_at(&path, true);
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("status --json should be valid JSON");
    assert_eq!(report["system_policy"], expected);
    assert_eq!(report["system_policy_exists"], false);
}

#[test]
fn status_reports_first_discovered_policy_in_human_and_json_modes() {
    let home = unique_home_path();
    let path = home.join(".dracon/utilities/system/dracon-system.toml");
    fs::create_dir_all(path.parent().expect("policy parent")).expect("create policy directory");
    fs::write(&path, "[guard]\nenabled = false\n").expect("write discovered policy");
    let expected = path.display().to_string();

    let human = run_status_with_home(&home, false);
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).contains(&expected),
        "human status should report the first discovered policy path"
    );
    assert_human_policy_existence(&human, true);

    let json = run_status_with_home(&home, true);
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("status --json should be valid JSON");
    assert_eq!(report["system_policy"], expected);
    assert_eq!(report["system_policy_exists"], true);

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn status_reports_canonical_fallback_when_no_policy_exists() {
    let home = unique_home_path();
    fs::create_dir_all(&home).expect("create isolated home");
    let path = home.join(".dracon/utilities/system/dracon-system.toml");
    let expected = path.display().to_string();

    let human = run_status_with_home(&home, false);
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).contains(&expected),
        "human status should report the canonical fallback policy path"
    );
    assert_human_policy_existence(&human, false);

    let json = run_status_with_home(&home, true);
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("status --json should be valid JSON");
    assert_eq!(report["system_policy"], expected);
    assert_eq!(report["system_policy_exists"], false);

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn missing_explicit_policy_exits_with_config_status() {
    let path = unique_policy_path();
    let output = run_guard_at(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to read"));
}

// ---------------------------------------------------------------------------
// Reload wiring (DECIDE #2 follow-up)
// ---------------------------------------------------------------------------

fn unit_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system-guard.service")
}

fn unit_text() -> String {
    fs::read_to_string(unit_path())
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", unit_path().display()))
}

/// Without an ExecReload, `systemctl --user reload` has no handler, so the
/// documented SIGHUP reload path is unreachable through the service manager.
///
/// The command's absolute path is checked for existence AND the executable
/// bit, not just for the string "HUP". A string-only assertion passes on a
/// directive that systemd cannot run: `ExecReload=/bin/kill -HUP $MAINPID`
/// contains both required tokens and still fails with 203/EXEC on NixOS,
/// where /bin holds only `sh`.
#[test]
fn shipped_unit_wires_execreload_to_sighup() {
    use std::os::unix::fs::PermissionsExt;

    let unit = unit_text();
    let exec_reload = unit
        .lines()
        .find(|l| l.trim_start().starts_with("ExecReload="))
        .unwrap_or_else(|| {
            panic!(
                "the shipped unit declares no ExecReload, so `systemctl --user reload` \
                 has no handler:\n{unit}"
            )
        });
    let value = exec_reload.trim_start_matches("ExecReload=").trim();
    assert!(
        value.contains("HUP") && value.contains("$MAINPID"),
        "ExecReload must signal the daemon's main PID: got {value:?}"
    );

    // The first token is the program systemd must exec.
    let program = value.split_whitespace().next().unwrap_or_default();
    assert!(
        program.starts_with('/'),
        "ExecReload must use an absolute program path (systemd will not resolve a bare \
         name), got {program:?}"
    );
    let path = Path::new(program);
    let meta = fs::metadata(path)
        .unwrap_or_else(|e| panic!("ExecReload program {program} does not exist: {e}"));
    assert!(
        meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        "ExecReload program {program} is not executable, so the reload would fail with 203/EXEC"
    );
}

fn watchdog_unit_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system-guard-watchdog.service")
}

fn watchdog_unit_text() -> String {
    fs::read_to_string(watchdog_unit_path())
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", watchdog_unit_path().display()))
}

/// 2026-10-03 (audit R4-SYS-16): the watchdog runs every 2 min as a
/// second entry point and needs no writes, so it carries the baseline
/// sandbox. Pin the exact directives (directive lines, not comments).
#[test]
fn watchdog_unit_carries_baseline_sandbox() {
    let unit = watchdog_unit_text();
    let directives: Vec<&str> = unit
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    for expected in [
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ProtectHome=read-only",
    ] {
        assert!(
            directives.contains(&expected),
            "watchdog unit must carry {expected} (it needs no writes):\n{unit}"
        );
    }
}

/// 2026-10-03 (audit R4-SYS-17): the watchdog ExecStart path must
/// equal install.sh's destination — drift means 203/EXEC every 2 min
/// with the guard un-backstopped. Also pins the repo source the
/// install line copies from (existence + executable bit, like the
/// ExecReload contract above).
#[test]
fn watchdog_execstart_matches_install_destination() {
    let unit = watchdog_unit_text();
    let exec_start = unit
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ExecStart="))
        .unwrap_or_else(|| panic!("watchdog unit has no ExecStart:\n{unit}"));
    let value = exec_start.trim_start_matches("ExecStart=").trim();
    assert!(
        !value.is_empty(),
        "watchdog ExecStart must not be empty:\n{unit}"
    );

    // install.sh lives at the utilities root (parent of this crate).
    let install = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("install.sh");
    let install_text = fs::read_to_string(&install)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", install.display()));
    // %h in the unit ≡ ~/ in install.sh; compare the suffix after it.
    let suffix = value.strip_prefix("%h").unwrap_or_else(|| {
        panic!("watchdog ExecStart must be %h-anchored, got {value:?}")
    });
    let dest_tilde = format!("~{suffix}");
    let copy_line = install_text.lines().find(|l| {
        l.contains("dracon-system-guard-watchdog.sh")
            && l.contains(&dest_tilde)
            && l.trim_start().starts_with("copy_unit")
    });
    assert!(
        copy_line.is_some(),
        "install.sh must copy the watchdog script TO the unit's ExecStart path ({value} ≡ {dest_tilde})"
    );
    assert!(
        install_text.lines().any(|l| l.contains("chmod +x")
            && l.contains("dracon-system-guard-watchdog.sh")),
        "install.sh must chmod +x the installed watchdog script"
    );

    // The copy SOURCE must exist in this repo and be executable.
    let source_line = copy_line.unwrap();
    let source_repo_path = source_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default();
    assert_eq!(
        source_repo_path,
        "dracon-system/scripts/dracon-system-guard-watchdog.sh",
        "unexpected watchdog copy source in install.sh: {source_repo_path:?}"
    );
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/dracon-system-guard-watchdog.sh");
    let meta = fs::metadata(&source)
        .unwrap_or_else(|e| panic!("watchdog script {} missing: {e}", source.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            "watchdog script {} is not executable",
            source.display()
        );
    }
    #[cfg(not(unix))]
    {
        assert!(meta.is_file());
    }
}

/// The reload is documented in README as covering every [guard] setting. If
/// the unit stops being reloadable, that documentation becomes a lie.
#[test]
fn readme_documents_the_reload_path() {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("README must be readable");
    assert!(
        readme.contains("Reloading Configuration"),
        "README must document which settings reload on SIGHUP and which need a restart"
    );
    for needle in ["ExecReload", "kill -HUP", "SIGHUP"] {
        assert!(
            readme.contains(needle),
            "README reload section should mention {needle:?}"
        );
    }
}

/// The unit comment and the README must not disagree about what re-reads the
/// policy per pass.
///
/// The daemon DOES re-read the file every pass, but `check_link_drift` takes
/// only `policy.links.entries` from it — `[guard]` and `[storage]` come from
/// the copy the daemon holds. A comment that says "the policy re-reads every
/// pass anyway" therefore implies a threshold change applies without the
/// signal, which is the opposite of the truth and contradicts the README's
/// reload section. If the unit ever mentions the per-pass read again, it has
/// to name the narrow thing that is actually re-read.
#[test]
fn unit_per_pass_comment_agrees_with_the_readme() {
    let unit = unit_text();
    let reloading_comment: String = unit
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if reloading_comment.contains("re-read") {
        assert!(
            reloading_comment.contains("links"),
            "the unit mentions a per-pass policy re-read without saying what is \
             actually re-read; only `links` entries are, so a threshold change \
             still needs SIGHUP or a restart:\n{reloading_comment}"
        );
    }
    // And the README must carry the same three-way split, not just the signal.
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("README must be readable");
    assert!(
        readme.contains("Read fresh on every pass"),
        "README must state what refreshes without a signal, so the two files cannot drift apart"
    );
}
