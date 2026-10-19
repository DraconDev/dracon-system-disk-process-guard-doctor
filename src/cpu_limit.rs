//! CPU leases are owned by the user manager, never by cgroup filesystem writes.
//! A scope has no timeout/stop job: systemd would kill its workloads on stop.
//! An independent timer releases the quota after a bounded lease even if the
//! guard crashes between moving the PID and persisting its mitigation ledger.
use std::{path::Path, time::Duration};
use tokio::process::Command;

const MANAGER: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";

async fn output(bin: &str, args: &[String]) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await.map_err(|_| format!("{bin} timed out"))?
        .map_err(|e| format!("{bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{bin}: {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn manager(method: &str, signature: &str, values: &[String]) -> Result<String, String> {
    let bin = crate::resolve_bin_strict("busctl").map_err(|e| e.to_string())?;
    let mut args: Vec<String> = ["--user", "--timeout=10s", "call", MANAGER, MANAGER_PATH,
        MANAGER, method, signature].iter().map(|s| s.to_string()).collect();
    args.extend_from_slice(values);
    output(&bin, &args).await
}

async fn control_group(unit: &str) -> Result<String, String> {
    let bin = crate::resolve_bin_strict("systemctl").map_err(|e| e.to_string())?;
    output(&bin, &["--user".into(), "show".into(), unit.into(), "-p".into(), "ControlGroup".into(), "--value".into()]).await
}

fn original_unit(path: &str) -> Result<&str, String> {
    let uid = unsafe { libc::geteuid() };
    let path = path.trim_start_matches('/');
    let prefix = format!("user.slice/user-{uid}.slice/user@{uid}.service/");
    if !path.starts_with(&prefix) || path.split('/').any(|s| s == ".." || s == "." || s.is_empty()) {
        return Err(format!("original cgroup is outside the user manager: {path}"));
    }
    let unit = path.rsplit('/').next().unwrap_or("");
    if !(unit.ends_with(".service") || unit.ends_with(".scope")) {
        return Err(format!("original cgroup is not a managed service/scope: {path}"));
    }
    Ok(unit)
}

pub(crate) fn is_lease(unit: &str) -> bool {
    unit.starts_with("dracon-cpu-") && unit.ends_with(".scope")
        && unit.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

async fn attach(unit: &str, pids: &[i32]) -> Result<(), String> {
    if pids.is_empty() { return Ok(()); }
    let mut args = vec![unit.into(), "".into(), pids.len().to_string()];
    args.extend(pids.iter().map(ToString::to_string));
    manager("AttachProcessesToUnit", "ssau", &args).await.map(|_| ())
}

async fn clear_quota(unit: &str) -> Result<(), String> {
    // CPUQuotaPerSecUSec=UINT64_MAX is systemd's infinity value.
    manager("SetUnitProperties", "sba(sv)", &[unit.into(), "true".into(), "1".into(),
        "CPUQuotaPerSecUSec".into(), "t".into(), u64::MAX.to_string()]).await.map(|_| ())
}

fn group_members(group: &str) -> Result<Vec<i32>, String> {
    if !group.starts_with("/user.slice/") || group.split('/').any(|s| s == "..") {
        return Err("invalid manager cgroup path".into());
    }
    let text = std::fs::read_to_string(Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/')).join("cgroup.procs"))
        .map_err(|e| format!("read scope members: {e}"))?;
    text.lines().map(|s| s.parse::<i32>().map_err(|e| e.to_string())).collect()
}

pub(crate) async fn cap(pid: i32, percent: u32) -> Result<(String, String), String> {
    if !(1..=100).contains(&percent) { return Err("invalid CPU quota".into()); }
    let identity = crate::read_proc_identity(Path::new("/proc"), pid)
        .map_err(|e| format!("process {pid} unavailable: {e}"))?;
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e| e.to_string())?;
    let orig = crate::parse_cgroup_rel_path(&cg).ok_or("invalid process cgroup")?.to_string();
    original_unit(&orig)?;
    let unit = format!("dracon-cpu-{pid}-{}.scope", identity.starttime);
    let expiry = format!("{}-expiry", unit.trim_end_matches(".scope"));
    // Arm recovery BEFORE starting the scope. No PID is moved if this fails.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let run = crate::resolve_bin_strict("systemd-run").map_err(|e| e.to_string())?;
    output(&run, &["--user".into(), "--collect".into(), format!("--unit={expiry}"),
        "--on-active=1h".into(), "--timer-property=AccuracySec=1s".into(),
        "--".into(), exe.to_string_lossy().into_owned(), "cpu-lease-release".into(), unit.clone(), orig.clone()]).await?;
    if !matches!(crate::process_identity_status(Path::new("/proc"), pid, &identity), crate::ProcessIdentityStatus::Match) {
        cancel_expiry(&unit).await;
        return Err("process identity changed before CPU cap".into());
    }
    let result = manager("StartTransientUnit", "ssa(sv)a(sa(sv))", &[
        unit.clone(), "fail".into(), "3".into(),
        "PIDs".into(), "au".into(), "1".into(), pid.to_string(),
        "CPUQuotaPerSecUSec".into(), "t".into(), (u64::from(percent) * 10_000).to_string(),
        "Description".into(), "s".into(), "Dracon reversible CPU lease".into(), "0".into(),
    ]).await;
    if let Err(e) = result {
        // A timeout is ambiguous: the manager may have moved the process.
        // Keep the already-armed expiry; never stop a possibly occupied scope.
        return Err(e);
    }
    Ok((unit, orig))
}

async fn cancel_expiry(unit: &str) {
    if let Ok(bin) = crate::resolve_bin_strict("systemctl") {
        let timer = format!("{}-expiry.timer", unit.trim_end_matches(".scope"));
        let _ = output(&bin, &["--user".into(), "stop".into(), timer]).await;
    }
}

pub(crate) async fn release(unit: &str, orig: &str) -> Result<(), String> {
    if !is_lease(unit) { return Err("not a Dracon CPU lease".into()); }
    let target = original_unit(orig)?;
    let group = control_group(unit).await?;
    if group.is_empty() { return Ok(()); }
    // Release quota FIRST. A failed migration must not leave an indefinite cap.
    clear_quota(unit).await?;
    // Migrate all inherited members too. Never stop the scope: a fork racing
    // the read/move can remain there, but it remains alive and uncapped.
    for _ in 0..3 {
        let members = match group_members(&group) {
            Ok(members) => members,
            Err(_) if !Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/')).exists() => break,
            Err(e) => return Err(e),
        };
        if members.is_empty() { break; }
        if let Err(e) = attach(target, &members).await {
            // One member may have exited between enumeration and the call.
            let live: Vec<_> = members.into_iter().filter(|pid| Path::new(&format!("/proc/{pid}")).exists()).collect();
            if !live.is_empty() { return Err(e); }
        }
    }
    if let Ok(members) = group_members(&group) {
        if !members.is_empty() { return Err("uncapped scope still has members; retaining recovery state".into()); }
    }
    cancel_expiry(unit).await;
    Ok(())
}

/// Migrate a legacy service member through the manager as well. Never write
/// cgroup.procs from the sandbox, and never fall back to a guessed cgroup.
pub(crate) async fn restore_legacy(pid: i32, orig: &str) -> Result<(), String> {
    attach(original_unit(orig)?, &[pid]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restoration_never_guesses_or_escapes_manager_ownership() {
        let uid = unsafe { libc::geteuid() };
        let valid = format!("user.slice/user-{uid}.slice/user@{uid}.service/app.slice/editor.scope");
        assert_eq!(original_unit(&valid).unwrap(), "editor.scope");
        for invalid in ["", "user.slice", "../../editor.scope", "system.slice/editor.service", "user.slice/user-999999.slice/user@999999.service/a.scope"] {
            assert!(original_unit(invalid).is_err());
        }
        assert!(original_unit(&valid.replace("app.slice", "..")).is_err());
    }
    #[test]
    fn recovery_only_accepts_guard_owned_scope_names() {
        assert!(is_lease("dracon-cpu-123-456.scope"));
        for invalid in ["editor.scope", "dracon-cpu-1.service", "dracon-cpu-../editor.scope", "dracon-cpu-;rm.scope"] {
            assert!(!is_lease(invalid));
        }
    }
}
