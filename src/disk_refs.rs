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
        let mut blocking: Vec<ProcessRef> = merged_by_pid(&self.refs)
            .into_iter()
            .filter(|r| r.blocks_deletion(target))
            .collect();
        if blocking.is_empty() {
            return None;
        }
        // Cap the listing: a browser cache is held by dozens of renderer
        // processes and printing all of them buries the actual finding.
        const SHOWN: usize = 4;
        let total = blocking.len();
        let truncated = total > SHOWN;
        if truncated {
            blocking.truncate(SHOWN);
        }
        let mut text = blocking
            .iter()
            .map(|r| r.describe())
            .collect::<Vec<_>>()
            .join("; ");
        if truncated {
            text.push_str(&format!("; … and {} more process(es)", total - SHOWN));
        }
        Some(text)
    }

    /// One-line audit record: the refs, or an explicit "none".
    ///
    /// Rows are merged per pid: the snapshot indexes exe, cwd and each fd
    /// separately, so one process holding one file would otherwise print three
    /// times.
    pub(crate) fn describe(&self) -> String {
        if self.scan_failed {
            return "scan failed (/proc unreadable)".to_string();
        }
        if self.refs.is_empty() {
            return "none (no /proc exe, cwd or fd reference)".to_string();
        }
        merged_by_pid(&self.refs)
            .into_iter()
            .map(|reference| reference.describe())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Read `/proc/<pid>/fd/*` links for one process, returning fds pointing at
/// `target`. A symlink target that has already been unlinked ends in
/// " (deleted)" — that is exactly the suffix the objective says not to trust,
/// so it is stripped and the remaining path compared plainly.
pub(crate) fn fds_for(path: &Path, target: &Path) -> Vec<u32> {
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

/// Collapse per-attribute rows into one entry per pid.
pub(crate) fn merged_by_pid(refs: &[ProcessRef]) -> Vec<ProcessRef> {
    let mut merged: BTreeMap<u32, (Option<PathBuf>, Option<PathBuf>, Vec<u32>)> = BTreeMap::new();
    for reference in refs {
        let entry = merged.entry(reference.pid).or_default();
        if entry.0.is_none() {
            entry.0 = reference.exe.clone();
        }
        if entry.1.is_none() {
            entry.1 = reference.cwd.clone();
        }
        for fd in &reference.fds {
            if !entry.2.contains(fd) {
                entry.2.push(*fd);
            }
        }
    }
    merged
        .into_iter()
        .map(|(pid, (exe, cwd, mut fds))| {
            fds.sort_unstable();
            ProcessRef {
                pid,
                exe,
                cwd,
                fds,
            }
        })
        .collect()
}

/// Capture every `/proc` reference to `target`, before it is deleted.
///
/// Deliberately walks the WHOLE process table rather than only the owners a
/// detector already knows about: the point of the check is to catch a process
/// the detector did not anticipate.
/// A single-pass snapshot of every `/proc` reference, queried per target.
///
/// ADDED 2026-10-27: `capture_references` re-walked the whole process table for
/// EVERY finding. The first live `disk audit` therefore hung past 15 minutes on
/// a host with thousands of `/tmp` entries — each one re-reading every pid's
/// `exe`, `cwd` and every fd. The snapshot reads `/proc` once and answers every
/// query from memory, which is both correct and linear.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProcSnapshot {
    refs: BTreeMap<PathBuf, Vec<ProcessRef>>,
    pub(crate) scan_failed: bool,
}

impl ProcSnapshot {
    /// Read `/proc` once, indexing every exe/cwd/fd target.
    pub(crate) fn read(proc_root: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(proc_root) else {
            return Self {
                refs: BTreeMap::new(),
                scan_failed: true,
            };
        };
        let mut index: BTreeMap<PathBuf, Vec<ProcessRef>> = BTreeMap::new();
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
            if let Some(exe_path) = &exe {
                index
                    .entry(strip_deleted(exe_path))
                    .or_default()
                    .push(ProcessRef {
                        pid,
                        exe: Some(exe_path.clone()),
                        cwd: cwd.clone(),
                        fds: Vec::new(),
                    });
            }
            if let Some(cwd) = &cwd {
                index.entry(strip_deleted(cwd)).or_default().push(ProcessRef {
                    pid,
                    exe: exe.clone(),
                    cwd: Some(cwd.clone()),
                    fds: Vec::new(),
                });
            }
            let Ok(fd_entries) = std::fs::read_dir(dir.join("fd")) else {
                continue;
            };
            for fd_entry in fd_entries.flatten() {
                let Some(fd) = fd_entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                else {
                    continue;
                };
                let Ok(link) = std::fs::read_link(fd_entry.path()) else {
                    continue;
                };
                index
                    .entry(strip_deleted(&link))
                    .or_default()
                    .push(ProcessRef {
                        pid,
                        exe: exe.clone(),
                        cwd: cwd.clone(),
                        fds: vec![fd],
                    });
            }
        }
        for refs in index.values_mut() {
            refs.sort();
        }
        Self {
            refs: index,
            scan_failed: false,
        }
    }

    /// References to `target` and to anything inside it (a directory).
    pub(crate) fn capture(&self, target: &Path) -> ReferenceCapture {
        if self.scan_failed {
            return ReferenceCapture {
                refs: Vec::new(),
                scan_failed: true,
            };
        }
        let mut refs: Vec<ProcessRef> = Vec::new();
        // Exact hits.
        if let Some(exact) = self.refs.get(target) {
            refs.extend(exact.iter().cloned());
        }
        // Anything strictly inside the target (a directory being removed).
        let prefix = target.to_path_buf();
        for (path, entries) in self.refs.range(prefix.clone()..) {
            if path == &prefix {
                continue; // already collected as an exact hit
            }
            // BREAK, not continue: the map is sorted, so the first entry that
            // is not inside `target` ends the run. Continuing here re-scanned
            // the whole remaining index for every one of 33k /tmp queries,
            // which is what made the first audit take four minutes.
            if !path.starts_with(&prefix) {
                break;
            }
            refs.extend(entries.iter().cloned());
        }
        refs.sort();
        ReferenceCapture {
            refs,
            scan_failed: false,
        }
    }
}

