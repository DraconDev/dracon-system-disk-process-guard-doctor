//! Tests for relocate.rs (cold-data relocation with symlink).

use super::*;
use std::fs;

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dracon-relocate-test-{}-{}",
        std::process::id(),
        name
    ))
}

#[test]
fn walk_stats_strict_agrees_with_lenient_on_readable_trees() {
    let root = test_root("strict-parity");
    let src = fixture_dir(&root);
    let (files, bytes) = crate::walk_stats(&src);
    let (sfiles, sbytes) = crate::walk_stats_strict(&src).expect("readable tree verifies");
    assert_eq!((files, bytes), (sfiles, sbytes));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn copy_tree_and_strict_stats_refuse_unreadable_entries() {
    // Audit MEDIUM: an entry neither side can read used to vanish from
    // both tallies while verification still passed. Unreadable entries
    // must now fail the copy and the verification instead.
    use std::os::unix::fs::PermissionsExt;
    let root = test_root("unreadable");
    let src = fixture_dir(&root);
    let locked = src.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("secret.txt"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let dst = root.join("dst");
    let err = crate::copy_tree(&src, &dst).unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot copy"),
        "copy must name the unreadable entry: {err:#}"
    );
    assert!(
        crate::walk_stats_strict(&src).is_err(),
        "strict stats must refuse the unreadable tree"
    );
    // Lenient reporting still works for sizing.
    let (files, _) = crate::walk_stats(&src);
    assert!(files >= 2);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

fn fixture_dir(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("nested")).unwrap();
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::write(src.join("nested").join("b.txt"), b"world!").unwrap();
    src
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn walk_stats_counts_files_and_bytes() {
    let root = test_root("stats");
    let src = fixture_dir(&root);
    let (files, bytes) = crate::walk_stats(&src);
    assert_eq!(files, 2);
    assert_eq!(bytes, 11);
    cleanup(&root);
}

#[test]
fn copy_tree_preserves_nested_content() {
    let root = test_root("copy");
    let src = fixture_dir(&root);
    let dst = root.join("dst");
    let skipped = crate::copy_tree(&src, &dst).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(fs::read(dst.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dst.join("nested").join("b.txt")).unwrap(),
        b"world!"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn copy_tree_recreates_symlinks_without_following() {
    use std::os::unix::fs::symlink;
    let root = test_root("links");
    let src = fixture_dir(&root);
    symlink("a.txt", src.join("alias.txt")).unwrap();
    let dst = root.join("dst");
    crate::copy_tree(&src, &dst).unwrap();
    let meta = fs::symlink_metadata(dst.join("alias.txt")).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(fs::read(dst.join("alias.txt")).unwrap(), b"hello");
    cleanup(&root);
}

#[test]
fn fits_in_avail_treats_unknown_space_as_not_fitting() {
    // R3-L28: "cannot ask" (df failure) must not authorize a copy.
    assert!(!crate::fits_in_avail(None, 1));
    assert!(!crate::fits_in_avail(None, 0));
    assert!(crate::fits_in_avail(Some(100), 100));
    assert!(crate::fits_in_avail(Some(101), 100));
    assert!(!crate::fits_in_avail(Some(99), 100));
}

#[test]
fn plan_relocate_reports_size_and_readiness() {
    let root = test_root("plan");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    assert_eq!(plan.files, 2);
    assert_eq!(plan.bytes, 11);
    assert!(plan.ready);
    assert!(plan.fits);
    assert!(plan.dest.ends_with("src"));
    cleanup(&root);
}

#[test]
fn plan_relocate_refuses_missing_source() {
    let root = test_root("missing");
    fs::create_dir_all(&root).unwrap();
    let err = crate::plan_relocate(&root.join("nope"), &root, &[], false).unwrap_err();
    assert!(format!("{err:#}").contains("cannot inspect"));
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn plan_relocate_refuses_symlink_source() {
    use std::os::unix::fs::symlink;
    let root = test_root("symsrc");
    let src = fixture_dir(&root);
    let link = root.join("linkdir");
    symlink(&src, &link).unwrap();
    let err = crate::plan_relocate(&link, &root, &[], false).unwrap_err();
    assert!(format!("{err:#}").contains("symlink"));
    cleanup(&root);
}

#[test]
fn plan_relocate_refuses_existing_dest() {
    let root = test_root("exists");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(dest_root.join("src")).unwrap();
    let err = crate::plan_relocate(&src, &dest_root, &[], false).unwrap_err();
    assert!(format!("{err:#}").contains("already exists"));
    cleanup(&root);
}

#[test]
fn plan_relocate_rejects_protected_source() {
    let root = test_root("prot");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let protected = vec![src.display().to_string()];
    let err = crate::plan_relocate(&src, &dest_root, &protected, false).unwrap_err();
    assert!(format!("{err:#}").contains("protected"));
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn apply_relocate_roundtrip_leaves_symlink() {
    let root = test_root("apply");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    let report = crate::apply_relocate(&plan).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.bytes, 11);
    // Source is now a symlink resolving to the moved content.
    let meta = fs::symlink_metadata(&src).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(fs::read(src.join("a.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::read(dest_root.join("src").join("nested").join("b.txt")).unwrap(),
        b"world!"
    );
    assert!(report.policy_snippet.contains("[[links.entries]]"));
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn apply_relocate_refuses_stale_staging_dir() {
    // Audit MEDIUM: the source is staged aside before the symlink is
    // created. A leftover staging dir from a previous failed run must
    // refuse the run rather than be silently reused or overwritten.
    let root = test_root("staging");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    let staging = src.with_file_name(format!(
        "{}.dracon-relocate-staging",
        src.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&staging).unwrap();
    let err = crate::apply_relocate(&plan).unwrap_err();
    assert!(
        format!("{err:#}").contains("stale staging"),
        "stale staging dir must refuse the run: {err:#}"
    );
    assert!(
        fs::read(src.join("a.txt")).is_ok(),
        "refused run must leave the source untouched"
    );
    cleanup(&root);
}

fn git_repo_with_tracked_subdir(root: &Path) -> PathBuf {
    use std::process::Command;
    let repo = root.join("repo");
    let sub = repo.join("tracked-dir");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("f.txt"), b"data").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(status.status.success());
    };
    run(&["-c", "init.defaultBranch=main", "init", "-q"]);
    run(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    sub
}

#[test]
fn plan_relocate_refuses_tracked_unless_allowed() {
    let root = test_root("tracked");
    let sub = git_repo_with_tracked_subdir(&root);
    assert!(crate::is_git_tracked(&sub).unwrap());
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let err = crate::plan_relocate(&sub, &dest_root, &[], false).unwrap_err();
    assert!(format!("{err:#}").contains("git-tracked"));
    let plan = crate::plan_relocate(&sub, &dest_root, &[], true).unwrap();
    assert!(plan.ready);
    cleanup(&root);
}

fn touch_aged(path: &Path, days_ago: &str) {
    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let status = std::process::Command::new("touch")
                .args(["-d", days_ago])
                .arg(entry.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
}

fn scan_fixture(root: &Path) -> PathBuf {
    let scan = root.join("scan");
    let big_old = scan.join("big-old");
    fs::create_dir_all(&big_old).unwrap();
    fs::write(big_old.join("f.bin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
    touch_aged(&big_old, "20 days ago");
    let big_fresh = scan.join("big-fresh");
    fs::create_dir_all(&big_fresh).unwrap();
    fs::write(big_fresh.join("f.bin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
    let small_old = scan.join("small-old");
    fs::create_dir_all(&small_old).unwrap();
    fs::write(small_old.join("f.txt"), b"x").unwrap();
    touch_aged(&small_old, "20 days ago");
    let nested = scan.join("nested");
    fs::create_dir_all(nested.join("inner")).unwrap();
    fs::write(nested.join("outer.bin"), vec![7u8; 1_600_000]).unwrap();
    fs::write(nested.join("inner").join("in.bin"), vec![7u8; 1_600_000]).unwrap();
    touch_aged(&nested, "20 days ago");
    let target = scan.join("proj").join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("big.bin"), vec![7u8; 5 * 1024 * 1024]).unwrap();
    touch_aged(&target, "20 days ago");
    scan
}

#[test]
fn find_cold_candidates_filters_size_age_and_build_dirs() {
    let root = test_root("scan");
    let scan = scan_fixture(&root);
    let found = crate::find_cold_candidates(std::slice::from_ref(&scan), 1024 * 1024, 14);
    let paths: Vec<&str> = found.iter().map(|c| c.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("big-old")),
        "big-old must qualify: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("big-fresh")),
        "fresh must not qualify: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("small-old")),
        "small must not qualify: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("target")),
        "build dirs must be pruned: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("proj")),
        "proj holds only a pruned target: {paths:?}"
    );
    cleanup(&root);
}

#[test]
fn find_cold_candidates_collapses_nesting() {
    let root = test_root("nest");
    let scan = scan_fixture(&root);
    let found = crate::find_cold_candidates(&[scan], 1024 * 1024, 14);
    let nested: Vec<&str> = found
        .iter()
        .map(|c| c.path.as_str())
        .filter(|p| p.contains("nested"))
        .collect();
    assert_eq!(nested.len(), 1, "outer only: {nested:?}");
    assert!(nested[0].ends_with("nested"));
    cleanup(&root);
}

#[test]
fn relocation_state_roundtrip() {
    // State path is home-derived; only assert load tolerance here —
    // record/load integration is covered live, not in unit tests.
    let records = crate::load_relocation_records();
    let _ = serde_json::to_string(&records).unwrap();
    let entries = crate::relocation_link_entries();
    assert_eq!(entries.len(), records.len());
}

#[test]
fn plan_relocate_allows_untracked_in_repo() {
    let root = test_root("untracked");
    let repo = root.join("repo");
    let sub = git_repo_with_tracked_subdir(&root);
    let fresh = repo.join("fresh-dir");
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("new.txt"), b"new").unwrap();
    assert!(!crate::is_git_tracked(&fresh).unwrap());
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&fresh, &dest_root, &[], false).unwrap();
    assert!(plan.ready);
    let _ = sub;
    cleanup(&root);
}

#[test]
fn is_git_tracked_sees_repo_roots() {
    // A repo root holds tracked content even though its parent is not a
    // work tree — the case the parent-only probe missed.
    let root = test_root("reporoot");
    let sub = git_repo_with_tracked_subdir(&root);
    let repo = sub.parent().unwrap().to_path_buf();
    assert!(crate::is_git_tracked(&repo).unwrap());
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let err = crate::plan_relocate(&repo, &dest_root, &[], false).unwrap_err();
    assert!(format!("{err:#}").contains("git-tracked"));
    cleanup(&root);
}

// ---------------------------------------------------------------------------
// find_cold_candidates — the daemon's autonomous "move this directory"
// selection. It had NO coverage before the 0.112.42 release review, which
// matters because in auto mode this drives a real move of user data with no
// operator command and no --allow-tracked escape hatch.
// ---------------------------------------------------------------------------

/// Create `dir` under `root` holding `bytes` of file data, with every
/// mtime set to `age_days` in the past so the idle filter is deterministic.
fn aged_dir(root: &Path, name: &str, bytes: usize, age_days: u64) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("payload.bin");
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    filetime_set(&path, age_days);
    filetime_set(&dir, age_days);
    dir
}

/// Set a path's mtime to `age_days` in the past.
///
/// `File::set_modified` is stable and needs no extra dependency; opening a
/// directory read-only is enough for futimens to stamp it on Linux.
fn filetime_set(path: &Path, age_days: u64) {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {} to set mtime: {e}", path.display()));
    f.set_modified(
        std::time::SystemTime::now() - std::time::Duration::from_secs(age_days * 86_400),
    )
    .unwrap_or_else(|e| panic!("set mtime on {}: {e}", path.display()));
}

#[test]
fn cold_candidates_respect_the_size_floor() {
    let root = test_root("cold-size");
    std::fs::create_dir_all(&root).unwrap();
    aged_dir(&root, "big", 64 * 1024, 40);
    aged_dir(&root, "small", 16, 40);
    let candidates = find_cold_candidates(std::slice::from_ref(&root), 32 * 1024, 0);
    let names: Vec<String> = candidates
        .iter()
        .map(|c| c.path.rsplit('/').next().unwrap().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "big"),
        "a directory over the floor must be a candidate, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "small"),
        "a directory under the floor must not be a candidate, got {names:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cold_candidates_respect_the_idle_floor() {
    let root = test_root("cold-age");
    std::fs::create_dir_all(&root).unwrap();
    aged_dir(&root, "stale", 64 * 1024, 90);
    aged_dir(&root, "fresh", 64 * 1024, 0);
    let candidates = find_cold_candidates(std::slice::from_ref(&root), 1024, 30);
    let names: Vec<String> = candidates
        .iter()
        .map(|c| c.path.rsplit('/').next().unwrap().to_string())
        .collect();
    assert!(names.iter().any(|n| n == "stale"), "got {names:?}");
    assert!(
        !names.iter().any(|n| n == "fresh"),
        "a directory younger than the idle floor must not be moved, got {names:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cold_candidates_never_include_a_git_tracked_directory() {
    // The single most important property of auto mode: the daemon relocates
    // on its own with no --allow-tracked escape hatch, so a repo must never
    // be selected. Moving one replaces it with a symlink and destroys the
    // working tree.
    let root = test_root("cold-tracked");
    std::fs::create_dir_all(&root).unwrap();
    let repo = aged_dir(&root, "repo", 64 * 1024, 90);
    // Mark it tracked: the production check probes the enclosing repo root.
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .status();
    std::fs::write(repo.join("tracked.txt"), b"content").unwrap();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["add", "tracked.txt"])
        .status();

    let candidates = find_cold_candidates(std::slice::from_ref(&root), 1024, 0);
    let names: Vec<String> = candidates
        .iter()
        .map(|c| c.path.rsplit('/').next().unwrap().to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n == "repo"),
        "a git-tracked directory must never be an auto-relocate candidate, got {names:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cold_candidates_never_include_the_scan_root_itself() {
    // Moving the root would move everything under it, including the
    // candidate list's own parent.
    let root = test_root("cold-root");
    std::fs::create_dir_all(&root).unwrap();
    aged_dir(&root, "child", 64 * 1024, 90);
    let candidates = find_cold_candidates(std::slice::from_ref(&root), 1024, 0);
    for c in &candidates {
        assert_ne!(
            c.path.trim_end_matches('/'),
            root.canonicalize()
                .unwrap_or(root.clone())
                .to_str()
                .unwrap(),
            "the scan root itself must never be a candidate"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// parse_extra_mounts — the space-tier extra-mount list. Also had no
// coverage; a stray empty entry here is stat'd as "" and reported as a
// broken mount on every guard pass.
// ---------------------------------------------------------------------------

#[test]
fn extra_mounts_parse_comma_separated_and_trim() {
    assert_eq!(
        parse_extra_mounts("/mnt/data, /mnt/media ,/mnt/other"),
        vec!["/mnt/data", "/mnt/media", "/mnt/other"]
    );
}

#[test]
fn extra_mounts_drop_empty_and_whitespace_entries() {
    // An empty entry would be stat'd as "" and reported as a broken mount.
    assert!(parse_extra_mounts("").is_empty());
    assert!(parse_extra_mounts("   ").is_empty());
    assert_eq!(parse_extra_mounts(",,"), Vec::<String>::new());
    assert_eq!(parse_extra_mounts(" , /mnt/data , "), vec!["/mnt/data"]);
}

#[test]
fn extra_mounts_default_is_empty_so_no_mount_is_visibility_only_by_default() {
    // disk_extra_mounts defaults to ""; a default that accidentally listed a
    // machine path would make the guard report a mount the operator never
    // named.
    assert!(parse_extra_mounts(&GuardPolicy::default().disk_extra_mounts).is_empty());
}

// ---------------------------------------------------------------------------
// clean_old_node_modules — 130 lines that DELETE directories, and which had
// no coverage at all before the 0.112.42 release review. Only the two
// properties that can lose data are asserted here: a dry run must be inert,
// and a protected path must survive regardless of the quarantine flag.
// ---------------------------------------------------------------------------

/// Build a node_modules tree whose directory mtime is `age_days` old, so
/// the age filter selects it deterministically.
fn aged_node_modules(root: &Path, age_days: u64) -> PathBuf {
    let nm = root.join("proj").join("node_modules");
    std::fs::create_dir_all(nm.join("pkg")).unwrap();
    std::fs::write(nm.join("pkg").join("index.js"), vec![b'j'; 2048]).unwrap();
    filetime_set(&nm.join("pkg"), age_days);
    filetime_set(&nm, age_days);
    filetime_set(nm.parent().unwrap(), age_days);
    nm
}

#[tokio::test]
async fn node_modules_dry_run_deletes_nothing_and_leaves_the_tree_intact() {
    // The single most important property: without `apply` the whole function
    // is a report. A regression here would make a read-only inspection
    // destructive.
    let root = test_root("nm-dry");
    let nm = aged_node_modules(&root, 90);
    let quarantine = root.join("quarantine");
    std::fs::create_dir_all(&quarantine).unwrap();
    assert!(nm.exists(), "fixture must exist before the run");

    let (_, cleaned) = clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        false,
        &[],
        false,
        &quarantine,
    )
    .await
    .expect("dry run must not error");

    assert!(
        !cleaned.is_empty(),
        "a dry run must still report candidates"
    );
    assert!(nm.exists(), "dry run must not delete anything");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn node_modules_quarantine_first_dry_run_also_deletes_nothing() {
    // The quarantine flag must not turn a dry run into a move.
    let root = test_root("nm-dry-quarantine");
    let nm = aged_node_modules(&root, 90);
    let quarantine = root.join("quarantine");
    std::fs::create_dir_all(&quarantine).unwrap();

    let (_, cleaned) = clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        false,
        &[],
        true,
        &quarantine,
    )
    .await
    .expect("dry run must not error");

    assert!(!cleaned.is_empty(), "must still report candidates");
    assert!(
        nm.exists(),
        "quarantine-first dry run must not move anything"
    );
    let entries: Vec<_> = std::fs::read_dir(&quarantine).unwrap().collect();
    assert!(
        entries.is_empty(),
        "nothing may be written to the quarantine root on a dry run"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn node_modules_protected_path_survives_a_real_apply() {
    // `protected_paths` must be honoured on the apply path, which is the
    // only path that deletes or moves anything.
    let root = test_root("nm-protected");
    let nm = aged_node_modules(&root, 90);
    let protected = root.join("proj");
    let quarantine = root.join("quarantine");
    std::fs::create_dir_all(&quarantine).unwrap();
    let protected_str = protected.to_string_lossy().to_string();

    clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        true,
        std::slice::from_ref(&protected_str),
        false,
        &quarantine,
    )
    .await
    .expect("apply must not error");

    assert!(
        nm.exists(),
        "a node_modules under a protected path must never be deleted"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn auto_relocate_pauses_when_df_fails() {
    // 2026-10-02 (audit L7): a df failure previously mapped to u8::MAX
    // (max pressure), so the guard kept moving trees with no signal.
    // Now it pauses: zero moves despite ready candidates. The
    // plan-readiness assertion pins the control — pre-fix, the same
    // fixture WOULD have moved (pressure MAX + ready plan + apply).
    let root = test_root("df-fail-pause");
    let scan = scan_fixture(&root);
    let cold = root.join("cold");
    fs::create_dir_all(&cold).unwrap();

    let big_old = scan.join("big-old");
    let plan = crate::plan_relocate(&big_old, &cold, &[], false).expect("plan computes");
    assert!(
        plan.ready,
        "fixture must be relocation-ready: {:?}",
        plan.issues
    );

    let guard = GuardPolicy {
        relocate_cold_root: cold.to_string_lossy().to_string(),
        relocate_candidate_roots: scan.to_string_lossy().to_string(),
        relocate_min_size_mb: 1,
        relocate_min_age_days: 14,
        auto_relocate_apply: true,
        disk_mount_path: "/nonexistent-mount-xyz".to_string(),
        disk_action_percent: 85,
        ..Default::default()
    };

    let (moves, _bytes, _cands) = crate::run_auto_relocate(&guard)
        .await
        .expect("relocate pass runs");
    assert_eq!(moves, 0, "df failure must pause moves, not move blind");
    assert!(
        big_old.is_dir() && !big_old.is_symlink(),
        "nothing must move while pressure is unreadable"
    );
    cleanup(&root);
}
