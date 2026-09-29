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
fn quarantine_restore_copy_path_drops_manifest_and_verifies() {
    // Audit falsification round: the copy fallback copied entry_dir
    // INCLUDING the manifest, so the tally never matched (manifest was
    // written after the source tally) and every cross-device restore
    // failed verification and deleted its own work — plus leaked a
    // manifest on the paths that got that far. /dev/shm is tmpfs, so a
    // quarantine root there forces the EXDEV copy path against an ext4
    // origin.
    if !std::path::Path::new("/dev/shm").is_dir() {
        return;
    }
    let root = test_root("xdev");
    let src = fixture_dir(&root);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let qdir = std::path::PathBuf::from(format!(
        "/dev/shm/dracon-quarantine-test-{}-{stamp}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&qdir);
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    assert!(!src.exists(), "move must vacate the source");
    let restored = crate::quarantine_restore(&qdir, &manifest.name).unwrap();
    assert_eq!(restored, PathBuf::from(&manifest.origin));
    assert_eq!(std::fs::read(src.join("a.txt")).unwrap(), b"hello");
    assert!(
        !src.join(".quarantine.json").exists(),
        "restored tree must not contain a stray manifest"
    );
    let _ = std::fs::remove_dir_all(&qdir);
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
    // The refusal happens before any manifest removal, so the entry stays
    // restorable once the blocking origin is cleared.
    assert!(qdir.join(&manifest.name).join(".quarantine.json").exists());
    cleanup(&root);
}

#[test]
fn quarantine_restore_refuses_traversal_names() {
    let root = test_root("traversal");
    let qdir = root.join("q");
    fs::create_dir_all(&qdir).unwrap();
    for hostile in ["../evil", "..", ".", "", "a/b", "/abs", "a\\b", "a\0b"] {
        let err = crate::quarantine_restore(&qdir, hostile).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid quarantine entry name"),
            "name {hostile:?} must be refused"
        );
    }
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn quarantine_restore_refuses_smuggled_symlink() {
    use std::os::unix::fs::symlink;
    let root = test_root("smlink");
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let qdir = root.join("q");
    fs::create_dir_all(&qdir).unwrap();
    symlink(&outside, qdir.join("linked")).unwrap();
    let err = crate::quarantine_restore(&qdir, "linked").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("symlink") || msg.contains("no manifest"),
        "smuggled symlink must be refused: {msg}"
    );
    // Nothing outside the root was touched.
    assert!(outside.is_dir() && !outside.join(".quarantine.json").exists());
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
fn quarantine_first_remove_same_fs_frees_nothing() {
    let root = test_root("firstsame");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let (manifest, freed) = crate::quarantine_first_remove(&src, &qdir, &[]).unwrap();
    assert_eq!(manifest.bytes, 11);
    assert_eq!(freed, 0);
    assert!(!src.exists());
    cleanup(&root);
}

#[test]
fn quarantine_first_remove_cross_fs_frees_bytes() {
    // /dev/shm is tmpfs (different device from /tmp on normal systems).
    let shm = PathBuf::from("/dev/shm");
    let probe = shm.join(format!("dracon-q-probe-{}", std::process::id()));
    if fs::write(&probe, b"x").is_err() {
        return;
    }
    let _ = fs::remove_file(&probe);
    let root = test_root("firstcross");
    let src = fixture_dir(&root);
    let qdir = shm.join(format!("dracon-q-test-{}", std::process::id()));
    let (_manifest, freed) = crate::quarantine_first_remove(&src, &qdir, &[]).unwrap();
    assert_eq!(freed, 11);
    let _ = fs::remove_dir_all(&qdir);
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
