//! Tests for guard.rs (guard daemon runtime, disk monitoring, process management)
//!
//! These tests verify the guard runtime components after extraction from main.rs.

use super::*;

// ---------------------------------------------------------------------------
// GuardRuntimeState
// ---------------------------------------------------------------------------

#[test]
fn guard_runtime_state_default_is_empty() {
    let state = crate::GuardRuntimeState::default();
    assert!(state.heavy_since.is_empty());
    assert!(state.notify_cooldowns.is_empty());
    assert!(state.last_disk_state.is_empty());
    assert!(state.oom_known_descendants.is_empty());
    assert!(state.oom_pending_descendants.is_empty());
}

#[test]
fn oom_descendant_candidates_only_select_new_untracked_user_processes() {
    let sample = |pid: i32, ppid: i32, starttime: u64, command: &str| ProcSample {
        pid,
        ppid,
        cpu_percent: 0.0,
        rss_mb: 1,
        nice: 0,
        command: command.to_string(),
        args: String::new(),
        starttime,
    };
    let samples = vec![
        sample(10, 1, 10, "root"),
        sample(11, 10, 11, "existing-child"),
        sample(12, 10, 12, "new-child"),
        sample(13, 12, 13, "new-grandchild"),
        sample(14, 1, 14, "unrelated"),
        sample(15, 10, 15, "kworker/0:1"),
        sample(16, 10, 16, "editor"),
        sample(17, 10, 17, "tracked-child"),
    ];
    let known = HashSet::from([(11, 11)]);
    let tracked = HashSet::from([10, 17]);
    let exempt = HashSet::from(["editor".to_string()]);

    let descendants = crate::process_descendant_samples(&samples, 10);
    assert_eq!(
        descendants
            .iter()
            .map(|sample| sample.pid)
            .collect::<Vec<_>>(),
        vec![11, 12, 13, 15, 16, 17]
    );
    let candidates = crate::oom_descendant_candidates(&samples, 10, &known, &tracked, &exempt);
    assert_eq!(
        candidates
            .iter()
            .map(|sample| sample.pid)
            .collect::<Vec<_>>(),
        vec![12, 13]
    );
}

#[test]
fn runtime_adjustment_plan_includes_every_reversible_limiter() {
    let mut state = crate::GuardRuntimeState::default();
    state.reniced_pids.insert(
        101,
        crate::LegacyReniceState {
            original_nice: 0,
            applied_nice: 5,
            identity: crate::ProcessIdentity {
                comm: "legacy-worker".to_string(),
                starttime: 1,
            },
        },
    );
    state.memory_reniced_pids.insert(
        102,
        crate::MemoryReniceState {
            original_nice: 3,
            applied_nice: 10,
            identity: crate::ProcessIdentity {
                comm: "memory-worker".to_string(),
                starttime: 2,
            },
        },
    );
    state.oom_biased_pids.insert(
        103,
        (
            -100,
            crate::ProcessIdentity {
                comm: "oom-worker".to_string(),
                starttime: 3,
            },
        ),
    );
    state.capped_pids.insert(
        104,
        (
            "dracon-cap.service".to_string(),
            "user.slice".to_string(),
            crate::ProcessIdentity {
                comm: "cap-worker".to_string(),
                starttime: 4,
            },
        ),
    );

    let plan = crate::runtime_adjustment_plan(&state);
    assert_eq!(plan.len(), 4);
    assert!(plan.contains(&crate::RuntimeAdjustment::Nice {
        pid: 101,
        original_nice: 0,
        identity: crate::ProcessIdentity {
            comm: "legacy-worker".to_string(),
            starttime: 1,
        },
        scope: crate::NiceRestoreScope::Legacy,
    }));
    assert!(plan.contains(&crate::RuntimeAdjustment::Nice {
        pid: 102,
        original_nice: 3,
        identity: crate::ProcessIdentity {
            comm: "memory-worker".to_string(),
            starttime: 2,
        },
        scope: crate::NiceRestoreScope::Memory,
    }));
    assert!(plan.contains(&crate::RuntimeAdjustment::OomBias {
        pid: 103,
        orig_adj: -100,
        identity: crate::ProcessIdentity {
            comm: "oom-worker".to_string(),
            starttime: 3,
        },
    }));
    assert!(plan.contains(&crate::RuntimeAdjustment::CpuCap {
        pid: 104,
        scope: "dracon-cap.service".to_string(),
        orig_cgroup: "user.slice".to_string(),
        identity: crate::ProcessIdentity {
            comm: "cap-worker".to_string(),
            starttime: 4,
        },
    }));
}

