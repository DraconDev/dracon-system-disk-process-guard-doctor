//! Cooperative registered-job OOM leases. Process ancestry is never authority.
use super::{
    oom_job::{self, Job, Phase},
    *,
};
use anyhow::{ensure, Context};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    time::{SystemTime, UNIX_EPOCH},
};

const RECOVERER: &str = "dracon-system-oom-recovery.service";
const LEASE_SECS: u64 = 120;
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Debt {
    admission: Job,
    deadline: u64,
    draining: bool,
    attempts: u64,
    last_error: Option<String>,
}

/// Pure recovery decisions. No observation below may retire a debt except an
/// explicit same-generation terminal certificate.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Keep the job lifetime debt and keep restoring.
    Drain,
    /// The lease is still running: keep the debt, restore nothing yet.
    Hold,
    /// The sentinel certified a workload-terminal generation: retire.
    Retire,
    /// The debt belongs to a previous boot: never touch current processes.
    ArchivePreviousBoot,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    same_boot: bool,
    terminal_proof: bool,
    same_admission: bool,
    deadline_passed: bool,
    draining: bool,
}
fn decide(facts: &Facts) -> Action {
    if !facts.same_boot {
        return Action::ArchivePreviousBoot;
    }
    if !facts.same_admission {
        // Unverifiable ownership: retain the debt rather than restore a job
        // whose admission identity cannot be proven.
        return Action::Drain;
    }
    if facts.terminal_proof {
        return Action::Retire;
    }
    if facts.draining || facts.deadline_passed {
        return Action::Drain;
    }
    Action::Hold
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn boot() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .into())
}
fn group_for(proc_root: &Path, pid: i32) -> Result<String> {
    fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup"))?
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::to_owned)
        .context("unified process cgroup unavailable")
}
/// True when the process runs in a registered managed job. Unregistered jobs
/// are deliberately unsupported for lifetime-debt recovery.
pub(crate) fn is_managed_job_process(pid: i32) -> bool {
    match group_for(Path::new("/proc"), pid) {
        Ok(group) => id_for(&group).is_ok(),
        Err(_) => false,
    }
}
/// The registered generation of the managed job this process belongs to.
pub(crate) fn job_id_for_process(pid: i32) -> Result<String> {
    let group = group_for(Path::new("/proc"), pid)?;
    Ok(id_for(&group)?.to_owned())
}
fn id_for(group: &str) -> Result<&str> {
    let name = group.rsplit('/').next().unwrap_or_default();
    let id = name
        .strip_prefix("dracon-managed-job-")
        .and_then(|s| s.strip_suffix(".scope"))
        .context("unsupported OOM workload: no managed launch admission")?;
    ensure!(oom_job::valid_id(id), "invalid managed job generation");
    Ok(id)
}
fn debt_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.debt.json"))
}
fn load_debt(root: &Path, id: &str) -> Result<Option<Debt>> {
    let path = debt_path(root, id);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "reading the OOM debt {}: {e}",
                path.display()
            ))
        }
    };
    let debt: Debt = serde_json::from_slice(&bytes)?;
    ensure!(
        debt.admission.id == id && debt.admission.version == 1,
        "mismatched OOM debt generation"
    );
    Ok(Some(debt))
}
fn save_debt(root: &Path, debt: &Debt) -> Result<()> {
    ensure!(
        oom_job::valid_id(&debt.admission.id),
        "invalid OOM debt generation"
    );
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(&serde_json::to_vec(debt)?)?;
    temp.as_file().sync_all()?;
    temp.persist(debt_path(root, &debt.admission.id))
        .map_err(|e| e.error)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
/// A previous boot's debt can never be restored into current processes. Keep
/// its bytes for operator review instead of deleting or applying them.
fn archive_previous_boot(root: &Path, debt: &Debt) -> Result<()> {
    let destination = root.join(format!("{}.previous-boot.json", debt.admission.id));
    if !destination.exists() {
        fs::copy(debt_path(root, &debt.admission.id), &destination)?;
    }
    fs::remove_file(debt_path(root, &debt.admission.id))?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
fn retire(root: &Path, debt: &Debt) -> Result<()> {
    // The retiring record must be the live debt of this generation. A stale
    // or foreign generation can never retire another generation's debt.
    let live = load_debt(root, &debt.admission.id)?
        .ok_or_else(|| anyhow::anyhow!("no live debt to retire"))?;
    ensure!(
        oom_job::same_admission(&live.admission, &debt.admission),
        "retiring record does not own the live debt"
    );
    let destination = root.join(format!("{}.retired.json", debt.admission.id));
    if destination.exists() {
        let previous: Debt = serde_json::from_slice(&fs::read(&destination)?)?;
        ensure!(
            oom_job::same_admission(&previous.admission, &debt.admission),
            "conflicting retired OOM generation"
        );
    }
    fs::rename(debt_path(root, &debt.admission.id), destination)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
async fn boundary(job: &Job) -> Result<PathBuf> {
    ensure!(
        job.version == 1 && oom_job::valid_id(&job.id) && id_for(&job.group)? == job.id,
        "invalid OOM admission"
    );
    ensure!(
        (-499..250).contains(&job.baseline),
        "unsupported OOM baseline"
    );
    ensure!(job.boot_id == boot()?, "OOM job belongs to previous boot");
    ensure!(
        cpu_limit::property(&job.unit, "InvocationID")
            .await
            .map_err(anyhow::Error::msg)?
            == job.invocation
            && !job.invocation.is_empty(),
        "OOM job invocation unavailable or replaced"
    );
    ensure!(
        cpu_limit::property(&job.unit, "ControlGroup")
            .await
            .map_err(anyhow::Error::msg)?
            == job.group,
        "OOM job cgroup changed"
    );
    ensure!(
        cpu_limit::property(&job.unit, "Delegate")
            .await
            .map_err(anyhow::Error::msg)?
            == "no",
        "delegated OOM job is unsupported"
    );
    ensure!(
        !Path::new(&job.group).components().any(|c| matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )),
        "invalid job cgroup path"
    );
    let path = Path::new("/sys/fs/cgroup").join(job.group.trim_start_matches('/'));
    let m = fs::metadata(&path)?;
    ensure!(
        m.dev() == job.group_device && m.ino() == job.group_inode,
        "OOM job cgroup incarnation changed"
    );
    oom_job::no_nested_groups(&path)?;
    Ok(path)
}
fn recoverer_armed(state: &str) -> bool {
    matches!(state, "active" | "activating" | "reloading")
}
async fn ensure_recoverer(root: &Path) -> Result<()> {
    let state = cpu_limit::property(RECOVERER, "ActiveState")
        .await
        .unwrap_or_default();
    if !recoverer_armed(&state) {
        let run = resolve_bin_strict("systemd-run")?;
        let args = vec![
            "--user".into(),
            "--collect".into(),
            format!("--unit={RECOVERER}"),
            "--property=Restart=on-failure".into(),
            "--property=RestartSec=2s".into(),
            "--property=StartLimitIntervalSec=0".into(),
            "--property=NoNewPrivileges=yes".into(),
            "--property=CapabilityBoundingSet=".into(),
            "--property=ProtectControlGroups=yes".into(),
            "--property=ProtectSystem=strict".into(),
            "--property=ProtectHome=read-only".into(),
            format!("--property=ReadWritePaths={}", root.display()),
            "--property=PrivateTmp=yes".into(),
            "--property=RestrictSUIDSGID=yes".into(),
            "--".into(),
            std::env::current_exe()?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("executable path is not UTF-8"))?,
            "oom-job-recover".into(),
        ];
        if let Err(start_error) = cpu_limit::output(&run, &args).await {
            ensure!(
                recoverer_armed(
                    &cpu_limit::property(RECOVERER, "ActiveState")
                        .await
                        .unwrap_or_default()
                ),
                "independent OOM recovery could not start: {start_error}"
            );
        }
    }
    let state = cpu_limit::property(RECOVERER, "ActiveState")
        .await
        .map_err(anyhow::Error::msg)?;
    ensure!(
        recoverer_armed(&state),
        "independent OOM recoverer is not active"
    );
    let pid = cpu_limit::property(RECOVERER, "MainPID")
        .await
        .map_err(anyhow::Error::msg)?
        .parse::<i32>()?;
    ensure!(pid > 0, "independent OOM recoverer has no process");
    let expected = fs::metadata(std::env::current_exe()?)?;
    let actual = fs::metadata(format!("/proc/{pid}/exe"))?;
    ensure!(
        expected.dev() == actual.dev() && expected.ino() == actual.ino(),
        "independent OOM recoverer runs a different binary"
    );
    let cmd = fs::read(format!("/proc/{pid}/cmdline"))?;
    let args: Vec<_> = cmd.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
    ensure!(
        args.len() == 2 && args[1] == b"oom-job-recover",
        "independent recovery command identity differs"
    );
    Ok(())
}

pub(crate) async fn bias(
    sample: &ProcSample,
    exempt: &HashSet<String>,
) -> Result<Option<(i32, i32)>, String> {
    bias_inner(sample, exempt)
        .await
        .map_err(|e| format!("{e:#}"))
}
async fn bias_inner(sample: &ProcSample, exempt: &HashSet<String>) -> Result<Option<(i32, i32)>> {
    ensure!(
        process_sample_is_current(sample),
        "process incarnation changed before managed OOM application"
    );
    let group = group_for(Path::new("/proc"), sample.pid)?;
    let id = id_for(&group)?;
    let root = oom_job::root()?;
    let _lock = oom_job::locked(&root, id)?;
    let mut job = oom_job::load(&root, id)?;
    ensure!(
        !job.terminal_proof && job.phase != Phase::Terminal,
        "managed job has terminated"
    );
    ensure!(job.group == group, "process is outside its admitted job");
    boundary(&job).await?;
    ensure!(
        sample.pid != job.sentinel
            && matches!(
                process_identity_status(Path::new("/proc"), job.sentinel, &job.sentinel_identity),
                ProcessIdentityStatus::Match
            ),
        "sentinel is exempt or cannot be verified"
    );
    let current = read_proc_identity(Path::new("/proc"), sample.pid)?;
    ensure!(
        current.starttime == sample.starttime
            && !exempt.contains(&current.comm)
            && !is_kernel_process(&current.comm),
        "current process is exempt or changed"
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/proc/{}/oom_score_adj", sample.pid))
        .map_err(|e| -> anyhow::Error {
            let owner = fs::metadata(format!("/proc/{}/oom_score_adj", sample.pid))
                .map(|m| {
                    use std::os::unix::fs::MetadataExt;
                    format!("uid={} mode={:o}", m.uid(), m.mode() & 0o777)
                })
                .unwrap_or_else(|_| "owner unavailable".into());
            let dumpable = fs::read_to_string(format!("/proc/{}/status", sample.pid))
                .ok()
                .and_then(|status| {
                    status
                        .lines()
                        .find(|l| l.starts_with("Uid:") || l.starts_with("Gid:"))
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "status unavailable".into());
            anyhow::anyhow!(
                "opening /proc/{}/oom_score_adj ({owner}; {dumpable}): {e}",
                sample.pid
            )
        })?;
    ensure!(
        process_sample_is_current(sample)
            && group_for(Path::new("/proc"), sample.pid)? == job.group,
        "process changed after descriptor open"
    );
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let original = text.trim().parse::<i32>()?;
    let previous = load_debt(&root, id)?;
    if let Some(ref debt) = previous {
        ensure!(
            oom_job::same_admission(&debt.admission, &job) && !debt.draining,
            "managed OOM job is draining or generation changed"
        );
    }
    ensure!(
        original == job.baseline || (original == OOM_BIAS_TARGET && previous.is_some()),
        "independent or unsupported OOM score change"
    );
    let mut debt = previous.unwrap_or(Debt {
        admission: job.clone(),
        deadline: 0,
        draining: false,
        attempts: 0,
        last_error: None,
    });
    debt.deadline = now()?.saturating_add(LEASE_SECS);
    save_debt(&root, &debt)?; // write-ahead lifetime debt, never per-root authority
    job.phase = Phase::Active;
    oom_job::save(&root, &job, false)?;
    ensure_recoverer(&root).await?; // no mutation unless separate recovery is armed
    if original == OOM_BIAS_TARGET {
        return Ok(None);
    }
    ensure!(
        process_sample_is_current(sample)
            && group_for(Path::new("/proc"), sample.pid)? == job.group,
        "process changed before managed OOM write"
    );
    file.seek(SeekFrom::Start(0))?;
    file.write_all(format!("{OOM_BIAS_TARGET}\n").as_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    text.clear();
    file.read_to_string(&mut text)?;
    ensure!(
        text.trim().parse::<i32>()? == OOM_BIAS_TARGET,
        "managed OOM apply verification failed; lifetime debt retained"
    );
    Ok(Some((original, OOM_BIAS_TARGET)))
}

async fn drain_one(root: &Path, id: &str, force: bool, exempt: &HashSet<String>) -> Result<bool> {
    let _lock = oom_job::locked(root, id)?;
    let Some(mut debt) = load_debt(root, id)? else {
        return Ok(false);
    };
    let mut job = oom_job::load(root, id)?;
    let facts = Facts {
        same_boot: job.boot_id == boot()?,
        terminal_proof: job.phase == Phase::Terminal && job.terminal_proof,
        same_admission: oom_job::same_admission(&job, &debt.admission),
        deadline_passed: force || now()? >= debt.deadline,
        draining: debt.draining,
    };
    match decide(&facts) {
        Action::Retire => {
            retire(root, &debt)?;
            return Ok(false);
        }
        Action::ArchivePreviousBoot => {
            archive_previous_boot(root, &debt)?;
            return Ok(false);
        }
        Action::Hold => return Ok(true),
        Action::Drain => {}
    }
    debt.draining = true;
    save_debt(root, &debt)?;
    job.phase = Phase::Draining;
    oom_job::save(root, &job, false)?;
    let result = async {
        let path = boundary(&job).await?;
        let pids: Vec<i32> = fs::read_to_string(path.join("cgroup.procs"))?
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        // A bounded pass is not retirement. Newborns/reparented members remain
        // covered by the job debt and future passes until sentinel proof.
        for pid in pids.into_iter().take(256) {
            if pid == job.sentinel {
                continue;
            }
            let identity = match read_proc_identity(Path::new("/proc"), pid) {
                Ok(i) => i,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            ensure!(
                !exempt.contains(&identity.comm) && !is_kernel_process(&identity.comm),
                "member currently exempt; job debt retained"
            );
            if group_for(Path::new("/proc"), pid)? != job.group {
                return Err(anyhow::anyhow!("member left admitted job; debt retained"));
            }
            oom_restore::restore(Path::new("/proc"), pid, &identity, job.baseline)
                .map_err(anyhow::Error::msg)?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    debt.attempts = debt.attempts.saturating_add(1);
    debt.last_error = result.err().map(|e| format!("{e:#}"));
    save_debt(root, &debt)?;
    if let Some(error) = &debt.last_error {
        // Report the first failure and then periodically, so a persistent
        // problem stays visible without flooding the journal.
        if debt.attempts == 1 || debt.attempts % 60 == 0 {
            let _ = writeln!(
                std::io::stderr().lock(),
                "managed OOM job {id}: restoration pending (attempt {}): {error}",
                debt.attempts
            );
        }
    }
    Ok(true)
}
/// Explicit operator reconciliation for a debt whose job boundary vanished
/// without a sentinel certificate. This never touches processes: it only
/// retires the RECORD after showing the evidence, and refuses while the
/// boundary or its sentinel is still present.
pub(crate) async fn reconcile_orphan(id: &str, confirmed: bool) -> Result<()> {
    ensure!(oom_job::valid_id(id), "invalid managed job generation");
    ensure!(confirmed, "reconciliation requires explicit confirmation");
    let root = oom_job::root()?;
    let Some(debt) = load_debt(&root, id)? else {
        anyhow::bail!("no outstanding debt for this generation")
    };
    let scope_state = cpu_limit::property(&debt.admission.unit, "LoadState")
        .await
        .unwrap_or_else(|_| String::from("unavailable"));
    let sentinel_alive = Path::new(&format!("/proc/{}", debt.admission.sentinel)).exists();
    let group = Path::new("/sys/fs/cgroup").join(debt.admission.group.trim_start_matches('/'));
    let boundary_present = group.exists();
    ensure!(
        scope_state == "not-found" || scope_state == "unavailable",
        "job scope is still known to the manager: {scope_state}"
    );
    ensure!(!boundary_present, "job cgroup still exists");
    ensure!(!sentinel_alive, "job sentinel is still running");
    let _=writeln!(std::io::stderr().lock(),"managed OOM job {id}: reconciled orphan debt (scope={scope_state}, boundary absent, sentinel gone)");
    retire(&root, &debt)?;
    Ok(())
}
/// Returns outstanding lifetime debts, not just currently visible biased roots.
pub(crate) async fn drain(force: bool, exempt: &HashSet<String>) -> Result<usize> {
    let root = oom_job::root()?;
    let mut pending = 0;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name
            .strip_suffix(".debt.json")
            .filter(|id| oom_job::valid_id(id))
        else {
            continue;
        };
        match drain_one(&root, id, force, exempt).await {
            Ok(true) => pending += 1,
            Ok(false) => {}
            Err(error) => {
                pending += 1;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "managed OOM job {id}: unresolved debt retained: {error:#}"
                );
            }
        }
    }
    Ok(pending)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn facts(
        same_boot: bool,
        terminal_proof: bool,
        same_admission: bool,
        deadline_passed: bool,
        draining: bool,
    ) -> Facts {
        Facts {
            same_boot,
            terminal_proof,
            same_admission,
            deadline_passed,
            draining,
        }
    }

    #[test]
    fn only_a_terminal_certificate_retires_a_lifetime_debt() {
        // A running lease, a clean scan, root exit and retry exhaustion all
        // keep the debt alive.
        assert_eq!(
            decide(&facts(true, false, true, false, false)),
            Action::Hold
        );
        assert_eq!(
            decide(&facts(true, false, true, true, false)),
            Action::Drain
        );
        assert_eq!(
            decide(&facts(true, false, true, false, true)),
            Action::Drain
        );
        // Unverifiable ownership drains conservatively and never retires.
        assert_eq!(
            decide(&facts(true, false, false, true, false)),
            Action::Drain
        );
        assert_eq!(
            decide(&facts(true, true, false, true, false)),
            Action::Drain
        );
        // Only the same-generation sentinel certificate retires.
        assert_eq!(decide(&facts(true, true, true, true, true)), Action::Retire);
        // A previous boot is archived, never restored into current processes.
        assert_eq!(
            decide(&facts(false, false, true, true, true)),
            Action::ArchivePreviousBoot
        );
        assert_eq!(
            decide(&facts(false, true, true, true, true)),
            Action::ArchivePreviousBoot
        );
    }

    #[test]
    fn lifetime_debt_survives_restart_and_is_never_silently_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let id = "12345678-1234-1234-1234-123456789abc";
        let pid = std::process::id() as i32;
        let admission = Job {
            version: 1,
            id: id.into(),
            boot_id: "boot".into(),
            unit: format!("dracon-managed-job-{id}.scope"),
            invocation: "inv".into(),
            group: "/x".into(),
            group_device: 1,
            group_inode: 2,
            sentinel: pid,
            sentinel_identity: read_proc_identity(Path::new("/proc"), pid).unwrap(),
            baseline: 200,
            phase: Phase::Active,
            terminal_proof: false,
        };
        let debt = Debt {
            admission: admission.clone(),
            deadline: 42,
            draining: false,
            attempts: 0,
            last_error: None,
        };
        save_debt(dir.path(), &debt).unwrap();
        // A restarted recoverer must find the debt again, not an empty set.
        let restored = load_debt(dir.path(), id)
            .unwrap()
            .expect("debt survived restart");
        assert_eq!(restored.deadline, 42);
        assert!(!restored.draining);
        // A conflicting generation cannot be retired over the existing record.
        let mut other = debt.clone();
        other.admission.baseline = 123;
        assert!(retire(dir.path(), &other).is_err());
        assert!(load_debt(dir.path(), id).unwrap().is_some());
    }

    #[test]
    fn previous_boot_debt_is_archived_and_removed_without_touching_processes() {
        let dir = tempfile::tempdir().unwrap();
        let id = "12345678-1234-1234-1234-123456789abc";
        let pid = std::process::id() as i32;
        let admission = Job {
            version: 1,
            id: id.into(),
            boot_id: "previous".into(),
            unit: format!("dracon-managed-job-{id}.scope"),
            invocation: "inv".into(),
            group: "/x".into(),
            group_device: 1,
            group_inode: 2,
            sentinel: pid,
            sentinel_identity: read_proc_identity(Path::new("/proc"), pid).unwrap(),
            baseline: 200,
            phase: Phase::Active,
            terminal_proof: false,
        };
        let debt = Debt {
            admission,
            deadline: 0,
            draining: false,
            attempts: 3,
            last_error: None,
        };
        save_debt(dir.path(), &debt).unwrap();
        archive_previous_boot(dir.path(), &debt).unwrap();
        assert!(load_debt(dir.path(), id).unwrap().is_none());
        let archived: Debt = serde_json::from_slice(
            &fs::read(dir.path().join(format!("{id}.previous-boot.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(archived.attempts, 3);
    }

    #[test]
    fn unregistered_workloads_are_not_treated_as_managed_jobs() {
        assert!(!id_for("/user.slice/app-foo.scope").is_ok());
        assert!(!id_for("/user.slice/dracon-managed-job-nope.scope").is_ok());
        assert!(!id_for("/user.slice/dracon-managed-job-.scope").is_ok());
        assert_eq!(
            id_for("/user.slice/dracon-managed-job-12345678-1234-1234-1234-123456789abc.scope")
                .unwrap(),
            "12345678-1234-1234-1234-123456789abc"
        );
    }
}

pub(crate) async fn recover_loop() -> Result<()> {
    loop {
        // Read the same configured exemptions on every pass; no privilege or
        // global same-score sweep substitutes for admitted membership.
        let (_, policy) = load_system_policy()?;
        let exempt = parse_kinds(&policy.guard.process_exempt_names);
        match drain(false, &exempt).await {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "managed OOM recovery pending: {error:#}"
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
