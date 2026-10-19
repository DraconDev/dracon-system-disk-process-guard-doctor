//! Cooperative launch-time provenance. This registry is not proof of enforced
//! containment: participating jobs must not migrate, change OOM policy, create
//! delegated subgroups, or exec exempt roles inside a biased job.
//! Runtime steering/recovery integration is a separate repair step.
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Admitted,
    Active,
    Draining,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Job {
    pub version: u32,
    pub id: String,
    pub boot_id: String,
    pub unit: String,
    pub invocation: String,
    pub group: String,
    pub group_device: u64,
    pub group_inode: u64,
    pub sentinel: i32,
    pub sentinel_identity: super::ProcessIdentity,
    pub baseline: i32,
    pub phase: Phase,
    /// Only the single-threaded, launch-closed sentinel may issue this proof.
    pub terminal_proof: bool,
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
pub(crate) fn root() -> Result<PathBuf> {
    use std::{
        ffi::CStr,
        os::unix::ffi::OsStrExt,
    };
    // Do not let a job's HOME override split admission and recovery registries.
    let uid = unsafe { libc::geteuid() };
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    ensure!(
        rc == 0 && !found.is_null(),
        "cannot resolve canonical user home"
    );
    let pwd = unsafe { pwd.assume_init() };
    ensure!(!pwd.pw_dir.is_null(), "user home unavailable");
    let home = Path::new(std::ffi::OsStr::from_bytes(
        unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes(),
    ));
    initialize_registry(home, uid, &mut |path| fs::File::open(path)?.sync_all())
}
fn initialize_registry(home:&Path, uid:u32, sync:&mut impl FnMut(&Path)->std::io::Result<()>) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let parent=home.join(".dracon");
    let root=parent.join("oom-jobs");
    fs::create_dir_all(&root)?;
    let metadata=fs::symlink_metadata(&root)?;
    ensure!(metadata.is_dir() && metadata.uid()==uid,"unsafe managed-job state directory");
    fs::set_permissions(&root,fs::Permissions::from_mode(0o700))?;
    // Admission must not outlive a newly initialized directory pathname.
    // Persist both links in the short directory chain before exposing it.
    for path in [&root,&parent,home] { sync(path)?; }
    Ok(root)
}
fn report_terminal_error(writer:&mut impl Write,id:&str,error:&anyhow::Error) {
    // A full filesystem or closed stderr must never kill the proof writer.
    let _=writeln!(writer,"managed-job {id}: terminal proof unresolved; retaining sentinel and retrying: {error:#}");
}
fn property(unit: &str, name: &str) -> Result<String> {
    let bin = super::resolve_bin_strict("systemctl")?;
    let out = Command::new(bin)
        .args(["--user", "show", unit, "--property", name, "--value"])
        .output()?;
    ensure!(
        out.status.success(),
        "manager failed reading {name} for {unit}"
    );
    Ok(String::from_utf8(out.stdout)?.trim().into())
}
pub(crate) fn locked(root: &Path, id: &str) -> Result<fs::File> {
    ensure!(valid_id(id), "invalid job generation");
    fs::create_dir_all(root)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{id}.lock")))?;
    FileExt::try_lock_exclusive(&f).context("managed job state busy")?;
    Ok(f)
}
pub(crate) fn save(root: &Path, job: &Job, initial: bool) -> Result<()> {
    ensure!(valid_id(&job.id), "invalid job generation");
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(&serde_json::to_vec(job)?)?;
    temp.as_file().sync_all()?;
    let destination = root.join(format!("{}.json", job.id));
    if initial {
        temp.persist_noclobber(destination).map_err(|e| e.error)?;
    } else {
        temp.persist(destination).map_err(|e| e.error)?;
    }
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
pub(crate) fn load(root: &Path, id: &str) -> Result<Job> {
    ensure!(valid_id(id), "invalid job generation");
    let job: Job = serde_json::from_slice(&fs::read(root.join(format!("{id}.json")))?)?;
    ensure!(
        job.version == 1 && job.id == id,
        "unsupported or mismatched job record"
    );
    Ok(job)
}

/// Runs only the newly requested command, never adopts a pre-existing process.
pub(crate) fn launch(command: &[OsString]) -> Result<()> {
    ensure!(
        !command.is_empty(),
        "managed-job requires a command after --"
    );
    let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    ensure!(valid_id(&id), "kernel returned invalid job generation");
    let unit = format!("dracon-managed-job-{id}.scope");
    let run = super::resolve_bin_strict("systemd-run")?;
    let status = Command::new(run)
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--property=Delegate=no",
            "--unit",
            &unit,
            "--",
        ])
        .arg(std::env::current_exe()?)
        .arg("managed-job-enter")
        .arg(id)
        .arg("--")
        .args(command)
        .status()?;
    successful(status)
}
#[derive(Debug)]
pub(crate) struct JobExit(pub i32);
impl std::fmt::Display for JobExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "managed job exited with status {}", self.0)
    }
}
impl std::error::Error for JobExit {}
fn successful(status: ExitStatus) -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    if status.success() {
        Ok(())
    } else {
        Err(JobExit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        )
        .into())
    }
}
fn read_members(group: &Path) -> Result<Vec<i32>> {
    fs::read_to_string(group.join("cgroup.procs"))?
        .split_whitespace()
        .map(|p| p.parse().map_err(Into::into))
        .collect()
}
pub(crate) fn no_nested_groups(group: &Path) -> Result<()> {
    for entry in fs::read_dir(group)? {
        ensure!(
            !entry?.file_type()?.is_dir(),
            "nested job cgroups are unsupported"
        );
    }
    Ok(())
}
fn terminal_candidate(sentinel: i32, members: &[i32], tasks: u64, own_threads: usize) -> bool {
    own_threads == 1 && tasks == 1 && members == [sentinel]
}