#[test]
fn guard_runtime_state_insert_and_retrieve_heavy_process() {
    let mut state = crate::GuardRuntimeState::default();
    state.heavy_since.insert(1234, (Instant::now(), 0));
    assert!(state.heavy_since.contains_key(&1234));
    state.heavy_since.remove(&1234);
    assert!(!state.heavy_since.contains_key(&1234));
}

// ---------------------------------------------------------------------------
// GuardPolicy — disk threshold accessors (pub fields)
// ---------------------------------------------------------------------------

#[test]
fn guard_policy_disk_thresholds_are_public() {
    let guard = GuardPolicy::default();
    assert_eq!(guard.disk_warn_percent, 80);
    assert_eq!(guard.disk_action_percent, 90);
    assert_eq!(guard.disk_critical_percent, 95);
    assert_eq!(guard.disk_early_warn_percent, 70);
}

// ---------------------------------------------------------------------------
// GuardReport
// ---------------------------------------------------------------------------

#[test]
fn guard_report_can_be_created_with_alerts() {
    use crate::GuardProcessAlert;
    use crate::GuardReport;

    let report = GuardReport {
        enabled: true,
        disk_use_percent: 72,
        disk_state: "warn".to_string(),
        sync_frozen: false,
        alerts: vec![GuardProcessAlert {
            pid: 12345,
            ppid: 1,
            command: "cargo".to_string(),
            args: "build".to_string(),
            cpu_percent: 250.0,
            rss_mb: 1024,
            sustained_secs: 35,
            action: "reniced".to_string(),
            nice_value: 5,
        }],
        memory: None,
        zombies: Vec::new(),
        disk_fill_gbph: None,
    };
    assert!(report.enabled);
    assert_eq!(report.disk_use_percent, 72);
    assert_eq!(report.alerts.len(), 1);
    assert_eq!(report.alerts[0].pid, 12345);
}

// ---------------------------------------------------------------------------
// graduated_nice_value
// ---------------------------------------------------------------------------

#[test]
fn graduated_nice_value_cpu_tier_180_percent() {
    // CPU >= 180% → nice 5
    assert_eq!(crate::graduated_nice_value(180.0, 0, 5), 5);
    assert_eq!(crate::graduated_nice_value(200.0, 0, 5), 5);
}

#[test]
fn graduated_nice_value_cpu_tier_300_percent() {
    // CPU >= 300% → nice 10
    assert_eq!(crate::graduated_nice_value(300.0, 0, 5), 10);
    assert_eq!(crate::graduated_nice_value(350.0, 0, 5), 10);
}

#[test]
fn graduated_nice_value_cpu_tier_500_percent() {
    // CPU >= 500% → nice 15
    assert_eq!(crate::graduated_nice_value(500.0, 0, 5), 15);
    assert_eq!(crate::graduated_nice_value(600.0, 0, 5), 15);
}

#[test]
fn graduated_nice_value_memory_4gb() {
    // 4 GB in MB = 4096
    assert_eq!(crate::graduated_nice_value(0.0, 4096, 5), 5);
}

#[test]
fn graduated_nice_value_memory_8gb() {
    // 8 GB in MB = 8192
    assert_eq!(crate::graduated_nice_value(0.0, 8192, 10), 10);
}

#[test]
fn graduated_nice_value_below_all_tiers_uses_base() {
    // Below all thresholds → base nice value
    assert_eq!(crate::graduated_nice_value(50.0, 100, 3), 3);
    assert_eq!(crate::graduated_nice_value(100.0, 500, 7), 7);
}

#[test]
fn graduated_nice_value_negative_base_clamped_to_zero() {
    assert_eq!(crate::graduated_nice_value(0.0, 0, -5), 0);
}

#[test]
fn graduated_nice_value_high_base_clamped_to_max() {
    // Nice values are capped at 19
    assert_eq!(crate::graduated_nice_value(0.0, 0, 20), 19);
}

