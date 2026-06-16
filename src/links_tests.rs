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
