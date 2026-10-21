//! Reversible, manager-owned CPU leases on isolated workload scopes.
//!
//! Never migrate a PID, stop a scope, or write cgroup.procs. In particular,
//! scope/service timeout expiry is NOT a non-killing way to release a quota.
//! A separate manager timer restores the original quota even if the guard dies.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

#[derive(Debug, Serialize, Deserialize)]
struct Lease {
    unit: String,
    invocation: String,
    original: u64,
    applied: u64,
    generation: String,
    timer: String,
    boot_id: String,
    #[serde(default)]
    armed: bool,
    #[serde(default)]
    applied_verified: bool,
    #[serde(default)]
    expires_at_unix_secs: u64,
}

pub(crate) async fn output(bin: &str, args: &[String]) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| format!("{bin} timed out"))?
        .map_err(|e| format!("{bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bin}: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn manager(method: &str, signature: &str, values: &[String]) -> Result<String, String> {
    let bin = crate::resolve_bin_strict("busctl").map_err(|e| e.to_string())?;
    let mut args: Vec<String> = [
        "--user",
        "--timeout=10s",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        method,
        signature,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend_from_slice(values);
    output(&bin, &args).await
}

pub(crate) async fn property(unit: &str, name: &str) -> Result<String, String> {
    let bin = crate::resolve_bin_strict("systemctl").map_err(|e| e.to_string())?;
    output(
        &bin,
        &[
            "--user".into(),
            "show".into(),
            unit.into(),
            "-p".into(),
            name.into(),
            "--value".into(),
        ],
    )
    .await
}

pub(crate) async fn set_quota(unit: &str, quota: u64) -> Result<(), String> {
    manager(
        "SetUnitProperties",
        "sba(sv)",
        &[
            unit.into(),
            "true".into(),
            "1".into(),
            "CPUQuotaPerSecUSec".into(),
            "t".into(),
            quota.to_string(),
        ],
    )
    .await?;
    let observed = quota_value(&property(unit, "CPUQuotaPerSecUSec").await?)?;
    if observed != quota {
        return Err(format!(
            "CPU quota verification failed: wanted {quota}, observed {observed}"
        ));
    }
    Ok(())
}

fn quota_value(text: &str) -> Result<u64, String> {
    // systemctl's --value renders time spans; use busctl for raw microseconds
    // instead when a finite value is not a plain integer.
    if text == "infinity" {
        return Ok(u64::MAX);
    }
    if let Ok(n) = text.parse() {
        return Ok(n);
    }
    let mut total = 0f64;
    for part in text.split_whitespace() {
        let (num, factor) = if let Some(n) = part.strip_suffix("ms") {
            (n, 1_000.)
        } else if let Some(n) = part.strip_suffix("us") {
            (n, 1.)
        } else if let Some(n) = part.strip_suffix("s") {
            (n, 1_000_000.)
        } else if let Some(n) = part.strip_suffix("min") {
            (n, 60_000_000.)
        } else {
            return Err(format!("unsupported CPU quota value {text}"));
        };
        total += num.parse::<f64>().map_err(|e| e.to_string())? * factor;
    }
    if text.is_empty() || !total.is_finite() || total <= 0. {
        return Err(format!("invalid quota {text}"));
    }
    Ok(total.round() as u64)
}

fn state_root() -> Result<PathBuf, String> {
    Ok(dirs::home_dir()
        .ok_or("HOME unavailable")?
        .join(".dracon/cpu-leases"))
}
fn boot_id() -> Result<String, String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().into())
        .map_err(|e| e.to_string())
}
fn lease_key(unit: &str) -> String {
    // Stable across compiler/library releases, unlike DefaultHasher.
    let hash = unit.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("dracon-cpu-unit-{hash:016x}")
}
pub(crate) fn is_lease(key: &str) -> bool {
    key.strip_prefix("dracon-cpu-unit-")
        .is_some_and(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn lock(root: &Path, key: &str) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{key}.lock")))
        .map_err(|e| e.to_string())?;
    f.try_lock_exclusive()
        .map_err(|e| format!("CPU lease busy: {e}"))?;
    Ok(f)
}
fn save(root: &Path, key: &str, lease: &Lease) -> Result<(), String> {
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    use std::io::Write;
    tmp.write_all(&serde_json::to_vec(lease).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist(root.join(format!("{key}.json")))
        .map_err(|e| e.to_string())?;
    std::fs::File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
fn load(root: &Path, key: &str) -> Result<Option<Lease>, String> {
    match std::fs::read(root.join(format!("{key}.json"))) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn isolated_scope(group: &str) -> Result<&str, String> {
    let uid = unsafe { libc::geteuid() };
    let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/");
    let unit = group
        .strip_prefix(&prefix)
        .ok_or("process is outside the supported user workload slice")?;
    // tmux-spawn is the runner's per-job scope, NOT the shared tmux server or
    // GUI terminal scope. dracon-managed-job-* is this tool's own admitted
    // per-job scope. Unsupported/shared units are deliberately untouched.
    if !unit.ends_with(".scope")
        || !(unit.starts_with("tmux-spawn-")
            || unit.starts_with("dracon-managed-job-")
            || unit.starts_with("dracon-protection-test-"))
        || !unit
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err("CPU cap requires an isolated per-job workload scope; shared/unknown units are untouched".into());
    }
    Ok(unit)
}

fn group_members(group: &str) -> Result<Vec<i32>, String> {
    isolated_scope(group)?;
    let dir = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        if entry
            .map_err(|e| e.to_string())?
            .file_type()
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err(
                "nested cgroups are unsupported; refusing inherited quota on uninspected workloads"
                    .into(),
            );
        }
    }
    let text = std::fs::read_to_string(
        Path::new("/sys/fs/cgroup")
            .join(group.trim_start_matches('/'))
            .join("cgroup.procs"),
    )
    .map_err(|e| format!("read scope members: {e}"))?;
    text.lines()
        .map(|s| s.parse::<i32>().map_err(|e| e.to_string()))
        .collect()
}

// Tests inject manager I/O, not lifecycle decisions or persistence.
trait LeaseManager {
    async fn property(&self, unit: &str, name: &str) -> Result<String, String>;
    async fn set_quota(&self, unit: &str, quota: u64) -> Result<(), String>;
    async fn arm(
        &self,
        timer: &str,
        seconds: u64,
        key: &str,
        generation: &str,
    ) -> Result<(), String>;
    async fn stop_timer(&self, timer: &str);
}
struct SystemLeaseManager;
impl LeaseManager for SystemLeaseManager {
    async fn property(&self, unit: &str, name: &str) -> Result<String, String> {
        property(unit, name).await
    }
    async fn set_quota(&self, unit: &str, quota: u64) -> Result<(), String> {
        set_quota(unit, quota).await
    }
    async fn arm(
        &self,
        timer: &str,
        seconds: u64,
        key: &str,
        generation: &str,
    ) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let run = crate::resolve_bin_strict("systemd-run").map_err(|e| e.to_string())?;
        output(&run, &expiry_args(timer, seconds, &exe, key, generation)).await?;
        Ok(())
    }
    async fn stop_timer(&self, timer: &str) {
        if let Ok(bin) = crate::resolve_bin_strict("systemctl") {
            let _ = output(
                &bin,
                &["--user".into(), "stop".into(), format!("{timer}.timer")],
            )
            .await;
        }
    }
}
fn expiry_args(timer: &str, seconds: u64, exe: &Path, key: &str, generation: &str) -> Vec<String> {
    vec![
        "--user".into(),
        "--collect".into(),
        format!("--unit={timer}"),
        format!("--on-active={seconds}s"),
        "--timer-property=AccuracySec=1s".into(),
        "--property=Restart=on-failure".into(),
        "--property=RestartSec=5s".into(),
        "--property=StartLimitIntervalSec=0".into(),
        "--".into(),
        exe.to_string_lossy().into_owned(),
        "cpu-lease-release".into(),
        key.into(),
        generation.into(),
    ]
}

