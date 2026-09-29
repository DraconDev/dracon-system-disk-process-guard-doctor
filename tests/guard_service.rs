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
