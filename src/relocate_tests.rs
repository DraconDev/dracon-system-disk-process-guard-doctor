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
    assert_eq!(fs::read(dst.join("nested").join("b.txt")).unwrap(), b"world!");
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
        "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init",
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