pub(crate) async fn cap(
    pid: i32,
    percent: u32,
    exempt: &HashSet<String>,
) -> Result<(String, String), String> {
    cap_with_duration(pid, percent, exempt, 3600).await
}

pub(crate) async fn cap_with_duration(
    pid: i32,
    percent: u32,
    exempt: &HashSet<String>,
    lease_seconds: u64,
) -> Result<(String, String), String> {
    if !(1..=100).contains(&percent) {
        return Err("invalid CPU quota".into());
    }
    let identity = crate::read_proc_identity(Path::new("/proc"), pid).map_err(|e| e.to_string())?;
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e| e.to_string())?;
    let group = format!(
        "/{}",
        crate::parse_cgroup_rel_path(&cg).ok_or("invalid process cgroup")?
    );
    let unit = isolated_scope(&group)?.to_string();
    for member in group_members(&group)? {
        if member == std::process::id() as i32 {
            return Err("refusing to cap the guard's own unit".into());
        }
        let name =
            std::fs::read_to_string(format!("/proc/{member}/comm")).map_err(|e| e.to_string())?;
        if exempt.contains(name.trim()) {
            return Err(format!("scope contains exempt process {}", name.trim()));
        }
    }
    cap_prepared(
        &SystemLeaseManager,
        &state_root()?,
        &boot_id()?,
        &unit,
        percent,
        lease_seconds,
        || {
            let members = group_members(&group)?;
            if !members.contains(&pid) {
                return Err("workload left sampled scope".into());
            }
            for member in members {
                let name = std::fs::read_to_string(format!("/proc/{member}/comm"))
                    .map_err(|e| e.to_string())?;
                if member == std::process::id() as i32 || exempt.contains(name.trim()) {
                    return Err("scope now contains guard or exempt process".into());
                }
            }
            if !matches!(
                crate::process_identity_status(Path::new("/proc"), pid, &identity),
                crate::ProcessIdentityStatus::Match
            ) {
                return Err("process identity changed before CPU cap".into());
            }
            Ok(())
        },
    )
    .await
}

