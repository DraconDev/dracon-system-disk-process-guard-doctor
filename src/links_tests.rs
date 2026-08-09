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

#[cfg(unix)]
#[test]
fn unique_backup_path_bumps_suffix_until_free() {
    let base = link_test_dir("backup-suffix");
    std::fs::create_dir_all(&base).unwrap();
    let name = "cfg.dracon-system-backup-123";
    std::fs::write(base.join(name), "one").unwrap();
    std::fs::write(base.join(format!("{name}-1")), "two").unwrap();
    // A BROKEN symlink at -2 must also count as occupied and be skipped.
    std::os::unix::fs::symlink("/nonexistent", base.join(format!("{name}-2"))).unwrap();

    let p = crate::unique_backup_path(&base, name);
    assert_eq!(
        p,
        base.join(format!("{name}-3")),
        "occupied names (incl. broken symlinks) must be skipped"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn backup_path_for_never_reuses_an_occupied_backup_name() {
    let base = link_test_dir("backup-unique");
    std::fs::create_dir_all(&base).unwrap();
    let link = base.join("config");

    // Occupied name — what a second-resolution implementation would
    // produce for this second (or a leftover from an earlier run).
    let occupied = base.join("config.dracon-system-backup-0");
    std::fs::write(&occupied, "old backup").unwrap();

    let backup = crate::backup_path_for(&link);
    assert_ne!(backup, occupied, "must not reuse an occupied backup name");
    let backup_name = backup.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        backup_name.starts_with("config.dracon-system-backup-"),
        "new backup must follow the naming pattern: {}",
        backup_name
    );
    assert!(!backup.exists(), "returned name must be free");
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn force_replace_preserves_two_same_second_backups() {
    // The audit scenario (LOW, 2026-08-10): two force_replace backups of
    // the same basename in one directory within one second must BOTH
    // survive. A file is pre-placed at the exact name a second-resolution
    // implementation would generate for this second — the new backup must
    // not silently overwrite it.
    let base = link_test_dir("backup-two");
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    std::fs::write(&target, "x").unwrap();
    let link = base.join("config");
    std::fs::write(&link, "old file").unwrap();

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let probe = base.join(format!("config.dracon-system-backup-{secs}"));
    std::fs::write(&probe, "earlier backup").unwrap();

    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: link.display().to_string(),
                target: target.display().to_string(),
            }],
        },
        ..SystemPolicy::default()
    };
    let report = crate::apply_link_policy(&policy, true).expect("force replace must succeed");
    assert_eq!(report.healthy, 1);

    assert_eq!(
        std::fs::read_to_string(&probe).unwrap(),
        "earlier backup",
        "the earlier backup must not be overwritten"
    );
    let backups: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.dracon-system-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 2, "probe + new backup must both survive");
    let _ = std::fs::remove_dir_all(&base);
}
}
