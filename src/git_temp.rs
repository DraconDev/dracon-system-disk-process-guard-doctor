//! Orphaned git temporary-object cleanup.
//!
//! ADDED 2026-10-27 after the 2026-10-26 root-disk incident: `ai-auto-writer`
//! had accumulated 11 `tmp_pack_*` files totalling 33.0 GiB (3–4.4 GiB each)
//! from interrupted or concurrent `git gc`/`repack` runs, and
//! `dracon/conv-work` held more than 4 GiB of `tmp_obj_*`. None of the existing
//! cleanup kinds looked inside `.git`, so the only remedy was manual `rm`.
//!
//! Git writes these files while packing and normally deletes them itself; a
//! killed process leaves them behind forever. They are pure scratch: no ref,
//! index, or object directory entry names them, and `git gc` would simply
//! recreate any of them it still needed. That makes them the safest possible
//! deletion class — with one caveat this module takes seriously: a *running*
//! git/cargo process may be writing one right now, so a candidate is only ever
//! removed when no owner is using it and it is older than the configured age
//! floor.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Filename prefixes git uses for in-progress pack/index/object scratch files.
///
/// - `tmp_pack_*` — a pack being written next to its siblings in `objects/pack`
/// - `tmp_idx_*`  — the matching `.idx` being written
/// - `tmp_obj_*`  — a loose object being written into a fanout directory
const TMP_PREFIXES: [&str; 3] = ["tmp_pack_", "tmp_idx_", "tmp_obj_"];

/// One orphaned git scratch file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitTempFile {
    /// Absolute path of the scratch file.
    pub(crate) path: PathBuf,
    /// Allocated size in bytes.
    pub(crate) bytes: u64,
    /// Age in hours, or `None` when the mtime cannot be read.
    pub(crate) age_hours: Option<u64>,
}

impl GitTempFile {
    /// Whether this file is old enough that no live pack operation can own it.
    ///
    /// A file whose mtime is unreadable is treated as NOT old enough: the age
    /// floor exists precisely to avoid racing an in-flight pack, and an
    /// unknown mtime cannot rule that race out.
    pub(crate) fn is_expired(&self, min_age_hours: u64) -> bool {
        match self.age_hours {
            Some(age) => age >= min_age_hours,
            None => false,
        }
    }
}

/// Outcome of a cleanup pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct GitTempCleanupOutcome {
    /// Candidates that were deleted (or, in dry-run, would have been).
    pub(crate) cleaned: Vec<GitTempFile>,
    /// Candidates skipped because they are too fresh to touch.
    pub(crate) skipped_fresh: Vec<GitTempFile>,
    /// Candidates skipped because a live process owns the repository.
    pub(crate) skipped_owned: Vec<GitTempFile>,
    /// Bytes represented by `cleaned`.
    pub(crate) reclaimed_bytes: u64,
}

/// Returns true when `name` is a git scratch filename this module owns.
pub(crate) fn is_git_temp_name(name: &str) -> bool {
    TMP_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix) && name.len() > prefix.len())
}

/// Walk `root` looking for `.git` directories, then inspect their object
/// storage for leftover scratch files.
///
/// Bounded on purpose: the caller supplies the roots it already trusts
/// (`rust_search_roots`-style comma lists) rather than sweeping `/`. Symlinked
/// directories are not followed, so a repo cannot lure the walk outside a
/// configured root, and recursion stops at the first `.git` found so nested
/// worktree metadata is not walked twice.
pub(crate) fn find_git_temp_files(root: &Path, max_depth: usize) -> Vec<GitTempFile> {
    let mut found = Vec::new();
    collect_git_temp_files(root, 0, max_depth, &mut found);
    found
}