pub(crate) fn same_admission(left: &Job, right: &Job) -> bool {
    let mut identity = left.clone();
    identity.phase = right.phase;
    identity.terminal_proof = right.terminal_proof;
    identity == *right
}

/// Must be called before creating a runtime or any threads. Keep a non-forking
/// sentinel alive until terminal evidence is durable; clean scans alone never
/// allow the guard/recoverer to retire a job debt.
pub(crate) fn enter(id: &str, command: &[OsString]) -> Result<()> {
    ensure!(
        valid_id(id) && !command.is_empty(),
        "invalid managed job entry"
    );
    ensure!(
        fs::read_dir("/proc/self/task")?.count() == 1,
        "job sentinel must be single-threaded"
    );
    let pid = std::process::id() as i32;
    let unit = format!("dracon-managed-job-{id}.scope");
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let group = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("unified cgroup unavailable")?
        .to_owned();
    ensure!(
        group.ends_with(&format!("/{unit}")),
        "sentinel is outside its new job scope"
    );
    ensure!(
        property(&unit, "Delegate")? == "no",
        "delegated jobs are unsupported"
    );
    let invocation = property(&unit, "InvocationID")?;
    ensure!(
        !invocation.is_empty() && property(&unit, "ActiveState")? == "active",
        "job scope is not active"
    );
    let directory = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    no_nested_groups(&directory)?;
    let initial_members = read_members(&directory)?;
    let initial_tasks = fs::read_to_string(directory.join("pids.current"))?
        .trim()
        .parse::<u64>()?;
    ensure!(
        terminal_candidate(
            pid,
            &initial_members,
            initial_tasks,
            fs::read_dir("/proc/self/task")?.count()
        ),
        "new job scope already contains another task"
    );
    ensure!(
        property(&unit, "ControlGroup")? == group,
        "manager cgroup does not match admission boundary"
    );
    let baseline = fs::read_to_string("/proc/self/oom_score_adj")?
        .trim()
        .parse::<i32>()?;
    ensure!(
        (-499..250).contains(&baseline),
        "job baseline is protected or cannot be safely biased"
    );
    let meta = fs::metadata(&directory)?;
    let root = root()?;
    let job = Job {
        version: 1,
        id: id.into(),
        boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into(),
        unit,
        invocation,
        group,
        group_device: meta.dev(),
        group_inode: meta.ino(),
        sentinel: pid,
        sentinel_identity: super::read_proc_identity(Path::new("/proc"), pid)?,
        baseline,
        phase: Phase::Admitted,
        terminal_proof: false,
    };
    {
        let _lock = locked(&root, id)?;
        save(&root, &job, true)?;
    }
    // Admission is durable before the first workload instruction. The command
    // inherits cwd, environment, standard descriptors and the known baseline.
    ensure!(
        fs::read_to_string("/proc/self/oom_score_adj")?
            .trim()
            .parse::<i32>()?
            == baseline,
        "baseline changed before workload launch"
    );
    // Even an exec/spawn failure closes admission through the same durable
    // empty-boundary proof, rather than leaving an orphan registration.
    let status = Command::new(&command[0])
        .args(&command[1..])
        .spawn()
        .map_err(|error| -> anyhow::Error {
            match error.kind() {
                std::io::ErrorKind::NotFound => JobExit(127).into(),
                std::io::ErrorKind::PermissionDenied => JobExit(126).into(),
                _ => error.into(),
            }
        })
        .and_then(|mut child| child.wait().map_err(Into::into));
    let mut failures = 0u64;
    loop {
        let attempt = (|| -> Result<bool> {
            no_nested_groups(&directory)?;
            let current = fs::metadata(&directory)?;
            ensure!(
                current.dev() == job.group_device && current.ino() == job.group_inode,
                "job cgroup was replaced"
            );
            // Read membership before the atomic charged-task count: an omitted
            // newborn must still prevent the single-task terminal certificate.
            let members = read_members(&directory)?;
            let tasks = fs::read_to_string(directory.join("pids.current"))?
                .trim()
                .parse::<u64>()?;
            let own_threads = fs::read_dir("/proc/self/task")?.count();
            if !terminal_candidate(pid, &members, tasks, own_threads) {
                return Ok(false);
            }
            let _lock = locked(&root, id)?;
            let mut final_job = load(&root, id)?;
            ensure!(same_admission(&final_job, &job), "job ownership changed");
            final_job.phase = Phase::Terminal;
            final_job.terminal_proof = true;
            save(&root, &final_job, false)?;
            Ok(true)
        })();
        match attempt {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                if failures.is_multiple_of(120) {
                    report_terminal_error(&mut std::io::stderr().lock(),id,&error);
                }
                failures = failures.saturating_add(1);
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    successful(status?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_use_registry_links_are_synced_before_admission_and_fail_closed() {
        for fail_at in 0..=3 {
            let home=tempfile::tempdir().unwrap();
            let mut order=Vec::new();
            let result=initialize_registry(home.path(),unsafe {libc::geteuid()},&mut |path| {
                order.push(path.to_owned());
                if order.len()==fail_at {return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));}
                fs::File::open(path)?.sync_all()
            });
            assert_eq!(result.is_ok(),fail_at==0);
            assert_eq!(order[0],home.path().join(".dracon/oom-jobs"));
            if fail_at==0 { assert_eq!(order,vec![home.path().join(".dracon/oom-jobs"),home.path().join(".dracon"),home.path().to_owned()]); }
            else { assert_eq!(order.len(),fail_at); }
            assert!(fs::read_dir(home.path().join(".dracon/oom-jobs")).unwrap().next().is_none());
        }
    }

    #[test]
    fn failed_retry_diagnostic_does_not_interrupt_terminal_retry() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self,_:&[u8])->std::io::Result<usize> {Err(std::io::Error::from_raw_os_error(libc::ENOSPC))}
            fn flush(&mut self)->std::io::Result<()> {Ok(())}
        }
        let mut retried=0;
        for _ in 0..2 {
            let error=anyhow::anyhow!("injected terminal publication failure");
            report_terminal_error(&mut Broken,"test-job",&error);
            retried+=1;
        }
        assert_eq!(retried,2);
    }

    #[test]
    fn terminal_evidence_excludes_surviving_descendants_and_multithreaded_sentinels() {
        assert!(terminal_candidate(10, &[10], 1, 1));
        assert!(!terminal_candidate(10, &[10, 20], 2, 1));
        assert!(!terminal_candidate(10, &[10], 2, 1)); // newborn omitted by procs scan
        assert!(!terminal_candidate(10, &[10], 2, 2));
        assert!(!terminal_candidate(10, &[], 0, 1));
        assert!(!terminal_candidate(10, &[20], 1, 1));
    }
    #[test]
    fn command_exit_codes_are_preserved_instead_of_becoming_infrastructure_errors() {
        use std::os::unix::process::ExitStatusExt;
        assert!(successful(ExitStatus::from_raw(0)).is_ok());
        for code in [1, 42, 126, 127] {
            let error = successful(ExitStatus::from_raw(code << 8)).unwrap_err();
            assert_eq!(super::super::exit_status_for_error(&error), code);
        }
        let error = successful(ExitStatus::from_raw(libc::SIGTERM)).unwrap_err();
        assert_eq!(
            super::super::exit_status_for_error(&error),
            128 + libc::SIGTERM
        );
    }

    #[test]
    fn admission_is_durable_and_cannot_replace_an_existing_generation() {
        let dir = tempfile::tempdir().unwrap();
        let id = "12345678-1234-1234-1234-123456789abc";
        let pid = std::process::id() as i32;
        let job = Job {
            version: 1,
            id: id.into(),
            boot_id: "boot".into(),
            unit: format!("dracon-managed-job-{id}.scope"),
            invocation: "invocation".into(),
            group: "/test".into(),
            group_device: 1,
            group_inode: 2,
            sentinel: pid,
            sentinel_identity: super::super::read_proc_identity(Path::new("/proc"), pid).unwrap(),
            baseline: 200,
            phase: Phase::Admitted,
            terminal_proof: false,
        };
        save(dir.path(), &job, true).unwrap();
        assert!(save(dir.path(), &job, true).is_err());
        let mut draining = load(dir.path(), id).unwrap();
        draining.phase = Phase::Draining;
        save(dir.path(), &draining, false).unwrap();
        assert_eq!(load(dir.path(), id).unwrap().phase, Phase::Draining);
        assert!(!load(dir.path(), id).unwrap().terminal_proof);
        assert!(same_admission(&draining, &job));
        draining.baseline += 1;
        assert!(!same_admission(&draining, &job));
        draining.baseline -= 1;
        draining.group_inode += 1;
        assert!(!same_admission(&draining, &job));
    }

    #[test]
    fn generation_cannot_be_a_path_or_reused_registration() {
        assert!(valid_id("12345678-1234-1234-1234-123456789abc"));
        for id in ["../job", "", "12345678/1234-1234-1234-123456789abc"] {
            assert!(!valid_id(id));
        }
        let dir = tempfile::tempdir().unwrap();
        let id = "12345678-1234-1234-1234-123456789abc";
        fs::write(
            dir.path().join(format!("{id}.json")),
            b"corrupt existing obligation",
        )
        .unwrap();
        assert!(load(dir.path(), id).is_err());
        let first = locked(dir.path(), id).unwrap();
        assert!(locked(dir.path(), id).is_err());
        drop(first);
        assert_eq!(
            fs::read(dir.path().join(format!("{id}.json"))).unwrap(),
            b"corrupt existing obligation"
        );
    }
}