// ---------------------------------------------------------------------------
// ProcSample parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_ps_output_extracts_all_fields() {
    // ps output uses KB for RSS (ps man page: rss: resident set size in KB)
    let output = "12345  1  50.0  1024  3  cargo\n  23456  12345  25.0  2048  7  rustc";
    let samples = crate::parse_ps_output(output);
    assert_eq!(samples.len(), 2);

    assert_eq!(samples[0].pid, 12345);
    assert_eq!(samples[0].ppid, 1);
    assert_eq!(samples[0].cpu_percent, 50.0);
    // RSS is in KB, converted to MB via /1024
    assert_eq!(samples[0].rss_mb, 1024 / 1024); // 1024 KB = 1 MB
    assert_eq!(samples[0].nice, 3);
    assert_eq!(samples[0].command, "cargo");

    assert_eq!(samples[1].pid, 23456);
    assert_eq!(samples[1].ppid, 12345);
    assert_eq!(samples[1].cpu_percent, 25.0);
    assert_eq!(samples[1].rss_mb, 2048 / 1024); // 2048 KB = 2 MB
    assert_eq!(samples[1].nice, 7);
    assert_eq!(samples[1].command, "rustc");
}

#[test]
fn parse_ps_output_empty_input() {
    let samples = crate::parse_ps_output("");
    assert!(samples.is_empty());
}

#[test]
fn parse_ps_output_malformed_lines_skipped() {
    // Malformed lines should be skipped, good lines parsed
    let output = "not_valid\n12345  1  75.0  512  0  cargo";
    let samples = crate::parse_ps_output(output);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].pid, 12345);
    assert_eq!(samples[0].cpu_percent, 75.0);
}

#[test]
fn nice_restore_privilege_requires_root_or_cap_sys_nice() {
    let unprivileged = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000000000\n";
    let privileged = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000800000\n";
    let root = "Uid:\t0\t0\t0\t0\nCapEff:\t0000000000000000\n";

    assert!(!crate::has_nice_restore_privilege_from_status(unprivileged));
    assert!(crate::has_nice_restore_privilege_from_status(privileged));
    assert!(crate::has_nice_restore_privilege_from_status(root));
}

// ---------------------------------------------------------------------------
// disk_state
// ---------------------------------------------------------------------------

// Note: disk_state() in main.rs doesn't handle early-warn state.
// That state is managed in the guard daemon loop via check_disk_early_warning.
// The basic disk_state() only classifies: critical > action > warn > ok.

#[test]
fn disk_state_ok_below_warn() {
    let guard = GuardPolicy::default();
    assert_eq!(crate::disk_state(50, &guard), "ok");
    assert_eq!(crate::disk_state(79, &guard), "ok");
}

#[test]
fn disk_state_warn_between_warn_and_action() {
    let guard = GuardPolicy::default();
    assert_eq!(crate::disk_state(80, &guard), "warn");
    assert_eq!(crate::disk_state(89, &guard), "warn");
}

#[test]
fn disk_state_action_between_action_and_critical() {
    let guard = GuardPolicy::default();
    assert_eq!(crate::disk_state(90, &guard), "action");
    assert_eq!(crate::disk_state(94, &guard), "action");
}

#[test]
fn disk_state_critical_at_or_above_critical() {
    let guard = GuardPolicy::default();
    assert_eq!(crate::disk_state(95, &guard), "critical");
    assert_eq!(crate::disk_state(100, &guard), "critical");
}

// ---------------------------------------------------------------------------
// df parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_df_use_percent_works() {
    // Typical df -P output
    let output = "Filesystem   1024-blocks    Used Available Capacity Mounted on\n/dev/sda1      19512345  15678901   3823444      80% /";
    assert_eq!(crate::parse_df_use_percent(output), Some(80));
}

#[test]
fn parse_df_use_percent_parses_without_percent_sign() {
    // parse_df_use_percent takes the 4th column and parses it as a number.
    // If the value is "80" without %, it still parses as Some(80).
    // This is normal df output behavior when the Use% column lacks the % sign.
    let output = "Filesystem   1024-blocks    Used Available Capacity Mounted on\n/dev/sda1      19512345  15678901   3823444      80 /";
    assert_eq!(crate::parse_df_use_percent(output), Some(80));
}

#[test]
fn parse_df_use_percent_no_matching_line() {
    assert_eq!(crate::parse_df_use_percent(""), None);
    assert_eq!(crate::parse_df_use_percent("no df output here"), None);
}

