//! Bounded live diagnostics. Only this command's disposable worker scope is
//! modified. It exercises production limiter functions under the caller's
//! actual service sandbox; it never manufactures host memory pressure.
use super::*;
use serde_json::json;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct ProbeState {
    pid: i32,
    child: i32,
    unit: String,
    group: String,
    key: String,
    generation: String,
    boot_id: String,
    worker_identity: ProcessIdentity,
    child_identity: ProcessIdentity,
    invocation: String,
    original_oom: i32,
}

fn verify_incarnations(state: &ProbeState, proc_root: &Path, boot: &str) -> Result<()> {
    anyhow::ensure!(
        state.boot_id == boot.trim(),
        "probe belongs to a different boot"
    );
    for (pid, identity) in [
        (state.pid, &state.worker_identity),
        (state.child, &state.child_identity),
    ] {
        anyhow::ensure!(
            matches!(
                process_identity_status(proc_root, pid, identity),
                ProcessIdentityStatus::Match
            ),
            "probe process {pid} exited, was reused, or cannot be verified"
        );
    }
    Ok(())
}

fn value(pid: i32) -> Result<i32> {
    Ok(fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))?
        .trim()
        .parse()?)
}
fn ticks(pid: i32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("bad stat"))?
        .1
        .split_whitespace()
        .collect();
    Ok(fields[11].parse::<u64>()? + fields[12].parse::<u64>()?)
}
async fn wait_file(path: &Path) -> Result<()> {
    for _ in 0..60 {
        if path.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("probe timed out waiting for {}", path.display())
}
fn checked<T>(r: std::result::Result<T, String>) -> Result<T> {
    r.map_err(anyhow::Error::msg)
}
/// Managed-job path: a disposable admitted job is biased through the
/// registered-job lease, then its own lifetime debt restores it. Only the
/// disposable job is touched.
async fn exercise_managed(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    anyhow::ensure!(
        !root.join("state.json").exists(),
        "use a fresh probe directory"
    );
    let exe = std::env::current_exe()?;
    // Launch a disposable admitted job whose workload exits on its own stop
    // marker, so the sentinel can certify an empty boundary afterwards.
    let mut child = tokio::process::Command::new(&exe)
        .args(["managed-job", "--"])
        .arg(&exe)
        .args(["protection-worker", root.to_str().unwrap_or_default()])
        .kill_on_drop(true)
        .spawn()?;
    let launcher = child.id().unwrap_or_default() as i32;
    let mut admitted = false;
    for _ in 0..60 {
        if let Ok(entries) = fs::read_dir(oom_job::root()?) {
            if entries.flatten().any(|entry| {
                entry.file_name().to_string_lossy().ends_with(".json")
                    && !entry.file_name().to_string_lossy().contains("debt")
            }) {
                admitted = true;
                break;
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(admitted, "no managed job admission appeared");
    // Find the admitted job's registered workload member. The sentinel is
    // never a bias target.
    let mut target = None;
    for _ in 0..80 {
        let samples = process_samples().await?;
        for sample in &samples {
            let Ok(id) = managed_oom::job_id_for_process(sample.pid) else {
                continue;
            };
            let Ok(job) = oom_job::load(&oom_job::root()?, &id) else {
                continue;
            };
            if sample.pid == job.sentinel {
                continue;
            }
            target = Some(sample.clone());
            break;
        }
        if target.is_some() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let sample =
        target.ok_or_else(|| anyhow::anyhow!("no admitted managed job member appeared"))?;
    let job_id = managed_oom::job_id_for_process(sample.pid)?;
    let baseline = fs::read_to_string(format!("/proc/{}/oom_score_adj", sample.pid))?
        .trim()
        .parse::<i32>()?;
    let exempt = HashSet::new();
    let applied = managed_oom::bias(&sample, &exempt)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(applied.is_some(), "managed OOM bias was not applied");
    let biased = fs::read_to_string(format!("/proc/{}/oom_score_adj", sample.pid))?
        .trim()
        .parse::<i32>()?;
    anyhow::ensure!(
        biased == OOM_BIAS_TARGET,
        "managed bias did not reach the target"
    );
    // The lifetime debt restores the job's baseline. The independent
    // recoverer owns this; the probe forces a pass to observe it promptly.
    let mut restored = false;
    for _ in 0..300 {
        let current = fs::read_to_string(format!("/proc/{}/oom_score_adj", sample.pid))?
            .trim()
            .parse::<i32>()
            .unwrap_or(biased);
        if current == baseline {
            restored = true;
            break;
        }
        managed_oom::drain(true, &exempt).await.ok();
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(restored, "managed OOM lifetime debt was not restored");
    fs::write(root.join("stop"), "")?;
    // The workload exits on the marker; the sentinel then certifies the empty
    // boundary and the lifetime debt retires against that certificate.
    let status = tokio::time::timeout(Duration::from_secs(120), child.wait()).await;
    let mut retired = false;
    for _ in 0..200 {
        if oom_job::root()
            .ok()
            .map(|root| !root.join(format!("{job_id}.debt.json")).exists())
            .unwrap_or(false)
        {
            retired = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(status.is_ok(), "managed job launcher did not finish");
    anyhow::ensure!(
        retired,
        "lifetime debt was not retired against the terminal certificate"
    );
    receipt(
        root,
        "managed",
        json!({"baseline":baseline,"biased":biased,"restored":restored,"retired":retired,"launcher":launcher}),
    )?;
    Ok(())
}

fn receipt(root: &Path, phase: &str, details: serde_json::Value) -> Result<()> {
    let exe = std::env::current_exe()?;
    let context = json!({"phase":phase,"pid":std::process::id(),"exe":exe,"version":env!("CARGO_PKG_VERSION"),
        "cgroup":fs::read_to_string("/proc/self/cgroup")?,"status":fs::read_to_string("/proc/self/status")?.lines().filter(|l| l.starts_with("Cap")||l.starts_with("NoNewPrivs")).collect::<Vec<_>>(),
        "cgroup_mount":fs::read_to_string("/proc/self/mountinfo")?.lines().find(|l|l.contains(" /sys/fs/cgroup ")),
        "details":details});
    fs::write(
        root.join(format!("{phase}.json")),
        serde_json::to_vec_pretty(&context)?,
    )?;
    println!("{}", serde_json::to_string(&context)?);
    Ok(())
}

pub(crate) fn worker(root: &Path, child: bool) -> Result<()> {
    let start = Instant::now();
    let mut spawned = false;
    let mut dumpable = true;
    while start.elapsed() < Duration::from_secs(600) && !root.join("stop").exists() {
        if !child && !spawned && root.join("fork").exists() {
            let process = std::process::Command::new(std::env::current_exe()?)
                .arg("protection-worker")
                .arg(root)
                .arg("--child")
                .spawn()?;
            fs::write(root.join("child.pid"), process.id().to_string())?;
            spawned = true;
        }
        if child {
            let wanted = !root.join("child-nondumpable").exists();
            if wanted != dumpable {
                if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, i32::from(wanted), 0, 0, 0) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                dumpable = wanted;
                fs::write(root.join("dumpable-state"), if wanted { "1" } else { "0" })?;
            }
        }
        if root.join("busy").exists() {
            let end = Instant::now() + Duration::from_millis(100);
            while Instant::now() < end {
                std::hint::black_box(42u64.wrapping_mul(17));
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

pub(crate) async fn run(root: &Path, phase: &str) -> Result<()> {
    fs::create_dir_all(root)?;
    if phase == "restart-check" {
        let s: ProbeState = serde_json::from_slice(&fs::read(root.join("state.json"))?)?;
        verify_incarnations(
            &s,
            Path::new("/proc"),
            &fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
        )?;
        anyhow::ensure!(
            checked(cpu_limit::property(&s.unit, "InvocationID").await)? == s.invocation,
            "probe scope was replaced"
        );
        anyhow::ensure!(
            fs::read_to_string(format!("/proc/{}/cgroup", s.child))? == s.group,
            "child ownership/cgroup changed"
        );
        let quota = checked(cpu_limit::property(&s.unit, "CPUQuotaPerSecUSec").await)?;
        anyhow::ensure!(
            quota == "infinity",
            "startup failed to restore quota: {quota}"
        );
        anyhow::ensure!(
            fs::read_to_string(format!("/proc/{}/cgroup", s.pid))? == s.group,
            "worker ownership/cgroup changed"
        );
        anyhow::ensure!(
            value(s.pid)? == s.original_oom && value(s.child)? == s.original_oom,
            "OOM values did not remain restored"
        );
        receipt(
            root,
            phase,
            json!({"quota":quota,"worker_alive":true,"child_alive":true,"cgroup_unchanged":true,"oom_restored":true}),
        )?;
        fs::write(root.join("stop"), "")?;
        return Ok(());
    }
    anyhow::ensure!(
        phase == "exercise" || phase == "managed",
        "unknown probe phase"
    );
    if phase == "managed" {
        return exercise_managed(root).await;
    }
    anyhow::ensure!(
        !root.join("state.json").exists(),
        "use a fresh probe directory"
    );
    let process = std::process::Command::new(std::env::current_exe()?)
        .arg("protection-worker")
        .arg(root)
        .spawn()?;
    let pid = process.id() as i32;
    // The disposable worker gets a known baseline instead of inheriting
    // whatever score the caller's ancestors happened to carry. Without this,
    // a caller that is itself biased cannot demonstrate the bias path at all.
    if let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .open(format!("/proc/{pid}/oom_score_adj"))
    {
        use std::io::Write;
        let _ = file.write_all(b"200\n");
    }
    // On any error, the worker still has a hard ten-minute lifetime; the
    // caller can write stop. Never signal an unverified/recycled PID.
    let result = exercise(root, pid).await;
    if result.is_err() {
        let _ = fs::write(root.join("stop"), "");
    }
    result
}

async fn exercise(root: &Path, pid: i32) -> Result<()> {
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned();
    let worker_identity = read_proc_identity(Path::new("/proc"), pid)?;
    let unit = format!("dracon-protection-test-{pid}.scope");
    checked(cpu_limit::create_probe_scope(&unit, pid as u32).await)?;
    for _ in 0..40 {
        if fs::read_to_string(format!("/proc/{pid}/cgroup"))?.contains(&unit) {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    let group = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    anyhow::ensure!(
        group.contains(&unit),
        "scope start job did not move disposable worker"
    );
    let invocation = checked(cpu_limit::property(&unit, "InvocationID").await)?;
    anyhow::ensure!(!invocation.is_empty(), "probe scope invocation missing");
    let original_oom = value(pid)?;
    let exempt = HashSet::new();
    fs::write(root.join("busy"), "")?;
    let (key, generation) = checked(cpu_limit::cap_with_duration(pid, 50, &exempt, 120).await)?;
    let quota = checked(cpu_limit::property(&unit, "CPUQuotaPerSecUSec").await)?;
    anyhow::ensure!(quota == "500ms", "unexpected quota {quota}");
    let before = ticks(pid)?;
    let start = Instant::now();
    sleep(Duration::from_secs(3)).await;
    let cpu_percent = (ticks(pid)? - before) as f64
        / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64
        / start.elapsed().as_secs_f64()
        * 100.;
    anyhow::ensure!(
        cpu_percent > 10. && cpu_percent < 75.,
        "quota did not yield bounded observed CPU: {cpu_percent}"
    );
    fs::remove_file(root.join("busy"))?;
    checked(cpu_limit::release(&key, &generation).await)?;
    anyhow::ensure!(
        checked(cpu_limit::property(&unit, "CPUQuotaPerSecUSec").await)? == "infinity",
        "release did not restore quota"
    );
    anyhow::ensure!(
        fs::read_to_string(format!("/proc/{pid}/cgroup"))? == group,
        "cap/release migrated worker"
    );
    // Preserve an already stricter limit, and do not overwrite an operator's
    // later edit while releasing a lease.
    checked(cpu_limit::set_quota(&unit, 250_000).await)?;
    anyhow::ensure!(
        cpu_limit::cap_with_duration(pid, 50, &exempt, 120)
            .await
            .is_err(),
        "stricter existing cap overwritten"
    );
    anyhow::ensure!(
        checked(cpu_limit::property(&unit, "CPUQuotaPerSecUSec").await)? == "250ms",
        "stricter quota changed"
    );
    checked(cpu_limit::set_quota(&unit, u64::MAX).await)?;
    let (key, generation) = checked(cpu_limit::cap_with_duration(pid, 50, &exempt, 120).await)?;
    checked(cpu_limit::set_quota(&unit, 300_000).await)?;
    checked(cpu_limit::release(&key, &generation).await)?;
    anyhow::ensure!(
        checked(cpu_limit::property(&unit, "CPUQuotaPerSecUSec").await)? == "300ms",
        "operator quota was overwritten"
    );
    checked(cpu_limit::set_quota(&unit, u64::MAX).await)?;
    // Independent expiry executes the installed executable, not this caller.
    let (expiry_key, expiry_generation) =
        checked(cpu_limit::cap_with_duration(pid, 50, &exempt, 2).await)?;
    let mut expired = false;
    for _ in 0..100 {
        if checked(cpu_limit::property(&unit, "CPUQuotaPerSecUSec").await)? == "infinity" {
            expired = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(expired, "independent timer did not release quota");
    checked(cpu_limit::release(&expiry_key, &expiry_generation).await)?;
    // OOM: normal inheritance, a real proc permission failure, durable retry,
    // and recovery after the target becomes readable again, without privileges.
    let samples = process_samples().await?;
    let sample = samples
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| anyhow::anyhow!("worker missing from process sample"))?;
    let mut state = GuardRuntimeState::default();
    anyhow::ensure!(
        checked(apply_oom_bias(sample, &samples, &mut state))?.is_some(),
        "OOM bias not applied"
    );
    let ledger = root.join("oom-ledger.json");
    anyhow::ensure!(
        persist_mitigations_to(&mut state, &ledger),
        "probe ledger write failed"
    );
    fs::write(root.join("fork"), "")?;
    wait_file(&root.join("child.pid")).await?;
    let child: i32 = fs::read_to_string(root.join("child.pid"))?.parse()?;
    let child_identity = read_proc_identity(Path::new("/proc"), child)?;
    anyhow::ensure!(
        value(child)? == OOM_BIAS_TARGET,
        "child did not inherit bias"
    );
    fs::write(root.join("child-nondumpable"), "")?;
    wait_file(&root.join("dumpable-state")).await?;
    anyhow::ensure!(
        fs::read_to_string(root.join("dumpable-state"))? == "0",
        "child is not nondumpable"
    );
    let samples = process_samples().await?;
    let mut recovered = GuardRuntimeState::default();
    hydrate_mitigations_from(&ledger, &mut recovered);
    for _ in 0..7 {
        sweep_stranded_oom_descendants(Path::new("/proc"), &samples, &mut recovered, &exempt);
    }
    anyhow::ensure!(
        recovered
            .oom_pending_descendants
            .keys()
            .any(|(p, _)| *p == child),
        "permission failure obligation was discarded"
    );
    anyhow::ensure!(
        persist_mitigations_to(&mut recovered, &ledger),
        "pending obligation persistence failed"
    );
    let mut after_restart = GuardRuntimeState::default();
    hydrate_mitigations_from(&ledger, &mut after_restart);
    anyhow::ensure!(
        after_restart
            .oom_pending_descendants
            .keys()
            .any(|(p, _)| *p == child),
        "restart lost unresolved child"
    );
    fs::remove_file(root.join("child-nondumpable"))?;
    for _ in 0..60 {
        if fs::read_to_string(root.join("dumpable-state"))? == "1" {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let restored = sweep_stranded_oom_descendants(
        Path::new("/proc"),
        &process_samples().await?,
        &mut after_restart,
        &exempt,
    );
    anyhow::ensure!(
        restored.deferred == 0 && value(child)? == original_oom,
        "child did not recover: deferred={} child_value={} original={} restored={:?}",
        restored.deferred,
        value(child).unwrap_or(-1),
        original_oom,
        restored.restored
    );
    checked(oom_restore::restore(
        Path::new("/proc"),
        pid,
        &process_sample_identity(sample),
        original_oom,
    ))?;
    anyhow::ensure!(value(pid)? == original_oom, "root did not recover");
    // Leave a harmless sleeping worker capped to test REAL daemon startup
    // recovery, while the separate timer remains an independent backstop.
    let (key, generation) = checked(cpu_limit::cap_with_duration(pid, 50, &exempt, 120).await)?;
    let state = ProbeState {
        pid,
        child,
        unit,
        group,
        key,
        generation,
        boot_id,
        worker_identity,
        child_identity,
        invocation,
        original_oom,
    };
    verify_incarnations(
        &state,
        Path::new("/proc"),
        &fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
    )?;
    fs::write(root.join("state.json"), serde_json::to_vec_pretty(&state)?)?;
    receipt(
        root,
        "exercise",
        json!({"worker":pid,"child":child,"sampled_cpu_percent":cpu_percent,"quota":quota,"normal_release":true,"stricter_preserved":true,"operator_edit_preserved":true,"independent_expiry":true,"oom_permission_failure_retained":true,"oom_pending_rehydrated":true,"oom_root_and_child_restored":true,"restart_probe_armed":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_receipt_requires_both_original_incarnations_and_boot() {
        let pid = std::process::id() as i32;
        let identity = read_proc_identity(Path::new("/proc"), pid).unwrap();
        let mut state = ProbeState {
            pid,
            child: pid,
            unit: "test.scope".into(),
            group: String::new(),
            key: String::new(),
            generation: String::new(),
            boot_id: "original-boot".into(),
            worker_identity: identity.clone(),
            child_identity: identity,
            invocation: "original-invocation".into(),
            original_oom: 0,
        };
        assert!(verify_incarnations(&state, Path::new("/proc"), "original-boot\n").is_ok());
        assert!(verify_incarnations(&state, Path::new("/proc"), "replacement-boot").is_err());
        state.worker_identity.starttime += 1;
        assert!(verify_incarnations(&state, Path::new("/proc"), "original-boot").is_err());
        state.worker_identity.starttime -= 1;
        state.child_identity.starttime += 1;
        assert!(verify_incarnations(&state, Path::new("/proc"), "original-boot").is_err());
        state.child = i32::MAX;
        assert!(verify_incarnations(&state, Path::new("/proc"), "original-boot").is_err());
    }
}
