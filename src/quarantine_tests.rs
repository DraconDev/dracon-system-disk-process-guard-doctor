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

#[cfg(unix)]
#[test]
fn quarantine_expire_skips_escaped_entries_without_aborting_batch() {
    // Audit falsification round: an entry that resolves outside the root
    // used to abort the whole batch (bail), discarding removals already
    // collected. It must now be skipped loudly while the batch completes.
    let root = test_root("expireskip");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
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
    // Smuggled symlink entry with an old manifest, resolving outside.
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join(".quarantine.json"),
        serde_json::to_string_pretty(&aged).unwrap(),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, qdir.join("evil")).unwrap();
    let removed = crate::quarantine_expire(&qdir, 30, true).unwrap();
    assert_eq!(removed, vec![manifest.name.clone()]);
    assert!(!entry_dir.exists(), "expired entry removed");
    assert!(
        fs::symlink_metadata(qdir.join("evil")).is_ok(),
        "escaped entry must survive"
    );
    assert!(outside.is_dir(), "escape target untouched");
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

// ---------------------------------------------------------------------------
// DECIDE #3 re-examined (2026-09-29): the fail-safe is KEPT, but it is
// no longer allowed to be silent, and there is a documented way out.
//
// The manifest is the only record of where an entry came from. Without it
// the entry is unrestorable, so expiring it would turn "recoverable by hand"
// into "gone". The quarantine root is written only by this daemon, so an
// unreadable manifest is corruption or outside interference — not a routine
// condition a TTL exists to bound — and a genuinely transient read error
// resolves itself, after which the entry ages normally.
//
// The defect these tests close: the fail-safe was SILENT. `quarantine list`
// showed origin "unknown" and `quarantine expire` said "No expired entries",
// which is indistinguishable from a quarantine holding nothing.
// ---------------------------------------------------------------------------

/// Create an entry in the quarantine root, optionally with a broken manifest.
fn raw_entry(root: &Path, name: &str, manifest: Option<&str>) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("data")).unwrap();
    fs::write(dir.join("data").join("file.bin"), vec![b'x'; 4096]).unwrap();
    if let Some(text) = manifest {
        fs::write(dir.join(".quarantine.json"), text).unwrap();
    }
    dir
}

const GOOD_MANIFEST: &str =
    r#"{"name":"e.1","origin":"/tmp/origin","moved_at_unix":1,"files":1,"bytes":4096}"#;

#[test]
fn an_unreadable_manifest_is_reported_as_pinned() {
    let root = test_root("pin-visible");
    let qdir = root.join("q");
    raw_entry(&qdir, "good.1", Some(GOOD_MANIFEST));
    raw_entry(&qdir, "broken.1", Some("{not json"));
    raw_entry(&qdir, "missing.1", None);

    let list = crate::quarantine_list(&qdir, 30).unwrap();

    let pinned: Vec<String> = list
        .entries
        .iter()
        .filter(|e| e.pinned)
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(
        pinned,
        vec!["broken.1", "missing.1"],
        "both a corrupt and an absent manifest must be visible as pinned"
    );
    assert!(list.pinned_bytes > 0, "pinned bytes must be reported");
    assert_eq!(list.pinned.len(), 2);
    for e in list.entries.iter().filter(|e| e.pinned) {
        assert!(
            e.pin_reason
                .as_deref()
                .unwrap_or_default()
                .contains("purge"),
            "the pin must tell the operator how to clear it, got {:?}",
            e.pin_reason
        );
    }
    // A healthy entry must NOT be flagged.
    assert!(
        !list
            .entries
            .iter()
            .find(|e| e.name == "good.1")
            .unwrap()
            .pinned
    );
    cleanup(&root);
}

#[test]
fn a_pinned_entry_is_never_expired() {
    let root = test_root("pin-not-expired");
    let qdir = root.join("q");
    raw_entry(&qdir, "ancient-broken.1", Some("{{{ not json"));
    // A TTL of 0 would expire everything, so use a live TTL; the entry is
    // old only by absence of a manifest, which is exactly the case under
    // test — the point is that no TTL path can reach it.
    let removed = crate::quarantine_expire(&qdir, 30, true).unwrap();
    assert!(
        removed.is_empty(),
        "an entry with an unreadable manifest must never be expired, got {removed:?}"
    );
    assert!(
        qdir.join("ancient-broken.1").exists(),
        "and must still be on disk"
    );
    cleanup(&root);
}