// ---------------------------------------------------------------------------
// should_notify cooldown logic
// ---------------------------------------------------------------------------

#[test]
fn should_notify_first_time_allowed() {
    let mut state = crate::GuardRuntimeState::default();
    let result = crate::should_notify(&mut state, "test-key", 300);
    assert!(result);
}

#[test]
fn should_notify_respects_cooldown() {
    let mut state = crate::GuardRuntimeState::default();
    // First call — allowed, records cooldown
    let first = crate::should_notify(&mut state, "my-event", 300);
    assert!(first);

    // Immediate second call — still in cooldown (cooldown recorded with future time)
    let second = crate::should_notify(&mut state, "my-event", 300);
    assert!(!second);

    // Remove the cooldown entry to simulate time passing — should be allowed again
    state.notify_cooldowns.remove("my-event");
    let third = crate::should_notify(&mut state, "my-event", 300);
    assert!(third);
}

#[test]
fn report_state_transition_suppresses_unchanged_events() {
    let mut state = crate::GuardRuntimeState::default();
    let (previous, first) = crate::report_state_transition(&mut state, "memory", "warn", 1800);
    assert!(previous.is_none());
    assert!(first);

    let (previous, repeated) = crate::report_state_transition(&mut state, "memory", "warn", 1800);
    assert_eq!(previous.as_deref(), Some("warn"));
    assert!(!repeated, "unchanged state must not emit every guard cycle");

    let (previous, recovery) = crate::report_state_transition(&mut state, "memory", "ok", 1800);
    assert_eq!(previous.as_deref(), Some("warn"));
    assert!(recovery, "state transitions should emit immediately");
}

#[test]
fn memory_pressure_ignores_swap_occupancy_without_active_pressure() {
    assert_eq!(
        crate::classify_memory_pressure(false, true, false),
        "ok",
        "cold pages in swap are not active pressure"
    );
    assert_eq!(crate::classify_memory_pressure(true, false, false), "warn");
    assert_eq!(
        crate::classify_memory_pressure(true, true, false),
        "critical"
    );
    assert_eq!(crate::classify_memory_pressure(false, false, true), "warn");
}

#[test]
fn memory_pressure_requires_persistence_before_transition() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    let (stable, previous, changed) =
        crate::stabilize_memory_pressure_at(&mut state, "warn", 120, start);
    assert_eq!(stable, "ok");
    assert!(previous.is_none());
    assert!(!changed);

    let (stable, _, changed) = crate::stabilize_memory_pressure_at(
        &mut state,
        "warn",
        120,
        start + Duration::from_secs(119),
    );
    assert_eq!(stable, "ok");
    assert!(!changed);

    let (stable, previous, changed) = crate::stabilize_memory_pressure_at(
        &mut state,
        "warn",
        120,
        start + Duration::from_secs(120),
    );
    assert_eq!(stable, "warn");
    assert_eq!(previous.as_deref(), Some("ok"));
    assert!(changed);

    let (stable, _, changed) = crate::stabilize_memory_pressure_at(
        &mut state,
        "ok",
        120,
        start + Duration::from_secs(121),
    );
    assert_eq!(stable, "warn");
    assert!(!changed);

    let (stable, previous, changed) = crate::stabilize_memory_pressure_at(
        &mut state,
        "ok",
        120,
        start + Duration::from_secs(241),
    );
    assert_eq!(stable, "ok");
    assert_eq!(previous.as_deref(), Some("warn"));
    assert!(changed);
}

// ---------------------------------------------------------------------------
// prediction
// ---------------------------------------------------------------------------

#[test]
fn predict_fill_time_requires_minimum_samples() {
    // Empty history → none
    let history: Vec<(Instant, u8)> = vec![];
    assert!(crate::predict_fill_time(&history).is_none());

    // Only 1 sample → none (need at least 2)
    let history = vec![(Instant::now(), 70)];
    assert!(crate::predict_fill_time(&history).is_none());
}

#[test]
fn predict_fill_time_returns_none_for_stable_disk() {
    // Stable disk (no change) → infinite fill time → none
    let now = Instant::now();
    let history: Vec<(Instant, u8)> = vec![
        (now - std::time::Duration::from_secs(3600), 50u8),
        (now - std::time::Duration::from_secs(1800), 50u8),
        (now, 50u8),
    ];
    assert!(crate::predict_fill_time(&history).is_none());
}

