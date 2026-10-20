//! The probe must not force drain/retire another registered generation.
//! Temporary registry only; no user units, global HOME or production debts.
use super::*;

fn job(id: &str, terminal: bool) -> Job {
    let pid = std::process::id() as i32;
    Job {
        version: 1,
        id: id.into(),
        boot_id: boot().unwrap(),
        unit: format!("dracon-managed-job-{id}.scope"),
        invocation: format!("inv-{id}"),
        group: format!("/fixture/dracon-managed-job-{id}.scope"),
        group_device: 1,
        group_inode: 2,
        sentinel: pid,
        sentinel_identity: read_proc_identity(Path::new("/proc"), pid).unwrap(),
        baseline: 200,
        phase: if terminal {
            Phase::Terminal
        } else {
            Phase::Active
        },
        terminal_proof: terminal,
    }
}
fn publish(root: &Path, job: &Job) {
    oom_job::save(root, job, true).unwrap();
    save_debt(
        root,
        &Debt {
            admission: job.clone(),
            deadline: u64::MAX,
            draining: false,
            attempts: 7,
            last_error: Some("unrelated retained error".into()),
        },
    )
    .unwrap();
}
fn bytes(root: &Path, name: &str) -> Vec<u8> {
    fs::read(root.join(name)).unwrap()
}

#[tokio::test]
async fn scoped_probe_restoration_and_retirement_leave_other_jobs_and_debts_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let own = job("11111111-1111-1111-1111-111111111111", true);
    let other = job("22222222-2222-2222-2222-222222222222", false);
    // A second terminal debt makes a regression to global drain immediately
    // observable without requiring a real manager or live OOM writes.
    let other_terminal = job("33333333-3333-3333-3333-333333333333", true);
    for j in [&own, &other, &other_terminal] {
        publish(root, j);
    }
    let names: Vec<_> = [&other, &other_terminal]
        .into_iter()
        .flat_map(|j| [format!("{}.json", j.id), format!("{}.debt.json", j.id)])
        .collect();
    let before: Vec<_> = names.iter().map(|name| bytes(root, name)).collect();
    let pid = std::process::id();
    let score = fs::read(format!("/proc/{pid}/oom_score_adj")).unwrap();
    // An unreadable/invalid unrelated debt must not even be parsed by this API.
    let corrupt = "44444444-4444-4444-4444-444444444444.debt.json";
    fs::write(root.join(corrupt), b"unrelated corrupt debt").unwrap();
    assert!(!drain_job(root, &own, &HashSet::new()).await.unwrap());
    assert!(job_retired(root, &own).unwrap());
    for (name, original) in names.iter().zip(before.iter()) {
        assert_eq!(
            &bytes(root, name),
            original,
            "probe changed unrelated {name}"
        );
    }
    assert_eq!(bytes(root, corrupt), b"unrelated corrupt debt");
    assert_eq!(
        fs::read(format!("/proc/{pid}/oom_score_adj")).unwrap(),
        score
    );
    assert!(!root
        .join(format!("{}.retired.json", other_terminal.id))
        .exists());
    // Retry is idempotent and still cannot range over another generation.
    assert!(!drain_job(root, &own, &HashSet::new()).await.unwrap());
    for (name, original) in names.iter().zip(before.iter()) {
        assert_eq!(&bytes(root, name), original);
    }
}

#[tokio::test]
async fn replaced_probe_admission_or_debt_is_rejected_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let original = job("11111111-1111-1111-1111-111111111111", true);
    publish(dir.path(), &original);
    let mut replaced = original.clone();
    replaced.invocation = "replacement".into();
    oom_job::save(dir.path(), &replaced, false).unwrap();
    let record = bytes(dir.path(), &format!("{}.json", original.id));
    let debt = bytes(dir.path(), &format!("{}.debt.json", original.id));
    assert!(drain_job(dir.path(), &original, &HashSet::new())
        .await
        .is_err());
    assert!(job_retired(dir.path(), &original).is_err());
    assert_eq!(bytes(dir.path(), &format!("{}.json", original.id)), record);
    assert_eq!(
        bytes(dir.path(), &format!("{}.debt.json", original.id)),
        debt
    );
    // Same-generation debt replacement is also rejected before draining.
    oom_job::save(dir.path(), &original, false).unwrap();
    let mut replacement_debt = load_debt(dir.path(), &original.id).unwrap().unwrap();
    replacement_debt.admission = replaced;
    save_debt(dir.path(), &replacement_debt).unwrap();
    let before = bytes(dir.path(), &format!("{}.debt.json", original.id));
    assert!(drain_job(dir.path(), &original, &HashSet::new())
        .await
        .is_err());
    assert_eq!(
        bytes(dir.path(), &format!("{}.debt.json", original.id)),
        before
    );
}

#[test]
fn absent_debt_is_not_a_probe_retirement_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let mut own = job("11111111-1111-1111-1111-111111111111", false);
    oom_job::save(dir.path(), &own, true).unwrap();
    assert!(!job_retired(dir.path(), &own).unwrap());
    own.phase = Phase::Terminal;
    own.terminal_proof = true;
    oom_job::save(dir.path(), &own, false).unwrap();
    assert!(!job_retired(dir.path(), &own).unwrap());
    let mut foreign = own.clone();
    foreign.invocation = "foreign".into();
    fs::write(
        dir.path().join(format!("{}.retired.json", own.id)),
        serde_json::to_vec(&Debt {
            admission: foreign,
            deadline: 0,
            draining: true,
            attempts: 0,
            last_error: None,
        })
        .unwrap(),
    )
    .unwrap();
    assert!(job_retired(dir.path(), &own).is_err());
}
