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
