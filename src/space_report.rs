//! Report-only detectors for reclaimable space that must never be auto-deleted.
//!
//! ADDED 2026-10-27 as part of the nitpicky disk cleaner. Everything in this
//! module is deliberately REPORT-ONLY: the guard prints it, sizes it and hands
//! the operator an exact command, but never acts.
//!
//! Two classes live here, for the same underlying reason — the tool cannot
//! prove the item is worthless:
//!
//! 1. **Dead swapfiles.** A swapfile that no longer appears in `/proc/swaps`
//!    looks like pure waste (the 2026-10-26 incident found a 32 GiB
//!    `/var/lib/swapfile` that nothing referenced). But activating a swapfile
//!    is a root operation and removing the wrong one can silently disable
//!    swap on a memory-pressured host, so the guard reports it and prints the
//!    `sudo rm` line instead of guessing. Root-owned paths are never attempted
//!    by an unprivileged tool: `rm` on them would only produce a permission
//!    error, or worse, succeed if the guard ever ran as root.
//!
//! 2. **Stray top-level directories and old backup bundles.** A 19 GiB
//!    `*.bundle` may be the only copy of history that was later rewritten
//!    away; a `dracon-lfs-proof/` may be referenced by an audit. Nothing here
//!    can establish provenance, so these are surfaced with sizes and left
//!    alone until a human approves each path.

use std::path::{Path, PathBuf};

/// A swapfile that exists on disk but is not currently active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadSwapfile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    /// True when the file is not writable by the current user, i.e. removing
    /// it requires root.
    pub(crate) needs_root: bool,
}

impl DeadSwapfile {
    /// The exact command an operator should run to reclaim this file.
    pub(crate) fn removal_command(&self) -> String {
        if self.needs_root {
            format!("sudo rm -f {}", self.path.display())
        } else {
            format!("rm -f {}", self.path.display())
        }
    }
}

/// A directory or backup artefact that looks abandoned but is not provably so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrayItem {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) kind: StrayKind,
}

/// Why a stray item is being reported rather than cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrayKind {
    /// A `*.bundle`/`*.tar.gz`/`*.tar.zst` backup archive.
    BackupArchive,
    /// A bare repository kept as a pre-rewrite safety copy.
    BackupRepo,
    /// A top-level directory whose name marks it as ad-hoc scratch.
    ScratchDir,
}

impl StrayKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::BackupArchive => "backup-archive",
            Self::BackupRepo => "backup-repo",
            Self::ScratchDir => "scratch-dir",
        }
    }
}

/// Markers that make a top-level directory report-worthy.
///
/// Kept as an explicit list rather than a heuristic so the report stays
/// predictable: an operator can grep for these names and know exactly what
/// will and will not appear.
const SCRATCH_DIR_MARKERS: [&str; 6] = [
    "-recovery-",
    "-proof",
    "-audit-new",
    "tmp-shield",
    "-orphans",
    "-pre-filterrepo-backup",
];

const BACKUP_ARCHIVE_SUFFIXES: [&str; 3] = [".bundle", ".tar.gz", ".tar.zst"];

/// Parse `/proc/swaps` into the set of active swap paths.
///
/// The first line is a header. A file that is not in this list is inactive —
/// which is exactly the condition that makes it a candidate.
pub(crate) fn active_swap_paths(swaps_contents: &str) -> Vec<PathBuf> {
    swaps_contents
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .map(PathBuf::from)
        .collect()
}

/// Decide whether `path` is a dead swapfile worth reporting.
///
/// `active` is the parsed `/proc/swaps` list. A file already listed there is
/// live swap and is never reported, regardless of how it was named.
pub(crate) fn classify_swapfile(path: &Path, bytes: u64, active: &[PathBuf]) -> Option<DeadSwapfile> {
    if active.iter().any(|a| a == path) {
        return None;
    }
    let needs_root = !is_writable_by_current_user(path);
    Some(DeadSwapfile {
        path: path.to_path_buf(),
        bytes,
        needs_root,
    })
}

/// True when the current process may unlink inside this file's directory.
///
/// Uses the parent directory rather than the file itself: unlinking requires
/// write permission on the directory, which is the operation that would fail.
fn is_writable_by_current_user(path: &Path) -> bool {
    path.parent()
        .map(|parent| {
            // A path we cannot even inspect is treated as root-owned: the
            // report then errs toward showing `sudo`.
            std::fs::metadata(parent)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = m.permissions().mode();
                    let uid = unsafe_geteuid();
                    // Owner/group/other write bits against our identity.
                    if uid == 0 {
                        return true;
                    }
                    let owner = std::os::unix::fs::MetadataExt::uid(&m);
                    let group = std::os::unix::fs::MetadataExt::gid(&m);
                    let in_group = group_in_current_groups(group);
                    if owner == uid {
                        mode & 0o200 != 0
                    } else if in_group {
                        mode & 0o020 != 0
                    } else {
                        mode & 0o002 != 0
                    }
                })
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

extern "C" {
    fn geteuid() -> u32;
}

/// The effective uid, which is what permission checks actually use.
fn unsafe_geteuid() -> u32 {
    // SAFETY: `geteuid` is a pure syscall wrapper with no memory safety
    // requirements and cannot fail.
    unsafe { geteuid() }
}

/// Whether `gid` is one of the current process's supplementary groups.
fn group_in_current_groups(gid: u32) -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Groups:"))
                .map(|line| {
                    line.trim_start_matches("Groups:")
                        .split_whitespace()
                        .filter_map(|g| g.parse::<u32>().ok())
                        .any(|g| g == gid)
                })
        })
        .unwrap_or(false)
}