fn collect_git_temp_files(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<GitTempFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            // Never follow: the target may live outside the configured roots.
            continue;
        }
        if file_type.is_dir() {
            let path = entry.path();
            if name == ".git" {
                scan_git_dir(&path, out);
                // A `.git` directory's own subdirectories hold no repos we
                // want (objects/, refs/, …), and worktree metadata can live
                // under modules/. One scan per repo is enough here.
                continue;
            }
            if depth < max_depth {
                subdirs.push(path);
            }
        }
    }
    for subdir in subdirs {
        collect_git_temp_files(&subdir, depth + 1, max_depth, out);
    }
}

/// Inspect one `.git` directory: `objects/pack` plus every 2-hex fanout dir.
fn scan_git_dir(git_dir: &Path, out: &mut Vec<GitTempFile>) {
    let pack_dir = git_dir.join("objects").join("pack");
    scan_flat_dir(&pack_dir, out);

    let objects_dir = git_dir.join("objects");
    let Ok(entries) = std::fs::read_dir(&objects_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Loose objects live in two-hex-character fanout directories. Restrict
        // the walk to those so `objects/info` and friends are never touched.
        if name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()) {
            scan_flat_dir(&entry.path(), out);
        }
    }
}

fn scan_flat_dir(dir: &Path, out: &mut Vec<GitTempFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_git_temp_name(name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        out.push(GitTempFile {
            path: entry.path(),
            bytes: metadata.len(),
            age_hours: age_hours(metadata.modified().ok()),
        });
    }
}

/// Age in whole hours, saturating at `u64::MAX` for clock skew in the future.
fn age_hours(modified: Option<SystemTime>) -> Option<u64> {
    let modified = modified?;
    let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::ZERO);
    Some(age.as_secs() / 3600)
}

/// A live git process and the directory it is running in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitOwner {
    pub(crate) pid: u32,
    pub(crate) name: String,
    /// The process's working directory, when readable.
    pub(crate) cwd: Option<PathBuf>,
    /// True when this git invocation can write scratch packs.
    pub(crate) writes_packs: bool,
}

/// Whether `name` is a git process that could be writing scratch files.
fn is_git_process_name(name: &str) -> bool {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name);
    base == "git" || base.starts_with("git-")
}

/// Subcommands that actually write `tmp_pack_*`/`tmp_idx_*`/`tmp_obj_*`.
///
/// REFINED 2026-10-27 (second live run): treating ANY git process as an owner
/// still skipped everything, because the sync daemon runs `git add`/`commit`
/// on a one-second pulse and therefore always has a `git` inside every watched
/// repo. But `add`/`commit`/`status` never produce scratch packs — only the
/// packing subcommands below do. Matching the subcommand is what makes the
/// owner check both sound (a real repack still blocks) and useful (routine
/// commits do not).
const PACK_WRITING_SUBCOMMANDS: [&str; 6] = [
    "gc",
    "repack",
    "pack-objects",
    "index-pack",
    "prune",
    "unpack-objects",
];

/// True when a git argv contains a subcommand that writes scratch files.
///
/// `argv` is the NUL-split `/proc/<pid>/cmdline`. For `git-remote-*` style
/// helpers the argv has no subcommand, so they are treated as pack-writing:
/// a push/fetch may be packing in the background and certainty is worth more
/// than the extra skip here.
pub(crate) fn argv_writes_packs(argv: &[String]) -> bool {
    if argv.iter().any(|a| {
        PACK_WRITING_SUBCOMMANDS
            .iter()
            .any(|sub| a == sub || a.ends_with(&format!("/{sub}")))
    }) {
        return true;
    }
    argv.first()
        .map(|a| {
            let base = Path::new(a)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(a);
            base.starts_with("git-remote-") || base == "git-upload-pack" || base == "git-receive-pack"
        })
        .unwrap_or(false)
}