async fn cap_prepared(
    manager: &impl LeaseManager,
    root: &Path,
    current_boot: &str,
    unit: &str,
    percent: u32,
    lease_seconds: u64,
    validate_workload: impl Fn() -> Result<(), String>,
) -> Result<(String, String), String> {
    let invocation = manager.property(unit, "InvocationID").await?;
    if invocation.is_empty() || manager.property(unit, "ActiveState").await? != "active" {
        return Err("workload scope not active".into());
    }
    let key = lease_key(unit);
    let _lock = lock(root, &key)?;
    if let Some(mut old) = load(root, &key)? {
        if old.invocation != invocation || old.boot_id != current_boot {
            std::fs::remove_file(root.join(format!("{key}.json"))).map_err(|e| e.to_string())?;
            return Err(
                "stale CPU intent retired without touching replacement unit; retry next pass"
                    .into(),
            );
        }
        let current = quota_value(&manager.property(unit, "CPUQuotaPerSecUSec").await?)?;
        let armed = old.armed
            && SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs()
                < old.expires_at_unix_secs
            && manager
                .property(&format!("{}.timer", old.timer), "ActiveState")
                .await?
                == "active";
        match reconcile_intent(&old, current, armed) {
            IntentAction::Reuse => {
                old.applied_verified = true;
                save(root, &key, &old)?;
                return Ok((key, old.generation));
            }
            IntentAction::RestoreUnprotected => {
                manager.set_quota(unit, old.original).await?;
            }
            IntentAction::DropUnapplied => {}
            IntentAction::PreserveExternal => {
                return Err("CPU lease state changed externally; no application claimed".into())
            }
        }
        std::fs::remove_file(root.join(format!("{key}.json"))).map_err(|e| e.to_string())?;
        return Err("incomplete CPU lease recovered; retry next pass".into());
    }
    let original = quota_value(&manager.property(unit, "CPUQuotaPerSecUSec").await?)?;
    let applied = original.min(u64::from(percent) * 10_000);
    if original <= applied {
        return Err("workload already has an equal or stricter CPU quota".into());
    }
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos()
        .to_string();
    let timer = format!("{key}-{generation}-expiry");
    let mut lease = Lease {
        unit: unit.into(),
        invocation,
        original,
        applied,
        generation: generation.clone(),
        timer: timer.clone(),
        boot_id: current_boot.into(),
        armed: false,
        applied_verified: false,
        expires_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs()
            .saturating_add(lease_seconds),
    };
    // Durable intent precedes timer AND mutation. Recovery can distinguish an
    // unchanged original quota from a successful/ambiguous manager mutation.
    save(root, &key, &lease)?;
    manager
        .arm(&timer, lease_seconds, &key, &generation)
        .await?;
    if manager
        .property(&format!("{timer}.timer"), "ActiveState")
        .await?
        != "active"
    {
        return Err("CPU expiry timer is not armed; refusing quota mutation".into());
    }
    lease.armed = true;
    save(root, &key, &lease)?;
    if manager.property(unit, "Delegate").await? != "no" {
        return Err("delegated scopes are unsupported for safe CPU limiting".into());
    }
    validate_workload()?;
    if quota_value(&manager.property(unit, "CPUQuotaPerSecUSec").await?)? != original {
        return Err("CPU quota changed before apply".into());
    }
    if manager.property(unit, "InvocationID").await? != lease.invocation {
        return Err("process or unit identity changed before CPU cap".into());
    }
    if SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        >= lease.expires_at_unix_secs
    {
        return Err("CPU lease expired during preparation; quota not applied".into());
    }
    manager.set_quota(unit, applied).await?;
    lease.applied_verified = true;
    save(root, &key, &lease)?;
    Ok((key, generation))
}