#[test]
fn predict_fill_time_estimates_for_filling_disk() {
    // predict_fill_time requires at least 3 samples
    let now = Instant::now();
    let history: Vec<(Instant, u8)> = vec![
        (now - std::time::Duration::from_secs(7200), 30u8), // 2 hours ago: 30%
        (now - std::time::Duration::from_secs(3600), 60u8), // 1 hour ago: 60%
        (now, 90u8),                                        // now: 90%
    ];
    let result = crate::predict_fill_time(&history);
    assert!(result.is_some());
    // 30% in 2 hours → 15%/hour → 10% remaining → ~40 minutes
    let hours = result.unwrap();
    assert!(hours > 0.0 && hours < 5.0); // Should be roughly 0.67 hours (40 min)
}

// ---------------------------------------------------------------------------
// AutoCleanupResult
// ---------------------------------------------------------------------------

#[test]
fn auto_cleanup_result_default() {
    let result = crate::AutoCleanupResult::default();
    assert_eq!(result.cleaned_count, 0);
    assert_eq!(result.reclaimed_bytes, 0);
    assert!(result.cleaned_paths.is_empty());
    assert!(result.protected_paths.is_empty());
}

// ---------------------------------------------------------------------------
// Memory pressure (ADDED 2026-08-10, v0.112.35)
// ---------------------------------------------------------------------------

#[test]
fn parse_meminfo_reads_all_four_fields() {
    let sample = r#"MemTotal:       32754688 kB
MemFree:         1098776 kB
MemAvailable:    9010908 kB
SwapTotal:      32680964 kB
SwapFree:       11556016 kB
"#;
    let m = crate::parse_meminfo(sample).expect("meminfo should parse");
    assert_eq!(m.mem_total_kb, 32754688);
    assert_eq!(m.mem_available_kb, 9010908);
    assert_eq!(m.swap_total_kb, 32680964);
    assert_eq!(m.swap_free_kb, 11556016);
    // 9010908 / 32754688 ≈ 27%
    assert_eq!(m.mem_available_percent(), 27);
    // (32680964 - 11556016) / 32680964 ≈ 64.6%
    assert_eq!(m.swap_used_percent(), 64);
}

#[test]
fn parse_meminfo_missing_swap_reports_zero_used() {
    let sample = "MemTotal:       1048576 kB\nMemAvailable:     65536 kB\n";
    let m = crate::parse_meminfo(sample).expect("meminfo should parse");
    assert_eq!(m.mem_available_percent(), 6);
    assert_eq!(m.swap_used_percent(), 0);
}

#[test]
fn parse_meminfo_garbage_returns_none() {
    assert!(crate::parse_meminfo("not meminfo at all").is_none());
    assert!(crate::parse_meminfo("").is_none());
}

#[test]
fn parse_pressure_memory_extracts_full_and_some() {
    let psi = r#"some avg10=1.23 avg60=2.00 avg300=0.66 total=3743158654
full avg10=4.56 avg60=0.30 avg300=0.65 total=3579061268
"#;
    let (full, some) = crate::parse_pressure_memory(psi).expect("psi should parse");
    assert!((full - 4.56).abs() < 1e-9);
    assert!((some - 1.23).abs() < 1e-9);
}

#[test]
fn parse_pressure_memory_no_full_line_returns_none() {
    assert!(
        crate::parse_pressure_memory("some avg10=1.00 avg60=1.00 avg300=1.00 total=1").is_none()
    );
}

#[test]
fn parse_vmstat_swap_reads_counters() {
    let vmstat = "pswpin 1022648496\npswpout 1187700919\nnr_free_pages 12345\n";
    let (pin, pout) = crate::parse_vmstat_swap(vmstat).expect("vmstat should parse");
    assert_eq!(pin, 1022648496);
    assert_eq!(pout, 1187700919);
}

// ---------------------------------------------------------------------------
// Zombie parsing (ADDED 2026-08-10, v0.112.35)
// ---------------------------------------------------------------------------