/// Collect the live git processes and their working directories.
///
/// REPLACED 2026-10-27 (first live run): the original rule was "any git process
/// anywhere blocks every deletion". On this host that is always true — the sync
/// daemon commits continuously — so the cleaner skipped all 22 candidates
/// (13.6 GiB) on every pass and could never do its job. The truthful question
/// is not "is git running" but "is git running IN THIS REPOSITORY", which the
/// process's working directory answers directly: `git gc`/`git repack` write
/// their temp packs while cwd is the repo root (or `.git`).
pub(crate) fn live_git_owners(proc_root: &Path) -> Vec<GitOwner> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut owners = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim().to_string();
        if !is_git_process_name(&comm) {
            continue;
        }
        let cwd = std::fs::read_link(entry.path().join("cwd")).ok();
        let argv: Vec<String> = std::fs::read(entry.path().join("cmdline"))
            .map(|raw| {
                raw.split(|b| *b == 0)
                    .filter(|chunk| !chunk.is_empty())
                    .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        owners.push(GitOwner {
            pid,
            name: comm,
            cwd,
            writes_packs: argv_writes_packs(&argv),
        });
    }
    owners
}

/// The git process currently working inside `repo`, if any.
///
/// `repo` is the directory that contains the `.git` directory being cleaned.
/// A process counts as an owner when its cwd is the repo itself or lies inside
/// it, which covers both "git ran in the worktree" and "git ran inside .git".
pub(crate) fn git_owner_for<'a>(owners: &'a [GitOwner], repo: &Path) -> Option<&'a GitOwner> {
    owners.iter().find(|owner| {
        owner.writes_packs
            && owner
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd == repo || cwd.starts_with(repo))
    })
}

/// The repository root that owns a scratch file, derived from its path.
///
/// Scratch files always live at `<repo>/.git/objects/...`, so the repo root is
/// the component immediately before `.git`. Returns `None` for a path without a
/// `.git` component, which means the file is not where git would put it.
pub(crate) fn repo_root_for_git_path(path: &Path) -> Option<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let git_index = components
        .iter()
        .rposition(|c| c.as_os_str() == ".git")?;
    if git_index == 0 {
        return None;
    }
    let mut root = PathBuf::new();
    for component in &components[..git_index] {
        root.push(component);
    }
    Some(root)
}