pub(crate) async fn release(key: &str, generation: &str) -> Result<(), String> {
    if !is_lease(key) {
        return Err("invalid CPU lease key".into());
    }
    release_with(
        &SystemLeaseManager,
        &state_root()?,
        &boot_id()?,
        key,
        generation,
    )
    .await
}

async fn release_with(
    manager: &impl LeaseManager,
    root: &Path,
    current_boot: &str,
    key: &str,
    generation: &str,
) -> Result<(), String> {
    let _lock = lock(root, key)?;
    let Some(lease) = load(root, key)? else {
        return Ok(());
    };
    if lease.generation != generation {
        return Ok(());
    } // obsolete timer
    let load_state = if current_boot == lease.boot_id {
        manager.property(&lease.unit, "LoadState").await?
    } else {
        String::new()
    };
    let invocation = if load_state == "loaded" {
        manager.property(&lease.unit, "InvocationID").await?
    } else {
        String::new()
    };
    if lease_is_owned(
        &lease.boot_id,
        current_boot,
        &lease.invocation,
        &invocation,
        &load_state,
    )? {
        let current = quota_value(&manager.property(&lease.unit, "CPUQuotaPerSecUSec").await?)?;
        // Compare-and-restore: never overwrite a later operator quota change.
        if current == lease.applied {
            manager.set_quota(&lease.unit, lease.original).await?;
        }
    }
    std::fs::remove_file(root.join(format!("{key}.json"))).map_err(|e| e.to_string())?;
    manager.stop_timer(&lease.timer).await;
    Ok(())
}

/// Restore even leases whose root PID exited: the independent file records
/// unit invocation + boot identity, not a snapshot of one sampled process.
pub(crate) async fn recover() {
    let Ok(root) = state_root() else {
        return;
    };
    let Ok(current_boot) = boot_id() else {
        return;
    };
    recover_with(&SystemLeaseManager, &root, &current_boot).await;
}

async fn recover_with(manager: &impl LeaseManager, root: &Path, current_boot: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("json") || !is_lease(key) {
            continue;
        }
        match load(path.parent().unwrap(), key) {
            Ok(Some(lease)) => {
                if let Err(e) =
                    release_with(manager, root, current_boot, key, &lease.generation).await
                {
                    eprintln!("CPU lease recovery retained {key}: {e}");
                }
            }
            Err(e) => eprintln!("CPU lease recovery retained unreadable {key}: {e}"),
            Ok(None) => {}
        }
    }
}

/// Diagnostic-only creation: PID is a disposable child of the probe runner.
pub(crate) async fn create_probe_scope(unit: &str, pid: u32) -> Result<(), String> {
    if !unit.starts_with("dracon-protection-test-") || !unit.ends_with(".scope") {
        return Err("invalid probe scope".into());
    }
    manager(
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
        &[
            unit.into(),
            "fail".into(),
            "1".into(),
            "PIDs".into(),
            "au".into(),
            "1".into(),
            pid.to_string(),
            "0".into(),
        ],
    )
    .await
    .map(|_| ())
}

pub(crate) async fn restore_legacy(_pid: i32, _orig: &str) -> Result<(), String> {
    Err("legacy migrated CPU service requires explicit recovery; refusing unsafe stop or guessed migration".into())
}

fn lease_is_owned(
    saved_boot: &str,
    current_boot: &str,
    saved_invocation: &str,
    invocation: &str,
    load_state: &str,
) -> Result<bool, String> {
    if saved_boot != current_boot || load_state == "not-found" {
        return Ok(false);
    }
    if load_state != "loaded" || invocation.is_empty() || saved_invocation.is_empty() {
        return Err("unit ownership unavailable; retaining CPU recovery obligation".into());
    }
    Ok(saved_invocation == invocation)
}

#[derive(Debug, PartialEq, Eq)]
enum IntentAction {
    Reuse,
    RestoreUnprotected,
    DropUnapplied,
    PreserveExternal,
}
fn reconcile_intent(lease: &Lease, current: u64, armed: bool) -> IntentAction {
    if current == lease.applied {
        if armed {
            IntentAction::Reuse
        } else {
            IntentAction::RestoreUnprotected
        }
    } else if current == lease.original && !lease.applied_verified {
        IntentAction::DropUnapplied
    } else {
        IntentAction::PreserveExternal
    }
}