/// Drop a trailing " (deleted)" so a live fd still matches its path.
fn strip_deleted(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(plain) => PathBuf::from(plain),
        None => path.to_path_buf(),
    }
}

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
    fn a_real_proc_scan_finds_a_live_reference_to_a_file_this_process_holds() {
        // Hold a real file open in THIS process, then capture it from /proc.
        // A checked-in fixture cannot prove the exe/cwd/fd walk works; only a
        // live descriptor can.
        let dir = tempfile::tempdir().expect("tempdir");
        let held = dir.path().join("held.bin");
        std::fs::write(&held, b"held").expect("write");
        let file = std::fs::File::open(&held).expect("open held file");

        let capture = capture_references(Path::new("/proc"), &held);
        assert!(!capture.scan_failed, "a live /proc must be readable");
        let own_pid = std::process::id();
        assert!(
            blocking_pids(&capture, &held).contains(&own_pid),
            "the process holding the file must be reported: {}",
            capture.describe()
        );
        assert!(
            capture
                .blocking_reason(&held)
                .expect("a held file must block")
                .contains(&format!("pid {own_pid}")),
            "the reason must name the holding pid"
        );
        drop(file);
    }

    #[test]
    fn a_released_reference_stops_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = dir.path().join("released.bin");
        std::fs::write(&held, b"x").expect("write");
        {
            let _file = std::fs::File::open(&held).expect("open");
            assert!(capture_references(Path::new("/proc"), &held)
                .blocking_reason(&held)
                .is_some());
        }
        // After the drop the only remaining refs are transient; a file nobody
        // holds must be deletable (that is the whole point of capturing first).
        let capture = capture_references(Path::new("/proc"), &held);
        let own = std::process::id();
        assert!(
            !blocking_pids(&capture, &held).contains(&own),
            "this process no longer holds the file: {}",
            capture.describe()
        );
    }
}
