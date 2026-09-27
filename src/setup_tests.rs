//! Tests for setup.rs (readiness checks and dir provisioning).

use super::*;
use std::fs;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dracon-setup-test-{}-{}",
        std::process::id(),
        name
    ))
}

#[test]
fn ensure_setup_dir_creates_nested_and_reports() {
    let root = test_root("create");
    let nested = root.join("a").join("b");
    let detail = crate::ensure_setup_dir(&nested).unwrap();
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
    let err = crate::ensure_setup_dir(&blocker.join("sub")).unwrap_err();
    assert!(format!("{err:#}").contains("cannot create"));
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