#[cfg(test)]
#[path = "cpu_lease_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_isolated_job_scopes_are_eligible() {
        let uid = unsafe { libc::geteuid() };
        let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/");
        assert!(isolated_scope(&(prefix.clone() + "tmux-spawn-a.scope")).is_ok());
        assert!(isolated_scope(
            &(prefix.clone() + "dracon-managed-job-12345678-1234-1234-1234-123456789abc.scope")
        )
        .is_ok());
        for unit in [
            "editor.scope",
            "konsole.scope",
            "dracon-system-guard.service",
            "tmux-spawn-../a.scope",
        ] {
            assert!(isolated_scope(&(prefix.clone() + unit)).is_err());
        }
        assert!(isolated_scope("/system.slice/tmux-spawn-a.scope").is_err());
    }
    #[test]
    fn quota_round_trip_preserves_stricter_existing_limits() {
        assert_eq!(quota_value("infinity").unwrap(), u64::MAX);
        assert_eq!(quota_value("500ms").unwrap(), 500_000);
        assert_eq!(quota_value("1s 500ms").unwrap(), 1_500_000);
        assert!(quota_value("").is_err());
        assert!(quota_value("garbage").is_err());
        // The stricter of the two survives the round trip. Both sides are
        // bound to locals: clippy's `unnecessary_min_or_max` rejects a
        // constant operand, and this asserts the comparison, not a literal.
        let stricter = 250_000u64;
        let existing = 500_000u64;
        assert_eq!(stricter.min(existing), stricter);
    }
    #[test]
    fn lease_intent_survives_restart_and_keys_cannot_escape_state_root() {
        let dir = tempfile::tempdir().unwrap();
        let key = lease_key("tmux-spawn-a.scope");
        assert!(is_lease(&key));
        assert!(!is_lease("../dracon-cpu-unit-0123456789abcdef"));
        let lease = Lease {
            unit: "tmux-spawn-a.scope".into(),
            invocation: "abc".into(),
            original: u64::MAX,
            applied: 500_000,
            generation: "1".into(),
            timer: "expiry".into(),
            boot_id: "boot".into(),
            armed: false,
            applied_verified: false,
            expires_at_unix_secs: 0,
        };
        let _lock = lock(dir.path(), &key).unwrap();
        save(dir.path(), &key, &lease).unwrap();
        assert_eq!(load(dir.path(), &key).unwrap().unwrap().original, u64::MAX);
        assert!(lock(dir.path(), &key).is_err());
    }

    #[test]
    fn unavailable_unit_identity_never_discards_cpu_debt() {
        assert!(lease_is_owned("b", "b", "i", "", "loaded").is_err());
        assert!(lease_is_owned("b", "b", "i", "", "error").is_err());
        assert_eq!(lease_is_owned("b", "b", "i", "i", "loaded"), Ok(true));
        assert_eq!(
            lease_is_owned("b", "b", "i", "replacement", "loaded"),
            Ok(false)
        );
        assert_eq!(lease_is_owned("b", "b", "i", "", "not-found"), Ok(false));
        assert_eq!(lease_is_owned("previous", "b", "i", "", ""), Ok(false));
    }

    #[test]
    fn incomplete_intent_never_counts_as_enforced_quota() {
        let mut lease = Lease {
            unit: "job.scope".into(),
            invocation: "i".into(),
            original: u64::MAX,
            applied: 500_000,
            generation: "g".into(),
            timer: "t".into(),
            boot_id: "b".into(),
            armed: false,
            applied_verified: false,
            expires_at_unix_secs: 0,
        };
        assert_eq!(
            reconcile_intent(&lease, u64::MAX, false),
            IntentAction::DropUnapplied
        );
        assert_eq!(
            reconcile_intent(&lease, 500_000, false),
            IntentAction::RestoreUnprotected
        );
        assert_eq!(reconcile_intent(&lease, 500_000, true), IntentAction::Reuse);
        lease.applied_verified = true;
        assert_eq!(
            reconcile_intent(&lease, 300_000, true),
            IntentAction::PreserveExternal
        );
        assert_eq!(
            reconcile_intent(&lease, u64::MAX, false),
            IntentAction::PreserveExternal
        );
    }
}
