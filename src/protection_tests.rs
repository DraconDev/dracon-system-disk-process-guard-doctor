//! Regression checks protect restoration obligations, not warning wording or
//! implementation call shapes. All proc fixtures and ledgers are disposable.
use super::*;

fn proc_fixture(root: &Path, pid: i32, identity: &ProcessIdentity, adj: Option<i32>) {
    fs::create_dir_all(root.join("self")).unwrap();
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("comm"), &identity.comm).unwrap();
    let mut fields = vec!["0".to_string(); 50];
    fields[0] = "S".into();
    fields[19] = identity.starttime.to_string();
    fs::write(
        dir.join("stat"),
        format!("{pid} ({}) {}", identity.comm, fields.join(" ")),
    )
    .unwrap();
    if let Some(adj) = adj {
        fs::write(dir.join("oom_score_adj"), format!("{adj}\n")).unwrap();
    }
}

#[test]
fn oom_failure_survives_retry_escalation_restart_and_then_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let pid = std::process::id() as i32;
    let identity = read_proc_identity(Path::new("/proc"), pid).unwrap();
    proc_fixture(tmp.path(), pid, &identity, None);
    let mut state = GuardRuntimeState::default();
    state.oom_pending_descendants.insert(
        (pid, identity.starttime),
        OomPendingDescendant {
            root_pid: 123,
            original_adj: 25,
            identity: identity.clone(),
            attempts: 0,
        },
    );
    for _ in 0..9 {
        assert_eq!(
            restore_pending_oom_descendants(tmp.path(), &mut state, &HashSet::new()).deferred,
            1
        );
    }
    assert_eq!(
        state.oom_pending_descendants[&(pid, identity.starttime)].attempts,
        9
    );
    let ledger = tmp.path().join("ledger.json");
    assert!(persist_mitigations_to(&mut state, &ledger));
    let mut restarted = GuardRuntimeState::default();
    hydrate_mitigations_from(&ledger, &mut restarted);
    assert_eq!(
        restarted.oom_pending_descendants[&(pid, identity.starttime)].original_adj,
        25
    );
    fs::write(tmp.path().join(format!("{pid}/oom_score_adj")), "250\n").unwrap();
    let result = restore_pending_oom_descendants(tmp.path(), &mut restarted, &HashSet::new());
    assert_eq!(result.restored.len(), 1);
    assert!(restarted.oom_pending_descendants.is_empty());
    assert_eq!(
        fs::read_to_string(tmp.path().join(format!("{pid}/oom_score_adj"))).unwrap(),
        "25\n"
    );
}

#[test]
fn oom_restore_preserves_external_changes_and_rejects_pid_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = ProcessIdentity {
        comm: "worker".into(),
        starttime: 42,
    };
    proc_fixture(tmp.path(), 123, &identity, Some(125));
    oom_restore::restore(tmp.path(), 123, &identity, 0).unwrap();
    assert_eq!(
        fs::read_to_string(tmp.path().join("123/oom_score_adj")).unwrap(),
        "125\n"
    );
    fs::write(tmp.path().join("123/oom_score_adj"), "250\n").unwrap();
    let reused = ProcessIdentity {
        starttime: 43,
        ..identity.clone()
    };
    assert!(oom_restore::restore(tmp.path(), 123, &reused, 0).is_err());
    assert_eq!(
        fs::read_to_string(tmp.path().join("123/oom_score_adj")).unwrap(),
        "250\n"
    );
    oom_restore::restore(tmp.path(), 123, &identity, 0).unwrap();
    assert_eq!(
        fs::read_to_string(tmp.path().join("123/oom_score_adj")).unwrap(),
        "0\n"
    );
}

#[test]
fn oom_exemption_retains_debt_without_mutating_and_gone_process_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = ProcessIdentity {
        comm: "worker".into(),
        starttime: 42,
    };
    proc_fixture(tmp.path(), 123, &identity, Some(250));
    let mut state = GuardRuntimeState::default();
    state.oom_pending_descendants.insert(
        (123, 42),
        OomPendingDescendant {
            root_pid: 122,
            original_adj: 0,
            identity,
            attempts: 0,
        },
    );
    restore_pending_oom_descendants(tmp.path(), &mut state, &HashSet::from(["worker".into()]));
    assert!(state.oom_pending_descendants.contains_key(&(123, 42)));
    assert_eq!(
        fs::read_to_string(tmp.path().join("123/oom_score_adj")).unwrap(),
        "250\n"
    );
    fs::remove_dir_all(tmp.path().join("123")).unwrap();
    restore_pending_oom_descendants(tmp.path(), &mut state, &HashSet::from(["worker".into()]));
    assert!(state.oom_pending_descendants.is_empty());
}

#[test]
fn oom_existing_descendant_provenance_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let pid = std::process::id() as i32;
    let identity = read_proc_identity(Path::new("/proc"), pid).unwrap();
    let mut before = GuardRuntimeState::default();
    before.oom_biased_pids.insert(pid, (200, identity));
    before
        .oom_known_descendants
        .insert(pid, HashSet::from([(987654, 123), (876543, 321)]));
    let ledger = tmp.path().join("ledger.json");
    assert!(persist_mitigations_to(&mut before, &ledger));
    let mut after = GuardRuntimeState::default();
    hydrate_mitigations_from(&ledger, &mut after);
    assert_eq!(
        after.oom_known_descendants.get(&pid),
        before.oom_known_descendants.get(&pid)
    );
}

#[test]
fn oom_write_ahead_failure_is_observable_and_does_not_claim_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("not-directory"), "x").unwrap();
    let mut state = GuardRuntimeState::default();
    assert!(!persist_mitigations_to(
        &mut state,
        &tmp.path().join("not-directory/ledger.json")
    ));
    assert!(state.last_persisted_mitigations.is_none());
}

/// ADDED 2026-10-24 (audit O1): a recorded bias is only trusted when the LIVE
/// value says so.
///
/// The guard used to trust its own bookkeeping and skip the process: a
/// record whose live value was not the bias target silently disabled OOM
/// steering for a live top-RSS process under critical pressure, because the
/// guard skipped the pid on every pass believing its own bookkeeping said
/// "already done". Two live processes carried exactly that record on this
/// host when the audit found it (the ledger said "biased", the live
/// `oom_score_adj` was back at 200, and the host was at 2% available
/// memory).
#[test]
fn a_recorded_bias_is_only_trusted_when_the_live_value_says_so() {
    assert!(
        !recorded_bias_is_stale(Some(OOM_BIAS_TARGET)),
        "a process that IS at the bias target is not stale"
    );
    assert!(
        recorded_bias_is_stale(Some(200)),
        "a process that is back at its original value IS stale"
    );
    assert!(
        !recorded_bias_is_stale(None),
        "an unreadable live value is not proof that the claim is false"
    );
}
