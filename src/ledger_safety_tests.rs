use super::*;

#[test]
fn corrupt_or_truncated_ledger_survives_startup_and_policy_reload() {
    for bytes in [b"{broken".as_slice(), b"".as_slice()] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        fs::write(&path, bytes).unwrap();
        let mut state = GuardRuntimeState {
            persist_mitigations: true,
            ..Default::default()
        };
        hydrate_mitigations_from(&path, &mut state);
        assert!(state.mitigation_ledger_unavailable);
        assert!(!persist_mitigations_to(&mut state, &path));
        let mut policy = GuardPolicy::default();
        assert_eq!(
            apply_policy_reload(&mut policy, &mut state, GuardPolicy::default(), true),
            ReloadOutcome::Deferred
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(&path, b"{\"records\":[]}").unwrap();
        hydrate_mitigations_from(&path, &mut state);
        assert!(!state.mitigation_ledger_unavailable);
        assert!(persist_mitigations_to(&mut state, &path));
    }
}

#[test]
fn unavailable_ledger_is_not_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    // A directory is unreadable as a ledger even for privileged test runners.
    let mut state = GuardRuntimeState::default();
    hydrate_mitigations_from(dir.path(), &mut state);
    assert!(state.mitigation_ledger_unavailable);
    assert!(!persist_mitigations_to(&mut state, dir.path()));
    assert!(dir.path().is_dir());
}

#[test]
fn second_owner_cannot_enter_or_truncate_shared_mitigation_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner.lock");
    let first = acquire_daemon_lock_at(&path).unwrap();
    fs::write(&path, b"first owner").unwrap();
    assert!(acquire_daemon_lock_at(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"first owner");
    drop(first);
    assert!(acquire_daemon_lock_at(&path).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn gone_legacy_root_never_authorizes_stopping_surviving_unit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("self")).unwrap();
    let ctl = dir.path().join("systemctl");
    let log = dir.path().join("calls");
    fs::write(
        &ctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf 'LoadState=loaded\\n'\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ctl, fs::Permissions::from_mode(0o700)).unwrap();
    let result = uncap_cpu_process_with_bin(
        &ctl,
        dir.path(),
        999999,
        "legacy.service",
        "user.slice",
        true,
    )
    .await;
    assert!(
        result.is_err(),
        "a gone root cannot resolve an occupied/unknown legacy unit"
    );
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("show legacy.service"));
    assert!(!calls.contains("stop"));
    assert!(!calls.contains("reset-failed"));
}

#[test]
fn previous_boot_pid_match_is_archived_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.json");
    let pid = std::process::id() as i32;
    let mut state = GuardRuntimeState::default();
    state.oom_biased_pids.insert(
        pid,
        (200, read_proc_identity(Path::new("/proc"), pid).unwrap()),
    );
    assert!(persist_mitigations_to(&mut state, &path));
    let mut encoded: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    encoded["boot_id"] = serde_json::json!("a-different-boot");
    let old = serde_json::to_vec(&encoded).unwrap();
    fs::write(&path, &old).unwrap();
    let mut restarted = GuardRuntimeState::default();
    hydrate_mitigations_from(&path, &mut restarted);
    assert!(restarted.oom_biased_pids.is_empty());
    assert!(!restarted.mitigation_ledger_unavailable);
    assert!(fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("guard-mitigations-previous-boot-")
            && fs::read(entry.path()).unwrap() == old));
}

#[test]
fn inherited_debt_checks_live_exemption_after_same_incarnation_exec() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("self")).unwrap();
    let pid = std::process::id() as i32;
    let identity = read_proc_identity(Path::new("/proc"), pid).unwrap();
    let proc = dir.path().join(pid.to_string());
    fs::create_dir(&proc).unwrap();
    fs::write(
        proc.join("stat"),
        fs::read(format!("/proc/{pid}/stat")).unwrap(),
    )
    .unwrap();
    fs::write(proc.join("comm"), "chrome").unwrap();
    fs::write(proc.join("oom_score_adj"), "250\n").unwrap();
    let mut state = GuardRuntimeState::default();
    state.oom_pending_descendants.insert(
        (pid, identity.starttime),
        OomPendingDescendant {
            root_pid: 1,
            original_adj: 200,
            identity,
            attempts: 0,
        },
    );
    restore_pending_oom_descendants(dir.path(), &mut state, &HashSet::from(["chrome".into()]));
    assert_eq!(
        fs::read_to_string(proc.join("oom_score_adj")).unwrap(),
        "250\n"
    );
    assert_eq!(state.oom_pending_descendants.len(), 1);
}
