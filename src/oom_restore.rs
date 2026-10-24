//! Compare-and-restore OOM steering without overwriting an external policy.
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) fn restore(
    proc_root: &Path,
    pid: i32,
    identity: &crate::ProcessIdentity,
    original: i32,
) -> Result<bool, String> {
    let path = proc_root.join(pid.to_string()).join("oom_score_adj");
    let describe = |stage: &str, error: std::io::Error| {
        #[cfg(unix)]
        let owner = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path)
                .map(|m| format!("uid={} mode={:o}", m.uid(), m.mode() & 0o777))
                .unwrap_or_else(|_| "metadata unavailable".into())
        };
        #[cfg(not(unix))]
        let owner = "metadata unavailable";
        format!(
            "{stage} {} starttime={} original={} ({owner}): {error}",
            path.display(),
            identity.starttime,
            original
        )
    };
    // Open once, then revalidate incarnation. An old proc fd cannot address a
    // replacement process if this PID disappears/recycles between operations.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| describe("open", e))?;
    if !matches!(
        crate::process_identity_status(proc_root, pid, identity),
        crate::ProcessIdentityStatus::Match
    ) {
        return Err("OOM restoration identity unavailable or changed".into());
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| describe("read", e))?;
    let current = text
        .trim()
        .parse::<i32>()
        .map_err(|e| format!("invalid oom_score_adj: {e}"))?;
    if current != crate::OOM_BIAS_TARGET {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| describe("seek", e))?;
    let value = format!("{original}\n");
    file.write_all(value.as_bytes())
        .map_err(|e| describe("write", e))?;
    // Real proc files report length zero. Ordinary-file test fixtures need
    // trailing bytes removed when e.g. 250 is restored to 25.
    if file.metadata().map_err(|e| describe("stat", e))?.len() > 0 {
        file.set_len(value.len() as u64)
            .map_err(|e| describe("truncate fixture", e))?;
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| describe("seek verify", e))?;
    text.clear();
    file.read_to_string(&mut text)
        .map_err(|e| describe("read verify", e))?;
    if text.trim().parse::<i32>().ok() != Some(original) {
        return Err("OOM restore verification failed; obligation retained".into());
    }
    Ok(true)
}
