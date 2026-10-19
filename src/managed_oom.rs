//! Cooperative registered-job OOM leases. Process ancestry is never authority.
use super::{oom_job::{self,Job,Phase}, *};
use std::{io::{Read,Seek,SeekFrom,Write},os::unix::fs::MetadataExt,time::{SystemTime,UNIX_EPOCH}};

const RECOVERER:&str="dracon-system-oom-recovery.service";
const LEASE_SECS:u64=120;
#[derive(Debug,Clone,Serialize,Deserialize)]
struct Debt {
    admission:Job,
    deadline:u64,
    draining:bool,
    attempts:u64,
    last_error:Option<String>,
}
fn now()->Result<u64> {Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())}
fn boot()->Result<String> {Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().into())}
fn group_for(proc_root:&Path,pid:i32)->Result<String> {
    fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup"))?.lines().find_map(|l|l.strip_prefix("0::")).map(str::to_owned).context("unified process cgroup unavailable")
}
fn id_for(group:&str)->Result<&str> {
    let name=group.rsplit('/').next().unwrap_or_default();
    let id=name.strip_prefix("dracon-managed-job-").and_then(|s|s.strip_suffix(".scope")).context("unsupported OOM workload: no managed launch admission")?;
    ensure!(oom_job::valid_id(id),"invalid managed job generation");
    Ok(id)
}
fn debt_path(root:&Path,id:&str)->PathBuf {root.join(format!("{id}.debt.json"))}
fn load_debt(root:&Path,id:&str)->Result<Option<Debt>> {
    let bytes=match fs::read(debt_path(root,id)) {Ok(b)=>b,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e.into())};
    let debt:Debt=serde_json::from_slice(&bytes)?;
    ensure!(debt.admission.id==id && debt.admission.version==1,"mismatched OOM debt generation");
    Ok(Some(debt))
}
fn save_debt(root:&Path,debt:&Debt)->Result<()> {
    ensure!(oom_job::valid_id(&debt.admission.id),"invalid OOM debt generation");
    let mut temp=tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(&serde_json::to_vec(debt)?)?;
    temp.as_file().sync_all()?;
    temp.persist(debt_path(root,&debt.admission.id)).map_err(|e|e.error)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
fn retire(root:&Path,debt:&Debt)->Result<()> {
    let destination=root.join(format!("{}.retired.json",debt.admission.id));
    if destination.exists() {
        let previous:Debt=serde_json::from_slice(&fs::read(&destination)?)?;
        ensure!(oom_job::same_admission(&previous.admission,&debt.admission),"conflicting retired OOM generation");
    }
    fs::rename(debt_path(root,&debt.admission.id),destination)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
async fn boundary(job:&Job)->Result<PathBuf> {
    ensure!(job.version==1 && oom_job::valid_id(&job.id) && id_for(&job.group)?==job.id,"invalid OOM admission");
    ensure!((-499..250).contains(&job.baseline),"unsupported OOM baseline");
    ensure!(job.boot_id==boot()?,"OOM job belongs to previous boot");
    ensure!(cpu_limit::property(&job.unit,"InvocationID").await.map_err(anyhow::Error::msg)?==job.invocation && !job.invocation.is_empty(),"OOM job invocation unavailable or replaced");
    ensure!(cpu_limit::property(&job.unit,"ControlGroup").await.map_err(anyhow::Error::msg)?==job.group,"OOM job cgroup changed");
    ensure!(cpu_limit::property(&job.unit,"Delegate").await.map_err(anyhow::Error::msg)?=="no","delegated OOM job is unsupported");
    ensure!(!Path::new(&job.group).components().any(|c| matches!(c,std::path::Component::ParentDir|std::path::Component::CurDir)),"invalid job cgroup path");
    let path=Path::new("/sys/fs/cgroup").join(job.group.trim_start_matches('/'));
    let m=fs::metadata(&path)?;
    ensure!(m.dev()==job.group_device && m.ino()==job.group_inode,"OOM job cgroup incarnation changed");
    oom_job::no_nested_groups(&path)?;
    Ok(path)
}
async fn ensure_recoverer(root:&Path)->Result<()> {
    let state=cpu_limit::property(RECOVERER,"ActiveState").await.unwrap_or_default();
    if state!="active" {
        let run=resolve_bin_strict("systemd-run")?;
        let args=vec!["--user".into(),"--collect".into(),format!("--unit={RECOVERER}"),
            "--property=Restart=on-failure".into(),"--property=RestartSec=2s".into(),"--property=StartLimitIntervalSec=0".into(),
            "--property=NoNewPrivileges=yes".into(),"--property=CapabilityBoundingSet=".into(),"--property=ProtectControlGroups=yes".into(),
            "--property=ProtectSystem=strict".into(),"--property=ProtectHome=read-only".into(),format!("--property=ReadWritePaths={}",root.display()),
            "--property=PrivateTmp=yes".into(),"--property=RestrictSUIDSGID=yes".into(),"--".into(),std::env::current_exe()?.into_os_string().into_string().map_err(|_|anyhow::anyhow!("executable path is not UTF-8"))?,"oom-job-recover".into()];
        if let Err(start_error)=cpu_limit::output(&run,&args).await {
            ensure!(cpu_limit::property(RECOVERER,"ActiveState").await.unwrap_or_default()=="active","independent OOM recovery could not start: {start_error}");
        }
    }
    ensure!(cpu_limit::property(RECOVERER,"ActiveState").await.map_err(anyhow::Error::msg)?=="active","independent OOM recoverer is not active");
    let pid=cpu_limit::property(RECOVERER,"MainPID").await.map_err(anyhow::Error::msg)?.parse::<i32>()?;
    ensure!(pid>0,"independent OOM recoverer has no process");
    let expected=fs::metadata(std::env::current_exe()?)?;
    let actual=fs::metadata(format!("/proc/{pid}/exe"))?;
    ensure!(expected.dev()==actual.dev() && expected.ino()==actual.ino(),"independent OOM recoverer runs a different binary");
    let cmd=fs::read(format!("/proc/{pid}/cmdline"))?;
    let args:Vec<_>=cmd.split(|b|*b==0).filter(|s|!s.is_empty()).collect();
    ensure!(args.len()==2 && args[1]==b"oom-job-recover","independent recovery command identity differs");
    Ok(())
}

pub(crate) async fn bias(sample:&ProcSample,exempt:&HashSet<String>)->Result<Option<(i32,i32)>,String> {
    bias_inner(sample,exempt).await.map_err(|e|format!("{e:#}"))
}
async fn bias_inner(sample:&ProcSample,exempt:&HashSet<String>)->Result<Option<(i32,i32)>> {
    ensure!(process_sample_is_current(sample),"process incarnation changed before managed OOM application");
    let group=group_for(Path::new("/proc"),sample.pid)?;
    let id=id_for(&group)?;
    let root=oom_job::root()?;
    let _lock=oom_job::locked(&root,id)?;
    let mut job=oom_job::load(&root,id)?;
    ensure!(!job.terminal_proof && job.phase!=Phase::Terminal,"managed job has terminated");
    ensure!(job.group==group,"process is outside its admitted job");
    boundary(&job).await?;
    ensure!(sample.pid!=job.sentinel && matches!(process_identity_status(Path::new("/proc"),job.sentinel,&job.sentinel_identity),ProcessIdentityStatus::Match),"sentinel is exempt or cannot be verified");
    let current=read_proc_identity(Path::new("/proc"),sample.pid)?;
    ensure!(current.starttime==sample.starttime && !exempt.contains(&current.comm) && !is_kernel_process(&current.comm),"current process is exempt or changed");
    let mut file=fs::OpenOptions::new().read(true).write(true).open(format!("/proc/{}/oom_score_adj",sample.pid))?;
    ensure!(process_sample_is_current(sample) && group_for(Path::new("/proc"),sample.pid)?==job.group,"process changed after descriptor open");
    let mut text=String::new(); file.read_to_string(&mut text)?;
    let original=text.trim().parse::<i32>()?;
    let previous=load_debt(&root,id)?;
    if let Some(ref debt)=previous {ensure!(oom_job::same_admission(&debt.admission,&job) && !debt.draining,"managed OOM job is draining or generation changed");}
    ensure!(original==job.baseline || (original==OOM_BIAS_TARGET && previous.is_some()),"independent or unsupported OOM score change");
    let mut debt=previous.unwrap_or(Debt {admission:job.clone(),deadline:0,draining:false,attempts:0,last_error:None});
    debt.deadline=now()?.saturating_add(LEASE_SECS);
    save_debt(&root,&debt)?; // write-ahead lifetime debt, never per-root authority
    job.phase=Phase::Active; oom_job::save(&root,&job,false)?;
    ensure_recoverer(&root).await?; // no mutation unless separate recovery is armed
    if original==OOM_BIAS_TARGET {return Ok(None);}
    ensure!(process_sample_is_current(sample) && group_for(Path::new("/proc"),sample.pid)?==job.group,"process changed before managed OOM write");
    file.seek(SeekFrom::Start(0))?;
    file.write_all(format!("{OOM_BIAS_TARGET}\n").as_bytes())?;
    file.seek(SeekFrom::Start(0))?; text.clear(); file.read_to_string(&mut text)?;
    ensure!(text.trim().parse::<i32>()?==OOM_BIAS_TARGET,"managed OOM apply verification failed; lifetime debt retained");
    Ok(Some((original,OOM_BIAS_TARGET)))
}

async fn drain_one(root:&Path,id:&str,force:bool,exempt:&HashSet<String>)->Result<bool> {
    let _lock=oom_job::locked(root,id)?;
    let Some(mut debt)=load_debt(root,id)? else {return Ok(false)};
    let mut job=oom_job::load(root,id)?;
    ensure!(oom_job::same_admission(&job,&debt.admission),"OOM admission identity changed; debt retained");
    if job.boot_id!=boot()? || (job.phase==Phase::Terminal && job.terminal_proof) {retire(root,&debt)?;return Ok(false);}
    if !force && !debt.draining && now()?<debt.deadline {return Ok(true);}
    debt.draining=true; save_debt(root,&debt)?;
    job.phase=Phase::Draining; oom_job::save(root,&job,false)?;
    let result=async {
        let path=boundary(&job).await?;
        let pids:Vec<i32>=fs::read_to_string(path.join("cgroup.procs"))?.split_whitespace().map(str::parse).collect::<std::result::Result<_,_>>()?;
        // A bounded pass is not retirement. Newborns/reparented members remain
        // covered by the job debt and future passes until sentinel proof.
        for pid in pids.into_iter().take(256) {
            if pid==job.sentinel {continue;}
            let identity=match read_proc_identity(Path::new("/proc"),pid) {Ok(i)=>i,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>continue,Err(e)=>return Err(e.into())};
            ensure!(!exempt.contains(&identity.comm) && !is_kernel_process(&identity.comm),"member currently exempt; job debt retained");
            if group_for(Path::new("/proc"),pid)?!=job.group {return Err(anyhow::anyhow!("member left admitted job; debt retained"));}
            oom_restore::restore(Path::new("/proc"),pid,&identity,job.baseline).map_err(anyhow::Error::msg)?;
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    debt.attempts=debt.attempts.saturating_add(1);
    debt.last_error=result.err().map(|e|format!("{e:#}"));
    save_debt(root,&debt)?;
    if let Some(error)=&debt.last_error {let _=writeln!(std::io::stderr().lock(),"managed OOM job {id}: restoration pending: {error}");}
    Ok(true)
}
/// Returns outstanding lifetime debts, not just currently visible biased roots.
pub(crate) async fn drain(force:bool,exempt:&HashSet<String>)->Result<usize> {
    let root=oom_job::root()?;
    let mut pending=0;
    for entry in fs::read_dir(&root)? {
        let entry=entry?;
        let name=entry.file_name(); let name=name.to_string_lossy();
        let Some(id)=name.strip_suffix(".debt.json").filter(|id|oom_job::valid_id(id)) else {continue};
        match drain_one(&root,id,force,exempt).await {
            Ok(true)=>pending+=1,Ok(false)=>{},
            Err(error)=>{pending+=1;let _=writeln!(std::io::stderr().lock(),"managed OOM job {id}: unresolved debt retained: {error:#}");}
        }
    }
    Ok(pending)
}
pub(crate) async fn recover_loop()->Result<()> {
    loop {
        // Read the same configured exemptions on every pass; no privilege or
        // global same-score sweep substitutes for admitted membership.
        let policy=load_system_policy()?;
        let exempt:HashSet<_>=policy.guard.memory_exempt_processes.iter().cloned().collect();
        match drain(false,&exempt).await {Ok(0)=>return Ok(()),Ok(_)=>{},Err(error)=>{let _=writeln!(std::io::stderr().lock(),"managed OOM recovery pending: {error:#}");}}
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
