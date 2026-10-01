//! Tests for setup.rs (readiness checks and dir provisioning).

use std::fs;
use std::path::PathBuf;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("dracon-setup-test-{}-{}", std::process::id(), name))
}

#[test]
fn ensure_setup_dir_creates_nested_and_reports() {
    let root = test_root("create");
    let nested = root.join("a").join("b");
    let detail = crate::ensure_setup_dir(&nested, &[]).unwrap();
    assert!(nested.is_dir());
    assert!(!detail.is_empty());
    // No probe file left behind.
    assert!(fs::read_dir(&nested).unwrap().next().is_none());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ensure_setup_dir_refuses_file_blocking() {
    let root = test_root("blocked");
    fs::create_dir_all(&root).unwrap();
    let blocker = root.join("file");
    fs::write(&blocker, b"x").unwrap();
    let err = crate::ensure_setup_dir(&blocker.join("sub"), &[]).unwrap_err();
    assert!(format!("{err:#}").contains("cannot create"));
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn ensure_setup_dir_refuses_symlinked_path() {
    // Audit MEDIUM: create_dir_all through a symlink builds into the link
    // target, so a symlinked cold/quarantine root silently manages the
    // wrong tree.
    let root = test_root("symlinked");
    let real = root.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = root.join("linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = crate::ensure_setup_dir(&link, &[]).unwrap_err();
    assert!(
        format!("{err:#}").contains("symlink"),
        "symlinked setup dir must be refused: {err:#}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ensure_setup_dir_refuses_protected_ancestor() {
    // A new managed root under a user-protected path must be refused
    // before anything is created.
    let root = test_root("protected");
    fs::create_dir_all(&root).unwrap();
    let fresh = root.join("cold");
    let protected = vec![root.display().to_string()];
    let err = crate::ensure_setup_dir(&fresh, &protected).unwrap_err();
    assert!(
        format!("{err:#}").contains("protected"),
        "protected ancestor must be refused: {err:#}"
    );
    assert!(!fresh.exists(), "refused setup must create nothing");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn setup_report_serializes() {
    let report = crate::SetupReport {
        policy_path: "/tmp/p.toml".to_string(),
        policy_exists: true,
        policy_error: None,
        checks: vec![crate::SetupCheck {
            name: "cold root".to_string(),
            ok: true,
            detail: "/mnt/data/cold".to_string(),
        }],
        ready: true,
    };
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("cold root"));
    assert!(json.contains("\"ready\":true"));
}

/// 2026-10-01 (audit): a policy file that exists but does not parse used to be
/// reported as "no policy — built-in defaults" with `policy_exists: false` and
/// `ready: true`, i.e. the operator was told to go configure something when
/// their file was simply broken.
#[test]
fn a_broken_policy_is_reported_as_broken_not_absent() {
    let home = crate::guard_test_tmp("setup-broken-policy");
    std::fs::create_dir_all(&home).unwrap();
    let policy_dir = home.join(".dracon/utilities/system");
    std::fs::create_dir_all(&policy_dir).unwrap();
    // A type error: disk_warn_percent is a number in the struct.
    std::fs::write(
        policy_dir.join("dracon-system.toml"),
        "[guard]\ndisk_warn_percent = \"80\"\n",
    )
    .unwrap();

    let report = crate::setup::collect_setup_report_in(&home);

    assert!(
        report.policy_error.is_some(),
        "a parse failure must be reported as a parse failure, got {report:?}"
    );
    assert!(
        report.policy_exists,
        "the file exists, so policy_exists must be true"
    );
    assert!(
        !report.ready,
        "readiness computed on built-in defaults must not claim ready"
    );
}

/// 2026-10-01 (audit): a relative `relocate_cold_root` must resolve to the same
/// absolute path in `setup` and in the daemon. The unit's WorkingDirectory is
/// %h, so "cold" means ~/cold — `setup --apply` used to create ./cold under
/// the invoking shell instead and report ready.
#[test]
fn relative_policy_paths_resolve_against_home() {
    let home = std::path::Path::new("/home/dracon");
    assert_eq!(
        crate::resolve_policy_path("cold"),
        home.join("cold"),
        "a relative policy path must resolve under $HOME (the unit's WorkingDirectory)"
    );
    assert_eq!(
        crate::resolve_policy_path("~/cold"),
        home.join("cold"),
        "an explicit ~ path must be unchanged"
    );
    assert_eq!(
        crate::resolve_policy_path("/mnt/cold"),
        std::path::PathBuf::from("/mnt/cold"),
        "an absolute path must be untouched"
    );
    assert!(
        crate::resolve_policy_path("cold").is_absolute(),
        "the nesting check compares absolute paths, so the result must be absolute"
    );
}
