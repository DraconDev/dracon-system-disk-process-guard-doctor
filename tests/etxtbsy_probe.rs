// TEMPORARY DIAGNOSTIC — deleted once the ETXTBSY holder is identified.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn holders_of(path: &Path) -> Vec<String> {
    let target = fs::read_link(path).unwrap_or_else(|_| path.to_path_buf());
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let pid = entry.file_name().to_string_lossy().to_string();
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let fd_dir = PathBuf::from("/proc").join(&pid).join("fd");
        for fd in fs::read_dir(&fd_dir).into_iter().flatten().flatten() {
            if let Ok(link) = fs::read_link(fd.path()) {
                if link == target || link == path {
                    let flags = fs::read_to_string(fd.path().join("../fdinfo"))
                        .unwrap_or_default();
                    let open_flags = flags
                        .lines()
                        .find(|l| l.starts_with("flags:"))
                        .unwrap_or("flags:?")
                        .to_string();
                    let cmd = fs::read_to_string(format!("/proc/{pid}/cmdline"))
                        .unwrap_or_default()
                        .replace('\0', " ");
                    found.push(format!("pid={pid} fd={:?} {open_flags} cmd={cmd}", fd.file_name()));
                }
            }
        }
    }
    found
}

fn spawn_fixture(path: &Path) -> Result<(), std::io::Error> {
    Command::new(path).output().map(|_| ())
}

fn one_worker(tag: &str, iters: u64, etxtbsy: &AtomicU64) {
    for i in 0..iters {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("probe_{tag}_{}_{}_{}", std::process::id(), nanos, i));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("renice");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        if let Err(e) = spawn_fixture(&script) {
            if e.raw_os_error() == Some(26) {
                etxtbsy.fetch_add(1, Ordering::SeqCst);
                eprintln!("ETXTBSY on {script:?}: holders={:?}", holders_of(&script));
            } else {
                eprintln!("other error on {script:?}: {e}");
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn probe_etxtbsy_single_thread() {
    let n = AtomicU64::new(0);
    one_worker("single", 2000, &n);
    eprintln!("single-thread etxtbsy={}", n.load(Ordering::SeqCst));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[test]
fn probe_etxtbsy_four_threads() {
    let n: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(0)));
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let n = n;
            std::thread::spawn(move || one_worker(&format!("t{t}"), 1500, n))
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    eprintln!("4-thread etxtbsy={}", n.load(Ordering::SeqCst));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