#[test]
fn record_swap_counters_preserves_pswpout() {
    let mut state = crate::GuardRuntimeState::default();
    crate::record_swap_counters(&mut state, 123, 456);
    let (_, pswpin, pswpout) = state.prev_swap_counters.expect("counters recorded");
    assert_eq!(pswpin, 123);
    assert_eq!(pswpout, 456);
}

#[test]
fn parse_proc_stat_zombie_detects_z_state() {
    // pid=1234, comm contains a space (valid), state=Z, ppid=567,
    // starttime=999 (field 22, index 19 after pid+comm).
    let line = "1234 (chrome --type=renderer) Z 567 1 567 0 -1 4194304 42 0 0 0 0 0 0 0 20 0 1 0 999 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
    let (pid, comm, ppid, starttime) =
        crate::parse_proc_stat_zombie(line).expect("zombie stat should parse");
    assert_eq!(pid, 1234);
    assert_eq!(comm, "chrome --type=renderer");
    assert_eq!(ppid, 567);
    assert_eq!(starttime, 999);
}

#[test]
fn parse_proc_stat_zombie_ignores_non_zombies() {
    let line = "42 (bash) S 1 42 42 0 -1 4194304 100 0 0 0 0 0 0 0 20 0 1 0 123 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
    assert!(crate::parse_proc_stat_zombie(line).is_none());
}

// ---------------------------------------------------------------------------
// Trash credential guard (ADDED 2026-08-10, v0.112.35)
// ---------------------------------------------------------------------------

#[test]
fn looks_credential_like_matches_known_patterns() {
    assert!(crate::looks_credential_like("Login Data"));
    assert!(crate::looks_credential_like("CREDENTIALS.md"));
    assert!(crate::looks_credential_like("secrets.rs"));
    assert!(crate::looks_credential_like(".env"));
    assert!(crate::looks_credential_like("github.env"));
    assert!(crate::looks_credential_like("id_ed25519.key"));
    assert!(crate::looks_credential_like("backup.age"));
    assert!(crate::looks_credential_like(".git-credentials"));
    assert!(crate::looks_credential_like(".npmrc"));
    assert!(crate::looks_credential_like("hosts.yml"));
    assert!(crate::looks_credential_like("password-store"));
    assert!(crate::looks_credential_like("wallet-token-cache"));
}

#[test]
fn looks_credential_like_ignores_benign_names() {
    assert!(!crate::looks_credential_like("target"));
    assert!(!crate::looks_credential_like("node_modules"));
    assert!(!crate::looks_credential_like("ai-vid-editor"));
    assert!(!crate::looks_credential_like("nixpkgs"));
    assert!(!crate::looks_credential_like("go"));
    assert!(!crate::looks_credential_like("saved_stuff"));
    assert!(!crate::looks_credential_like("references"));
}

// ---------------------------------------------------------------------------
// Disk fill rate (ADDED 2026-08-10, v0.112.35)
// ---------------------------------------------------------------------------

#[test]
fn disk_fill_rate_gbph_computes_sustained_rate() {
    let now = Instant::now();
    // 10 GiB per hour for 2 hours = 20 GiB growth.
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(7200), 500u64 * 1024 * 1024 * 1024),
        (now - Duration::from_secs(3600), 510u64 * 1024 * 1024 * 1024),
        (now, 520u64 * 1024 * 1024 * 1024),
    ];
    let rate = crate::disk_fill_rate_gbph(&history).expect("rate should compute");
    assert!((rate - 10.0).abs() < 0.5, "expected ~10 GiB/h, got {rate}");
}

#[test]
fn disk_fill_rate_gbph_requires_minimum_samples_and_span() {
    let now = Instant::now();
    assert!(crate::disk_fill_rate_gbph(&[]).is_none());
    assert!(crate::disk_fill_rate_gbph(&[(now, 100), (now, 200)]).is_none());
    // Span too short (10s < 60s).
    let short: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(10), 100),
        (now - Duration::from_secs(5), 200),
        (now, 300),
    ];
    assert!(crate::disk_fill_rate_gbph(&short).is_none());
}

#[test]
fn disk_fill_rate_gbph_returns_none_when_disk_shrinks() {
    let now = Instant::now();
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(7200), 600u64 * 1024 * 1024 * 1024),
        (now - Duration::from_secs(3600), 550u64 * 1024 * 1024 * 1024),
        (now, 500u64 * 1024 * 1024 * 1024),
    ];
    assert!(crate::disk_fill_rate_gbph(&history).is_none());
}

