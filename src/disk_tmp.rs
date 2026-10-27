//! `/tmp`, `/var/tmp` and oversized/rotated log detection for the disk cleaner.
//!
//! Objective class 2: "`/tmp` and `/var/tmp` cruft, oversized/rotated logs".
//! On this host `/tmp` alone held 9.8 GiB that the cleaner never reported,
//! because those scans existed only inside the guard *daemon* path and were
//! unreachable from a one-shot cleanup command.
//!
//! Safety: the guard daemon already owns an age floor for `/tmp`
//! (`tmp_e2e_min_age_hours`). This module reuses that knob rather than
//! inventing a second one, and it never touches a path held open by a live
//! process — the caller captures `disk_refs` references before deciding.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A `/tmp`/`/var/tmp` entry old enough to be cruft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TempEntry {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) is_dir: bool,
    pub(crate) age_hours: u64,
}

impl TempEntry {
    pub(crate) fn label(&self) -> &'static str {
        if self.is_dir {
            "tmp-dir"
        } else {
            "tmp-file"
        }
    }
}

/// A log file that is oversized and/or part of a rotated family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OversizedLog {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) rotated: bool,
    pub(crate) age_hours: u64,
}

impl OversizedLog {
    pub(crate) fn reason(&self) -> String {
        if self.rotated {
            format!("rotated log, {} old", human_age(self.age_hours))
        } else {
            format!(
                "active log over {} MiB",
                OVERSIZED_LOG_BYTES / (1024 * 1024)
            )
        }
    }
}

/// Logs above this size are reported even when they are not rotated.
pub(crate) const OVERSIZED_LOG_BYTES: u64 = 256 * 1024 * 1024;

fn human_age(hours: u64) -> String {
    if hours >= 48 {
        format!("{} days", hours / 24)
    } else {
        format!("{hours}h")
    }
}

/// Age in whole hours, or `None` when the mtime cannot be read.
pub(crate) fn age_hours(modified: SystemTime, now: SystemTime) -> Option<u64> {
    now.duration_since(modified)
        .ok()
        .map(|delta| delta.as_secs() / 3600)
}

/// Whether a filename looks like a rotated log (`app.log.1`, `app.log.2.gz`,
/// `app-20261026.log`).
pub(crate) fn is_rotated_log_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.contains(".log") {
        return false;
    }
    // app.log.1 / app.log.2.gz
    if lower.rsplit_once(".log").is_some_and(|(_, rest)| {
        let rest = rest.trim_end_matches(".gz").trim_end_matches(".xz");
        !rest.is_empty() && rest.trim_start_matches('.').chars().all(|c| c.is_ascii_digit())
    }) {
        return true;
    }
    // app-20261026.log / app.log-20261026
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part.len() == 8 && part.chars().all(|c| c.is_ascii_digit()))
}

/// Directories scanned for `/tmp`-style cruft, in order.
pub(crate) const TEMP_ROOTS: [&str; 2] = ["/tmp", "/var/tmp"];

/// Directories scanned for oversized/rotated logs.
pub(crate) const LOG_ROOTS: [&str; 2] = ["/var/log", "/home/dracon/.dracon/logs"];

/// Collect `/tmp`/`/var/tmp` entries older than `min_age_hours`.
///
/// Only the top level is considered: `/tmp` children are the unit of cleanup
/// (a source tree in `/tmp/foo` is removed entirely), and descending into them
/// would report thousands of individual files for one logical target.
pub(crate) fn find_temp_cruft(
    roots: &[&str],
    min_age_hours: u64,
    now: SystemTime,
) -> Vec<TempEntry> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // Never follow or report symlinks: removing one reclaims nothing
            // and can point anywhere.
            if metadata.file_type().is_symlink() {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Some(hours) = age_hours(modified, now) else {
                continue;
            };
            if hours < min_age_hours {
                continue;
            }
            found.push(TempEntry {
                path,
                // Bytes are filled in after the loop, in parallel.
                bytes: if metadata.is_dir() { 0 } else { metadata.len() },
                is_dir: metadata.is_dir(),
                age_hours: hours,
            });
        }
    }
    // Size the directories in parallel: serial lstat over 33k entries was the
    // single largest cost of the whole audit.
    let dir_paths: Vec<PathBuf> = found
        .iter()
        .filter(|entry| entry.is_dir)
        .map(|entry| entry.path.clone())
        .collect();
    let sizes = directory_bytes_parallel(&dir_paths);
    let mut sizes = sizes.into_iter();
    for entry in found.iter_mut().filter(|entry| entry.is_dir) {
        entry.bytes = sizes.next().unwrap_or(0);
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found
}

/// Collect oversized and rotated logs under `roots` (one level deep, plus a
/// `*.log` fanout for rotated families).
pub(crate) fn find_oversized_logs(
    roots: &[&str],
    min_age_hours: u64,
    now: SystemTime,
) -> Vec<OversizedLog> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let rotated = is_rotated_log_name(&name);
            let bytes = metadata.len();
            if !rotated && bytes < OVERSIZED_LOG_BYTES {
                continue;
            }
            let age = metadata
                .modified()
                .ok()
                .and_then(|modified| age_hours(modified, now))
                .unwrap_or(0);
            // A rotated log is only cruft once it is genuinely old; a fresh
            // rotation is the live log one rename ago.
            if rotated && age < min_age_hours {
                continue;
            }
            found.push(OversizedLog {
                path,
                bytes,
                rotated,
                age_hours: age,
            });
        }
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found
}

