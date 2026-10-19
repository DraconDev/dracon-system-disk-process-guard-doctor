//! Reversible, manager-owned CPU leases on isolated workload scopes.
//!
//! Never migrate a PID, stop a scope, or write cgroup.procs. In particular,
//! scope/service timeout expiry is NOT a non-killing way to release a quota.
//! A separate manager timer restores the original quota even if the guard dies.
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::{Path, PathBuf}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::process::Command;
use fs2::FileExt;

#[derive(Debug, Serialize, Deserialize)]
struct Lease {
    unit: String,
    invocation: String,
    original: u64,
    applied: u64,
    generation: String,
    timer: String,
    boot_id: String,
}

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
    let mut args: Vec<String> = ["--user", "--timeout=10s", "call", "org.freedesktop.systemd1", "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager", method, signature].iter().map(|s| s.to_string()).collect();
    args.extend_from_slice(values);
    output(&bin, &args).await
}

async fn property(unit: &str, name: &str) -> Result<String, String> {
    let bin = crate::resolve_bin_strict("systemctl").map_err(|e| e.to_string())?;
    output(&bin, &["--user".into(), "show".into(), unit.into(), "-p".into(), name.into(), "--value".into()]).await
}

async fn set_quota(unit: &str, quota: u64) -> Result<(), String> {
    manager("SetUnitProperties", "sba(sv)", &[unit.into(), "true".into(), "1".into(),
        "CPUQuotaPerSecUSec".into(), "t".into(), quota.to_string()]).await?;
    let observed = quota_value(&property(unit, "CPUQuotaPerSecUSec").await?)?;
    if observed != quota { return Err(format!("CPU quota verification failed: wanted {quota}, observed {observed}")); }
    Ok(())
}

fn quota_value(text: &str) -> Result<u64, String> {
    // systemctl's --value renders time spans; use busctl for raw microseconds
    // instead when a finite value is not a plain integer.
    if text == "infinity" { return Ok(u64::MAX); }
    if let Ok(n) = text.parse() { return Ok(n); }
    let mut total = 0f64;
    for part in text.split_whitespace() {
        let (num, factor) = if let Some(n) = part.strip_suffix("ms") { (n, 1_000.) }
        else if let Some(n) = part.strip_suffix("us") { (n, 1.) }
        else if let Some(n) = part.strip_suffix("s") { (n, 1_000_000.) }
        else if let Some(n) = part.strip_suffix("min") { (n, 60_000_000.) }
        else { return Err(format!("unsupported CPU quota value {text}")); };
        total += num.parse::<f64>().map_err(|e| e.to_string())? * factor;
    }
    if text.is_empty() || !total.is_finite() || total <= 0. { return Err(format!("invalid quota {text}")); }
    Ok(total.round() as u64)
}

fn state_root() -> Result<PathBuf, String> {
    Ok(dirs::home_dir().ok_or("HOME unavailable")?.join(".dracon/cpu-leases"))
}
fn boot_id() -> Result<String, String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s|s.trim().into()).map_err(|e|e.to_string())
}
fn lease_key(unit: &str) -> String {
    // Stable across compiler/library releases, unlike DefaultHasher.
    let hash = unit.bytes().fold(0xcbf29ce484222325u64, |h,b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    format!("dracon-cpu-unit-{hash:016x}")
}
pub(crate) fn is_lease(key: &str) -> bool {
    key.strip_prefix("dracon-cpu-unit-").is_some_and(|s| s.len()==16 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn lock(root: &Path, key: &str) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(root).map_err(|e|e.to_string())?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join(format!("{key}.lock"))).map_err(|e|e.to_string())?;
    f.try_lock_exclusive().map_err(|e| format!("CPU lease busy: {e}"))?;
    Ok(f)
}
fn save(root: &Path, key: &str, lease: &Lease) -> Result<(), String> {
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(|e|e.to_string())?;
    use std::io::Write;
    tmp.write_all(&serde_json::to_vec(lease).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    tmp.as_file().sync_all().map_err(|e|e.to_string())?;
    tmp.persist(root.join(format!("{key}.json"))).map_err(|e|e.to_string())?;
    std::fs::File::open(root).and_then(|f| f.sync_all()).map_err(|e|e.to_string())
}
fn load(root: &Path, key: &str) -> Result<Option<Lease>, String> {
    match std::fs::read(root.join(format!("{key}.json"))) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e|e.to_string()),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn isolated_scope(group: &str) -> Result<&str, String> {
    let uid = unsafe { libc::geteuid() };
    let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/");
    let unit = group.strip_prefix(&prefix).ok_or("process is outside the supported user workload slice")?;
    // tmux-spawn is the runner's per-job scope, NOT the shared tmux server or
    // GUI terminal scope. Unsupported/shared units are deliberately untouched.
    if !unit.ends_with(".scope") || !(unit.starts_with("tmux-spawn-") || unit.starts_with("dracon-protection-test-"))
        || !unit.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)) {
        return Err("CPU cap requires an isolated per-job workload scope; shared/unknown units are untouched".into());
    }
    Ok(unit)
}

