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

#[cfg(unix)]
#[test]
fn scan_broken_symlinks_detects_broken_chains() {
    // Chain detection pin (audit LOW, 2026-08-10): leaf -> mid ->
    // (missing). `fs::metadata` FOLLOWS symlinks, so both leaf and mid
    // are reported broken. The old comment claimed metadata "doesn't
    // follow symlinks"; a future "fix" to `symlink_metadata` would
    // report mid as existing and leaf as fine — this test fails then.
    let base = link_test_dir("chain");
    std::fs::create_dir_all(&base).unwrap();
    std::os::unix::fs::symlink(base.join("missing"), base.join("mid")).unwrap();
    std::os::unix::fs::symlink(base.join("mid"), base.join("leaf")).unwrap();

    let (count, broken) = crate::scan_broken_symlinks(&base, 3);
    assert_eq!(count, 2, "both symlinks are scanned");
    assert_eq!(
        broken.len(),
        2,
        "leaf and mid must BOTH be reported broken (chain followed): {:#?}",
        broken
    );
    let names: Vec<String> = broken
        .iter()
        .map(|b| std::path::Path::new(&b.path).file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"mid".to_string()) && names.contains(&"leaf".to_string()));

    // A chain ending in a REAL file is not broken.
    std::fs::write(base.join("real"), "x").unwrap();
    std::os::unix::fs::symlink(base.join("real"), base.join("ok-mid")).unwrap();
    std::os::unix::fs::symlink(base.join("ok-mid"), base.join("ok-leaf")).unwrap();
     let (count, broken) = crate::scan_broken_symlinks(&base, 3);
    assert_eq!(count, 4);
    assert_eq!(broken.len(), 2, "healthy chain must not be reported broken");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn lexical_normalize_collapses_dot_components() {
    use std::path::Path;
    use std::path::PathBuf;
    assert_eq!(
        crate::lexical_normalize(Path::new("/a/./b")),
        PathBuf::from("/a/b")
    );
    assert_eq!(crate::lexical_normalize(Path::new("/a/../b")), PathBuf::from("/b"));
    // `..` cannot climb above the root.
    assert_eq!(
        crate::lexical_normalize(Path::new("/../b")),
        PathBuf::from("/../b")
    );
    assert_eq!(
        crate::lexical_normalize(Path::new("a/b/../../c")),
        PathBuf::from("c")
    );
    assert_eq!(crate::lexical_normalize(Path::new("a/../b")), PathBuf::from("b"));
    assert_eq!(crate::lexical_normalize(Path::new("/")), PathBuf::from("/"));
}

#[cfg(unix)]
#[test]
fn evaluate_link_accepts_equivalent_noncanonical_target() {
    // audit LOW, 2026-08-10: the actual link target is written as
    // `<base>/a/../b` while the configured target is `<base>/b`, and
    // the intermediate `a` does NOT exist — canonicalize fails, so the
    // old code compared RAW strings and reported link_target_mismatch
    // for an in-sync link. The lexical fallback must equate them.
    let base = link_test_dir("equiv");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("b"), "x").unwrap();
    // NOTE: base/a is deliberately NOT created.
    let link = base.join("l");
    std::os::unix::fs::symlink(base.join("a/../b"), &link).unwrap();

    // One side ..-form (actual), other canonical form.
    let entry = LinkEntry {
        link: link.display().to_string(),
        target: base.join("b").display().to_string(),
    };
    let status = crate::evaluate_link(&entry);
    assert!(
        status.in_sync,
        "equivalent ..-form target must be in sync, issue: {:?}",
        status.issue
    );
    assert_eq!(status.issue, "ok");

    // NOTE: a "both sides ..-form" variant is unreachable through the
    // public gate — if the configured target path does not fully
    // resolve (missing intermediate), `target.exists()` is false and
    // the entry reports target_missing before any comparison runs.

    // A genuinely different target must still report mismatch.
    std::fs::write(base.join("other"), "y").unwrap();
    let entry3 = LinkEntry {
        link: link.display().to_string(),
        target: base.join("other").display().to_string(),
    };
    let status3 = crate::evaluate_link(&entry3);
    assert!(!status3.in_sync, "different target must stay mismatched");
    assert_eq!(status3.issue, "link_target_mismatch");
    let _ = std::fs::remove_dir_all(&base);
}
