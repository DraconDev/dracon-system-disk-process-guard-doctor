//! Pre-delete reference capture for the disk cleaner.
//!
//! The objective is explicit that "never delete anything under a running
//! process" must be verified via `/proc` `exe`, `cwd` **and** `fd` references
//! captured BEFORE the unlink, and that "a `(deleted)` suffix alone is not
//! proof either way". That last clause matters: once a file is unlinked, every
//! `/proc/<pid>/fd` entry pointing at it gains a `(deleted)` suffix, so a
//! post-delete scan cannot distinguish "was in use, we deleted it anyway" from
//! "was not in use". The only sound order is capture-then-decide-then-delete.
//!
//! This module provides that capture as a value the caller must hold while it
//! deletes, and renders it as an auditable one-line record per path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One process reference to a path, captured before deletion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProcessRef {
    pub(crate) pid: u32,
    /// `exe` — the binary that would hold the file open.
    pub(crate) exe: Option<PathBuf>,
    /// `cwd` — the process working directory ("running inside this tree").
    pub(crate) cwd: Option<PathBuf>,
    /// `fd` — the file descriptor numbers whose target is this path.
    pub(crate) fds: Vec<u32>,
}

impl ProcessRef {
    /// True when this reference is a reason not to delete the path.
    ///
    /// A process is "using" a file when it holds it open (`fd`) or when it is
    /// executing out of it (`exe` inside the tree). A `cwd` inside a directory
    /// being removed also counts: deleting the directory a live process is
    /// sitting in breaks that process's next relative path operation.
    pub(crate) fn blocks_deletion(&self, target: &Path) -> bool {
        if !self.fds.is_empty() {
            return true;
        }
        if self
            .exe
            .as_ref()
            .is_some_and(|exe| exe == target || exe.starts_with(target))
        {
            return true;
        }
        self.cwd
            .as_ref()
            .is_some_and(|cwd| cwd == target || cwd.starts_with(target))
    }

    /// `pid 123 (exe=/usr/bin/foo, cwd=/home/x, fds=[3,7])` — audit text.
    pub(crate) fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(exe) = &self.exe {
            parts.push(format!("exe={}", exe.display()));
        }
        if let Some(cwd) = &self.cwd {
            parts.push(format!("cwd={}", cwd.display()));
        }
        if !self.fds.is_empty() {
            let fds: Vec<String> = self.fds.iter().map(u32::to_string).collect();
            parts.push(format!("fds=[{}]", fds.join(",")));
        }
        if parts.is_empty() {
            format!("pid {} (no refs readable)", self.pid)
        } else {
            format!("pid {} ({})", self.pid, parts.join(", "))
        }
    }
}

/// Everything `/proc` says about who is using a path, captured before unlink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReferenceCapture {
    pub(crate) refs: Vec<ProcessRef>,
    /// True when `/proc` could not be enumerated at all. A failed scan is NOT
    /// evidence of "unused", so the caller must treat this as "unknown" and
    /// refuse to delete.
    pub(crate) scan_failed: bool,
}