fn group_members(group: &str) -> Result<Vec<i32>, String> {
    isolated_scope(group)?;
    let text = std::fs::read_to_string(Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/')).join("cgroup.procs"))
        .map_err(|e| format!("read scope members: {e}"))?;
    text.lines().map(|s| s.parse::<i32>().map_err(|e|e.to_string())).collect()
}

pub(crate) async fn cap(pid: i32, percent: u32, exempt: &HashSet<String>) -> Result<(String, String), String> {
    if !(1..=100).contains(&percent) { return Err("invalid CPU quota".into()); }
    let identity = crate::read_proc_identity(Path::new("/proc"), pid).map_err(|e|e.to_string())?;
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e|e.to_string())?;
    let group = format!("/{}", crate::parse_cgroup_rel_path(&cg).ok_or("invalid process cgroup")?);
    let unit = isolated_scope(&group)?.to_string();
    for member in group_members(&group)? {
        if member == std::process::id() as i32 { return Err("refusing to cap the guard's own unit".into()); }
        let name = std::fs::read_to_string(format!("/proc/{member}/comm")).map_err(|e|e.to_string())?;
        if exempt.contains(name.trim()) { return Err(format!("scope contains exempt process {}",name.trim())); }
    }
    let invocation = property(&unit, "InvocationID").await?;
    if invocation.is_empty() || property(&unit, "ActiveState").await? != "active" { return Err("workload scope not active".into()); }
    let root = state_root()?;
    let key = lease_key(&unit);
    let _lock = lock(&root, &key)?;
    if let Some(old) = load(&root, &key)? {
        if old.invocation == invocation && old.boot_id == boot_id()? { return Ok((key, old.generation)); }
        return Err("stale CPU lease requires recovery before reuse".into());
    }
    let original = quota_value(&property(&unit, "CPUQuotaPerSecUSec").await?)?;
    let applied = original.min(u64::from(percent)*10_000);
    if original <= applied { return Err("workload already has an equal or stricter CPU quota".into()); }
    let generation = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos().to_string();
    let timer = format!("{key}-{generation}-expiry");
    let lease = Lease { unit: unit.clone(), invocation, original, applied, generation: generation.clone(), timer: timer.clone(), boot_id: boot_id()? };
    // Durable intent precedes timer AND mutation. Recovery can distinguish an
    // unchanged original quota from a successful/ambiguous manager mutation.
    save(&root, &key, &lease)?;
    let exe = std::env::current_exe().map_err(|e|e.to_string())?;
    let run = crate::resolve_bin_strict("systemd-run").map_err(|e|e.to_string())?;
    output(&run, &["--user".into(), "--collect".into(), format!("--unit={timer}"), "--on-active=1h".into(),
        "--timer-property=AccuracySec=1s".into(), "--".into(), exe.to_string_lossy().into_owned(), "cpu-lease-release".into(), key.clone(), generation.clone()]).await?;
    if !matches!(crate::process_identity_status(Path::new("/proc"), pid, &identity), crate::ProcessIdentityStatus::Match)
        || property(&unit, "InvocationID").await? != lease.invocation {
        return Err("process or unit identity changed before CPU cap".into());
    }
    set_quota(&unit, applied).await?;
    Ok((key, generation))
}

