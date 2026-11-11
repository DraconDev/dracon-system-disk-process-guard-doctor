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

#[cfg(unix)]
#[test]
fn stale_staging_refusal_names_crash_window_recovery() {
    // Audit R4-SYS-09: staging present + source MISSING is the crash
    // window (rename done, symlink never made). The refusal must say
    // so and give the `mv` recovery — not "clear it manually".
    let root = test_root("staging-crash");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    // Simulate the crash: source renamed aside, symlink never created.
    let staging = src.with_file_name(format!(
        "{}.dracon-relocate-staging",
        src.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(&src, &staging).unwrap();
    assert!(!src.exists(), "crash simulation must remove the source");
    let err = crate::apply_relocate(&plan).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("MISSING") && msg.contains("mv "),
        "crash-window refusal must name the case + mv recovery: {msg}"
    );
    assert!(
        msg.contains("stale staging"),
        "keeps the stable phrase: {msg}"
    );
    assert!(
        staging.join("a.txt").exists(),
        "refused run must leave staging data intact"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn stale_staging_refusal_names_completed_move_case() {
    // Audit R4-SYS-09: staging present + source already a symlink
    // means the move completed and only staging cleanup failed.
    // Staging is a redundant duplicate: say so + give `rm -rf`.
    let root = test_root("staging-dup");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    let staging = src.with_file_name(format!(
        "{}.dracon-relocate-staging",
        src.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(&src, &staging).unwrap();
    std::os::unix::fs::symlink(&dest_root, &src).unwrap();
    let err = crate::apply_relocate(&plan).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already a symlink") && msg.contains("rm -rf"),
        "completed-move refusal must name the case + rm recovery: {msg}"
    );
    cleanup(&root);
}

#[test]
fn find_stale_staging_dirs_scans_one_level() {
    // Audit R4-SYS-09: the setup scan finds level-1 staging dirs,
    // misses deeper nesting by design, and skips unreadable roots.
    let root = test_root("staging-scan");
    let cand = root.join("cand");
    fs::create_dir_all(cand.join("deep")).unwrap();
    fs::create_dir_all(cand.join("media.dracon-relocate-staging")).unwrap();
    fs::create_dir_all(cand.join("deep").join("x.dracon-relocate-staging")).unwrap();
    fs::create_dir_all(cand.join("plain-dir")).unwrap();
    let found = crate::relocate::find_stale_staging_dirs(std::slice::from_ref(&cand));
    assert_eq!(found, vec![cand.join("media.dracon-relocate-staging")]);
    // Missing/unreadable roots yield nothing, never an error.
    let missing = crate::relocate::find_stale_staging_dirs(&[root.join("nope")]);
    assert!(missing.is_empty());
    cleanup(&root);
}

#[test]
fn apply_relocate_rechecks_space_at_apply_start() {
    // Audit R4-SYS-05: a plan that fit at dry-run time must not copy
    // blind — apply re-runs the avail check so a stale plan refuses
    // instead of ENOSPC-ing mid-copy. u64::MAX bytes can never fit,
    // so this is deterministic on any disk.
    let root = test_root("sys05-space");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let mut plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    assert!(plan.ready);
    plan.bytes = u64::MAX;
    let dest = PathBuf::from(&plan.dest);
    let err = crate::apply_relocate(&plan).unwrap_err();
    assert!(
        format!("{err:#}").contains("free but plan needs"),
        "stale plan must refuse at apply time: {err:#}"
    );
    assert!(
        fs::symlink_metadata(&dest).is_err(),
        "refused apply must not create the dest"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn apply_relocate_removes_partial_dest_on_copy_failure() {
    // Audit R4-SYS-05: a failed copy must not strand retries behind
    // "appeared since planning" — the partial dest is removed and
    // the source left intact. The plan is built while readable (strict
    // planning refuses unreadable trees), then a subdir is locked so
    // the copy itself fails.
    use std::os::unix::fs::PermissionsExt;
    let root = test_root("sys05-cleanup");
    let src = fixture_dir(&root);
    let dest_root = root.join("cold");
    fs::create_dir_all(&dest_root).unwrap();
    let plan = crate::plan_relocate(&src, &dest_root, &[], false).unwrap();
    let dest = PathBuf::from(&plan.dest);
    let locked = src.join("nested");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = crate::apply_relocate(&plan).unwrap_err();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        format!("{err:#}").contains("cannot copy"),
        "copy must fail on the locked dir: {err:#}"
    );
    assert!(
        fs::symlink_metadata(&dest).is_err(),
        "partial dest must be removed so a retry is not stranded"
    );
    assert!(
        fs::read(src.join("a.txt")).is_ok(),
        "failed apply must leave the source untouched"
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
fn is_git_tracked_fails_closed_when_git_unrunnable() {
    // Audit R4-SYS-04: Command spawn failure (git missing from PATH,
    // EACCES, ...) is UNKNOWN, not "untracked" — must Err so the
    // callers fail closed (plan_relocate `?`, cold-scan `unwrap_or(true)`).
    let root = test_root("sys04");
    let sub = git_repo_with_tracked_subdir(&root);
    // A nonexistent binary exercises the exact spawn-failure arm that
    // git-missing-from-PATH hits (both are spawn ENOENT) with no
    // process-global PATH swap, so this is race-free in parallel runs.
    let err = crate::is_git_tracked_with(&sub, "/nonexistent-git-binary-xyz").unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot run git rev-parse"),
        "git-missing must fail closed with spawn context, got: {err:#}"
    );
    cleanup(&root);
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
// and a protected path must survive regardless of the stash flag.
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
    let stash = root.join("stash");
    std::fs::create_dir_all(&stash).unwrap();
    assert!(nm.exists(), "fixture must exist before the run");

    let outcome = clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        false,
        &[],
        false,
        &stash,
    )
    .await
    .expect("dry run must not error");
    let cleaned = outcome.cleaned;

    assert!(
        !cleaned.is_empty(),
        "a dry run must still report candidates"
    );
    assert!(nm.exists(), "dry run must not delete anything");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn node_modules_stash_first_dry_run_also_deletes_nothing() {
    // The stash flag must not turn a dry run into a move.
    let root = test_root("nm-dry-stash");
    let nm = aged_node_modules(&root, 90);
    let stash = root.join("stash");
    std::fs::create_dir_all(&stash).unwrap();

    let outcome = clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        false,
        &[],
        true,
        &stash,
    )
    .await
    .expect("dry run must not error");
    let cleaned = outcome.cleaned;

    assert!(!cleaned.is_empty(), "must still report candidates");
    assert!(
        nm.exists(),
        "stash-first dry run must not move anything"
    );
    let entries: Vec<_> = std::fs::read_dir(&stash).unwrap().collect();
    assert!(
        entries.is_empty(),
        "nothing may be written to the stash root on a dry run"
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
    let stash = root.join("stash");
    std::fs::create_dir_all(&stash).unwrap();
    let protected_str = protected.to_string_lossy().to_string();

    clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        true,
        std::slice::from_ref(&protected_str),
        false,
        &stash,
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

    let (moves, _bytes, _cands, _cold) = crate::run_auto_relocate(&guard)
        .await
        .expect("relocate pass runs");
    assert_eq!(moves, 0, "df failure must pause moves, not move blind");
    assert!(
        big_old.is_dir() && !big_old.is_symlink(),
        "nothing must move while pressure is unreadable"
    );
    cleanup(&root);
}

/// ADDED 2026-10-24 (audit F4): a dry run must not count a PROTECTED tree as
/// reclaimable space.
///
/// The refusal used to run only when applying, so the preview listed every
/// protected tree as reclaimable AND counted its bytes — the same
/// overstatement that was fixed in the rust-target path on 2026-10-02, and
/// one that misled the operator twice in one session.
#[tokio::test]
async fn node_modules_dry_run_does_not_count_a_protected_tree() {
    let root = test_root("nm-dry-protected");
    let nm = aged_node_modules(&root, 90);
    let protected = root.join("proj");
    let protected_str = protected.to_string_lossy().to_string();
    let stash = root.join("stash");
    std::fs::create_dir_all(&stash).unwrap();

    let outcome = clean_old_node_modules(
        std::slice::from_ref(&root),
        30,
        false,
        std::slice::from_ref(&protected_str),
        false,
        &stash,
    )
    .await
    .expect("dry run must not error");

    assert_eq!(
        outcome.reclaimed, 0,
        "a protected tree is not reclaimable, so a dry run must not count it"
    );
    assert!(
        outcome.cleaned.is_empty(),
        "a protected tree must not be listed as a cleanup candidate"
    );
    assert_eq!(
        outcome.protected.len(),
        1,
        "the refusal must be named, not silently dropped"
    );
    assert!(nm.exists(), "dry run must not delete anything");
    let _ = std::fs::remove_dir_all(&root);
}

/// ADDED 2026-10-24 (audit F6): an absurd `node_modules_max_age_days` must
/// not overflow the age computation.
///
/// `max_age_days * 24 * 3600` was the only non-saturating age computation in
/// the crate: it wrapped in release builds and panicked in debug ones, which
/// turned a nonsense knob into a wrong age — and, in debug builds, a crash.
#[tokio::test]
async fn node_modules_absurd_max_age_does_not_overflow() {
    let root = test_root("nm-absurd-age");
    let nm = aged_node_modules(&root, 90);
    let stash = root.join("stash");
    std::fs::create_dir_all(&stash).unwrap();

    let outcome = clean_old_node_modules(
        std::slice::from_ref(&root),
        u64::MAX,
        false,
        &[],
        false,
        &stash,
    )
    .await
    .expect("an absurd age must not panic or wrap");

    assert_eq!(
        outcome.reclaimed, 0,
        "a saturated age means nothing is old enough, which is the safe direction"
    );
    assert!(
        outcome.cleaned.is_empty(),
        "nothing may be listed under an absurd age"
    );
    assert!(nm.exists(), "the tree must be left alone");
    let _ = std::fs::remove_dir_all(&root);
}


// ---------------------------------------------------------------------------
// 2026-11-11 disk-full incident — link-safety and duplicate-tree regressions
// ---------------------------------------------------------------------------
//
// Both fixtures below reproduce the exact conditions of the real incident:
// a directory hidden from git by a DIRECTORY-ONLY ignore rule, and a tree
// whose bytes already live on the cold root.

/// Initialise a git work tree at `root` with the given `.gitignore` content.
#[cfg(unix)]
fn git_fixture(root: &Path, gitignore: &str) -> PathBuf {
    use std::process::Command;
    fs::create_dir_all(root).unwrap();
    let init = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["init", "-q"])
        .status()
        .expect("git must be available for the link-safety fixture");
    assert!(init.success(), "git init failed in {}", root.display());
    fs::write(root.join(".gitignore"), gitignore).unwrap();
    root.to_path_buf()
}

#[cfg(unix)]
#[test]
fn link_safety_refuses_a_directory_only_ignore_rule() {
    // THE REGRESSION: `out/` hides the directory, but git only applies a
    // trailing-slash pattern to a directory. Once relocate replaces the tree
    // with a symlink the rule stops matching and the whole tree becomes an
    // untracked gitlink — which is how a 163 GiB media tree nearly landed in
    // the index on 2026-11-11.
    let root = test_root("link-safety-dir-only");
    cleanup(&root);
    let repo = git_fixture(&root, "out/\n");
    let out = repo.join("out");
    fs::create_dir_all(out.join("audio")).unwrap();
    fs::write(out.join("audio").join("a.mp3"), b"x").unwrap();

    let verdict = crate::link_safety(&out).expect("git is available and the rule parses");
    match verdict {
        crate::LinkSafety::DirectoryOnlyIgnore { ref pattern } => {
            assert_eq!(pattern, "out/", "the trailing-slash rule must be reported");
        }
        other => panic!("expected a directory-only verdict, got {other:?}"),
    }
    assert!(
        verdict.exposes_after_swap(),
        "a directory-only rule exposes the path after the swap"
    );
    assert!(
        verdict.refusal_reason().is_some(),
        "the refusal must explain itself to the operator"
    );

    // The plan must CARRY that verdict so auto mode can refuse it, and
    // `apply_relocate` must roll the swap back if it is attempted anyway.
    let elsewhere = root.join("cold");
    fs::create_dir_all(&elsewhere).unwrap();
    let plan = crate::plan_relocate(&out, &elsewhere, &[], true).expect("plan computes");
    assert!(
        plan.exposes_after_swap,
        "the plan must flag the exposure: {:?}",
        plan.issues
    );
    assert!(
        plan.issues
            .iter()
            .any(|i| i.contains("DIRECTORY-ONLY") || i.contains("directory-only")),
        "the issue must name the trap: {:?}",
        plan.issues
    );

    // End-to-end rollback: the copy is made, the symlink would expose the
    // tree, and apply puts the ORIGINAL directory back untouched.
    let err = crate::apply_relocate(&plan).expect_err("the swap must be rolled back");
    let text = format!("{err:#}");
    assert!(
        text.contains("ROLLED BACK"),
        "the failure must report a rollback rather than a lost tree: {text}"
    );
    let restored = fs::symlink_metadata(&out).expect("the source must exist again");
    assert!(
        restored.is_dir() && !restored.file_type().is_symlink(),
        "the original directory must be restored, not left as a symlink"
    );
    assert!(out.join("audio").join("a.mp3").is_file());
    assert!(
        !elsewhere.join("out").exists(),
        "the redundant cold copy must be removed by the rollback"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn link_safety_allows_a_rule_without_a_trailing_slash() {
    // The fix we applied to the real repo (`/out` in addition to `out/`) must
    // be accepted: this proves the guard is not a blanket "no git work trees".
    let root = test_root("link-safety-slashless");
    cleanup(&root);
    let repo = git_fixture(&root, "/out\nout/\n");
    let out = repo.join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("a.mp3"), b"x").unwrap();

    let verdict = crate::link_safety(&out).expect("git is available");
    assert_eq!(
        verdict,
        crate::LinkSafety::IgnoredAsSymlink,
        "a slashless rule keeps matching after the swap"
    );
    assert!(!verdict.exposes_after_swap());
    assert!(verdict.refusal_reason().is_none());

    // And git really does ignore the same PATH once it IS a symlink — the
    // property the guard is asserting, verified against git itself rather than
    // assumed. The probe swaps `out` itself, because `/out` matches the PATH,
    // not a differently-named link that happens to point at it.
    let storage = root.join("storage");
    fs::create_dir_all(&storage).unwrap();
    fs::rename(&out, storage.join("out")).unwrap();
    std::os::unix::fs::symlink(storage.join("out"), &out).unwrap();
    let check = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["check-ignore", "--no-index", "-q"])
        .arg(&out)
        .status()
        .expect("git check-ignore runs");
    assert!(
        check.success(),
        "a symlink at an ignored path must still be ignored by the slashless rule"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn post_swap_exposure_reports_an_exposed_symlink_and_is_silent_when_ignored() {
    let root = test_root("post-swap");
    cleanup(&root);
    let repo = git_fixture(&root, "out/\n");
    let out = repo.join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("a.mp3"), b"x").unwrap();

    // Simulate the swap: replace the ignored directory with a symlink.
    let storage = root.join("storage");
    fs::create_dir_all(&storage).unwrap();
    fs::rename(&out, storage.join("out")).unwrap();
    std::os::unix::fs::symlink(storage.join("out"), &out).unwrap();

    let exposure = crate::post_swap_exposure(&out).expect("git status runs");
    assert!(
        exposure.is_some(),
        "an exposed symlink must be reported so the caller can roll back"
    );

    // With the slashless rule the same swap stays invisible to git.
    fs::write(repo.join(".gitignore"), "/out\nout/\n").unwrap();
    let exposure = crate::post_swap_exposure(&out).expect("git status runs");
    assert!(
        exposure.is_none(),
        "the slashless rule must keep the swapped path ignored"
    );
    cleanup(&root);
}

#[test]
fn second_identical_tree_is_reported_already_cold_not_as_a_candidate() {
    // THE SECOND REGRESSION: 163 GiB of media sat on `/` while a byte-identical
    // copy already lived on the cold root. The scanner happily reported the
    // live tree as a relocation candidate (and the age floor hid it forever),
    // so the duplicate was never surfaced. A duplicated tree must be reported
    // as `already cold` and must NOT appear as a candidate.
    let root = test_root("already-cold");
    cleanup(&root);
    let scan = root.join("scan");
    let cold = root.join("cold");
    let live = scan.join("media");
    fs::create_dir_all(live.join("audio")).unwrap();
    fs::create_dir_all(cold.join("media").join("audio")).unwrap();
    fs::write(live.join("audio").join("a.mp3"), vec![3u8; 1_100_000]).unwrap();
    fs::write(cold.join("media").join("audio").join("a.mp3"), vec![3u8; 1_100_000]).unwrap();
    touch_aged(&live, "20 days ago");

    let report = crate::find_already_cold_trees(
        std::slice::from_ref(&scan),
        1024 * 1024,
        &cold,
    );
    assert_eq!(report.len(), 1, "exactly one duplicated tree: {report:?}");
    assert!(report[0].path.ends_with("media"));
    assert_eq!(report[0].files, 1);
    assert_eq!(report[0].bytes, 1_100_000);
    assert!(report[0].twin.ends_with("cold/media"));

    // With the cold root supplied the duplicate is dropped from the candidate
    // list; without it the old behaviour (a candidate) is reproduced, which is
    // what makes this a regression test rather than a tautology.
    let with_cold = crate::find_cold_candidates_with(
        std::slice::from_ref(&scan),
        1024 * 1024,
        14,
        Some(&cold),
    );
    assert!(
        !with_cold.iter().any(|c| c.path.ends_with("media")),
        "an already-cold tree must not be reported as a candidate: {with_cold:?}"
    );
    let without_cold = crate::find_cold_candidates_with(
        std::slice::from_ref(&scan),
        1024 * 1024,
        14,
        None,
    );
    assert!(
        without_cold.iter().any(|c| c.path.ends_with("media")),
        "sanity: without duplicate screening the tree IS a candidate: {without_cold:?}"
    );
    cleanup(&root);
}

#[test]
fn already_cold_requires_identical_manifests_not_just_matching_totals() {
    // A size-only screen would call these identical (same file count, same
    // total bytes) while their CONTENT differs. The check must not.
    let root = test_root("already-cold-content");
    cleanup(&root);
    let scan = root.join("scan");
    let cold = root.join("cold");
    fs::create_dir_all(scan.join("pair")).unwrap();
    fs::create_dir_all(cold.join("pair")).unwrap();
    fs::write(scan.join("pair").join("one.bin"), vec![1u8; 600_000]).unwrap();
    fs::write(scan.join("pair").join("two.bin"), vec![2u8; 600_000]).unwrap();
    // Same names, same totals (1.2 MB across 2 files), swapped sizes.
    fs::write(cold.join("pair").join("one.bin"), vec![1u8; 600_001]).unwrap();
    fs::write(cold.join("pair").join("two.bin"), vec![2u8; 599_999]).unwrap();

    assert!(
        !crate::already_cold_under(&scan.join("pair"), &cold).expect("manifests are readable"),
        "differing per-file sizes must not read as already cold"
    );
    assert!(crate::find_already_cold_trees(std::slice::from_ref(&scan), 1024 * 1024, &cold).is_empty());
    cleanup(&root);
}


/// End-to-end behaviour of the real incident scenario on the LIVE policy shape:
/// candidate trees under a scanned root, one of which is byte-identical to an
/// existing cold-root twin. The duplicate must be skipped and the guard pass
/// must not move it.
#[cfg(unix)]
#[test]
fn auto_relocate_pass_skips_the_duplicate_tree_instead_of_moving_it() {
    let root = test_root("auto-duplicate");
    cleanup(&root);
    let scan = root.join("scan");
    let cold = root.join("cold");
    let live = scan.join("media");
    fs::create_dir_all(live.join("audio")).unwrap();
    fs::create_dir_all(cold.join("media").join("audio")).unwrap();
    let payload = vec![5u8; 2 * 1024 * 1024 + 7];
    fs::write(live.join("audio").join("a.mp3"), &payload).unwrap();
    fs::write(cold.join("media").join("audio").join("a.mp3"), &payload).unwrap();
    touch_aged(&live, "20 days ago");

    let guards = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = GuardPolicy {
        relocate_cold_root: cold.to_string_lossy().to_string(),
        relocate_candidate_roots: scan.to_string_lossy().to_string(),
        relocate_min_size_mb: 1,
        relocate_min_age_days: 14,
        auto_relocate_apply: true,
        disk_mount_path: "/".to_string(),
        disk_action_percent: 0, // force past the pressure gate
        ..Default::default()
    };
    let (moves, bytes, cands, already) = guards
        .block_on(crate::run_auto_relocate(&guard))
        .expect("relocate pass runs");

    assert_eq!(moves, 0, "a duplicated tree must never be moved");
    assert_eq!(bytes, 0);
    assert!(
        !cands.iter().any(|c| c.path.ends_with("media")),
        "the duplicate must not be a candidate: {cands:?}"
    );
    assert_eq!(
        already.len(),
        1,
        "the duplicate must be REPORTED as already cold: {already:?}"
    );
    assert!(live.is_dir() && !live.is_symlink(), "the live tree stays put");
    cleanup(&root);
}