/// Size many directories in parallel.
///
/// ADDED 2026-10-27: a serial walk of `/var/tmp` (33k entries, ~2 GiB) took 96
/// seconds on this host even though `du -sb` needs similar time — the cost is
/// per-entry `lstat`, not bytes. Parallelising across directories is the only
/// lever that helps a stat-bound walk, and it cut the full audit from 244s to
/// interactive territory.
pub(crate) fn directory_bytes_parallel(roots: &[PathBuf]) -> Vec<u64> {
    if roots.is_empty() {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out = vec![0u64; roots.len()];
    // Each worker writes only its own slots, so the result vector can be split
    // into disjoint chunks per worker without locking.
    let chunk_size = roots.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let mut rest = out.as_mut_slice();
        let mut remaining = roots;
        while !rest.is_empty() {
            let take = chunk_size.min(rest.len());
            let (mine, tail) = rest.split_at_mut(take);
            let (roots_mine, roots_tail) = remaining.split_at(take);
            let next = &next;
            scope.spawn(move || {
                for (slot, root) in mine.iter_mut().zip(roots_mine.iter()) {
                    *slot = directory_bytes(root);
                }
                let _ = next;
            });
            rest = tail;
            remaining = roots_tail;
        }
    });
    out
}

/// Total allocated bytes of a directory tree, without following symlinks.
///
/// Bounded: it stops counting past `limit` entries so a pathological tree
/// cannot stall a report.
pub(crate) fn directory_bytes(root: &Path) -> u64 {
    const LIMIT: usize = 200_000;
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > LIMIT {
                return total;
            }
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tmp_log_tests {
    use super::*;
    use std::time::Duration;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn write(path: &Path, bytes: usize, age_hours: u64) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, vec![b'x'; bytes]).expect("write");
        let when = now() - Duration::from_secs(age_hours * 3600 + 60);
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open");
        file.set_modified(when).expect("set mtime");
    }

    #[test]
    fn only_entries_past_the_age_floor_are_cruft() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("old.txt"), 10, 48);
        write(&root.join("fresh.txt"), 10, 1);

        let found = find_temp_cruft(&[root.to_str().expect("utf8")], 24, now());
        let names: Vec<String> = found
            .iter()
            .map(|e| e.path.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["old.txt".to_string()]);
        assert_eq!(found[0].bytes, 10);
        assert!(!found[0].is_dir);
    }

    #[test]
    fn symlinks_are_never_reported_because_removing_them_reclaims_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("real.txt"), 5, 48);
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link")).expect("symlink");

        let found = find_temp_cruft(&[root.to_str().expect("utf8")], 1, now());
        let names: Vec<String> = found
            .iter()
            .map(|e| e.path.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["real.txt".to_string()]);
    }

    #[test]
    fn a_directory_reports_the_size_of_its_whole_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("build/one.bin"), 100, 48);
        write(&root.join("build/nested/two.bin"), 200, 48);
        let build = root.join("build");
        // Age the directory itself: its mtime is what find_temp_cruft gates on.
        let aged = now() - Duration::from_secs(48 * 3600 + 60);
        std::fs::File::options()
            .read(true)
            .open(&build)
            .expect("open dir")
            .set_modified(aged)
            .expect("set dir mtime");

        let found = find_temp_cruft(&[root.to_str().expect("utf8")], 24, now());
        assert_eq!(found.len(), 1);
        assert!(found[0].is_dir);
        assert_eq!(found[0].bytes, 300, "directory size is the whole tree");
    }

    #[test]
    fn rotated_log_families_are_recognised() {
        assert!(is_rotated_log_name("app.log.1"));
        assert!(is_rotated_log_name("app.log.2.gz"));
        assert!(is_rotated_log_name("app-20261026.log"));
        assert!(!is_rotated_log_name("app.log"), "the live log is not rotated");
        assert!(!is_rotated_log_name("notes.txt"));
    }

    #[test]
    fn a_fresh_rotation_is_not_reported_but_an_old_one_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("app.log.1"), 10, 1);
        write(&root.join("app.log.2"), 10, 72);

        let found = find_oversized_logs(&[root.to_str().expect("utf8")], 24, now());
        let names: Vec<String> = found
            .iter()
            .map(|e| e.path.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["app.log.2".to_string()],
            "the live rotation must survive: it became the current log one rename ago"
        );
        assert!(found[0].rotated);
    }

    #[test]
    fn an_oversized_live_log_is_reported_even_when_not_rotated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let path = root.join("huge.log");
        std::fs::create_dir_all(root).expect("mkdir");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(OVERSIZED_LOG_BYTES + 1).expect("sparse set_len");
        drop(file);

        let found = find_oversized_logs(&[root.to_str().expect("utf8")], 24, now());
        assert_eq!(found.len(), 1);
        assert!(!found[0].rotated);
        assert!(found[0].reason().contains("active log over"));
    }

    #[test]
    fn a_small_recent_log_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("small.log"), 100, 0);
        assert!(find_oversized_logs(&[root.to_str().expect("utf8")], 24, now()).is_empty());
    }
}