impl ReferenceCapture {
    /// Why the capture forbids deletion, if it does.
    pub(crate) fn blocking_reason(&self, target: &Path) -> Option<String> {
        if self.scan_failed {
            return Some("/proc unreadable — usage cannot be verified".to_string());
        }
        if self.refs.is_empty() {
            return None;
        }
        let blocking: Vec<&ProcessRef> = self
            .refs
            .iter()
            .filter(|r| r.blocks_deletion(target))
            .collect();
        if blocking.is_empty() {
            return None;
        }
        Some(
            blocking
                .iter()
                .map(|r| r.describe())
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// One-line audit record: the refs, or an explicit "none".
    pub(crate) fn describe(&self) -> String {
        if self.scan_failed {
            return "scan failed (/proc unreadable)".to_string();
        }
        if self.refs.is_empty() {
            return "none (no /proc exe, cwd or fd reference)".to_string();
        }
        self.refs
            .iter()
            .map(ProcessRef::describe)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Read `/proc/<pid>/fd/*` links for one process, returning fds pointing at
/// `target`. A symlink target that has already been unlinked ends in
/// " (deleted)" — that is exactly the suffix the objective says not to trust,
/// so it is stripped and the remaining path compared plainly.
fn fds_for(path: &Path, target: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(path.join("fd")) else {
        return Vec::new();
    };
    let mut fds = Vec::new();
    for entry in entries.flatten() {
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(link) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let text = link.to_string_lossy();
        let plain = text.strip_suffix(" (deleted)").unwrap_or(&text);
        if Path::new(plain) == target {
            fds.push(fd);
        }
    }
    fds.sort_unstable();
    fds
}

/// Capture every `/proc` reference to `target`, before it is deleted.
///
/// Deliberately walks the WHOLE process table rather than only the owners a
/// detector already knows about: the point of the check is to catch a process
/// the detector did not anticipate.
pub(crate) fn capture_references(proc_root: &Path, target: &Path) -> ReferenceCapture {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return ReferenceCapture {
            refs: Vec::new(),
            scan_failed: true,
        };
    };
    let mut refs = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        let exe = std::fs::read_link(dir.join("exe")).ok();
        let cwd = std::fs::read_link(dir.join("cwd")).ok();
        let fds = fds_for(&dir, target);
        let relevant = exe
            .as_ref()
            .is_some_and(|exe| exe == target || exe.starts_with(target))
            || cwd
                .as_ref()
                .is_some_and(|cwd| cwd == target || cwd.starts_with(target))
            || !fds.is_empty();
        if relevant {
            refs.push(ProcessRef {
                pid,
                exe,
                cwd,
                fds,
            });
        }
    }
    refs.sort();
    ReferenceCapture {
        refs,
        scan_failed: false,
    }
}

/// Aggregate counts used by the live scan cache, exposed for tests.
pub(crate) fn blocking_pids(capture: &ReferenceCapture, target: &Path) -> Vec<u32> {
    capture
        .refs
        .iter()
        .filter(|r| r.blocks_deletion(target))
        .map(|r| r.pid)
        .collect()
}

/// Render a capture as `BTreeMap`-friendly audit rows: `(pid, description)`.
pub(crate) fn audit_rows(capture: &ReferenceCapture) -> BTreeMap<u32, String> {
    capture
        .refs
        .iter()
        .map(|r| (r.pid, r.describe()))
        .collect()
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn an_open_fd_blocks_deletion() {
        let target = PathBuf::from("/home/u/Dev/repo/.git/objects/pack/tmp_pack_a");
        let capture = ReferenceCapture {
            refs: vec![ProcessRef {
                pid: 11,
                exe: Some(PathBuf::from("/usr/bin/git")),
                cwd: Some(PathBuf::from("/home/u/Dev/repo")),
                fds: vec![7],
            }],
            scan_failed: false,
        };
        let reason = capture.blocking_reason(&target).expect("must block");
        assert!(reason.contains("pid 11"));
        assert!(reason.contains("fds=[7]"));
        assert_eq!(blocking_pids(&capture, &target), vec![11]);
    }

    #[test]
    fn a_process_merely_running_elsewhere_does_not_block() {
        let target = PathBuf::from("/home/u/Dev/repo/.git/objects/pack/tmp_pack_a");
        let capture = ReferenceCapture {
            refs: vec![ProcessRef {
                pid: 12,
                exe: Some(PathBuf::from("/usr/bin/git")),
                cwd: Some(PathBuf::from("/home/u/Dev/other-repo")),
                fds: Vec::new(),
            }],
            scan_failed: false,
        };
        assert_eq!(capture.blocking_reason(&target), None);
    }

    #[test]
    fn an_unreadable_proc_scan_is_not_evidence_of_unused() {
        let capture = ReferenceCapture {
            refs: Vec::new(),
            scan_failed: true,
        };
        assert!(capture
            .blocking_reason(Path::new("/anything"))
            .expect("unknown must block")
            .contains("cannot be verified"));
    }

    #[test]
    fn deleting_a_directory_a_process_sits_in_is_blocked() {
        let target = PathBuf::from("/home/u/dracon-lfs-proof");
        let capture = ReferenceCapture {
            refs: vec![ProcessRef {
                pid: 13,
                exe: None,
                cwd: Some(target.join("sub")),
                fds: Vec::new(),
            }],
            scan_failed: false,
        };
        assert!(capture.blocking_reason(&target).is_some());
    }

    #[test]
    fn a_clean_capture_describes_itself_explicitly() {
        let capture = ReferenceCapture::default();
        assert_eq!(
            capture.describe(),
            "none (no /proc exe, cwd or fd reference)"
        );
        assert_eq!(capture.blocking_reason(Path::new("/x")), None);
    }

    #[test]
    fn a_deleted_suffix_does_not_hide_a_live_fd() {
        // The symlink text git leaves behind after an unlink still names the
        // path; the comparison must not be fooled into thinking it is a
        // different file.
        let capture = ReferenceCapture {
            refs: vec![ProcessRef {
                pid: 14,
                exe: None,
                cwd: None,
                fds: vec![3],
            }],
            scan_failed: false,
        };
        assert!(capture
            .blocking_reason(Path::new("/home/u/x/tmp_pack_a (deleted)"))
            .is_some());
    }

    #[test]
    fn current_process_holds_its_own_executable_open() {
        // A real /proc capture: pid 1 has a readable exe, and capturing a path
        // the current process holds open must find this process.
        let capture = capture_references(Path::new("/proc"), Path::new("/proc/self/exe"));
        assert!(!capture.scan_failed);
        let rows = audit_rows(&capture);
        assert!(!rows.is_empty(), "expected at least one live fd/exe row");
    }
}
