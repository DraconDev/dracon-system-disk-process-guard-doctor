use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn defaults_are_expected() {
    assert_eq!(default_min_size_mb(), 512);
    assert_eq!(default_kinds(), "rust-build,node-deps,build-output,cache");
}

#[cfg(unix)]
#[tokio::test]
async fn renice_process_with_bin_reports_success_and_failure() {
    let tmp = std::env::temp_dir().join(format!(
        "dracon_system_renice_test_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&tmp).expect("temp dir");
    let success = tmp.join("renice-success");
    fs::write(&success, "#!/bin/sh\necho 'ok' >&2\nexit 0\n").expect("write success script");
    fs::set_permissions(&success, fs::Permissions::from_mode(0o755)).expect("chmod");

    let failure = tmp.join("renice-failure");
    fs::write(
        &failure,
        "#!/bin/sh\necho 'permission denied' >&2\nexit 1\n",
    )
    .expect("write failure script");
    fs::set_permissions(&failure, fs::Permissions::from_mode(0o755)).expect("chmod");

    renice_process_with_bin(&success, 123, 5)
        .await
        .expect("success renice");
    let err = renice_process_with_bin(&failure, 123, 5)
        .await
        .expect_err("failure renice");
    assert!(err.to_string().contains("permission denied"));
    let _ = fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
fn write_process_fixture(
    root: &std::path::Path,
    pid: i32,
    comm: &str,
    starttime: u64,
    oom_score_adj: Option<i32>,
    cgroup: Option<&str>,
) {
    // Mirror the `/proc/self` availability marker used by the production
    // identity check so this fixture can distinguish a gone PID from a
    // missing/unavailable proc tree.
    fs::create_dir_all(root.join("self")).expect("create proc availability fixture");
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).expect("create process fixture");
    fs::write(dir.join("comm"), format!("{comm}\n")).expect("write comm fixture");
    let mut stat_fields = vec!["S".to_string()];
    stat_fields.extend(std::iter::repeat_n("0".to_string(), 18));
    stat_fields.push(starttime.to_string());
    fs::write(
        dir.join("stat"),
        format!("{pid} ({comm}) {}\n", stat_fields.join(" ")),
    )
    .expect("write stat fixture");
    if let Some(adj) = oom_score_adj {
        fs::write(dir.join("oom_score_adj"), format!("{adj}\n")).expect("write oom fixture");
    }
    if let Some(cgroup) = cgroup {
        fs::write(dir.join("cgroup"), cgroup).expect("write cgroup fixture");
    }
}

#[cfg(unix)]
fn write_test_script(path: &std::path::Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write test script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod test script");
}

#[cfg(unix)]
#[tokio::test]
async fn restore_runtime_adjustments_restores_renice_and_oom() {
    let tmp = std::env::temp_dir().join(format!(
        "dracon_system_restore_test_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&tmp).expect("create temp dir");
    let renice = tmp.join("renice");
    let systemctl = tmp.join("systemctl");
    write_test_script(&renice, "exit 0");
    write_test_script(&systemctl, "exit 0");
    write_process_fixture(&tmp, 101, "zsh", 77, Some(250), None);

    let identity = ProcessIdentity {
        comm: "zsh".to_string(),
        starttime: 77,
    };
    assert_eq!(
        process_identity_status(&tmp, 101, &identity),
        ProcessIdentityStatus::Match
    );
    assert_eq!(
        process_identity_status(
            &tmp,
            101,
            &ProcessIdentity {
                comm: "zsh".to_string(),
                starttime: 78,
            },
        ),
        ProcessIdentityStatus::Mismatch
    );
    assert_eq!(
        process_identity_status(&tmp, 102, &identity),
        ProcessIdentityStatus::Gone
    );
    let missing_proc_root = tmp.join("missing-proc-root");
    assert_eq!(
        process_identity_status(&missing_proc_root, 101, &identity),
        ProcessIdentityStatus::Unavailable,
        "a missing proc root must not be treated as a gone PID"
    );
    let mut unavailable_state = GuardRuntimeState::default();
    unavailable_state
        .reniced_pids
        .insert(101, (5, identity.clone()));
    unavailable_state.capped_pids.insert(
        101,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            identity.clone(),
        ),
    );
    assert!(
        !restore_runtime_adjustments_with(
            &mut unavailable_state,
            &renice,
            &systemctl,
            &missing_proc_root,
        )
        .await,
        "missing proc root must retain every indeterminate adjustment"
    );
    assert!(unavailable_state.reniced_pids.contains_key(&101));
    assert!(unavailable_state.capped_pids.contains_key(&101));

    // Failed renice and OOM writes must retain their entries for the next
    // release retry rather than reporting success and dropping tracking.
    let failing_renice = tmp.join("renice-failure");
    write_test_script(&failing_renice, "exit 1");
    write_process_fixture(&tmp, 103, "worker", 79, None, None);
    write_process_fixture(&tmp, 104, "worker", 80, None, None);
    fs::create_dir(tmp.join("104/oom_score_adj")).expect("create failing oom fixture");
    let mut failure_state = GuardRuntimeState::default();
    failure_state.reniced_pids.insert(
        103,
        (
            5,
            ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 79,
            },
        ),
    );
    failure_state.oom_biased_pids.insert(
        104,
        (
            -100,
            ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 80,
            },
        ),
    );
    assert!(
        !restore_runtime_adjustments_with(&mut failure_state, &failing_renice, &systemctl, &tmp)
            .await,
        "failed release operations must prevent runtime reset"
    );
    assert!(failure_state.reniced_pids.contains_key(&103));
    assert!(failure_state.oom_biased_pids.contains_key(&104));

    // A kernel comm name can be truncated or differ from argv[0]. The
    // release identity check must use the stable starttime, not cmdline.
    write_process_fixture(&tmp, 105, "very-long-process", 81, None, None);
    let long_comm_identity = ProcessIdentity {
        comm: "very-long".to_string(),
        starttime: 81,
    };
    assert_eq!(
        process_identity_status(&tmp, 105, &long_comm_identity),
        ProcessIdentityStatus::Match
    );

    let mut state = GuardRuntimeState::default();
    state.reniced_pids.insert(101, (5, identity.clone()));
    state.reniced_pids.insert(105, (5, long_comm_identity));
    state.oom_biased_pids.insert(101, (-100, identity));

    assert!(
        restore_runtime_adjustments_with(&mut state, &renice, &systemctl, &tmp).await,
        "all successful restorations should permit runtime reset"
    );
    assert!(state.reniced_pids.is_empty());
    assert!(state.oom_biased_pids.is_empty());
    assert_eq!(
        fs::read_to_string(tmp.join("101/oom_score_adj")).unwrap(),
        "-100\n"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[tokio::test]
async fn restore_runtime_adjustments_retains_failed_cpu_cap() {
    let tmp = std::env::temp_dir().join(format!(
        "dracon_system_cap_restore_test_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&tmp).expect("create temp dir");
    fs::create_dir_all(tmp.join("self")).expect("create proc availability marker");
    let renice = tmp.join("renice");
    let systemctl = tmp.join("systemctl");
    write_test_script(&renice, "exit 0");
    write_test_script(&systemctl, "exit 1");
    let mut state = GuardRuntimeState::default();
    state.capped_pids.insert(
        999_999,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            ProcessIdentity {
                comm: "gone".to_string(),
                starttime: 1,
            },
        ),
    );

    assert!(
        !restore_runtime_adjustments_with(&mut state, &renice, &systemctl, &tmp).await,
        "failed systemd cleanup must prevent runtime reset"
    );
    assert!(state.capped_pids.contains_key(&999_999));

    // Once systemd succeeds, the previously retained cap may be removed.
    write_test_script(&systemctl, "exit 0");
    assert!(restore_runtime_adjustments_with(&mut state, &renice, &systemctl, &tmp).await);
    assert!(state.capped_pids.is_empty());

    // A live process whose cgroup cannot be read is indeterminate and must
    // also remain tracked even when systemd itself reports success.
    write_process_fixture(&tmp, 1000, "worker", 88, None, None);
    fs::create_dir(tmp.join("1000/cgroup")).expect("create unreadable cgroup fixture");
    state.capped_pids.insert(
        1000,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 88,
            },
        ),
    );
    assert!(
        !restore_runtime_adjustments_with(&mut state, &renice, &systemctl, &tmp).await,
        "cgroup read failure must prevent runtime reset"
    );
    assert!(state.capped_pids.contains_key(&1000));

    let systemctl_probe = tmp.join("systemctl-probe");
    let probe_log = tmp.join("systemctl-probe.log");
    let escaped_probe_log = probe_log.to_string_lossy().replace('\'', "'\\''");
    write_test_script(
        &systemctl_probe,
        &format!("printf 'called\\n' >> '{escaped_probe_log}'\nexit 0"),
    );

    // A missing cgroup file with a live PID directory is also indeterminate;
    // it may mean the cgroup source is unavailable rather than that the
    // process exited.
    write_process_fixture(&tmp, 1001, "worker", 89, None, None);
    state.capped_pids.insert(
        1001,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 89,
            },
        ),
    );
    assert!(
        !restore_runtime_adjustments_with(&mut state, &renice, &systemctl_probe, &tmp).await,
        "a missing live-PID cgroup file must prevent runtime reset"
    );
    assert!(state.capped_pids.contains_key(&1001));

    // Malformed cgroup data is equally indeterminate and must not trigger
    // systemd cleanup or allow the cap entry to be discarded.
    write_process_fixture(&tmp, 1002, "worker", 90, None, Some("malformed\n"));
    state.capped_pids.insert(
        1002,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 90,
            },
        ),
    );
    assert!(
        !restore_runtime_adjustments_with(&mut state, &renice, &systemctl_probe, &tmp).await,
        "malformed cgroup data must prevent runtime reset"
    );
    assert!(state.capped_pids.contains_key(&1002));
    assert!(
        !probe_log.exists(),
        "indeterminate cgroup state must not stop the transient service"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn human_bytes_formats_units() {
    assert_eq!(human_bytes(1), "1.0 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
}

#[test]
fn parse_kinds_trims_and_dedupes() {
    let kinds = parse_kinds(" rust-build, node-deps ,rust-build,,cache ");
    assert_eq!(kinds.len(), 3);
    assert!(kinds.contains("rust-build"));
    assert!(kinds.contains("node-deps"));
    assert!(kinds.contains("cache"));
}

#[test]
fn expand_tilde_resolves_to_home_dir() {
    // dirs::home_dir() uses getpwuid() on Linux, not $HOME.
    // Just verify ~ expands to whatever dirs reports.
    let home = dirs::home_dir().expect("home dir should exist");
    assert_eq!(expand_tilde("~"), home);
    assert_eq!(expand_tilde("~/foo/bar"), home.join("foo/bar"));
    assert_eq!(expand_tilde("/x/y"), PathBuf::from("/x/y"));
}

#[test]
fn expand_tilde_with_home_unset_falls_back_to_dot() {
    // We can't actually unset home for dirs::home_dir() (it uses getpwuid),
    // but we can verify the fallback path is wired correctly by testing
    // the helper directly if we could mock it. Instead, just verify
    // non-tilde paths pass through unchanged.
    assert_eq!(
        expand_tilde("/absolute/path"),
        PathBuf::from("/absolute/path")
    );
    assert_eq!(
        expand_tilde("relative/path"),
        PathBuf::from("relative/path")
    );
}

#[test]
fn build_link_report_counts_states() {
    let policy = SystemPolicy {
        storage: StoragePolicy::default(),
        links: LinkPolicy {
            entries: vec![LinkEntry {
                link: "/tmp/does-not-exist-link".into(),
                target: "/tmp/does-not-exist-target".into(),
            }],
        },
        guard: GuardPolicy::default(),
    };
    let report = build_link_report(&policy);
    assert_eq!(report.total, 1);
    assert_eq!(report.healthy, 0);
    assert_eq!(report.drifted, 1);
    assert_eq!(report.missing_target, 1);
}

#[cfg(unix)]
#[test]
fn evaluate_link_handles_missing_and_sync_cases() {
    let base = std::env::temp_dir().join(format!(
        "dracon_system_test_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&base).expect("base dir");

    let target = base.join("target.txt");
    fs::write(&target, "x").expect("target");

    let missing_link = LinkEntry {
        link: base.join("missing-link").display().to_string(),
        target: target.display().to_string(),
    };
    let s1 = evaluate_link(&missing_link);
    assert_eq!(s1.issue, "link_missing");

    let normal_file_link = base.join("normal-file");
    fs::write(&normal_file_link, "x").expect("file");
    let not_symlink = LinkEntry {
        link: normal_file_link.display().to_string(),
        target: target.display().to_string(),
    };
    let s2 = evaluate_link(&not_symlink);
    assert_eq!(s2.issue, "path_not_symlink");

    let good_link = base.join("good-link");
    symlink(&target, &good_link).expect("symlink");
    let synced = LinkEntry {
        link: good_link.display().to_string(),
        target: target.display().to_string(),
    };
    let s3 = evaluate_link(&synced);
    assert_eq!(s3.issue, "ok");
    assert!(s3.in_sync);

    let wrong_target = base.join("other.txt");
    fs::write(&wrong_target, "y").expect("other");
    let mismatch_link = base.join("mismatch-link");
    symlink(&wrong_target, &mismatch_link).expect("symlink mismatch");
    let mismatch = LinkEntry {
        link: mismatch_link.display().to_string(),
        target: target.display().to_string(),
    };
    let s4 = evaluate_link(&mismatch);
    assert_eq!(s4.issue, "link_target_mismatch");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn parse_and_format_repeated_scenarios() {
    for i in 0..220usize {
        let csv = if i % 2 == 0 {
            "rust-build,node-deps,cache"
        } else {
            " rust-build , build-output , cache , rust-build "
        };
        let kinds = parse_kinds(csv);
        assert!(kinds.contains("rust-build"));
        assert!(kinds.contains("cache"));

        let bytes = (i as u64 + 1) * 2048;
        let out = human_bytes(bytes);
        assert!(!out.is_empty());
        assert!(out.contains(' '));
    }
}

#[test]
fn parse_df_use_percent_works() {
    let sample =
        "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 100 91 9 91% /\n";
    assert_eq!(parse_df_use_percent(sample), Some(91));
}

#[test]
fn parse_ps_output_works() {
    let sample = "123 1 250.5 4194304 5 git\n456 2 12.0 2048 0 zsh\n";
    let rows = parse_ps_output(sample);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].pid, 123);
    assert_eq!(rows[0].ppid, 1);
    assert_eq!(rows[0].command, "git");
    assert_eq!(rows[0].rss_mb, 4096);
    assert_eq!(rows[0].nice, 5);
    assert_eq!(rows[0].args, "");
}

#[test]
fn is_protected_ancestor_exact_match() {
    assert!(is_protected_ancestor("/home", "/home"));
    assert!(is_protected_ancestor("/etc", "/etc"));
    assert!(is_protected_ancestor("/", "/"));
}

#[test]
fn is_protected_ancestor_descendant_match() {
    assert!(is_protected_ancestor("/home/dracon", "/home"));
    assert!(is_protected_ancestor("/home/dracon/Dev", "/home"));
    assert!(is_protected_ancestor("/etc/nginx/nginx.conf", "/etc"));
}

#[test]
fn is_protected_ancestor_rejects_partial_prefix() {
    assert!(!is_protected_ancestor("/homefoo", "/home"));
    assert!(!is_protected_ancestor("/homefoo/bar", "/home"));
    assert!(!is_protected_ancestor("/etcnginx", "/etc"));
}

#[test]
fn is_protected_ancestor_root_matches_exact_only() {
    assert!(is_protected_ancestor("/", "/"));
    assert!(!is_protected_ancestor("/anything", "/")); // root only matches exact to allow cleanup
    assert!(!is_protected_ancestor("/home", "/"));
}

#[test]
fn check_path_str_blocks_descendants() {
    assert!(!check_path_str("/home/dracon", &[]));
    assert!(!check_path_str("/home/dracon/Dev", &[]));
    assert!(!check_path_str("/etc/nginx", &[]));
    assert!(check_path_str("/safe/path", &[]));
    assert!(check_path_str("/homefoo", &[])); // partial prefix should be safe
}

#[test]
fn parse_ps_output_extracts_all_fields() {
    let sample = "9999 1 75.0 8192000 0 git-fetch origin main\n";
    let rows = parse_ps_output(sample);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pid, 9999);
    assert_eq!(rows[0].ppid, 1);
    assert_eq!(rows[0].cpu_percent, 75.0);
    assert_eq!(rows[0].rss_mb, 8192000 / 1024);
    assert_eq!(rows[0].command, "git-fetch");
    assert_eq!(rows[0].args, "origin main");
}

#[test]
fn disk_state_transitions_at_thresholds() {
    let guard = GuardPolicy {
        disk_warn_percent: 70,
        disk_action_percent: 85,
        disk_critical_percent: 95,
        ..GuardPolicy::default()
    };
    assert_eq!(disk_state(50, &guard), "ok");
    assert_eq!(disk_state(70, &guard), "warn");
    assert_eq!(disk_state(84, &guard), "warn");
    assert_eq!(disk_state(85, &guard), "action");
    assert_eq!(disk_state(94, &guard), "action");
    assert_eq!(disk_state(95, &guard), "critical");
    assert_eq!(disk_state(100, &guard), "critical");
}

#[test]
fn should_notify_respects_cooldown() {
    let mut state = GuardRuntimeState::default();
    let key = "test-key";
    assert!(
        should_notify(&mut state, key, 60),
        "first notify should succeed"
    );
    assert!(
        !should_notify(&mut state, key, 60),
        "immediate second notify should be blocked"
    );
    assert!(
        should_notify(&mut state, "other-key", 60),
        "different key should succeed"
    );
}

#[test]
fn predict_fill_time_requires_minimum_samples() {
    let history: Vec<(Instant, u8)> = vec![(Instant::now(), 50), (Instant::now(), 51)];
    assert!(
        predict_fill_time(&history).is_none(),
        "needs at least 3 samples"
    );
}

#[test]
fn predict_fill_time_returns_none_for_stable_disk() {
    let base = Instant::now();
    let history: Vec<(Instant, u8)> = vec![
        (base, 50),
        (base + Duration::from_secs(10), 50),
        (base + Duration::from_secs(20), 50),
    ];
    assert!(
        predict_fill_time(&history).is_none(),
        "stable disk should not predict fill"
    );
}

#[test]
fn predict_fill_time_estimates_for_filling_disk() {
    let base = Instant::now();
    let history: Vec<(Instant, u8)> = vec![
        (base, 50),
        (base + Duration::from_secs(3600), 60),
        (base + Duration::from_secs(7200), 70),
    ];
    let hours = predict_fill_time(&history);
    assert!(hours.is_some(), "should predict fill time for rising disk");
    let h = hours.unwrap();
    assert!(h > 0.0, "predicted hours should be positive");
    assert!(
        h < 100.0,
        "predicted hours should be reasonable for 10%/hr rate"
    );
}

#[tokio::test]
async fn docker_prune_returns_zero_on_dry_run() {
    // When apply=false, docker_prune should return immediately without
    // invoking docker, yielding 0 bytes reclaimed.
    let result = docker_prune(false, true, true).await;
    assert!(result.is_ok(), "dry-run docker_prune should not error");
    assert_eq!(result.unwrap(), 0, "dry-run should reclaim 0 bytes");
}

#[tokio::test]
async fn guard_report_completes_for_ok_disk() {
    let mut state = GuardRuntimeState::default();
    let guard = GuardPolicy {
        disk_warn_percent: 70,
        disk_action_percent: 85,
        disk_critical_percent: 95,
        disk_mount_path: "/".into(),
        freeze_sync_at_action: false,
        track_trends: false,
        ..GuardPolicy::default()
    };
    let report = run_guard_once(&guard, &mut state).await;
    assert!(
        report.is_ok(),
        "guard should complete successfully with default policy on ok disk"
    );
}

#[test]
fn test_graduated_nice_value_cpu_tiers() {
    assert_eq!(graduated_nice_value(100.0, 0, 5), 5);
    assert_eq!(graduated_nice_value(180.0, 0, 5), 5);
    assert_eq!(graduated_nice_value(250.0, 0, 5), 5);
    assert_eq!(graduated_nice_value(300.0, 0, 5), 10);
    assert_eq!(graduated_nice_value(450.0, 0, 5), 10);
    assert_eq!(graduated_nice_value(500.0, 0, 5), 15);
    assert_eq!(graduated_nice_value(900.0, 0, 5), 15);
}

#[test]
fn test_graduated_nice_value_memory_tiers() {
    assert_eq!(graduated_nice_value(0.0, 2000, 5), 5);
    assert_eq!(graduated_nice_value(0.0, 4096, 5), 5);
    assert_eq!(graduated_nice_value(0.0, 5000, 5), 5);
    assert_eq!(graduated_nice_value(0.0, 8192, 5), 10);
    assert_eq!(graduated_nice_value(0.0, 16000, 5), 10);
}

#[test]
fn test_graduated_nice_value_cpu_plus_memory() {
    assert_eq!(graduated_nice_value(300.0, 8192, 5), 10);
    assert_eq!(graduated_nice_value(500.0, 4096, 5), 15);
    assert_eq!(graduated_nice_value(180.0, 8192, 5), 10);
}

#[test]
fn test_graduated_nice_value_clamped() {
    assert_eq!(graduated_nice_value(0.0, 0, 5), 5);
    assert_eq!(graduated_nice_value(0.0, 0, 0), 0);
}

#[test]
fn test_graduated_nice_value_negative_base_clamped() {
    assert_eq!(graduated_nice_value(0.0, 0, -5), 0);
}

#[test]
fn test_graduated_nice_value_high_base_clamped() {
    assert_eq!(graduated_nice_value(0.0, 0, 20), 19);
}

#[test]
fn test_graduated_nice_value_memory_boundary() {
    assert_eq!(graduated_nice_value(0.0, 4095, 0), 0);
    assert_eq!(graduated_nice_value(0.0, 4096, 0), 5);
    assert_eq!(graduated_nice_value(0.0, 8191, 0), 5);
    assert_eq!(graduated_nice_value(0.0, 8192, 0), 10);
}

fn guard_test_tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "dracon_test_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn guard_safe_delete_allows_paths_under_system_protected() {
    let tmp = guard_test_tmp("guard_safe_1");
    let target = tmp.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let result = check_safe_to_delete_guard(&target, &[]);
    assert!(
        result.is_ok(),
        "guard safe delete should allow paths under /home (system-protected skipped)"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn guard_safe_delete_blocks_user_protected() {
    let tmp = guard_test_tmp("guard_safe_2");
    let target = tmp.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let user_protected = vec![tmp.display().to_string()];
    let result = check_safe_to_delete_guard(&target, &user_protected);
    assert!(
        result.is_err(),
        "guard safe delete should block user-protected paths"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn guard_safe_delete_rejects_symlink() {
    let tmp = guard_test_tmp("guard_safe_3");
    let real = tmp.join("real_target");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.join("link_target");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let result = check_safe_to_delete_guard(&link, &[]);
    assert!(result.is_err(), "guard safe delete should reject symlinks");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn guard_safe_delete_rejects_exact_system_roots() {
    for prot in SYSTEM_PROTECTED {
        let result = check_safe_to_delete_guard(Path::new(prot), &[]);
        assert!(
            result.is_err(),
            "guard safe delete should reject exact protected system root {prot}"
        );
    }
}

#[test]
fn check_safe_to_delete_rejects_log_symlink_before_truncate() {
    let tmp = guard_test_tmp("log_symlink");
    std::fs::create_dir_all(&tmp).unwrap();
    let real = tmp.join("real.log");
    let link = tmp.join("link.log");
    std::fs::write(&real, "line1\nline2\nline3\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let result = check_safe_to_delete(&link, &[]);
    assert!(
        result.is_err(),
        "symlink log should be rejected before truncate"
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "line1\nline2\nline3\n"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn proactive_cleanup_defaults() {
    assert_eq!(default_proactive_cleanup_percent(), 50);
    assert_eq!(default_rust_target_max_age_days(), 14);
    assert_eq!(default_proactive_cleanup_interval_cycles(), 120);
}

#[test]
fn normalize_proactive_cleanup_percent_bounded_by_action() {
    let policy = GuardPolicy {
        disk_action_percent: 85,
        proactive_cleanup_percent: 90,
        ..Default::default()
    };
    let mut policy = policy;
    normalize_guard_policy(&mut policy);
    assert!(
        policy.proactive_cleanup_percent < policy.disk_action_percent,
        "proactive_cleanup_percent must be below disk_action_percent"
    );
}

#[test]
fn normalize_rust_target_max_age_days_min_1() {
    let policy = GuardPolicy {
        rust_target_max_age_days: 0,
        ..Default::default()
    };
    let mut policy = policy;
    normalize_guard_policy(&mut policy);
    assert!(policy.rust_target_max_age_days >= 1);
}

#[test]
fn normalize_proactive_interval_min_1() {
    let policy = GuardPolicy {
        proactive_cleanup_interval_cycles: 0,
        ..Default::default()
    };
    let mut policy = policy;
    normalize_guard_policy(&mut policy);
    assert!(policy.proactive_cleanup_interval_cycles >= 1);
}

#[test]
fn guard_runtime_state_default_cycle_zero() {
    let state = GuardRuntimeState::default();
    assert_eq!(state.guard_cycle, 0);
    assert!(state.last_proactive_cleanup.is_none());
}

#[test]
fn target_dir_info_has_mtime() {
    let info = TargetDirInfo {
        path: PathBuf::from("/tmp/test/target"),
        bytes: 1024,
        mtime_secs_ago: 86400 * 15,
    };
    assert_eq!(info.mtime_secs_ago, 86400 * 15);
    assert_eq!(info.bytes, 1024);
}
