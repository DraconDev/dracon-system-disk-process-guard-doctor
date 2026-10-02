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

#[test]
fn quarantine_preserves_a_reserved_user_file() {
    let temp = tempfile::tempdir().unwrap();
    let src = fixture_dir(temp.path());
    fs::write(src.join(".quarantine.json"), b"original user data").unwrap();
    assert!(crate::quarantine_move(&src, &temp.path().join("q"), &[]).is_err());
    assert_eq!(
        fs::read(src.join(".quarantine.json")).unwrap(),
        b"original user data"
    );
    assert_eq!(fs::read(src.join("a.txt")).unwrap(), b"hello");
}

#[cfg(unix)]
#[test]
fn quarantine_preserves_reserved_symlinks_and_their_targets() {
    use std::os::unix::fs::symlink;
    for dangling in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let src = fixture_dir(temp.path());
        let outside = temp.path().join("outside");
        if !dangling {
            fs::write(&outside, b"outside user data").unwrap();
        }
        symlink(&outside, src.join(".quarantine.json")).unwrap();
        assert!(crate::quarantine_move(&src, &temp.path().join("q"), &[]).is_err());
        assert_eq!(
            fs::read_link(src.join(".quarantine.json")).unwrap(),
            outside
        );
        if dangling {
            assert!(!outside.exists());
        } else {
            assert_eq!(fs::read(&outside).unwrap(), b"outside user data");
        }
    }
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

/// Backdate a path's mtime by `days`.
///
/// Without this the fail-safe test is unfalsifiable: a fresh entry is age 0,
/// and `0 > 30` is false under any implementation, so an expiry path that
/// *did* reach manifest-less entries would still pass. The directory mtime is
/// what a mtime-based expiry would read, so backdating it puts the entry
/// genuinely past the TTL.
fn backdate_mtime(path: &Path, days: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    f.set_modified(when)
        .unwrap_or_else(|e| panic!("backdate {}: {e}", path.display()));
}