// ── OOM-bias steering (v0.112.36) ────────────────────────────────────────

#[test]
fn oom_bias_target_raises_neutral_values_to_250() {
    assert_eq!(crate::oom_bias_target(0), Some(250));
    assert_eq!(crate::oom_bias_target(100), Some(250));
    assert_eq!(crate::oom_bias_target(-100), Some(250));
    assert_eq!(crate::oom_bias_target(-300), Some(250));
}

#[test]
fn oom_bias_target_never_raises_already_biased_or_protected() {
    // Already at/above the target: leave alone.
    assert_eq!(crate::oom_bias_target(250), None);
    assert_eq!(crate::oom_bias_target(500), None);
    assert_eq!(crate::oom_bias_target(1000), None);
    // Deliberately protected (unkillable or strongly shielded): never touch.
    assert_eq!(crate::oom_bias_target(-500), None);
    assert_eq!(crate::oom_bias_target(-800), None);
    assert_eq!(crate::oom_bias_target(-1000), None);
}

#[test]
fn oom_bias_target_boundaries() {
    assert_eq!(crate::oom_bias_target(249), Some(250));
    assert_eq!(crate::oom_bias_target(250), None);
    assert_eq!(crate::oom_bias_target(-499), Some(250));
    assert_eq!(crate::oom_bias_target(-500), None);
}

// ── Memory-limiter policy defaults (v0.112.36) ──────────────────────────

#[test]
fn memory_limiter_policy_defaults_are_safe() {
    let p = crate::GuardPolicy::default();
    assert!(!p.freeze_sync_at_action, "sync freeze must be opt-in");
    assert!(!p.auto_renice, "CPU priority changes must be opt-in");
    assert!(p.auto_renice_on_memory, "renice-on-memory default on");
    assert!(p.bias_oom_on_pressure, "oom-bias default on");
    assert_eq!(p.memory_pressure_sustain_secs, 120);
    assert_eq!(p.report_repeat_secs, 1800);
    assert_eq!(
        p.cap_offenders_cpu_percent, 0,
        "CPUQuota offender caps default OFF (opt-in)"
    );
}

#[test]
fn normalize_guard_policy_clamps_cpu_cap_percent() {
    let mut policy = crate::GuardPolicy {
        cap_offenders_cpu_percent: u32::MAX,
        ..Default::default()
    };
    crate::normalize_guard_policy(&mut policy);
    assert_eq!(policy.cap_offenders_cpu_percent, 100);

    policy.cap_offenders_cpu_percent = 100;
    crate::normalize_guard_policy(&mut policy);
    assert_eq!(policy.cap_offenders_cpu_percent, 100);
}

#[test]
fn clean_node_modules_flag_defaults_true_and_gates_toml_off() {
    // Audit M3 (2026-08-21): node_modules cleanup was the only cleanup
    // kind with no feature flag. The knob must default true (behavior
    // preserved) and parse false from TOML; run_auto_cleanup consults it
    // exactly like its clean_trash / clean_nix_garbage siblings, while
    // explicit `guard clean --node-modules` stays ungated.
    let policy: crate::GuardPolicy =
        toml::from_str("").expect("empty policy parses with defaults");
    assert!(policy.clean_node_modules);
    let policy: crate::GuardPolicy =
        toml::from_str("clean_node_modules = false\n").expect("flag-off parses");
    assert!(!policy.clean_node_modules);
}

#[test]
fn policy_load_roundtrip_memory_limiter_knobs() {
    // TOML parse → defaults preserved when keys absent.
    let policy: crate::GuardPolicy = toml::from_str("").expect("empty policy parses with defaults");
    assert!(policy.auto_renice_on_memory);
    assert!(policy.bias_oom_on_pressure);
    assert_eq!(policy.cap_offenders_cpu_percent, 0);
    // Explicit values round-trip.
    let policy: crate::GuardPolicy = toml::from_str(
        "auto_renice_on_memory = false\nbias_oom_on_pressure = false\ncap_offenders_cpu_percent = 50\n",
    )
    .expect("explicit knobs parse");
    assert!(!policy.auto_renice_on_memory);
    assert!(!policy.bias_oom_on_pressure);
    assert_eq!(policy.cap_offenders_cpu_percent, 50);
}