/// Report a stray backup/scratch item, if `name` matches a known marker.
///
/// `bytes` is the caller's already-measured allocated size, so a report can be
/// produced from one bounded `du` walk rather than re-statting every file.
pub(crate) fn classify_stray(path: &Path, name: &str, bytes: u64, is_dir: bool) -> Option<StrayItem> {
    if !is_dir {
        if BACKUP_ARCHIVE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            return Some(StrayItem {
                path: path.to_path_buf(),
                bytes,
                kind: StrayKind::BackupArchive,
            });
        }
        return None;
    }
    if name.ends_with(".git") && name.contains("backup") {
        return Some(StrayItem {
            path: path.to_path_buf(),
            bytes,
            kind: StrayKind::BackupRepo,
        });
    }
    if SCRATCH_DIR_MARKERS.iter().any(|marker| name.contains(marker)) {
        return Some(StrayItem {
            path: path.to_path_buf(),
            bytes,
            kind: StrayKind::ScratchDir,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_swaps_header_and_entries() {
        let contents = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                        /dev/zram0                              partition\t63897596\t0\t5\n\
                        /mnt/data/swapfile                      file\t\t67108860\t131704\t-1\n";
        let active = active_swap_paths(contents);
        assert_eq!(
            active,
            vec![PathBuf::from("/dev/zram0"), PathBuf::from("/mnt/data/swapfile")]
        );
    }

    #[test]
    fn empty_swaps_file_has_no_active_paths() {
        assert!(active_swap_paths("").is_empty());
        assert!(active_swap_paths("Filename Type Size Used Priority\n").is_empty());
    }

    #[test]
    fn an_active_swapfile_is_never_reported_as_dead() {
        let active = vec![PathBuf::from("/mnt/data/swapfile")];
        assert!(
            classify_swapfile(Path::new("/mnt/data/swapfile"), 1024, &active).is_none(),
            "live swap must not be reported"
        );
    }

    #[test]
    fn an_inactive_swapfile_is_reported() {
        let active = vec![PathBuf::from("/dev/zram0")];
        let dead = classify_swapfile(Path::new("/var/lib/swapfile"), 34359738368, &active)
            .expect("inactive swapfile should be reported");
        assert_eq!(dead.bytes, 34359738368);
        assert!(dead.needs_root, "/var/lib is root-owned");
        assert_eq!(dead.removal_command(), "sudo rm -f /var/lib/swapfile");
    }

    #[test]
    fn a_user_owned_swapfile_needs_no_sudo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("swapfile");
        std::fs::write(&path, b"x").expect("write");
        let dead = classify_swapfile(&path, 1, &[]).expect("reported");
        assert!(!dead.needs_root);
        assert!(dead.removal_command().starts_with("rm -f "));
    }

    #[test]
    fn backup_archives_are_recognised() {
        for name in [
            "dracon-platform-pre-slim-20260930.bundle",
            "junk-runner-current-tree.tar.gz",
            "holdout.tar.zst",
        ] {
            let item = classify_stray(Path::new(name), name, 10, false)
                .unwrap_or_else(|| panic!("{name} should be a backup archive"));
            assert_eq!(item.kind, StrayKind::BackupArchive);
        }
    }

    #[test]
    fn ordinary_files_are_not_strays() {
        for name in ["README.md", "Cargo.toml", "main.rs"] {
            assert!(classify_stray(Path::new(name), name, 10, false).is_none());
        }
    }

    #[test]
    fn scratch_and_backup_directories_are_recognised() {
        let cases = [
            ("hegemon-pre-filterrepo-backup-20260706.git", StrayKind::BackupRepo),
            ("dracon-platform-orphans", StrayKind::ScratchDir),
            ("dracon-lfs-proof", StrayKind::ScratchDir),
            ("eve-audit-new", StrayKind::ScratchDir),
            ("tmp-shield", StrayKind::ScratchDir),
            (".dracon-git-recovery-20260831-1610", StrayKind::ScratchDir),
        ];
        for (name, expected) in cases {
            let item = classify_stray(Path::new(name), name, 10, true)
                .unwrap_or_else(|| panic!("{name} should be a stray"));
            assert_eq!(item.kind, expected, "wrong kind for {name}");
        }
    }

    #[test]
    fn ordinary_directories_are_not_strays() {
        for name in ["Dev", "Documents", "Pictures", "Videos"] {
            assert!(
                classify_stray(Path::new(name), name, 10, true).is_none(),
                "{name} must never be reported as stray"
            );
        }
    }
}
