//! Cooperative launch-time provenance. This registry is not proof of enforced
//! containment: participating jobs must not migrate, change OOM policy, create
//! delegated subgroups, or exec exempt roles inside a biased job.
//! Runtime steering/recovery integration is a separate repair step.
use anyhow::{bail, ensure, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, fs, io::Write, os::unix::fs::MetadataExt, path::{Path, PathBuf}, process::{Command, ExitStatus}, thread, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub(crate) enum Phase { Admitted, Active, Draining, Terminal }

#[derive(Debug, Serialize, Deserialize)]
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

pub(crate) fn valid_id(id:&str) -> bool {
    id.len()==36 && id.bytes().enumerate().all(|(i,b)| if [8,13,18,23].contains(&i) {b==b'-'} else {b.is_ascii_hexdigit()})
}
pub(crate) fn root() -> Result<PathBuf> { Ok(dirs::home_dir().context("HOME unavailable")?.join(".dracon/oom-jobs")) }
fn property(unit:&str,name:&str)->Result<String> {
    let bin=super::resolve_bin_strict("systemctl")?;
    let out=Command::new(bin).args(["--user","show",unit,"--property",name,"--value"]).output()?;
    ensure!(out.status.success(),"manager failed reading {name} for {unit}");
    Ok(String::from_utf8(out.stdout)?.trim().into())
}
fn locked(root:&Path,id:&str)->Result<fs::File> {
    ensure!(valid_id(id),"invalid job generation");
    fs::create_dir_all(root)?;
    let f=fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join(format!("{id}.lock")))?;
    FileExt::try_lock_exclusive(&f).context("managed job state busy")?;
    Ok(f)
}
pub(crate) fn save(root:&Path,job:&Job,initial:bool)->Result<()> {
    ensure!(valid_id(&job.id),"invalid job generation");
    let mut temp=tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(&serde_json::to_vec(job)?)?;
    temp.as_file().sync_all()?;
    let destination=root.join(format!("{}.json",job.id));
    if initial {temp.persist_noclobber(destination).map_err(|e|e.error)?;} else {temp.persist(destination).map_err(|e|e.error)?;}
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
pub(crate) fn load(root:&Path,id:&str)->Result<Job> {
    ensure!(valid_id(id),"invalid job generation");
    let job:Job=serde_json::from_slice(&fs::read(root.join(format!("{id}.json")))?)?;
    ensure!(job.version==1 && job.id==id,"unsupported or mismatched job record");
    Ok(job)
}

/// Runs only the newly requested command, never adopts a pre-existing process.
pub(crate) fn launch(command:&[OsString])->Result<()> {
    ensure!(!command.is_empty(),"managed-job requires a command after --");
    let id=fs::read_to_string("/proc/sys/kernel/random/uuid")?.trim().to_owned();
    ensure!(valid_id(&id),"kernel returned invalid job generation");
    let unit=format!("dracon-managed-job-{id}.scope");
    let run=super::resolve_bin_strict("systemd-run")?;
    let status=Command::new(run).args(["--user","--scope","--quiet","--same-dir","--unit",&unit,"--"])
        .arg(std::env::current_exe()?).arg("managed-job-enter").arg(id).arg("--").args(command).status()?;
    successful(status)
}
fn successful(status:ExitStatus)->Result<()> {
    if status.success() {Ok(())} else {bail!("managed job exited unsuccessfully: {status}")}
}
fn read_members(group:&Path)->Result<Vec<i32>> {
    fs::read_to_string(group.join("cgroup.procs"))?.split_whitespace().map(|p|p.parse().map_err(Into::into)).collect()
}
fn no_nested_groups(group:&Path)->Result<()> {
    for entry in fs::read_dir(group)? {ensure!(!entry?.file_type()?.is_dir(),"nested job cgroups are unsupported");}
    Ok(())
}
fn terminal_candidate(sentinel:i32,members:&[i32],tasks:u64,own_threads:usize)->bool {
    own_threads==1 && tasks==1 && members==[sentinel]
}

/// Must be called before creating a runtime or any threads. Keep a non-forking
/// sentinel alive until terminal evidence is durable; clean scans alone never
/// allow the guard/recoverer to retire a job debt.
pub(crate) fn enter(id:&str,command:&[OsString])->Result<()> {
    ensure!(valid_id(id) && !command.is_empty(),"invalid managed job entry");
    ensure!(fs::read_dir("/proc/self/task")?.count()==1,"job sentinel must be single-threaded");
    let pid=std::process::id() as i32;
    let unit=format!("dracon-managed-job-{id}.scope");
    let text=fs::read_to_string("/proc/self/cgroup")?;
    let group=text.lines().find_map(|line|line.strip_prefix("0::")).context("unified cgroup unavailable")?.to_owned();
    ensure!(group.ends_with(&format!("/{unit}")),"sentinel is outside its new job scope");
    ensure!(property(&unit,"Delegate")?=="no","delegated jobs are unsupported");
    let invocation=property(&unit,"InvocationID")?;
    ensure!(!invocation.is_empty() && property(&unit,"ActiveState")?=="active","job scope is not active");
    let directory=Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    no_nested_groups(&directory)?;
    ensure!(read_members(&directory)?==[pid],"new job scope already contains another process");
    let baseline=fs::read_to_string("/proc/self/oom_score_adj")?.trim().parse::<i32>()?;
    ensure!((-499..250).contains(&baseline),"job baseline is protected or cannot be safely biased");
    let meta=fs::metadata(&directory)?;
    let root=root()?;
    let job=Job {version:1,id:id.into(),boot_id:fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().into(),unit,invocation,group,group_device:meta.dev(),group_inode:meta.ino(),sentinel:pid,sentinel_identity:super::read_proc_identity(Path::new("/proc"),pid)?,baseline,phase:Phase::Admitted,terminal_proof:false};
    {
        let _lock=locked(&root,id)?;
        save(&root,&job,true)?;
    }
    // Admission is durable before the first workload instruction. The command
    // inherits cwd, environment, standard descriptors and the known baseline.
    let mut child=Command::new(&command[0]).args(&command[1..]).spawn()?;
    let status=child.wait()?;
    loop {
        no_nested_groups(&directory)?;
        let current=fs::metadata(&directory)?;
        ensure!(current.dev()==job.group_device && current.ino()==job.group_inode,"job cgroup was replaced");
        let members=read_members(&directory)?;
        let tasks=fs::read_to_string(directory.join("pids.current"))?.trim().parse::<u64>()?;
        let own_threads=fs::read_dir("/proc/self/task")?.count();
        if terminal_candidate(pid,&members,tasks,own_threads) {
            let _lock=locked(&root,id)?;
            let mut final_job=load(&root,id)?;
            ensure!(final_job.boot_id==job.boot_id && final_job.invocation==job.invocation && final_job.sentinel_identity.starttime==job.sentinel_identity.starttime,"job ownership changed");
            final_job.phase=Phase::Terminal;
            final_job.terminal_proof=true;
            save(&root,&final_job,false)?;
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    successful(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_evidence_excludes_surviving_descendants_and_multithreaded_sentinels() {
        assert!(terminal_candidate(10,&[10],1,1));
        assert!(!terminal_candidate(10,&[10,20],2,1));
        assert!(!terminal_candidate(10,&[10],2,1)); // newborn omitted by procs scan
        assert!(!terminal_candidate(10,&[10],2,2));
        assert!(!terminal_candidate(10,&[],0,1));
        assert!(!terminal_candidate(10,&[20],1,1));
    }
    #[test]
    fn generation_cannot_be_a_path_or_reused_registration() {
        assert!(valid_id("12345678-1234-1234-1234-123456789abc"));
        for id in ["../job","","12345678/1234-1234-1234-123456789abc"] { assert!(!valid_id(id)); }
        let dir=tempfile::tempdir().unwrap();
        let id="12345678-1234-1234-1234-123456789abc";
        fs::write(dir.path().join(format!("{id}.json")),b"corrupt existing obligation").unwrap();
        assert!(load(dir.path(),id).is_err());
        let first=locked(dir.path(),id).unwrap();
        assert!(locked(dir.path(),id).is_err());
        drop(first);
        assert_eq!(fs::read(dir.path().join(format!("{id}.json"))).unwrap(),b"corrupt existing obligation");
    }
}
