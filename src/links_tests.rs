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
fn link_report_with_extra_merges_auto_entries() {
    let policy = SystemPolicy::default();
    let extra = vec![LinkEntry {
        link: "/tmp/does-not-exist-auto-link".to_string(),
        target: "/tmp/does-not-exist-auto-target".to_string(),
    }];
    let report = crate::build_link_report_with(&policy, &extra);
    assert_eq!(report.total, 1);
    assert_eq!(report.drifted, 1);
    assert_eq!(report.missing_target, 1);
    let plain = crate::build_link_report(&policy);
    assert_eq!(plain.total, 0);
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
        errors: vec![],
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

#[test]
fn force_replace_honours_user_protected_paths() {
    // Audit MEDIUM: force_replace used to check the link against an empty
    // protected list, so a user-protected file was backed up and replaced.
    // The guard's protected list must refuse the replacement instead.
    let base = link_test_dir("protected");
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    std::fs::write(&target, "x").unwrap();
    let link = base.join("config");
    std::fs::write(&link, "old file").unwrap();

    let guard = crate::GuardPolicy {
        protected_paths: vec![base.display().to_string()],
        ..Default::default()
    };
    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: link.display().to_string(),
                target: target.display().to_string(),
            }],
        },
        guard,
        ..SystemPolicy::default()
    };
    // 2026-10-01 (audit): the refusal is now reported per entry instead of
    // aborting the batch, so the contract is "a report with an error in it",
    // not "Err". The safety property is unchanged: the file is untouched.
    let report = crate::apply_link_policy(&policy, true).expect("batch must return a report");
    assert!(
        !report.errors.is_empty(),
        "a protected link must be reported as a failure, not silently skipped"
    );
    assert!(
        report.errors.iter().any(|e| e.contains("protected")),
        "the error must name the protected-path refusal: {:?}",
        report.errors
    );
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        "old file",
        "refused replacement must leave the file untouched"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 2026-10-01 (audit HIGH): `force_replace` called the STRICT safety check,
/// whose SYSTEM_PROTECTED list contains `/home` and whose test is a DESCENDANT
/// test — so every link under `$HOME` was refused and the flag was dead for
/// every real-world link, including the one the example config ships. The
/// guard-specific variant keeps the exact-root, user-protected, symlink and
/// canonicalisation checks while allowing a $HOME descendant.
#[test]
fn force_replace_works_for_a_link_under_home() {
    let Some(home) = dirs::home_dir() else {
        return; // no home to test against
    };
    let home_str = home.display().to_string();
    if !home_str.starts_with("/home/") && home_str != "/home" {
        return; // the defect is specific to a /home descendant
    }
    let base = home.join(format!(".cache/dracon-system-links-home-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    std::fs::write(&target, "payload").unwrap();
    let link = base.join("config");
    std::fs::write(&link, "old file").unwrap();

    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: link.display().to_string(),
                target: target.display().to_string(),
            }],
        },
        ..SystemPolicy::default()
    };
    let report = crate::apply_link_policy(&policy, true).expect("a $HOME link must be replaceable");
    assert!(
        report.errors.is_empty(),
        "a $HOME link must not be refused: {:?}",
        report.errors
    );
    assert!(
        std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "the link location must now be a symlink"
    );
    // The replaced file must be recoverable from its backup.
    let backups: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains("dracon-system-backup"))
        .collect();
    assert_eq!(backups.len(), 1, "the original file must be backed up");
    assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), "old file");
    let _ = std::fs::remove_dir_all(&base);
}

/// 2026-10-01 (audit): a `?` inside the entry loop aborted the whole batch, so
/// one bad entry left every later entry unrepaired and produced no report at
/// all. The batch must survive a refusal and still apply the good entries.
#[test]
fn a_refused_entry_does_not_abort_the_rest_of_the_batch() {
    let base = link_test_dir("batch");
    std::fs::create_dir_all(&base).unwrap();

    // Entry 1: inside a user-protected path, so it is refused.
    let protected_dir = base.join("protected");
    std::fs::create_dir_all(&protected_dir).unwrap();
    let refused_target = protected_dir.join("t1.txt");
    std::fs::write(&refused_target, "one").unwrap();
    let refused_link = protected_dir.join("l1");
    std::fs::write(&refused_link, "old one").unwrap();

    // Entry 2: a plain file that must be replaced.
    let ok_target = base.join("t2.txt");
    std::fs::write(&ok_target, "two").unwrap();
    let ok_link = base.join("l2");
    std::fs::write(&ok_link, "old two").unwrap();

    let policy = SystemPolicy {
        links: LinkPolicy {
            entries: vec![
                LinkEntry {
                    link: refused_link.display().to_string(),
                    target: refused_target.display().to_string(),
                },
                LinkEntry {
                    link: ok_link.display().to_string(),
                    target: ok_target.display().to_string(),
                },
            ],
        },
        guard: crate::GuardPolicy {
            protected_paths: vec![protected_dir.display().to_string()],
            ..Default::default()
        },
        ..SystemPolicy::default()
    };

    let report = crate::apply_link_policy(&policy, true).expect("a refused entry must not abort");
    assert_eq!(report.errors.len(), 1, "exactly one entry failed: {report:?}");
    assert_eq!(
        std::fs::read_to_string(&refused_link).unwrap(),
        "old one",
        "the refused entry's file must be untouched"
    );
    assert!(
        std::fs::symlink_metadata(&ok_link).unwrap().file_type().is_symlink(),
        "the entry AFTER the failure must still be applied"
    );
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
        .map(|b| {
            std::path::Path::new(&b.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
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
    assert_eq!(
        crate::lexical_normalize(Path::new("/a/../b")),
        PathBuf::from("/b")
    );
    // `..` cannot climb above the root.
    assert_eq!(
        crate::lexical_normalize(Path::new("/../b")),
        PathBuf::from("/../b")
    );
    assert_eq!(
        crate::lexical_normalize(Path::new("a/b/../../c")),
        PathBuf::from("c")
    );
    assert_eq!(
        crate::lexical_normalize(Path::new("a/../b")),
        PathBuf::from("b")
    );
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

/// 2026-10-01 (audit): `force_replace` renamed the user's file to a backup and
/// then created the symlink with `?`, so a failing `symlink(2)` left the path
/// gone and the data only in a backup. The restore step is what must not be
/// wrong, so it is exercised directly (inducing a `symlink(2)` failure inside
/// the apply would need a race).
#[test]
fn a_failed_symlink_restores_the_backed_up_file() {
    let base = link_test_dir("rollback");
    std::fs::create_dir_all(&base).unwrap();
    let link = base.join("config");
    let backup = base.join("config.dracon-system-backup-test");
    std::fs::write(&backup, "original contents").unwrap();

    let err = std::io::Error::new(std::io::ErrorKind::Other, "injected symlink failure");
    crate::links_restore_after_failed_symlink_for_tests(&backup, &link, &err);

    assert!(
        !backup.exists(),
        "the backup must be consumed by the restore"
    );
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        "original contents",
        "the user's file must be back where it was"
    );
    let _ = std::fs::remove_dir_all(&base);
}