/// Delete expired scratch files, or report what would be deleted.
///
/// `apply == false` performs the same discovery and classification without
/// touching the filesystem, so a dry run and a real run always agree on what
/// is in scope.
pub(crate) fn clean_git_temp_files<F>(
    candidates: Vec<GitTempFile>,
    min_age_hours: u64,
    owner_of: F,
    apply: bool,
) -> GitTempCleanupOutcome
where
    F: Fn(&Path) -> Option<String>,
{
    let mut outcome = GitTempCleanupOutcome::default();
    for candidate in candidates {
        if owner_of(&candidate.path).is_some() {
            outcome.skipped_owned.push(candidate);
            continue;
        }
        if !candidate.is_expired(min_age_hours) {
            outcome.skipped_fresh.push(candidate);
            continue;
        }
        if apply && std::fs::remove_file(&candidate.path).is_err() {
            // A file we cannot remove is simply not removed; it stays visible
            // as a fresh candidate on the next pass rather than being reported
            // as reclaimed.
            continue;
        }
        outcome.reclaimed_bytes += candidate.bytes;
        outcome.cleaned.push(candidate);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    #[test]
    fn recognises_git_scratch_names() {
        assert!(is_git_temp_name("tmp_pack_g5Sj3N"));
        assert!(is_git_temp_name("tmp_idx_CRj8PZ"));
        assert!(is_git_temp_name("tmp_obj_p6iIa3"));
        // The prefix alone is not a filename git writes.
        assert!(!is_git_temp_name("tmp_pack_"));
        // Real packs and ordinary files must never match.
        assert!(!is_git_temp_name("pack-6a804519.pack"));
        assert!(!is_git_temp_name("main"));
        // DELIBERATE (2026-10-27): matching is by git's own prefix glob, so a
        // decorated scratch name is still treated as scratch. Git never writes
        // one, but anything carrying these prefixes inside objects/ is scratch
        // by definition, and a narrower rule risks missing a real variant.
        assert!(is_git_temp_name("tmp_pack_g5Sj3N.pack"));
    }

    #[test]
    fn finds_pack_and_loose_scratch_files_but_ignores_real_objects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let git = tmp.path().join("repo").join(".git");
        write_file(&git.join("objects/pack/tmp_pack_aaa"), 64);
        write_file(&git.join("objects/pack/pack-1234.pack"), 64);
        write_file(&git.join("objects/c6/tmp_obj_bbb"), 32);
        write_file(&git.join("objects/c6/abcdef"), 32);
        write_file(&git.join("objects/info/packs"), 8);

        let mut found = find_git_temp_files(tmp.path(), 4);
        found.sort_by(|a, b| a.path.cmp(&b.path));

        let names: Vec<String> = found
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["tmp_obj_bbb".to_string(), "tmp_pack_aaa".to_string()]);
    }

    #[test]
    fn does_not_follow_symlinks_out_of_the_root() {
        let outside = tempfile::tempdir().expect("outside");
        let fake_git = outside.path().join(".git/objects/pack");
        write_file(&fake_git.join("tmp_pack_zzz"), 64);

        let root = tempfile::tempdir().expect("root");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");

        assert!(find_git_temp_files(root.path(), 4).is_empty());
    }

    #[test]
    fn zero_age_floor_still_refuses_fresh_files() {
        // age is computed in whole hours; a file written now is 0h old.
        let now_file = GitTempFile {
            path: PathBuf::from("/nonexistent"),
            bytes: 10,
            age_hours: Some(0),
        };
        assert!(!now_file.is_expired(1));
        assert!(now_file.is_expired(0));
    }

    #[test]
    fn unknown_mtime_is_never_expired() {
        let unknown = GitTempFile {
            path: PathBuf::from("/nonexistent"),
            bytes: 10,
            age_hours: None,
        };
        assert!(!unknown.is_expired(0));
        assert!(!unknown.is_expired(24));
    }

    #[test]
    fn a_live_git_process_blocks_every_deletion() {
        let candidates = vec![GitTempFile {
            path: PathBuf::from("/nonexistent/tmp_pack_a"),
            bytes: 10,
            age_hours: Some(100),
        }];
        let outcome = clean_git_temp_files(candidates, 1, |_| Some("git".to_string()), true);
        assert!(outcome.cleaned.is_empty());
        assert_eq!(outcome.skipped_owned.len(), 1);
        assert_eq!(outcome.reclaimed_bytes, 0);
    }

    #[test]
    fn dry_run_reports_without_deleting() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tmp_pack_a");
        write_file(&path, 16);
        let candidates = vec![GitTempFile {
            path: path.clone(),
            bytes: 16,
            age_hours: Some(48),
        }];

        let outcome = clean_git_temp_files(candidates, 1, |_| None, false);
        assert!(path.exists(), "dry run must not delete");
        assert_eq!(outcome.cleaned.len(), 1);
        assert_eq!(outcome.reclaimed_bytes, 16);
    }

    #[test]
    fn apply_deletes_expired_files_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let old = tmp.path().join("tmp_pack_old");
        let fresh = tmp.path().join("tmp_pack_fresh");
        write_file(&old, 8);
        write_file(&fresh, 8);

        let candidates = vec![
            GitTempFile { path: old.clone(), bytes: 8, age_hours: Some(72) },
            GitTempFile { path: fresh.clone(), bytes: 8, age_hours: Some(0) },
        ];
        let outcome = clean_git_temp_files(candidates, 1, |_| None, true);

        assert!(!old.exists());
        assert!(fresh.exists(), "fresh scratch must survive the age floor");
        assert_eq!(outcome.cleaned.len(), 1);
        assert_eq!(outcome.skipped_fresh.len(), 1);
        assert_eq!(outcome.reclaimed_bytes, 8);
    }

    #[test]
    fn only_a_git_running_inside_the_repo_blocks_that_repo() {
        let busy = PathBuf::from("/home/u/Dev/busy-repo");
        let quiet = PathBuf::from("/home/u/Dev/quiet-repo");
        let owners = vec![GitOwner {
            pid: 42,
            name: "git".to_string(),
            cwd: Some(busy.join(".git")),
            writes_packs: true,
        }];
        assert!(git_owner_for(&owners, &busy).is_some(), "git inside this repo owns it");
        assert!(
            git_owner_for(&owners, &quiet).is_none(),
            "a git process elsewhere must NOT block an unrelated repo"
        );
    }

    #[test]
    fn a_git_with_unreadable_cwd_blocks_nothing_specific() {
        let owners = vec![GitOwner {
            pid: 7,
            name: "git".to_string(),
            cwd: None,
            writes_packs: true,
        }];
        assert!(git_owner_for(&owners, Path::new("/home/u/Dev/repo")).is_none());
    }

    #[test]
    fn repo_root_is_derived_from_the_git_component() {
        assert_eq!(
            repo_root_for_git_path(Path::new(
                "/home/u/Dev/repo/.git/objects/pack/tmp_pack_a"
            )),
            Some(PathBuf::from("/home/u/Dev/repo"))
        );
        assert_eq!(
            repo_root_for_git_path(Path::new("/home/u/Dev/repo/.git/objects/37/tmp_obj_b")),
            Some(PathBuf::from("/home/u/Dev/repo"))
        );
        assert_eq!(
            repo_root_for_git_path(Path::new("/home/u/proj/.git")),
            Some(PathBuf::from("/home/u/proj"))
        );
        // No .git component means this is not git object storage.
        assert_eq!(
            repo_root_for_git_path(Path::new("/home/u/Dev/repo/src/tmp_pack_a")),
            None
        );
    }

    #[test]
    fn git_process_names_are_recognised_without_matching_cargo() {
        assert!(is_git_process_name("git"));
        assert!(is_git_process_name("/usr/bin/git"));
        assert!(is_git_process_name("git-remote-https"));
        assert!(!is_git_process_name("cargo"));
        assert!(!is_git_process_name("gitk"));
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn packing_subcommands_count_as_owners() {
        assert!(argv_writes_packs(&argv(&["git", "gc", "--quiet"])));
        assert!(argv_writes_packs(&argv(&["git", "repack", "-ad"])));
        assert!(argv_writes_packs(&argv(&["git", "pack-objects", "out"])));
        assert!(argv_writes_packs(&argv(&["/usr/bin/git", "prune"])));
    }

    #[test]
    fn routine_commit_subcommands_do_not_block_cleanup() {
        // These run on the sync daemon's one-second pulse; if they counted as
        // owners the cleaner could never act.
        assert!(!argv_writes_packs(&argv(&["git", "add", "-A"])));
        assert!(!argv_writes_packs(&argv(&["git", "commit", "-m", "x"])));
        assert!(!argv_writes_packs(&argv(&["git", "status", "--porcelain"])));
        assert!(!argv_writes_packs(&argv(&["git", "push", "origin", "main"])));
    }

    #[test]
    fn transport_helpers_count_because_they_may_pack() {
        assert!(argv_writes_packs(&argv(&["git-remote-https", "origin", "url"])));
        assert!(argv_writes_packs(&argv(&["git-receive-pack", "/repo"])));
    }

    #[test]
    fn non_git_argv_is_not_an_owner() {
        assert!(!argv_writes_packs(&argv(&["cargo", "build"])));
        assert!(!argv_writes_packs(&[]));
    }

    #[test]
    fn a_commit_in_the_same_repo_does_not_block_a_real_deletion() {
        let repo = PathBuf::from("/home/u/Dev/repo");
        let owners = vec![GitOwner {
            pid: 99,
            name: "git".to_string(),
            cwd: Some(repo.clone()),
            writes_packs: false,
        }];
        assert!(
            git_owner_for(&owners, &repo).is_none(),
            "a routine git commit must not block scratch cleanup"
        );
    }
}
