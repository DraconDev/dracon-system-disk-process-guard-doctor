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
