//! Tests for quarantine.rs (hold-then-delete staging with TTL).

use super::*;
use std::fs;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dracon-quarantine-test-{}-{}",
        std::process::id(),
        name
    ))
}

fn fixture_dir(root: &Path) -> PathBuf {
    let src = root.join("work").join("proj");
    fs::create_dir_all(src.join("nested")).unwrap();
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::write(src.join("nested").join("b.txt"), b"world!").unwrap();
    src
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn quarantine_move_list_restore_roundtrip() {
    let root = test_root("roundtrip");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    assert_eq!(manifest.files, 2);
    assert_eq!(manifest.bytes, 11);
    assert!(!src.exists());

    let list = crate::quarantine_list(&qdir, 30).unwrap();
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.entries[0].origin, manifest.origin);
    assert!(!list.entries[0].expired);

    let restored = crate::quarantine_restore(&qdir, &manifest.name).unwrap();
    assert_eq!(restored, PathBuf::from(&manifest.origin));
    assert_eq!(fs::read(src.join("a.txt")).unwrap(), b"hello");
    // Manifest must not leak back into the restored tree.
    assert!(!src.join(".quarantine.json").exists());
    let list = crate::quarantine_list(&qdir, 30).unwrap();
    assert!(list.entries.is_empty());
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn quarantine_move_refuses_symlink() {
    use std::os::unix::fs::symlink;
    let root = test_root("symlink");
    let src = fixture_dir(&root);
    let link = root.join("linkdir");
    symlink(&src, &link).unwrap();
    let err = crate::quarantine_move(&link, &root.join("q"), &[]).unwrap_err();
    assert!(format!("{err:#}").contains("symlink"));
    cleanup(&root);
}

#[test]
fn quarantine_restore_refuses_existing_origin() {
    let root = test_root("exists");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    fs::create_dir_all(&src).unwrap();
    let err = crate::quarantine_restore(&qdir, &manifest.name).unwrap_err();
    assert!(format!("{err:#}").contains("already exists"));
    cleanup(&root);
}

#[test]
fn quarantine_expire_deletes_only_past_ttl() {
    let root = test_root("expire");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    // Age the entry 40 days by rewriting its manifest.
    let aged = crate::QuarantineManifest {
        name: manifest.name.clone(),
        origin: manifest.origin.clone(),
        moved_at_unix: crate::now_unix().saturating_sub(40 * 86_400),
        bytes: manifest.bytes,
        files: manifest.files,
    };
    let entry_dir = qdir.join(&manifest.name);
    fs::write(
        entry_dir.join(".quarantine.json"),
        serde_json::to_string_pretty(&aged).unwrap(),
    )
    .unwrap();

    // Fresh TTL window: nothing expires.
    let listed = crate::quarantine_list(&qdir, 60).unwrap();
    assert!(!listed.entries[0].expired);
    // Past TTL: dry-run reports, apply deletes.
    let listed = crate::quarantine_list(&qdir, 30).unwrap();
    assert!(listed.entries[0].expired);
    let dry = crate::quarantine_expire(&qdir, 30, false).unwrap();
    assert_eq!(dry, vec![manifest.name.clone()]);
    assert!(entry_dir.exists());
    let gone = crate::quarantine_expire(&qdir, 30, true).unwrap();
    assert_eq!(gone, vec![manifest.name.clone()]);
    assert!(!entry_dir.exists());
    cleanup(&root);
}

#[test]
fn quarantine_expire_zero_ttl_disables() {
    let root = test_root("zerottl");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let out = crate::quarantine_expire(&qdir, 0, true).unwrap();
    assert!(out.is_empty());
    assert!(qdir.join(&manifest.name).exists());
    cleanup(&root);
}

#[test]
fn quarantine_list_empty_root() {
    let root = test_root("empty");
    let list = crate::quarantine_list(&root.join("q"), 30).unwrap();
    assert!(list.entries.is_empty());
    assert_eq!(list.total_bytes, 0);
    cleanup(&root);
}