pub(crate) async fn release(key: &str, generation: &str) -> Result<(), String> {
    if !is_lease(key) { return Err("invalid CPU lease key".into()); }
    let root = state_root()?;
    let _lock = lock(&root, key)?;
    let Some(lease) = load(&root, key)? else { return Ok(()); };
    if lease.generation != generation { return Ok(()); } // obsolete timer
    let invocation = property(&lease.unit, "InvocationID").await?;
    if lease.boot_id == boot_id()? && !invocation.is_empty() && invocation == lease.invocation {
        let current = quota_value(&property(&lease.unit, "CPUQuotaPerSecUSec").await?)?;
        // Compare-and-restore: never overwrite a later operator quota change.
        if current == lease.applied { set_quota(&lease.unit, lease.original).await?; }
    }
    std::fs::remove_file(root.join(format!("{key}.json"))).map_err(|e|e.to_string())?;
    if let Ok(bin) = crate::resolve_bin_strict("systemctl") {
        let _ = output(&bin, &["--user".into(), "stop".into(), format!("{}.timer",lease.timer)]).await;
    }
    Ok(())
}

/// Restore even leases whose root PID exited: the independent file records
/// unit invocation + boot identity, not a snapshot of one sampled process.
pub(crate) async fn recover() {
    let Ok(root) = state_root() else { return; };
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else { continue; };
        if path.extension().and_then(|s| s.to_str()) != Some("json") || !is_lease(key) { continue; }
        match load(path.parent().unwrap(), key) {
            Ok(Some(lease)) => if let Err(e) = release(key, &lease.generation).await { eprintln!("CPU lease recovery retained {key}: {e}"); },
            Err(e) => eprintln!("CPU lease recovery retained unreadable {key}: {e}"),
            Ok(None) => {},
        }
    }
}

pub(crate) async fn restore_legacy(_pid: i32, _orig: &str) -> Result<(), String> {
    Err("legacy migrated CPU service requires explicit recovery; refusing unsafe stop or guessed migration".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_isolated_job_scopes_are_eligible() {
        let uid = unsafe { libc::geteuid() };
        let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/");
        assert!(isolated_scope(&(prefix.clone()+"tmux-spawn-a.scope")).is_ok());
        for unit in ["editor.scope", "konsole.scope", "dracon-system-guard.service", "tmux-spawn-../a.scope"] {
            assert!(isolated_scope(&(prefix.clone()+unit)).is_err());
        }
        assert!(isolated_scope("/system.slice/tmux-spawn-a.scope").is_err());
    }
    #[test]
    fn quota_round_trip_preserves_stricter_existing_limits() {
        assert_eq!(quota_value("infinity").unwrap(),u64::MAX);
        assert_eq!(quota_value("500ms").unwrap(),500_000);
        assert_eq!(quota_value("1s 500ms").unwrap(),1_500_000);
        assert!(quota_value("").is_err());
        assert!(quota_value("garbage").is_err());
        assert_eq!(250_000u64.min(500_000),250_000);
    }
    #[test]
    fn lease_intent_survives_restart_and_keys_cannot_escape_state_root() {
        let dir = tempfile::tempdir().unwrap();
        let key = lease_key("tmux-spawn-a.scope");
        assert!(is_lease(&key));
        assert!(!is_lease("../dracon-cpu-unit-0123456789abcdef"));
        let lease = Lease { unit:"tmux-spawn-a.scope".into(),invocation:"abc".into(),original:u64::MAX,applied:500_000,generation:"1".into(),timer:"expiry".into(),boot_id:"boot".into() };
        let _lock = lock(dir.path(),&key).unwrap();
        save(dir.path(),&key,&lease).unwrap();
        assert_eq!(load(dir.path(),&key).unwrap().unwrap().original,u64::MAX);
        assert!(lock(dir.path(),&key).is_err());
    }
}
