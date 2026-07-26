//! Tests for links.rs (symlink management and reconciliation)
//!
//! These tests verify the link management components after extraction from main.rs.

use super::*;

#[test]
fn link_entry_stores_link_and_target() {
    let entry = LinkEntry {
        link: "/home/user/link".to_string(),
        target: "/home/user/target".to_string(),
    };
    assert_eq!(entry.link, "/home/user/link");
    assert_eq!(entry.target, "/home/user/target");
}

#[test]
fn link_policy_empty_by_default() {
    let policy = LinkPolicy::default();
    assert!(policy.entries.is_empty());
}

#[test]
fn system_policy_has_link_section() {
    let policy = SystemPolicy::default();
    // Links section exists (empty by default)
    assert!(policy.links.entries.is_empty());
}

#[test]
fn evaluate_link_missing_link_returns_missing() {
    let entry = LinkEntry {
        link: "/tmp/does-not-exist-link".to_string(),
        target: "/tmp/does-not-exist-target".to_string(),
    };
    let status = crate::evaluate_link(&entry);
    assert_eq!(status.link, entry.link);
    assert!(!status.is_symlink);
    assert!(!status.target_exists);
    assert!(!status.in_sync);
    assert!(!status.issue.is_empty());
}

#[test]
fn link_entry_status_debug() {
    let status = crate::LinkEntryStatus {
        link: "/tmp/mylink".to_string(),
        target: "/tmp/mytarget".to_string(),
        exists: false,
        is_symlink: false,
        target_exists: false,
        points_to: String::new(),
        in_sync: false,
        issue: "missing".to_string(),
    };
    let debug = format!("{:?}", status);
    assert!(debug.contains("/tmp/mylink"));
    assert!(debug.contains("missing"));
}

#[test]
fn link_status_report_debug() {
    let report = crate::LinkStatusReport {
        entries: vec![],
        total: 0,
        healthy: 0,
        drifted: 0,
        missing_target: 0,
        missing_link: 0,
    };
    let debug = format!("{:?}", report);
    assert!(debug.contains("total"));
    assert!(debug.contains("0"));
}

// ADDED 2026-07-26 (audit H-13): regression tests for the apply path,
// which previously routed existing symlinks through check_safe_to_delete
// (always refuses symlinks) and therefore could never succeed.

#[cfg(unix)]
fn link_test_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "dracon_link_test_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[cfg(unix)]
#[test]
fn apply_link_policy_fixes_drifted_symlink_and_is_idempotent() {
    let base = link_test_dir("drift");
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    let wrong = base.join("wrong.txt");
    std::fs::write(&target, "x").unwrap();
    std::fs::write(&wrong, "y").unwrap();
    let link = base.join("the-link");
    std::os::unix::fs::symlink(&wrong, &link).unwrap();

    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: link.display().to_string(),
                target: target.display().to_string(),
            }],
        },
        ..SystemPolicy::default()
    };

    // Pre-fix: this errored with "refusing to delete symlink".
    let report = crate::apply_link_policy(&policy, false).expect("apply must fix drifted symlink");
    assert_eq!(report.healthy, 1, "link should be in sync after apply");
    let actual = std::fs::read_link(&link).unwrap();
    assert_eq!(actual, target);

    // In-sync short-circuit: a second apply is a no-op success.
    let report2 = crate::apply_link_policy(&policy, false).expect("re-apply must be a no-op");
    assert_eq!(report2.healthy, 1);
    assert_eq!(std::fs::read_link(&link).unwrap(), target);

    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn apply_link_policy_creates_missing_link() {
    let base = link_test_dir("create");
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    std::fs::write(&target, "x").unwrap();
    let link = base.join("new-link");

    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: link.display().to_string(),
                target: target.display().to_string(),
            }],
        },
        ..SystemPolicy::default()
    };

    let report = crate::apply_link_policy(&policy, false).expect("apply must create missing link");
    assert_eq!(report.healthy, 1);
    assert_eq!(std::fs::read_link(&link).unwrap(), target);

    let _ = std::fs::remove_dir_all(&base);
}