#[test]
fn a_pinned_entry_is_never_expired_however_old_it_is() {
    // Backdated 400 days against a 30-day TTL: no age-based expiry path can
    // reach this entry, and if one did it would delete the bytes.
    let root = test_root("pin-not-expired");
    let qdir = root.join("q");
    let entry = raw_entry(&qdir, "ancient-broken.1", Some("{{{ not json"));
    backdate_mtime(&entry, 400);

    let removed = crate::quarantine_expire(&qdir, 30, true).unwrap();

    assert!(
        removed.is_empty(),
        "an entry with an unreadable manifest must never be expired, got {removed:?}"
    );
    assert!(
        entry.exists(),
        "a 400-day-old entry with an unreadable manifest must still be on disk"
    );
    // And it must still be reported as held, not quietly dropped from the list.
    let list = crate::quarantine_list(&qdir, 30).unwrap();
    assert_eq!(
        list.pinned,
        vec!["ancient-broken.1".to_string()],
        "the held entry must still be reported as pinned after an expire run"
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

// --- 2026-10-02: invariants the daemon-side expiry pass depends on ----------
//
// The guard now calls expiry itself, unattended. These pin the properties that
// call assumes but that no existing test covered: that a corrupt manifest can
// never become an un-datable deletion, and that the outcome carries enough
// detail for the daemon to log a deletion an operator could act on.

/// Age an entry by rewriting its manifest with a timestamp `days` in the past.
fn age_entry(qdir: &Path, manifest: &crate::QuarantineManifest, days: u64) -> PathBuf {
    let aged = crate::QuarantineManifest {
        name: manifest.name.clone(),
        origin: manifest.origin.clone(),
        moved_at_unix: crate::now_unix().saturating_sub(days * 86_400),
        bytes: manifest.bytes,
        files: manifest.files,
    };
    let entry_dir = qdir.join(&manifest.name);
    fs::write(
        entry_dir.join(".quarantine.json"),
        serde_json::to_string_pretty(&aged).unwrap(),
    )
    .unwrap();
    entry_dir
}

#[test]
fn quarantine_expire_never_removes_a_pinned_entry() {
    let root = test_root("pinned-expire");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let entry_dir = age_entry(&qdir, &manifest, 40);

    // Destroy the manifest so the entry can never be dated. This is the
    // `node-compile-cache` case from the live host: an entry nobody can restore
    // and nobody can age out.
    fs::remove_file(entry_dir.join(".quarantine.json")).unwrap();

    let outcome = crate::quarantine_expire_detailed(&qdir, 30, true).unwrap();
    assert!(
        outcome.removed.is_empty(),
        "an entry with no manifest must never be deleted: {:?}",
        outcome.removed
    );
    assert!(
        entry_dir.exists(),
        "the pinned entry must survive an apply-mode expiry"
    );
    // And it must be REPORTED, not silently skipped — otherwise it accumulates
    // in a directory whose whole contract is bounded growth.
    assert_eq!(outcome.pinned, vec![manifest.name.clone()]);
    assert!(outcome.pinned_bytes > 0);
    cleanup(&root);
}

#[test]
fn quarantine_expire_outcome_carries_origin_and_bytes() {
    let root = test_root("expire-detailed");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let entry_dir = age_entry(&qdir, &manifest, 40);

    // Captured before the expiry, since afterwards the entry is gone.
    let listed_size = crate::quarantine_list(&qdir, 30).unwrap().entries[0].bytes;

    let outcome = crate::quarantine_expire_detailed(&qdir, 30, true).unwrap();
    assert_eq!(outcome.removed.len(), 1);
    let gone = &outcome.removed[0];
    assert_eq!(gone.name, manifest.name);
    // The daemon logs both of these on every deletion; without them the
    // journal line is just a bare entry name with nothing to act on.
    assert_eq!(gone.origin, manifest.origin);
    // The size must be the one `quarantine list` reports for the same entry —
    // i.e. the directory as it actually sits on disk, manifest included — so an
    // operator reconciling the journal against the listing sees the same number.
    assert_eq!(gone.bytes, listed_size);
    assert!(gone.bytes >= manifest.bytes);
    assert!(!entry_dir.exists());
    cleanup(&root);
}

#[test]
fn quarantine_expire_detailed_dry_run_removes_nothing_but_reports() {
    let root = test_root("expire-dryrun");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let entry_dir = age_entry(&qdir, &manifest, 40);

    let outcome = crate::quarantine_expire_detailed(&qdir, 30, false).unwrap();
    assert_eq!(
        outcome.removed.len(),
        1,
        "dry run must still report the plan"
    );
    assert!(entry_dir.exists(), "dry run must not delete");
    cleanup(&root);
}

#[test]
fn quarantine_expire_detailed_zero_ttl_reports_nothing() {
    // TTL 0 means "never expire". It must not be treated as "expire
    // everything" — the classic off-by-sentinel that a floor!() would cause.
    let root = test_root("expire-ttl-zero");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let entry_dir = age_entry(&qdir, &manifest, 4000);

    let outcome = crate::quarantine_expire_detailed(&qdir, 0, true).unwrap();
    assert!(outcome.removed.is_empty());
    assert!(entry_dir.exists());
    cleanup(&root);
}

/// `protected_paths` is the operator's only way to say "never reclaim here",
/// and the whole protection rests on ONE call: `check_safe_to_delete_guard`
/// inside `quarantine_move`. If that call is ever dropped in a refactor, the
/// config keeps parsing, the guard keeps reporting candidates, and the protected
/// tree gets quarantined anyway — silently, because nothing else notices.
///
/// So this pins the behaviour from the quarantine side: a protected ancestor
/// refuses the move, and a sibling outside it still moves. Without the second
/// half the test would pass trivially if protection simply refused everything.
#[test]
fn quarantine_move_refuses_a_user_protected_ancestor() {
    let root = test_root("protected");
    let prot = root.join("protected");
    let inside = prot.join("proj").join("target");
    fs::create_dir_all(&inside).unwrap();
    fs::write(inside.join("a.txt"), b"keep me").unwrap();

    let outside = root.join("other").join("target");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("b.txt"), b"reclaimable").unwrap();

    let qdir = root.join("q");
    let user_protected = vec![prot.display().to_string()];

    let err = crate::quarantine_move(&inside, &qdir, &user_protected).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("protected"),
        "a user-protected ancestor must refuse the move, got: {msg}"
    );
    assert!(
        inside.join("a.txt").exists(),
        "the protected tree must be left completely intact"
    );
    assert!(
        !qdir.join("target").exists(),
        "nothing may be staged for a refused move"
    );

    // A sibling outside the protected ancestor is unaffected.
    let manifest = crate::quarantine_move(&outside, &qdir, &user_protected).unwrap();
    assert!(!outside.exists());
    assert!(qdir.join(&manifest.name).join("b.txt").exists());

    cleanup(&root);
}

/// Audit advisory: with the TTL disabled, pinned entries used to go unreported
/// because the expiry pass returned before the directory walk. Nothing is
/// deleted either way, but an operator who turns expiry off still has entries
/// sitting there and one of them may be un-ageable — so the report must still
/// be produced.
#[test]
fn pinned_entries_are_reported_even_when_the_ttl_is_zero() {
    let root = test_root("pinned-ttl-zero");
    let src = fixture_dir(&root);
    let qdir = root.join("q");
    let manifest = crate::quarantine_move(&src, &qdir, &[]).unwrap();
    let entry_dir = qdir.join(&manifest.name);
    // No manifest: nothing is computable, so nothing may age out.
    fs::remove_file(entry_dir.join(".quarantine.json")).unwrap();

    let outcome = crate::quarantine_expire_detailed(&qdir, 0, true).unwrap();
    assert!(outcome.removed.is_empty(), "ttl 0 must remove nothing");
    assert!(entry_dir.exists(), "ttl 0 must delete nothing");
    assert_eq!(
        outcome.pinned,
        vec![manifest.name.clone()],
        "the pinned report must not depend on expiry being enabled"
    );
    assert!(outcome.pinned_bytes > 0);
    cleanup(&root);
}