#[test]
fn expire_zero_ttl_still_cannot_reach_a_pinned_entry() {
    // ttl_days == 0 disables expiry entirely, so this asserts the
    // fail-safe does not depend on the TTL being non-zero to hold.
    let root = test_root("pin-ttl0");
    let qdir = root.join("q");
    raw_entry(&qdir, "broken.1", Some("nope"));
    assert!(crate::quarantine_expire(&qdir, 0, true).unwrap().is_empty());
    assert!(qdir.join("broken.1").exists());
    cleanup(&root);
}

#[test]
fn purge_dry_run_deletes_nothing_and_reports_the_cost() {
    let root = test_root("purge-dry");
    let qdir = root.join("q");
    raw_entry(&qdir, "broken.1", Some("not json"));

    let (contents, bytes, was_pinned) = crate::quarantine_purge(&qdir, "broken.1", false).unwrap();

    assert!(was_pinned, "purge must report that the entry was pinned");
    assert!(bytes > 0, "purge must report the size it would reclaim");
    assert!(contents.contains("files"), "got {contents:?}");
    assert!(
        qdir.join("broken.1").exists(),
        "a dry-run purge must never delete anything"
    );
    cleanup(&root);
}

#[test]
fn purge_apply_removes_a_pinned_entry_that_restore_cannot() {
    let root = test_root("purge-apply");
    let qdir = root.join("q");
    raw_entry(&qdir, "broken.1", Some("not json"));

    // First prove the safe path really cannot handle it — that is why the
    // escape hatch exists at all.
    let restore_err = crate::quarantine_restore(&qdir, "broken.1").unwrap_err();
    assert!(
        format!("{restore_err:#}").contains("manifest"),
        "restore must still refuse, got: {restore_err:#}"
    );

    let (_, bytes, was_pinned) = crate::quarantine_purge(&qdir, "broken.1", true).unwrap();
    assert!(was_pinned && bytes > 0);
    assert!(
        !qdir.exists() || !qdir.join("broken.1").exists(),
        "purge must remove it"
    );
    cleanup(&root);
}

#[test]
fn purge_refuses_the_same_names_restore_refuses() {
    // The escape hatch must not be the weakest link: it shares the
    // containment check, so traversal is refused here too.
    let root = test_root("purge-traversal");
    let qdir = root.join("q");
    fs::create_dir_all(&qdir).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();

    for bad in ["..", ".", "", "a/b", "a\\b", "../outside"] {
        let err = crate::quarantine_purge(&qdir, bad, true).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid quarantine entry name")
                || format!("{err:#}").contains("no such quarantine entry"),
            "purge must refuse {bad:?}, got: {err:#}"
        );
    }
    assert!(outside.exists(), "nothing outside the root may be touched");
    cleanup(&root);
}

#[test]
fn purge_refuses_to_operate_through_a_symlinked_entry() {
    let root = test_root("purge-symlink");
    let qdir = root.join("q");
    let victim = root.join("precious");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("keep.txt"), b"keep").unwrap();
    fs::create_dir_all(&qdir).unwrap();
    std::os::unix::fs::symlink(&victim, qdir.join("sneaky")).unwrap();

    let err = crate::quarantine_purge(&qdir, "sneaky", true).unwrap_err();
    assert!(
        format!("{err:#}").contains("symlink"),
        "purge must refuse a symlinked entry, got: {err:#}"
    );
    assert!(
        victim.join("keep.txt").exists(),
        "the target must be untouched"
    );
    cleanup(&root);
}

#[test]
fn purge_also_handles_a_healthy_entry() {
    // The escape hatch is not only for corrupt entries; a normal entry past
    // its TTL can be purged by name too, which is the same deliberate
    // operator action with the same dry-run default.
    let root = test_root("purge-healthy");
    let qdir = root.join("q");
    raw_entry(&qdir, "ok.1", Some(GOOD_MANIFEST));

    let (_, bytes, was_pinned) = crate::quarantine_purge(&qdir, "ok.1", false).unwrap();
    assert!(!was_pinned, "a healthy entry is not pinned");
    assert!(bytes > 0);
    assert!(qdir.join("ok.1").exists(), "dry run keeps it");
    crate::quarantine_purge(&qdir, "ok.1", true).unwrap();
    assert!(!qdir.join("ok.1").exists());
    cleanup(&root);
}
