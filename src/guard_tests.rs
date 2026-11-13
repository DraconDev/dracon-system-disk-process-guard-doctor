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
        swap_mb: Some(0),
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
        extra_mounts: Vec::new(),
        relocate_candidates: Vec::new(),
        relocated_count: 0,
        relocated_bytes: 0,
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
        service_memory_growth: crate::service_growth::Tracker::default().observe(
            &GuardPolicy::default(),
            Vec::new(),
            Instant::now(),
        ),
        zombies: Vec::new(),
        reap_candidates: Vec::new(),
        reaped: Vec::new(),
        retired_sessions: Vec::new(),
        session_fleet: SessionFleetReport::default(),
        disk_fill_gbph: None,
        swap_issues: Vec::new(),
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
    assert_eq!(crate::graduated_nice_value(180.0, 0, 5, 0), 5);
    assert_eq!(crate::graduated_nice_value(200.0, 0, 5, 0), 5);
}

#[test]
fn graduated_nice_value_cpu_tier_300_percent() {
    // CPU >= 300% → nice 10
    assert_eq!(crate::graduated_nice_value(300.0, 0, 5, 0), 10);
    assert_eq!(crate::graduated_nice_value(350.0, 0, 5, 0), 10);
}

#[test]
fn graduated_nice_value_cpu_tier_500_percent() {
    // CPU >= 500% → nice 15
    assert_eq!(crate::graduated_nice_value(500.0, 0, 5, 0), 15);
    assert_eq!(crate::graduated_nice_value(600.0, 0, 5, 0), 15);
}

#[test]
fn graduated_nice_value_memory_4gb() {
    // 4 GB in MB = 4096
    assert_eq!(crate::graduated_nice_value(0.0, 4096, 5, 0), 5);
}

#[test]
fn graduated_nice_value_memory_8gb() {
    // 8 GB in MB = 8192
    assert_eq!(crate::graduated_nice_value(0.0, 8192, 10, 0), 10);
}

#[test]
fn graduated_nice_value_below_all_tiers_uses_base() {
    // Below all thresholds → base nice value
    assert_eq!(crate::graduated_nice_value(50.0, 100, 3, 0), 3);
    assert_eq!(crate::graduated_nice_value(100.0, 500, 7, 0), 7);
}

#[test]
fn graduated_nice_value_preserves_current_nice() {
    // A process already at a lower priority must not be raised to a tier's
    // smaller absolute nice value.
    assert_eq!(crate::graduated_nice_value(180.0, 0, 5, 10), 10);
    assert_eq!(crate::graduated_nice_value(50.0, 100, 5, 10), 10);
}

#[test]
fn graduated_nice_value_negative_base_clamped_to_zero() {
    assert_eq!(crate::graduated_nice_value(0.0, 0, -5, 0), 0);
}

#[test]
fn graduated_nice_value_high_base_clamped_to_max() {
    // Nice values are capped at 19
    assert_eq!(crate::graduated_nice_value(0.0, 0, 20, 0), 19);
}

#[test]
fn memory_offender_nice_value_uses_swap_aware_footprint() {
    // A mostly-swapped low-RSS offender must still reach the graduated
    // memory tiers (regression: mitigation tiers previously ran on RSS alone,
    // so this class never graduated past base nice).
    let mut p = crate::parse_ps_output("4242 1 0 102400 0 worker\n")
        .pop()
        .unwrap();
    assert_eq!(p.rss_mb, 100);
    p.swap_mb = Some(30 * 1024);
    assert_eq!(crate::memory_pressure_footprint_mb(&p), 100 + 30 * 1024);
    assert_eq!(crate::memory_offender_nice_value(&p, 0), 10);
    // Unknown swap falls back to RSS behavior, not to a tier.
    p.swap_mb = None;
    assert_eq!(crate::memory_offender_nice_value(&p, 0), 0);
}

#[test]
fn load_process_cmdline_skips_args_for_dead_pid() {
    // A PID that cannot exist must keep its existing args: overwriting them
    // after the incarnation changed would attach a stranger's (or empty)
    // cmdline to the reported row.
    let mut p = crate::parse_ps_output("2147483647 1 0 102400 0 worker\n")
        .pop()
        .unwrap();
    p.args = String::from("sentinel");
    let out = crate::load_process_cmdline(p);
    assert_eq!(out.args, "sentinel");
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

// ---------------------------------------------------------------------------
// memory-pressure reporting: class hysteresis + the exempt-consumer note
//
// ADDED 2026-11-08 (audit guard-memory-pressure-reporting-2026-11-08).
// The stabilized level flipped warn<->critical on a ~10-20 minute cadence
// for hours on this host, because the fourth classification input
// (swap-in velocity) is bursty. Every flip counted as a transition, and the
// memory notification path never used `should_notify`, so each flip
// produced a desktop Error with no cooldown — and an event, which the sync
// daemon then auto-committed into the machine-state repo.
//
// The daemon's test build never persists events (`events::emit_event`
// returns under `cfg!(test)`), so these tests drive the SAME decision
// function the daemon calls and assert on the decision itself.
// ---------------------------------------------------------------------------

/// A level oscillating between warn and critical is NOT a new condition.
///
/// Before the fix, each flip was a transition: one event and one desktop
/// notification per flip. Now the pair share the class "pressure", so the
/// oscillation emits nothing and notifies nothing.
#[test]
fn memory_pressure_flipping_between_warn_and_critical_is_not_a_transition() {
    let mut state = crate::GuardRuntimeState::default();

    // Start clean: no pressure at all. The first "ok" pass is not an event
    // and not a notification.
    let decision = crate::memory_pressure_report_decision(&mut state, "ok", 1800);
    assert!(!decision.emit_event, "a calm pass must not emit");
    assert!(!decision.notify, "a calm pass must not notify");

    // Now oscillate: warn, critical, warn, critical ... 40 passes.
    let mut events = 0usize;
    let mut notifications = 0usize;
    for index in 0..40 {
        let level = if index % 2 == 0 { "warn" } else { "critical" };
        let decision = crate::memory_pressure_report_decision(&mut state, level, 1800);
        if decision.emit_event {
            events += 1;
        }
        if decision.notify {
            notifications += 1;
        }
    }
    assert_eq!(
        events, 1,
        "an oscillating level must produce exactly one event (the arrival), got {events}"
    );
    assert_eq!(
        notifications, 1,
        "an oscillating level must produce exactly one notification (the arrival), got {notifications}"
    );
}

/// A GENUINE arrival into pressure still reports, immediately.
#[test]
fn memory_pressure_arrival_still_emits_and_notifies() {
    let mut state = crate::GuardRuntimeState::default();
    assert!(!crate::memory_pressure_report_decision(&mut state, "ok", 1800).emit_event);

    let arrival = crate::memory_pressure_report_decision(&mut state, "critical", 1800);
    assert!(arrival.emit_event, "arrival into pressure must emit");
    assert!(arrival.notify, "arrival into pressure must notify");

    // And it is not repeated while the condition persists.
    let sustained = crate::memory_pressure_report_decision(&mut state, "warn", 1800);
    assert!(!sustained.emit_event, "a sustained condition must not re-emit");
    assert!(!sustained.notify, "a sustained condition must not re-notify");
}

/// A GENUINE recovery still reports, immediately.
#[test]
fn memory_pressure_recovery_still_emits() {
    let mut state = crate::GuardRuntimeState::default();
    crate::memory_pressure_report_decision(&mut state, "warn", 1800);

    let recovery = crate::memory_pressure_report_decision(&mut state, "ok", 1800);
    assert!(recovery.emit_event, "recovery must emit");
    assert!(!recovery.notify, "recovery must not notify: nothing is wrong");
}

/// The report must say WHY nothing was mitigated when every top consumer is
/// exempt.
///
/// The offenders list excludes exempt processes BY POLICY, so the report
/// could read "critical, top: (none)" while the real consumers — the
/// operator's agent sessions and browser — were invisible. That opaque
/// report is what turns into a manual intervention under pressure
/// (docs/design/performance-swap-thrash-2026-10-21.md).
#[test]
fn memory_pressure_note_explains_an_exempt_only_offender_set() {
    let exempt = vec![("pi".to_string(), 3_072u64), ("chrome".to_string(), 4_608)];
    let note = crate::build_mitigation_note(&[], &exempt);
    assert!(
        note.contains("no mitigation applied"),
        "the note must say no mitigation was applied: {note}"
    );
    assert!(
        note.contains("exempt"),
        "the note must say the consumers are exempt: {note}"
    );
    assert!(note.contains("pi"), "the note must name the consumer: {note}");
    assert!(
        note.contains("chrome"),
        "the note must name every named consumer: {note}"
    );
}

/// When there IS a mitigation candidate, the note must say so and must not
/// claim nothing was done.
#[test]
fn memory_pressure_note_reports_mitigation_candidates() {
    let sample = ProcSample {
        swap_mb: Some(64),
        pid: 4242,
        ppid: 1,
        cpu_percent: 0.0,
        rss_mb: 512,
        nice: 0,
        command: "node".to_string(),
        args: String::new(),
        starttime: 99,
    };
    let note = crate::build_mitigation_note(&[sample], &[]);
    assert!(
        !note.contains("no mitigation applied"),
        "a report with candidates must not claim nothing was done: {note}"
    );
    assert!(
        note.contains("mitigation candidates"),
        "the note must point at the candidates: {note}"
    );
}

/// The exempt-consumer footprint is tallied PER COMMAND and ordered by
/// footprint, so the report names "pi 3072MiB" rather than 31 separate
/// lines.
#[test]
fn exempt_consumer_footprint_groups_by_command() {
    let sample = |pid: i32, command: &str, rss_mb: u64| ProcSample {
        swap_mb: Some(0),
        pid,
        ppid: 1,
        cpu_percent: 0.0,
        rss_mb,
        nice: 0,
        command: command.to_string(),
        args: String::new(),
        starttime: pid as u64,
    };
    let samples = vec![
        sample(10, "pi", 300),
        sample(11, "pi", 200),
        sample(12, "chrome", 400),
        sample(13, "node", 900),
    ];
    let exempt: std::collections::HashSet<String> =
        ["pi".to_string(), "chrome".to_string()].into_iter().collect();
    let rows = crate::exempt_consumer_footprint(&samples, &exempt);
    assert_eq!(rows.len(), 2, "one row per exempt command: {rows:?}");
    assert_eq!(rows[0].0, "pi", "the biggest consumer is named first");
    assert_eq!(rows[1].0, "chrome");
    assert_eq!(
        rows[0].1, 500,
        "the footprint of a command is summed across its processes"
    );
    assert_eq!(rows[1].1, 400);
    assert!(
        !rows.iter().any(|(command, _)| command == "node"),
        "a non-exempt process must not be reported as an exempt consumer"
    );
}

/// REGRESSION (audit 2026-11-13, MED): the fleet-aware exemption must
/// survive the "every session is idle" case.
///
/// Twelve parked sessions with a bound of eight is an over-bound fleet
/// whose sessions are ALL idle, so the pass set is legitimately EMPTY.
/// The consumer must read that empty set as "no session holds the
/// exemption" — NOT as "no lifecycle configured". Reading it the old way
/// restored the blanket per-name exemption over the whole fleet, which is
/// the incident's own failure mode: the guard reports a crisis it has no
/// authority to act on.
#[test]
fn an_all_idle_over_bound_fleet_is_reported_as_offenders_not_exempt_consumers() {
    let sample = |pid: i32, command: &str, rss_mb: u64| ProcSample {
        swap_mb: Some(0),
        pid,
        ppid: 1,
        cpu_percent: 0.0,
        rss_mb,
        nice: 0,
        command: command.to_string(),
        args: String::new(),
        starttime: pid as u64,
    };
    // 12 parked sessions, fleet bound 8 -> over bound, all idle.
    let samples: Vec<ProcSample> = (0..12).map(|i| sample(1000 + i, "pi", 300)).collect();
    let exempt: std::collections::HashSet<String> = ["pi".to_string()].into_iter().collect();

    // The lifecycle IS configured and NO session holds the pass: the empty
    // set, exactly as the producer computes it for an all-idle fleet.
    let session_exempt_pids: std::collections::HashSet<i32> = std::collections::HashSet::new();

    let (offenders, _) =
        crate::memory_pressure_offenders_with_sessions(&samples, &exempt, Some(&session_exempt_pids));
    assert!(
        !offenders.is_empty(),
        "an all-idle over-bound fleet must be mitigation candidates, not exempt"
    );
    assert!(
        offenders.iter().all(|p| p.command == "pi"),
        "the candidates are exactly the parked sessions"
    );
    assert!(
        offenders.len() <= crate::MEMORY_OFFENDER_LIMIT,
        "the report is capped, so the count is a ceiling rather than a total"
    );

    let exempt_rows = crate::exempt_consumer_footprint_with_sessions(
        &samples,
        &exempt,
        Some(&session_exempt_pids),
    );
    assert!(
        exempt_rows.is_empty(),
        "a session that gave up its pass must not be reported as an exempt consumer"
    );

    // The UNCONFIGURED lifecycle is a different state and keeps the plain
    // name-based behaviour: the sessions stay exempt.
    let (offenders, _) = crate::memory_pressure_offenders_with_sessions(&samples, &exempt, None);
    assert!(
        offenders.is_empty(),
        "without a lifecycle the name-based exemption applies unchanged"
    );
    let exempt_rows = crate::exempt_consumer_footprint_with_sessions(&samples, &exempt, None);
    assert_eq!(
        exempt_rows.len(),
        1,
        "without a lifecycle the exempt consumer is still reported"
    );
}

/// warn and critical are the same CLASS; ok is its own class.
#[test]
fn memory_pressure_class_collapses_warn_and_critical() {
    assert_eq!(crate::memory_pressure_class("ok"), "ok");
    assert_eq!(crate::memory_pressure_class("warn"), "pressure");
    assert_eq!(crate::memory_pressure_class("critical"), "pressure");
}

/// The EMITTED text must carry the explanation.
///
/// The daemon's test build never persists events, so the event text has to
/// be assertable directly. This is the operator-facing surface: without
/// it, a `critical` event gives no hint that the guard is not permitted to
/// act on what is consuming the memory.
#[test]
fn memory_pressure_event_text_explains_an_exempt_only_offender_set() {
    let exempt = vec![("pi".to_string(), 2_752u64), ("chrome".to_string(), 4_704)];
    let note = crate::build_mitigation_note(&[], &exempt);
    let message = crate::memory_pressure_event_message(
        "critical",
        "critical",
        12,
        29,
        Some(2.65),
        &[],
        0,
        &note,
    );
    assert!(
        message.contains("no mitigation applied"),
        "the event text must say no mitigation was applied: {message}"
    );
    assert!(
        message.contains("exempt"),
        "the event text must say the consumers are exempt: {message}"
    );
    assert!(
        message.contains("pi") && message.contains("chrome"),
        "the event text must name the exempt consumers: {message}"
    );
    // The original facts must survive alongside the explanation.
    assert!(message.contains("critical"), "{message}");
    assert!(message.contains("mem available 12%"), "{message}");
}

/// The NOTIFICATION text must carry the same explanation.
#[test]
fn memory_pressure_notification_text_explains_the_mitigation_outcome() {
    let exempt = vec![("pi".to_string(), 2_752u64)];
    let note = crate::build_mitigation_note(&[], &exempt);
    let body = crate::memory_pressure_notification_body(
        "critical",
        12,
        29,
        Some(2.65),
        "(none)",
        0,
        &note,
    );
    assert!(
        body.contains("no mitigation applied"),
        "the notification body must say no mitigation was applied: {body}"
    );
    assert!(body.contains("pi"), "the notification body must name it: {body}");
}

/// ADDED 2026-10-27 (wrong-swap check, falsification fix): the periodic
/// finding must be OBSERVABLE from the daemon pass — recorded in the
/// pass state (entry/repeat dedup) and returned for the report — not
/// silently computed. This pins the state key and the ok/wrong agreement;
/// live swap contents stay host-dependent and are asserted nowhere.
#[tokio::test]
async fn swap_devices_check_records_its_state_every_pass() {
    let guard = crate::GuardPolicy::default();
    let mut state = crate::GuardRuntimeState::default();
    let issues = crate::check_swap_devices(&guard, &mut state).await;
    let recorded = state.report_states["swap-devices"].value.clone();
    assert_eq!(
        recorded,
        if issues.is_empty() { "ok" } else { "wrong" },
        "the recorded state must agree with what the pass found"
    );
    // A second pass with unchanged findings keeps the state (entry fires
    // once, repeats go through the event stream at report_repeat_secs).
    let again = crate::check_swap_devices(&guard, &mut state).await;
    assert_eq!(again, issues);
    assert_eq!(state.report_states["swap-devices"].value, recorded);
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
fn swapin_velocity_flags_thrash_at_or_above_threshold() {
    // 2026-10-04: si ran at 100k+ pages/s while PSI sat at 3.9 and the
    // verdict read "ok", because velocity was only measured as a
    // PSI-absent fallback. Velocity is now a first-class thrash signal.
    assert!(!crate::swapin_velocity_thrash(None, 1000));
    assert!(!crate::swapin_velocity_thrash(Some(0.0), 1000));
    assert!(!crate::swapin_velocity_thrash(Some(999.9), 1000));
    assert!(crate::swapin_velocity_thrash(Some(1000.0), 1000));
    assert!(crate::swapin_velocity_thrash(Some(170_000.0), 1000));
}

#[test]
fn swapin_velocity_threshold_defaults_to_the_legacy_fallback() {
    // The old hardcoded PSI-fallback constant was 1000 pages/s; the knob
    // keeps that value, so PSI-absent hosts see no behavior change.
    assert_eq!(
        crate::GuardPolicy::default().mem_swapin_warn_pages_per_sec,
        1000
    );
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
// disk-trend warning
// ---------------------------------------------------------------------------

/// The disk-trend warning must fire for a SLOW fill — the case the
/// rapid-fill threshold misses.
///
/// ADDED 2026-10-25 (audit D8): the prediction used to regress the PERCENT
/// history over a ten-minute window. Below roughly 50 GiB/h the disk does
/// not move a whole percent inside that window (1% of a 907 GiB disk is
/// ~9 GiB), so the predictor found no growth and reported "not filling". The
/// guard therefore stayed silent about a disk that was hours from full.
#[test]
fn a_slow_fill_warns_even_though_it_is_below_the_rapid_fill_threshold() {
    use crate::{disk_trend_warning, DiskFillRate};
    let gib = 1_073_741_824u64;
    // 138 GiB free, filling at 10 GiB/h → 13.8 hours to full, inside the
    // 24-hour horizon, and BELOW the 20 GiB/h rapid-fill threshold.
    let warning = disk_trend_warning(DiskFillRate::Filling(10.0), 138 * gib, 24, 20.0)
        .expect("a disk 13.8 hours from full must warn");
    assert!(
        (warning.hours - 13.8).abs() < 0.05,
        "the figure must be the honest one: {}",
        warning.hours
    );
    assert_eq!(warning.gbph, 10.0);
}

/// The rapid-fill alarm owns the fast case: the trend warning must not
/// raise a second alarm saying the same thing.
#[test]
fn the_rapid_fill_alarm_owns_the_fast_case() {
    use crate::{disk_trend_warning, DiskFillRate};
    let gib = 1_073_741_824u64;
    // 138 GiB free at 55.8 GiB/h → 2.5 hours. The rapid-fill alarm (>= 20
    // GiB/h) already says this, with the same figure.
    assert!(
        disk_trend_warning(DiskFillRate::Filling(55.8), 138 * gib, 24, 20.0).is_none(),
        "a fast fill is the rapid-fill alarm's job — two alarms saying one thing is noise"
    );
}

/// A rate that was not measured must never become a deadline.
#[test]
fn an_unmeasured_rate_never_produces_a_deadline() {
    use crate::{disk_trend_warning, DiskFillRate};
    let gib = 1_073_741_824u64;
    // "Unmeasured" is not "not filling" and not "hours until full" (the D1
    // rule): the guard states nothing it has not measured.
    assert!(disk_trend_warning(DiskFillRate::Unmeasured, 138 * gib, 24, 20.0).is_none());
    // A disk that is genuinely not filling has no deadline either.
    assert!(disk_trend_warning(DiskFillRate::NotFilling, 138 * gib, 24, 20.0).is_none());
}

/// The horizon and the "never alert" sentinel both silence the warning.
#[test]
fn the_trend_horizon_bounds_the_warning() {
    use crate::{disk_trend_warning, DiskFillRate};
    let gib = 1_073_741_824u64;
    // 138 GiB free at 10 GiB/h → 13.8 h. Outside a 12-hour horizon → quiet.
    assert!(disk_trend_warning(DiskFillRate::Filling(10.0), 138 * gib, 12, 20.0).is_none());
    // `trend_warn_hours = 0` is the documented "never alert" sentinel.
    assert!(disk_trend_warning(DiskFillRate::Filling(10.0), 138 * gib, 0, 20.0).is_none());
}

/// Every MONITORING capability must leave a RECORD, not just a popup.
///
/// ADDED 2026-10-25 (audit D9): five of the guard's observations reached
/// only a desktop notification — the disk pressure transition, the disk
/// early warning, the heavy-process alert, the inode warning and the
/// large-log warning. A popup is transient and belongs to the desktop: if
/// the operator was away, or the session had no display, the observation
/// was erased completely — the guard had noticed something and there was no
/// way to find out what. The guard's own rule (from the log-directory
/// check) is that "a silent skip is exactly how a monitoring feature
/// disappears without anyone noticing".
///
/// This pins the list: each check must emit an event under a named path, so
/// `dracon-system events` shows what the guard saw.
#[test]
fn every_monitoring_capability_records_an_event() {
    const SOURCE: &str = include_str!("main.rs");
    const CHECKS: &[(&str, &str)] = &[
        ("fn check_disk_state_change(", "disk/state-change"),
        ("fn check_disk_early_warning(", "disk/early-warning"),
        ("fn check_rapid_disk_fill(", "disk/rapid-fill"),
        ("fn check_disk_trends(", "disk/trend-warning"),
        ("fn check_memory_pressure(", "memory/pressure"),
        ("fn check_heavy_processes(", "process/heavy"),
        ("fn check_zombie_processes(", "process/zombies"),
        ("fn check_inode_usage(", "filesystem/inode-warning"),
        ("fn check_large_logs(", "filesystem/log-size"),
        ("fn check_extra_mounts(", "disk/extra-mount"),
        ("fn check_reap_candidates(", "process/reap-candidates"),
        ("fn check_link_drift(", "links-drifted"),
    ];
    for (signature, event_path) in CHECKS {
        let body = top_level_fn_source(SOURCE, signature);
        assert!(
            body.contains("emit_event("),
            "{} must record what it observed — a desktop popup is transient and is erased when the operator is away",
            signature
        );
        assert!(
            body.contains(event_path),
            "{} must record it as `{}` so it can be found in the event stream",
            signature,
            event_path
        );
    }
}

/// The SECONDARY disks must be reported by the DAEMON, not only by the
/// one-shot table.
///
/// ADDED 2026-10-25 (audit D10): `disk_extra_mounts` was read on every pass
/// and the result was thrown away — the daemon discards the `GuardReport`,
/// so the extra mounts were visible to `dracon-system guard once` and to
/// nobody else. It matters because the guard's reclaim strategy leans on
/// that disk: the stash lives on it, and a stash move to a full disk FAILS,
/// so the guard stops reclaiming anything while the reason goes unmentioned.
#[test]
fn the_daemon_reports_the_secondary_mounts() {
    const SOURCE: &str = include_str!("main.rs");

    // The guard PASS calls the reporter — the daemon discards the report, so
    // the check has to be inside the pass to be seen at all.
    let pass = top_level_fn_source(SOURCE, "pub(crate) async fn run_guard_once(");
    assert!(
        pass.contains("disk_details_for(&mount)"),
        "the guard pass must have been located — otherwise the assertion below would pass on an empty region"
    );
    assert!(
        pass.contains("check_extra_mounts("),
        "the guard pass must report the secondary mounts: the daemon discards the GuardReport, so a check outside the pass is invisible"
    );

    // An unreadable mount is collected, not dropped at a verbosity the daemon
    // never prints.
    assert!(
        !pass.contains("veprintln!(1, \"extra mount"),
        "an unreadable secondary mount must be COLLECTED and reported, not dropped at verbosity 1"
    );
}

/// The abandoned dev/test process WORKLIST must be reported by the daemon.
///
/// ADDED 2026-10-25 (audit D11): the candidates went into the `GuardReport`
/// and the daemon DISCARDS the `GuardReport`, so the worklist was visible
/// only to `dracon-system guard once` — a worklist the operator has to ask
/// for is a worklist nobody acts on. The candidates are dev servers that
/// have been alive 24+ hours with almost no CPU, on a host that runs at
/// 1–3% available memory.
#[test]
fn the_daemon_reports_the_abandoned_dev_server_worklist() {
    const SOURCE: &str = include_str!("main.rs");
    let pass = top_level_fn_source(SOURCE, "pub(crate) async fn run_guard_once(");
    assert!(
        pass.contains("reap_candidates(guard)"),
        "the guard pass must have been located — otherwise the assertion below would pass on an empty region"
    );
    assert!(
        pass.contains("check_reap_candidates("),
        "the guard pass must report the abandoned dev/test worklist: the daemon discards the GuardReport, so a check outside the pass is invisible"
    );
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
    // CHANGED 2026-10-25 (audit, operator decision): the loose substring
    // rule is gone, so a name that merely CONTAINS a credential word no
    // longer counts. It kept an entire 5 GiB deleted item alive on its
    // own.
    assert!(!crate::looks_credential_like("wallet-token-cache"));
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
fn disk_fill_rate_computes_sustained_rate() {
    let now = Instant::now();
    // 10 GiB per hour for 2 hours = 20 GiB growth.
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(7200), 500u64 * 1024 * 1024 * 1024),
        (now - Duration::from_secs(3600), 510u64 * 1024 * 1024 * 1024),
        (now, 520u64 * 1024 * 1024 * 1024),
    ];
    let rate = crate::disk_fill_rate(&history)
        .rate_gbph()
        .expect("rate should compute");
    assert!((rate - 10.0).abs() < 0.5, "expected ~10 GiB/h, got {rate}");
}

#[test]
fn disk_fill_rate_requires_minimum_samples_and_span() {
    let now = Instant::now();
    assert_eq!(
        crate::disk_fill_rate(&[]),
        crate::DiskFillRate::Unmeasured,
        "no samples at all is unmeasured, not 'not filling'"
    );
    assert_eq!(
        crate::disk_fill_rate(&[(now, 100), (now, 200)]),
        crate::DiskFillRate::Unmeasured,
        "too few samples is unmeasured"
    );
    // Span too short (10s < 60s).
    let short: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(10), 100),
        (now - Duration::from_secs(5), 200),
        (now, 300),
    ];
    assert_eq!(
        crate::disk_fill_rate(&short),
        crate::DiskFillRate::Unmeasured,
        "a window that spans under 60s is unmeasured"
    );
}

#[test]
fn disk_fill_rate_reports_not_filling_when_the_disk_is_flat() {
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    // A flat disk: no decrease, so the growth window is the whole history,
    // and the measured slope is zero. That is the ONE case the guard can
    // state as a fact: the disk is not filling.
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(7200), 600 * gib),
        (now - Duration::from_secs(3600), 600 * gib),
        (now, 600 * gib),
    ];
    assert_eq!(
        crate::disk_fill_rate(&history),
        crate::DiskFillRate::NotFilling,
        "a flat disk is genuinely not filling"
    );
}

#[test]
fn a_shrinking_disk_is_unmeasured_because_a_decrease_starts_a_new_window() {
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    // Every sample is lower than the last. The reclamation rule starts a
    // fresh growth window at the last decrease, and that window holds a
    // single sample — so the trend cannot be measured. The guard must say
    // "unmeasured" here, NOT "not filling": the disk may be shrinking
    // because something reclaimed space, and the next samples will say.
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(7200), 600 * gib),
        (now - Duration::from_secs(3600), 550 * gib),
        (now, 500 * gib),
    ];
    assert_eq!(
        crate::disk_fill_rate(&history),
        crate::DiskFillRate::Unmeasured
    );
}

/// ADDED 2026-10-24 (audit): a reclamation resets the growth window, so the
/// rate is briefly unmeasurable. That is a THIRD state, distinct from "the
/// disk is not filling", and the report must not conflate them.
#[test]
fn a_reclamation_makes_the_rate_unmeasured_rather_than_not_filling() {
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    let history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(600), 600 * gib),
        (now - Duration::from_secs(300), 700 * gib),
        // The sweep reclaimed space: the used bytes drop.
        (now - Duration::from_secs(10), 400 * gib),
        (now, 410 * gib),
    ];
    assert_eq!(
        crate::disk_fill_rate(&history),
        crate::DiskFillRate::Unmeasured,
        "a fresh growth window that is too young to measure is unmeasured"
    );
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
fn resolve_bin_falls_back_to_bare_name_off_nixos() {
    // The lenient helper preserves the old PATH-relative behavior for
    // non-privilege callers (nix tooling).
    assert_eq!(
        crate::resolve_bin("dracon-definitely-not-a-binary"),
        "dracon-definitely-not-a-binary"
    );
    // A repeated lookup hits the cache and agrees with itself.
    assert_eq!(
        crate::resolve_bin("dracon-definitely-not-a-binary"),
        "dracon-definitely-not-a-binary"
    );
}

#[test]
fn resolve_bin_strict_refuses_unresolvable_names() {
    // Audit falsification round: privilege-adjacent execs (renice,
    // systemctl) must never fall back to a bare PATH-relative name that
    // PATH poisoning could redirect. Unresolvable names are an error.
    let err = crate::resolve_bin_strict("dracon-definitely-not-a-binary").unwrap_err();
    assert!(
        format!("{err:#}").contains("refusing PATH-relative exec"),
        "strict resolve must refuse, not fall back: {err:#}"
    );
}

#[test]
fn reap_policy_from_guard_maps_thresholds_and_shares_exempts() {
    // Audit MEDIUM: the daemon wiring between policy knobs and the reap
    // scanner had zero coverage. The exempt list is shared with pressure
    // mitigation on purpose (too important to renice ⇒ too important to
    // look abandoned), so pin that coupling here.
    let guard = crate::GuardPolicy {
        reap_report_min_idle_hours: 48,
        reap_report_max_cpu_seconds: 120,
        reap_report_signatures: "vite,mytool".to_string(),
        process_exempt_names: "keepme,alsokeep".to_string(),
        ..Default::default()
    };
    let policy = crate::reap_policy_from_guard(&guard);
    assert_eq!(policy.min_idle_hours, 48);
    assert_eq!(policy.max_cpu_seconds, 120);
    assert_eq!(
        policy.signatures,
        vec!["mytool".to_string(), "vite".to_string()]
    );
    assert!(policy.exempt_names.contains(&"keepme".to_string()));
    assert!(policy.exempt_names.contains(&"alsokeep".to_string()));
}

#[test]
fn reap_candidates_runs_against_live_proc_without_panicking() {
    // Shape-only smoke test: the daemon wiring must degrade to an empty
    // (or populated) Vec on a real machine, never panic or block the pass.
    let guard = crate::GuardPolicy::default();
    let found = crate::reap_candidates(&guard);
    for c in &found {
        assert!(c.idle_hours >= guard.reap_report_min_idle_hours);
        assert!(c.cpu_seconds <= guard.reap_report_max_cpu_seconds);
        assert!(!c.args.is_empty());
    }
}

#[test]
fn zombie_since_keys_carry_starttime_and_prune() {
    // A recycled PID must not inherit a dead zombie's age: first-seen is
    // keyed by (pid, starttime), and keys for vanished pids are pruned.
    let mut state = crate::GuardRuntimeState::default();
    state
        .zombies_since
        .insert((999_998, 12345), std::time::Instant::now());
    let _ = crate::zombie_details(&mut state);
    assert!(
        !state.zombies_since.contains_key(&(999_998, 12345)),
        "stale zombie key for a vanished pid must be pruned"
    );
}

#[test]
fn unavailable_identity_defer_drops_at_retry_limit() {
    // The oom-restore and cpu-uncap loops must not retain an entry whose
    // process identity stays unreadable forever (audit LOW: unbounded
    // defer leaks the tracking maps inside the 250 MiB cgroup).
    let mut attempts = std::collections::HashMap::new();
    assert!(!crate::record_unavailable_attempt(&mut attempts, 4242));
    assert!(!crate::record_unavailable_attempt(&mut attempts, 4242));
    assert!(crate::record_unavailable_attempt(&mut attempts, 4242));
    assert_eq!(attempts.get(&4242), Some(&3));
    // A different pid counts independently.
    assert!(!crate::record_unavailable_attempt(&mut attempts, 4243));
}

#[test]
fn clean_node_modules_flag_defaults_true_and_gates_toml_off() {
    // Audit M3 (2026-08-21): node_modules cleanup was the only cleanup
    // kind with no feature flag. The knob must default true (behavior
    // preserved) and parse false from TOML; run_auto_cleanup consults it
    // exactly like its clean_trash / clean_nix_garbage siblings, while
    // explicit `guard clean --node-modules` stays ungated.
    let policy: crate::GuardPolicy = toml::from_str("").expect("empty policy parses with defaults");
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

// ---------------------------------------------------------------------------
// SIGHUP reload — bounded, and never half-configured
//
// DECIDE #2 follow-up. The reload used to swap the policy unconditionally,
// so a partial restore of process adjustments could pair the NEW policy with
// the OLD policy's leftover runtime state. The concrete harm: if the new
// policy disables the mitigation that made an adjustment, nothing retries it
// and the process stays reniced for the daemon's lifetime.
// ---------------------------------------------------------------------------

fn runtime_with_pending_nice(pid: i32) -> GuardRuntimeState {
    let mut state = GuardRuntimeState::default();
    state.memory_reniced_pids.insert(
        pid,
        MemoryReniceState {
            original_nice: 3,
            applied_nice: 10,
            identity: ProcessIdentity {
                comm: "worker".to_string(),
                starttime: 42,
            },
        },
    );
    state
}

#[test]
fn a_fully_restored_reload_applies_the_new_policy_and_clears_runtime() {
    let mut guard = GuardPolicy::default();
    let mut runtime = runtime_with_pending_nice(4242);
    let new_policy = GuardPolicy {
        disk_warn_percent: 55,
        ..Default::default()
    };

    let outcome = apply_policy_reload(&mut guard, &mut runtime, new_policy, true);

    assert_eq!(outcome, ReloadOutcome::Applied);
    assert_eq!(guard.disk_warn_percent, 55, "new policy must be live");
    assert!(
        runtime.memory_reniced_pids.is_empty(),
        "a clean restore must reset the runtime so no old state survives"
    );
}

#[test]
fn a_partial_restore_keeps_the_previous_policy_live() {
    let mut guard = GuardPolicy {
        disk_warn_percent: 80,
        ..Default::default()
    };
    let mut runtime = runtime_with_pending_nice(4242);
    let new_policy = GuardPolicy {
        disk_warn_percent: 55,
        ..Default::default()
    };

    let outcome = apply_policy_reload(&mut guard, &mut runtime, new_policy, false);

    assert_eq!(outcome, ReloadOutcome::Deferred);
    assert_eq!(
        guard.disk_warn_percent, 80,
        "a deferred reload must not adopt the new policy"
    );
    assert_eq!(
        runtime.memory_reniced_pids.len(),
        1,
        "the pending adjustment must be retained so the still-armed mitigation retries it"
    );
}

/// The exact harm the ordering bug caused: a new policy that turns the
/// renice mitigation OFF, adopted on top of an unrestored adjustment, means
/// nothing ever restores the process.
#[test]
fn a_partial_restore_cannot_strand_an_adjustment_the_new_policy_disables() {
    let mut guard = GuardPolicy {
        auto_renice_on_memory: true,
        ..Default::default()
    };
    let mut runtime = runtime_with_pending_nice(4242);
    let new_policy = GuardPolicy {
        auto_renice_on_memory: false,
        ..Default::default()
    };

    let outcome = apply_policy_reload(&mut guard, &mut runtime, new_policy, false);

    assert_eq!(outcome, ReloadOutcome::Deferred);
    assert!(
        guard.auto_renice_on_memory,
        "the mitigation that produced the outstanding adjustment must stay armed, \
         otherwise the process is reniced forever with no path to restore it"
    );
    assert!(
        runtime.memory_reniced_pids.contains_key(&4242),
        "the outstanding adjustment must survive so it is retried"
    );
}

#[test]
fn a_partial_restore_with_only_threshold_changes_is_also_deferred() {
    // Deferral is not limited to mitigation toggles: any new policy is
    // refused, because the retained state is tied to the old one and we
    // cannot reason about the combination.
    let mut guard = GuardPolicy::default();
    let mut runtime = runtime_with_pending_nice(7);
    let new_policy = GuardPolicy {
        interval_secs: 5,
        ..Default::default()
    };

    assert_eq!(
        apply_policy_reload(&mut guard, &mut runtime, new_policy, false),
        ReloadOutcome::Deferred
    );
    assert_ne!(
        guard.interval_secs, 5,
        "the new interval must not be adopted"
    );
}

#[test]
fn disabling_the_guard_is_applied_even_after_a_partial_restore() {
    // Stopping is the safe direction, and the daemon's shutdown restore
    // re-tries the retained adjustments before it returns — so `enabled =
    // false` must not be blocked by a failed restore.
    let mut guard = GuardPolicy::default();
    let mut runtime = runtime_with_pending_nice(99);
    let new_policy = GuardPolicy {
        enabled: false,
        ..Default::default()
    };

    let outcome = apply_policy_reload(&mut guard, &mut runtime, new_policy, false);

    assert_eq!(outcome, ReloadOutcome::Applied);
    assert!(!guard.enabled, "enabled = false must always take effect");
    assert_eq!(
        runtime.memory_reniced_pids.len(),
        1,
        "state is retained for the shutdown restore, not dropped"
    );
}

#[test]
fn reload_is_idempotent_across_repeated_sighups() {
    let mut guard = GuardPolicy::default();
    let mut runtime = GuardRuntimeState::default();
    for _ in 0..5 {
        let p = GuardPolicy {
            disk_warn_percent: 70,
            ..Default::default()
        };
        assert_eq!(
            apply_policy_reload(&mut guard, &mut runtime, p, true),
            ReloadOutcome::Applied
        );
    }
    assert_eq!(guard.disk_warn_percent, 70);
    assert!(runtime.memory_reniced_pids.is_empty());
}

#[test]
fn a_reload_that_succeeds_after_a_deferred_one_recovers() {
    // A deferred reload must not wedge the daemon: once the restore
    // succeeds, the next SIGHUP applies normally.
    let mut guard = GuardPolicy::default();
    let mut runtime = runtime_with_pending_nice(1);
    let p1 = GuardPolicy {
        disk_warn_percent: 55,
        ..Default::default()
    };
    assert_eq!(
        apply_policy_reload(&mut guard, &mut runtime, p1, false),
        ReloadOutcome::Deferred
    );

    let p2 = GuardPolicy {
        disk_warn_percent: 55,
        ..Default::default()
    };
    assert_eq!(
        apply_policy_reload(&mut guard, &mut runtime, p2, true),
        ReloadOutcome::Applied
    );
    assert_eq!(guard.disk_warn_percent, 55);
    assert!(runtime.memory_reniced_pids.is_empty());
}

/// 2026-10-01 (audit HIGH): the `oom_score_adj` and CPUQuota RELEASES were inside
/// `if pressure == "ok" && can_restore_nice`, but their APPLIES are not gated by
/// that flag. On a host without CAP_SYS_NICE — a manual `guard daemon` run, or a
/// unit predating v0.112.39's `AmbientCapabilities` — the guard wrote
/// `oom_score_adj=250` and a CPUQuota cap under critical pressure and could then
/// never release either: the only recovery was SIGHUP or a restart, while the
/// sole warning printed said "renice mitigation disabled".
///
/// The defect was structural, so the guard is structural: the two release
/// halves must live OUTSIDE the `can_restore_nice` gate, which now wraps only
/// the renice half. Behavioural coverage would need the capability absent,
/// which this process cannot arrange, so `gated_release_markers` is a pure
/// function over the source text and is itself tested (see the next test).
fn gated_release_markers(src: &str) -> Result<Vec<String>, String> {
    let start = src
        .find("if pressure == \"ok\" {")
        .ok_or_else(|| "the pressure-release block was not found".to_string())?;
    let gate = src[start..]
        .find("if can_restore_nice {")
        .map(|i| start + i)
        .ok_or_else(|| {
            "the renice half is no longer gated on the restore capability".to_string()
        })?;
    let body = &src[gate..];

    // Brace-count to the gate's matching close.
    let mut depth = 0i32;
    let mut end = None;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(gate + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| "the capability gate is not brace-balanced".to_string())?;
    let gated = &src[gate..end];

    let mut wrongly_gated = Vec::new();
    for marker in [
        "Restore oom_score_adj after the release window",
        "Lift CPUQuota scopes after the release window",
    ] {
        if gated.contains(marker) {
            wrongly_gated.push(marker.to_string());
        }
        if !src[start..].contains(marker) {
            return Err(format!(
                "'{marker}' no longer exists: the release was deleted, not just ungated"
            ));
        }
    }
    // The renice half must still be inside the gate: it is the one that really
    // needs CAP_SYS_NICE, and its apply is gated too.
    if !gated.contains("mem-unrenice") {
        return Err(
            "the renice release left the can_restore_nice gate, but its apply is still gated"
                .to_string(),
        );
    }
    Ok(wrongly_gated)
}

#[test]
fn oom_and_cpu_releases_are_not_gated_by_the_nice_capability() {
    let wrongly = gated_release_markers(include_str!("main.rs"))
        .expect("the release block must be shaped as the fix requires");
    assert!(
        wrongly.is_empty(),
        "these releases are inside the can_restore_nice gate again, so a host without \
         CAP_SYS_NICE can never release them: {wrongly:?}"
    );
}

/// The guard above is only worth having if it FAILS on the old shape. This
/// feeds it a synthetic snippet with the pre-fix structure and requires it to
/// report both releases as gated.
#[test]
fn the_structural_guard_catches_a_reintroduced_defect() {
    let pre_fix = r#"
    if pressure == "ok" {
        if can_restore_nice {
            eprintln!("mem-unrenice");
        }
        // Restore oom_score_adj after the release window.
        let x = 1;
        // Lift CPUQuota scopes after the release window.
        let y = 2;
    } else {
    }
"#;
    // Nest the two releases back inside the gate, exactly as the old code had it.
    let pre_fix_nested = pre_fix.replace(
        "        if can_restore_nice {\n            eprintln!(\"mem-unrenice\");\n        }\n",
        "        if can_restore_nice {\n            eprintln!(\"mem-unrenice\");\n            // Restore oom_score_adj after the release window.\n            let x = 1;\n            // Lift CPUQuota scopes after the release window.\n            let y = 2;\n        }\n",
    );
    let wrongly = gated_release_markers(&pre_fix_nested)
        .expect("the synthetic snippet must still be parseable");
    assert_eq!(
        wrongly.len(),
        2,
        "the guard must flag both releases when they are nested in the gate: {wrongly:?}"
    );
}

/// 2026-10-01 (audit): the three `deferred += 1` sites in the descendant
/// oom-restoration sweep retried forever, so one child whose identity or
/// `oom_score_adj` was unreadable pinned its root at 250 for the life of the
/// process, grew the pending map without bound, and made every SIGHUP reload
/// permanently `Deferred`. The counter must stop the retries and the entry must
/// be dropped so the root can be released.
#[test]
fn pending_oom_descendant_retries_are_bounded() {
    let mut pending = OomPendingDescendant {
        root_pid: 1,
        original_adj: 0,
        identity: ProcessIdentity {
            starttime: 2,
            comm: "child".to_string(),
        },
        attempts: 0,
    };
    for attempt in 1..OOM_DESCENDANT_RETRY_LIMIT {
        assert!(
            !oom_descendant_attempts_exhausted(&mut pending),
            "attempt {attempt} must still be retried"
        );
        assert_eq!(pending.attempts, attempt);
    }
    assert!(
        oom_descendant_attempts_exhausted(&mut pending),
        "attempt {} must give up so the root is not pinned forever",
        OOM_DESCENDANT_RETRY_LIMIT
    );
    assert_eq!(pending.attempts, OOM_DESCENDANT_RETRY_LIMIT);
}

/// 2026-10-01 (audit): `oom_known_descendants` grew by one entry per descendant
/// incarnation ever seen and was only cleared when the root left
/// `oom_biased_pids`, so a long critical episode under a forking root
/// accumulated thousands of dead keys. The cap must actually prune.
#[test]
fn remembered_oom_descendants_are_pruned_past_the_cap() {
    use std::collections::HashSet;

    // A biased root with many dead descendants and one live one.
    let mut state = GuardRuntimeState::default();
    let identity = ProcessIdentity {
        starttime: 1,
        comm: "root".to_string(),
    };
    state.oom_biased_pids.insert(100, (0, identity.clone()));
    let live = (999, 42u64);
    let mut known: HashSet<(i32, u64)> = HashSet::new();
    known.insert(live);
    for i in 0..OOM_KNOWN_DESCENDANTS_CAP {
        known.insert((1000 + i as i32, u64::MAX - i as u64));
    }
    assert!(
        known.len() > OOM_KNOWN_DESCENDANTS_CAP,
        "the fixture must exceed the cap to exercise the prune"
    );
    state.oom_known_descendants.insert(100, known);
    // An unrelated, unbiased root must be dropped outright.
    state
        .oom_known_descendants
        .insert(200, HashSet::from([(1, 1)]));

    let all_processes = vec![crate::ProcSample {
        swap_mb: Some(0),
        pid: live.0,
        ppid: 1,
        cpu_percent: 0.0,
        rss_mb: 0,
        nice: 0,
        command: "child".to_string(),
        args: String::new(),
        starttime: live.1,
    }];
    prune_oom_known_descendants(&mut state, &all_processes);

    assert!(
        !state.oom_known_descendants.contains_key(&200),
        "a root that is no longer biased must lose its bookkeeping"
    );
    let kept = &state.oom_known_descendants[&100];
    assert!(
        kept.len() <= OOM_KNOWN_DESCENDANTS_CAP,
        "the set must not stay above the cap, got {}",
        kept.len()
    );
    assert!(
        kept.contains(&live),
        "a descendant that is still alive must be remembered"
    );
}

/// 2026-10-01 (audit): applied mitigations lived only in memory, so a panic or a
/// `MemoryMax=250M` OOM kill during a critical-pressure episode restarted the
/// guard with empty maps and left `nice`/`oom_score_adj`/CPUQuota applied to live
/// processes with no record that they needed restoring. The ledger must survive
/// the round trip that a restart performs.
#[test]
fn the_mitigation_ledger_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!(
        "dracon-system-mitigation-ledger-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("guard-mitigations.json");

    // The CURRENT process stands in for an adjusted one: its identity is
    // definitely current, so adoption must keep it.
    let me = std::process::id() as i32;
    let live_identity = {
        // Read the real starttime/comm so the identity check passes.
        let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap_or_default();
        let after = stat.rsplit_once(") ").map(|(_, rest)| rest.to_string());
        let starttime = after
            .as_deref()
            .and_then(|rest| rest.split_whitespace().nth(19))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        ProcessIdentity {
            starttime,
            comm: "self".to_string(),
        }
    };

    let mut before = GuardRuntimeState::default();
    before.memory_reniced_pids.insert(
        me,
        MemoryReniceState {
            original_nice: 0,
            applied_nice: 5,
            identity: live_identity.clone(),
        },
    );
    before
        .oom_biased_pids
        .insert(me, (0, live_identity.clone()));
    persist_mitigations_to(&mut before, &path);
    assert!(path.exists(), "the ledger must be written");
    // An unchanged pass must not rewrite the file.
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    persist_mitigations_to(&mut before, &path);
    let second = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        first, second,
        "an unchanged pass must not rewrite the ledger"
    );

    // A fresh process (a restart) adopts them.
    let mut after_restart = GuardRuntimeState::default();
    hydrate_mitigations_from(&path, &mut after_restart);
    assert!(
        after_restart.memory_reniced_pids.contains_key(&me),
        "a renice left applied by the previous run must be adopted"
    );
    assert!(
        after_restart.oom_biased_pids.contains_key(&me),
        "an oom bias left applied by the previous run must be adopted"
    );

    // A RECYCLED pid is never adopted: a wrong starttime must not match.
    let mut recycled = GuardRuntimeState::default();
    let mut bad = std::fs::read_to_string(&path).unwrap();
    bad = bad.replace(
        &format!("\"starttime\":{}", live_identity.starttime),
        "\"starttime\":999999999",
    );
    std::fs::write(&path, bad).unwrap();
    hydrate_mitigations_from(&path, &mut recycled);
    assert!(
        recycled.memory_reniced_pids.is_empty() && recycled.oom_biased_pids.is_empty(),
        "a ledger entry whose identity does not match must be ignored, not adopted"
    );

    // A corrupt ledger is not fatal and not adopted.
    std::fs::write(&path, "{ not json").unwrap();
    let mut corrupt = GuardRuntimeState::default();
    hydrate_mitigations_from(&path, &mut corrupt);
    assert!(
        corrupt.memory_reniced_pids.is_empty(),
        "a corrupt ledger must not block the daemon"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Mitigation ledger: CPU-cap cgroup fidelity (audit 2026-10-01)
// ---------------------------------------------------------------------------

/// A unique scratch directory, and this process's real identity so the ledger's
/// identity check matches (same shape as the round-trip test above).
fn cap_cgroup_fixture(label: &str) -> (std::path::PathBuf, i32, ProcessIdentity) {
    let dir = std::env::temp_dir().join(format!(
        "dracon-system-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let me = std::process::id() as i32;
    let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap_or_default();
    let starttime = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let identity = ProcessIdentity {
        starttime,
        comm: "self".to_string(),
    };
    (dir.join("guard-mitigations.json"), me, identity)
}

#[test]
fn mitigation_ledger_round_trips_the_capped_pids_original_cgroup() {
    let (path, me, identity) = cap_cgroup_fixture("ledger-cap-cgroup");
    let orig_cgroup = "user.slice/user-1000.slice/user@1000.service/app.slice";

    let mut before = GuardRuntimeState::default();
    before.capped_pids.insert(
        me,
        (
            "run-r1.service".to_string(),
            orig_cgroup.to_string(),
            identity.clone(),
        ),
    );
    persist_mitigations_to(&mut before, &path);

    let mut after_restart = GuardRuntimeState::default();
    hydrate_mitigations_from(&path, &mut after_restart);
    let (scope, restored_cgroup, _) = after_restart
        .capped_pids
        .get(&me)
        .expect("a cap left applied by the previous run must be adopted");
    assert_eq!(scope, "run-r1.service");
    assert_eq!(
        restored_cgroup, orig_cgroup,
        "the cgroup to restore to must survive the restart, not hydrate empty"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_legacy_ledger_without_the_cgroup_still_hydrates_and_never_guesses_a_path() {
    let (path, me, identity) = cap_cgroup_fixture("ledger-cap-legacy");
    // A ledger written before cap_orig_cgroup existed: the field is absent, and
    // serde must read it as None instead of rejecting the whole file.
    // Built with the real serializer, then asserted to carry no cap_orig_cgroup:
    // this is exactly the shape a ledger written before that field existed has.
    let legacy = serde_json::json!({
        "records": [{
            "pid": me,
            "original_nice": serde_json::Value::Null,
            "original_oom_adj": serde_json::Value::Null,
            "cap_scope": "run-r9.service",
            "identity": {"starttime": identity.starttime, "comm": "self"},
        }]
    })
    .to_string();
    assert!(
        !legacy.contains("cap_orig_cgroup"),
        "the legacy fixture must not contain the newer field"
    );
    std::fs::write(&path, legacy).unwrap();

    let mut after_restart = GuardRuntimeState::default();
    hydrate_mitigations_from(&path, &mut after_restart);
    let (_, restored_cgroup, _) = after_restart
        .capped_pids
        .get(&me)
        .expect("a legacy ledger entry with a matching identity must still be adopted");
    assert_eq!(
        restored_cgroup, "",
        "a legacy ledger has no cgroup, and must be adopted with it empty"
    );

    // The empty value resolves to the transient unit's PARENT cgroup, never to
    // the empty path `/sys/fs/cgroup//cgroup.procs` (the cgroup root).
    let current = "user.slice/user-1000.slice/user@1000.service/run-r9.service";
    assert_eq!(
        restore_cgroup_target("", current).as_deref(),
        Some("user.slice/user-1000.slice/user@1000.service"),
        "with no recorded cgroup the pid goes back to the scope unit's parent"
    );
    // A recorded cgroup always wins.
    assert_eq!(
        restore_cgroup_target("user.slice/app.slice", current).as_deref(),
        Some("user.slice/app.slice")
    );
    // No recorded cgroup AND no parent to derive: refuse rather than guess.
    assert_eq!(restore_cgroup_target("", "run-r9.service"), None);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn disk_fill_rate_recovers_after_large_reclamation() {
    let now = Instant::now();
    let gib = 1024 * 1024 * 1024;
    let history = [
        (now - Duration::from_secs(120), 800 * gib),
        (now - Duration::from_secs(90), 801 * gib),
        (now - Duration::from_secs(60), 600 * gib),
        (now - Duration::from_secs(30), 601 * gib),
        (now, 602 * gib),
    ];
    let rate = crate::disk_fill_rate(&history).rate_gbph().unwrap();
    assert!((rate - 120.0).abs() < 0.01);
    // Cleanup alone must not manufacture a positive growth observation.
    assert_eq!(
        crate::disk_fill_rate(&history[..3]),
        crate::DiskFillRate::Unmeasured
    );
    assert_eq!(
        crate::disk_fill_rate(&history[..4]),
        crate::DiskFillRate::Unmeasured
    );
}

/// ADDED 2026-10-25 (audit D13): the live host published "Disk growing at
/// 184.5 GiB/h ... estimated 0.5 hours until full" for a disk an independent
/// 10-second sampling found FLAT (peak 1.91 GiB in 180 s; net +0.03 GiB).
/// The cause: the guard's own cleanup truncates the measurement window to
/// 60-90 s, and a least-squares slope over such a window is decided by the
/// steepest segment — one burst.
///
/// The sustained fill is 20 GiB/h. A burst of 2.4 GiB inside a single
/// 30-second sample interval must not be published as the disk's rate.
#[test]
fn disk_fill_rate_ignores_a_single_burst_inside_the_window() {
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    let step = 30u64;
    // 20 GiB/h = 0.16667 GiB per 30 s.
    let steady = 20.0 / 120.0;
    // Match the actual append-ordered, latest-30-sample history. Older
    // pre-cleanup samples must precede the drop, not be appended after now.
    let mut history: Vec<(Instant, u64)> = vec![
        (now - Duration::from_secs(step * 29), 300 * gib),
        (now - Duration::from_secs(step * 28), 301 * gib),
    ];
    let mut used = 100.0f64;
    for i in 0..28u64 {
        used += steady;
        // A 2.4 GiB burst across the last two sample intervals.
        if i == 26 || i == 27 {
            used += 2.4;
        }
        history.push((
            now - Duration::from_secs(step * (27 - i)),
            (used * gib as f64) as u64,
        ));
    }
    // Cleanup resets the window; the sustained-fill and burst assertions
    // below are unchanged, with both bursts inside the actual 30 samples.
    assert!(history.windows(2).all(|pair| pair[0].0 < pair[1].0));

    let rate = crate::disk_fill_rate(&history)
        .rate_gbph()
        .expect("the window has a measurable rate");
    assert!(
        (rate - 20.0).abs() < 2.0,
        "a single burst must not become the published rate: expected ~20 GiB/h, got {rate}"
    );
    assert!(
        rate < 45.0,
        "the published rate ({rate} GiB/h) must not track the burst"
    );
}

/// ADDED 2026-10-25 (audit D13): the same quantity must not change value
/// because a burst arrived. Before the fix the live guard published 25.1,
/// 36.3, 57.7, 42.9, 124.6, 107.2, 36.9 and 184.5 GiB/h within one hour
/// for a disk whose fill never changed — and contradicted the trend
/// warning's figure for the same quantity a minute later.
#[test]
fn the_published_rate_is_stable_when_a_burst_arrives() {
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    let step = 30u64;
    let steady = 20.0 / 120.0;
    let mut history: Vec<(Instant, u64)> = Vec::new();
    let mut used = 100.0f64;
    for i in 0..30u64 {
        used += steady;
        history.push((
            now - Duration::from_secs(step * (29 - i)),
            (used * gib as f64) as u64,
        ));
    }
    let before = crate::disk_fill_rate(&history)
        .rate_gbph()
        .expect("a steady fill measures");
    assert!(
        (before - 20.0).abs() < 2.0,
        "expected ~20 GiB/h, got {before}"
    );

    // Now a burst lands in the newest interval.
    used += 4.8;
    history.push((now, (used * gib as f64) as u64));
    let after = crate::disk_fill_rate(&history)
        .rate_gbph()
        .expect("the window still measures");
    assert!(
        (after - before).abs() < 5.0,
        "one burst moved the published rate from {before} to {after} GiB/h"
    );
}

#[test]
fn rapid_fill_warning_reports_user_available_runway() {
    let message = crate::rapid_disk_fill_message(40.0, 90, 20 * 1024 * 1024 * 1024);
    assert!(message.contains("20.0 GiB available"));
    assert!(message.contains("0.5 hours until full if growth continues"));
    let full = crate::rapid_disk_fill_message(40.0, 100, 0);
    assert!(full.contains("0.0 hours"));
}

#[tokio::test]
async fn disk_reclamation_rearms_the_rapid_fill_notification() {
    // REWRITTEN 2026-11-08: this test used to simulate a reclamation as an
    // UNMEASURED rate (two samples 30 s apart), assert that
    // `check_rapid_disk_fill` parked the state at "unmeasured", and assert
    // that the next "rapid" therefore re-notified. That is the conflation
    // the `DiskFillRate` type was introduced to prevent: a growth window
    // too young to measure is not evidence that the disk stopped filling.
    // Because the guard measures every 30 s, this made the alarm re-fire
    // for one continuous episode — the live `disk/rapid-fill` events
    // quoted 31-146 GiB/h against a 20 GiB/h threshold, so "rapid" never
    // actually stopped being true.
    //
    // The re-arm guarantee itself is PRESERVED, and is asserted below on
    // the evidence that actually means reclamation: a non-positive slope.
    let guard = crate::GuardPolicy::default();
    let mut state = crate::GuardRuntimeState::default();
    crate::report_state_transition(&mut state, "disk-rapid-fill", "rapid", 3600);

    // 1. A window too young to measure holds the state, and check_rapid_disk_fill
    //    reports no rate (there is no deadline it can honestly compute).
    let now = Instant::now();
    state.disk_bytes_history = vec![
        (now - Duration::from_secs(60), 800),
        (now - Duration::from_secs(30), 801),
    ];
    assert!(
        crate::check_rapid_disk_fill(&guard, &mut state, 600, 60, 400)
            .await
            .is_none()
    );
    assert_eq!(
        state.report_states["disk-rapid-fill"].value, "rapid",
        "an unmeasurable rate must HOLD the state, not re-arm it"
    );

    // 2. A genuine reclamation — a non-positive slope — IS evidence, so it
    //    states the "ok" verdict. After the sustain window that becomes the
    //    accepted state, so re-entering "rapid" is a real transition and
    //    the operator is notified again for the new episode.
    let threshold = guard.disk_rapid_fill_gbph;
    let reclaimed = crate::disk_rapid_observed_state(
        crate::DiskFillRate::NotFilling,
        true,
        threshold,
    );
    assert_eq!(
        reclaimed.map(|(state, _)| state),
        Some("ok"),
        "a non-positive slope must state the ok verdict"
    );
    let clock = Instant::now();
    crate::report_state_transition_stabilized(
        &mut state,
        "disk-rapid-fill",
        "ok",
        guard.report_repeat_secs,
        crate::REPORT_STATE_SUSTAIN_SECS,
        clock,
    );
    let settled = crate::report_state_transition_stabilized(
        &mut state,
        "disk-rapid-fill",
        "ok",
        guard.report_repeat_secs,
        crate::REPORT_STATE_SUSTAIN_SECS,
        clock + Duration::from_secs(crate::REPORT_STATE_SUSTAIN_SECS + 1),
    );
    assert!(settled.changed, "the reclamation is accepted after the sustain window");

    // 3. Rapid growth resumes: this is a NEW episode, so it re-notifies —
    //    after its own sustain window, because every accepted state change
    //    waits for the value to hold (a spike must not raise an alarm).
    let resume_at = clock + Duration::from_secs(2 * crate::REPORT_STATE_SUSTAIN_SECS + 2);
    let resume_candidate = crate::report_state_transition_stabilized(
        &mut state,
        "disk-rapid-fill",
        "rapid",
        guard.report_repeat_secs,
        crate::REPORT_STATE_SUSTAIN_SECS,
        resume_at,
    );
    assert!(
        !resume_candidate.changed,
        "a re-entry must hold for the sustain window before it is accepted"
    );
    let resumed = crate::report_state_transition_stabilized(
        &mut state,
        "disk-rapid-fill",
        "rapid",
        guard.report_repeat_secs,
        crate::REPORT_STATE_SUSTAIN_SECS,
        resume_at + Duration::from_secs(crate::REPORT_STATE_SUSTAIN_SECS + 1),
    );
    assert!(
        resumed.changed,
        "resumed growth is a fresh transition and re-notifies"
    );
}

/// ADDED 2026-10-24 (audit): the fill rate is measured ONCE per pass and
/// stored in the state, so the scratch ETA and the rapid-fill alarm quote
/// the SAME figure. They used to be measured twice, from two different
/// sample windows — the scratch one from a frozen clone of the history,
/// taken before the pass's own deletions — and on the live host the scratch
/// line said "the disk is not filling" while the alarm in the same pass
/// reported a 51.3 GiB/h fill.
#[tokio::test]
async fn the_pass_stores_the_measured_fill_rate_for_the_scratch_report() {
    let guard = crate::GuardPolicy::default();
    let mut state = crate::GuardRuntimeState::default();
    let now = Instant::now();
    let gib = 1024u64 * 1024 * 1024;
    // 10 GiB/h over two hours — below the 20 GiB/h alarm threshold, so the
    // test does not send a notification.
    state.disk_bytes_history = vec![
        (now - Duration::from_secs(7200), 500 * gib),
        (now - Duration::from_secs(3600), 510 * gib),
    ];
    let reported = crate::check_rapid_disk_fill(&guard, &mut state, 520 * gib, 50, 100 * gib).await;
    let stored = state
        .disk_fill_rate
        .rate_gbph()
        .expect("the pass must have measured a rate");
    assert!(
        (stored - 10.0).abs() < 0.5,
        "expected ~10 GiB/h, got {stored}"
    );
    assert_eq!(
        reported,
        Some(stored),
        "the alarm and the stored rate must be the same figure"
    );
}

/// ADDED 2026-10-24 (audit): the scratch line's number is how much writing
/// the held-back scratch would absorb — NOT how long until the disk is full.
/// It must not borrow the rapid-fill alarm's wording for it, or the journal
/// reads "hours until full: 0.3" beside "estimated 2.7 hours until full".
#[test]
fn the_scratch_eta_does_not_claim_to_be_the_hours_until_the_disk_is_full() {
    let gib = 1024u64 * 1024 * 1024;
    let text = crate::scratch_eta_text(16 * gib, crate::DiskFillRate::Filling(51.3));
    assert!(
        !text.contains("hours until full"),
        "the scratch figure must not reuse the alarm's wording: {text}"
    );
    assert!(
        text.contains("0.3 hours of writes at 51.3 GiB/h"),
        "the figure must say what it measures: {text}"
    );

    // A disk that is not filling, and a rate that cannot be measured, are
    // different facts — and neither is "the disk is not filling" unless the
    // disk really is not filling.
    let not_filling = crate::scratch_eta_text(16 * gib, crate::DiskFillRate::NotFilling);
    assert!(
        not_filling.contains("the disk is not filling"),
        "a flat disk says so: {not_filling}"
    );
    let unmeasured = crate::scratch_eta_text(16 * gib, crate::DiskFillRate::Unmeasured);
    assert!(
        unmeasured.contains("cannot be measured yet"),
        "an unmeasurable rate says so: {unmeasured}"
    );
    assert!(
        !unmeasured.contains("the disk is not filling"),
        "an unmeasurable rate must NOT claim the disk is not filling: {unmeasured}"
    );
}

/// ADDED 2026-10-24 (audit): a rate that cannot be measured must not be
/// reported as "the disk is not filling".
#[tokio::test]
async fn an_unmeasurable_rate_is_stored_as_unmeasured_not_as_not_filling() {
    let guard = crate::GuardPolicy::default();
    let mut state = crate::GuardRuntimeState::default();
    // A brand-new state has no history at all.
    assert_eq!(state.disk_fill_rate, crate::DiskFillRate::Unmeasured);
    assert!(
        crate::check_rapid_disk_fill(&guard, &mut state, 600, 60, 400)
            .await
            .is_none()
    );
    assert_eq!(
        state.disk_fill_rate,
        crate::DiskFillRate::Unmeasured,
        "no history yet is unmeasured — it is NOT 'the disk is not filling'"
    );
}

// ---------------------------------------------------------------------------
// User-namespace aware priority restore (ADDED 2026-10-24)
// ---------------------------------------------------------------------------
//
// `CAP_SYS_NICE` is NAMESPACED: a process holding it in a nested user
// namespace has it in that namespace and in its DESCENDANTS, never in an
// ancestor. The guard's unit runs in a nested user namespace while most
// of the host's processes live in the initial one, so the guard could
// always LOWER a process's priority and could never RAISE it again.
//
// The renice mitigation was therefore a one-way ratchet, and because the
// undo always failed with "Permission denied", every SIGHUP policy reload
// was deferred forever: one unrestorable renice made the whole guard
// impossible to reconfigure.

#[test]
fn the_namespace_link_parses_to_an_inode_number() {
    assert_eq!(
        parse_namespace_inode(Path::new("user:[4026531837]")),
        Some(4026531837)
    );
    assert_eq!(
        parse_namespace_inode(Path::new("user:[4026532749]")),
        Some(4026532749)
    );
    assert_eq!(
        parse_namespace_inode(Path::new("user:[not-a-number]")),
        None
    );
    assert_eq!(parse_namespace_inode(Path::new("pid:[4026531837]")), None);
    assert_eq!(parse_namespace_inode(Path::new("4026531837")), None);
}

#[test]
fn a_capability_in_one_namespace_does_not_reach_another() {
    // The same namespace: the guard's CAP_SYS_NICE reaches the target.
    assert!(nice_restore_namespace_ok(
        Some(4026531837),
        Some(4026531837)
    ));
    // A different namespace: it does not. This is the case that made the
    // mitigation one-way on this host.
    assert!(!nice_restore_namespace_ok(
        Some(4026532749),
        Some(4026531837)
    ));
    assert!(!nice_restore_namespace_ok(
        Some(4026531837),
        Some(4026532749)
    ));
    // Unreadable on either side fails closed.
    assert!(!nice_restore_namespace_ok(None, Some(4026531837)));
    assert!(!nice_restore_namespace_ok(Some(4026531837), None));
    assert!(!nice_restore_namespace_ok(None, None));
}

#[test]
fn a_permission_failure_is_permanent_but_a_transient_one_is_not() {
    assert!(nice_restore_failure_is_permanent(
        "renice exited with status exit status: 1: renice: failed to set priority for 42 (process ID): Permission denied"
    ));
    assert!(nice_restore_failure_is_permanent("Operation not permitted"));
    // A transient failure must keep the OLD policy live so the mitigation
    // is retried on the next pass instead of being forgotten.
    assert!(!nice_restore_failure_is_permanent(
        "renice exited with status exit status: 1"
    ));
    assert!(!nice_restore_failure_is_permanent(
        "failed to invoke /usr/bin/renice"
    ));
}

/// A fake proc tree holding one process, optionally in a user namespace.
fn fake_proc_tree(name: &str, pid: i32, comm: &str, starttime: u64, ns: Option<u64>) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dracon-ns-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("self")).expect("self entry");
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).expect("pid dir");
    fs::write(dir.join("comm"), format!("{comm}\n")).expect("comm");
    let stat = format!(
        "{pid} ({comm}) S 1 {pid} {pid} 0 0 -1 4194304 0 0 0 0 0 0 0 20 0 1 0 {starttime} 0 0 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0"
    );
    fs::write(dir.join("stat"), stat).expect("stat");
    if let Some(ns) = ns {
        let ns_dir = dir.join("ns");
        fs::create_dir_all(&ns_dir).expect("ns dir");
        symlink(format!("user:[{ns}]"), ns_dir.join("user")).expect("ns symlink");
    }
    root
}

#[test]
fn the_user_namespace_is_read_from_proc() {
    let root = fake_proc_tree("read", 4242, "minimax-code", 424_200, Some(4026531837));
    assert_eq!(user_namespace_inode(&root, 4242), Some(4026531837));
    // A pid with no namespace link is UNREADABLE, not "the same namespace".
    assert_eq!(user_namespace_inode(&root, 9999), None);
    let _ = fs::remove_dir_all(&root);
}

/// THE REGRESSION TEST for 2026-10-24: a nice adjustment the guard cannot
/// restore must not hold the policy reload hostage forever.
///
/// Before the fix this restore failed with "Permission denied", the entry
/// stayed tracked, and `apply_policy_reload` kept the OLD policy live and
/// deferred every later reload — the guard could not be reconfigured at
/// all.
#[tokio::test]
async fn a_sighup_reload_is_not_deferred_by_an_unrestorable_nice_adjustment() {
    let starttime = 424_200;
    // A namespace that is NOT the test's own: the guard must prove it
    // cannot restore this process's priority.
    let own = crate::parse_namespace_inode(&fs::read_link("/proc/self/ns/user").expect("self ns"));
    let other_ns = own.map(|ns| ns.wrapping_add(1)).unwrap_or(4026531837);
    let root = fake_proc_tree("sighup", 4242, "minimax-code", starttime, Some(other_ns));
    let mut state = crate::GuardRuntimeState::default();
    state.reniced_pids.insert(
        4242,
        LegacyReniceState {
            original_nice: 0,
            applied_nice: 5,
            identity: ProcessIdentity {
                comm: "minimax-code".to_string(),
                starttime,
            },
        },
    );

    // The renice binary is never reached: the guard can prove it cannot
    // restore this process, so the adjustment is dropped rather than
    // retried forever. A path that does not exist makes that visible.
    let restored = crate::restore_runtime_adjustments_with_samples(
        &mut state,
        Path::new("/nonexistent/renice"),
        Path::new("/nonexistent/systemctl"),
        &root,
        &[],
    )
    .await;

    assert!(
        restored,
        "an unrestorable adjustment must not defer the policy reload"
    );
    assert!(
        state.reniced_pids.is_empty(),
        "the unrestorable adjustment must be dropped, not retried forever"
    );
    let _ = fs::remove_dir_all(&root);
}

/// THE CREDENTIAL HEURISTIC IS A SUBSTRING MATCH, and it flags files that
/// merely MENTION a credential word.
///
/// Measured on this host on 2026-10-24: 55 top-level trash entries, 11 GiB,
/// kept forever — including a 3.9 GiB web bundle (`android-chrome-192x192.png`),
/// five 865 MiB repository checkouts (test files named `…-token-….test.ts`
/// and `…-chrome.md`), and the operator's own manuscript (a chapter called
/// "borrowed credentials").
///
/// This test PINS that behaviour deliberately: it is a known cost, not an
/// accident, and narrowing the heuristic is a decision that must be made
/// consciously rather than by accident in a refactor. If this test starts
/// failing, the heuristic was narrowed — which is a good outcome, but it
/// must be a deliberate one.
///
/// CHANGED 2026-10-25 (audit, operator decision): the heuristic was
/// narrowed from "a loose substring anywhere in the name" to "the file IS
/// a credential file by its own name". The broad rule protected an ENTIRE
/// deleted item from one matching file anywhere inside it: a Rust build
/// artefact called `trust_token.o` kept 5.0 GiB of `dg-verify`, and a
/// chapter called `02-borrowed-credentials.md` kept 1.8 GiB of `books`.
/// 112 items, 28.0 GiB, on a disk filling at over 100 GiB/h.
///
/// The broad names are now NOT flagged. The genuine ones still are.
#[test]
fn the_credential_heuristic_only_flags_names_that_are_credential_files() {
    // A web asset, not a credential.
    assert!(!crate::looks_credential_like("android-chrome-192x192.png"));
    assert!(!crate::looks_credential_like("chrome.CdV59toY.css"));
    // A test file about a scanner, not a credential.
    assert!(!crate::looks_credential_like(
        "artifact-secret-scan-2026-10-01.test.ts"
    ));
    assert!(!crate::looks_credential_like(
        "aud-271-premium-token-validity-2026-08-05.test.ts"
    ));
    // A chapter title: the strong word is in the MIDDLE of the name.
    assert!(!crate::looks_credential_like("02-borrowed-credentials.md"));
    assert!(!crate::looks_credential_like("01-the-archivists-secret.md"));
    // A compiled build artefact.
    assert!(!crate::looks_credential_like("trust_token.o"));
    assert!(!crate::looks_credential_like("pmbtoken.o"));
    // The genuine ones must keep matching.
    assert!(crate::looks_credential_like("Login Data"));
    assert!(crate::looks_credential_like("id_ed25519.key"));
    assert!(crate::looks_credential_like("cli.key"));
    assert!(crate::looks_credential_like(".env"));
    assert!(crate::looks_credential_like(".env.production"));
    assert!(crate::looks_credential_like("client_secret.json"));
    assert!(crate::looks_credential_like("secrets.yaml"));
    assert!(crate::looks_credential_like("credentials_backup"));
    assert!(crate::looks_credential_like("id_ed25519"));
}

/// A capability that is switched OFF must not look identical to one that
/// has nothing to do.
///
/// ADDED 2026-10-25 (audit): with `auto_cleanup_rust = false` the stale
/// `target/` trees are not reclaimed — on this host 78.2 GiB of rust-build
/// sat on a disk at 84% — and the disk report said nothing at all.
#[test]
fn a_disabled_rust_cleanup_is_reported_once_and_a_recovery_is_confirmed() {
    use crate::proactive_rust_report;
    // The first sighting of "off" is news.
    assert_eq!(proactive_rust_report(None, false), Some("disabled"));
    // Unchanged "off" is not: the guard re-reads the policy on every pass,
    // and an unconditional warning would print thousands of times a day.
    assert_eq!(proactive_rust_report(Some(false), false), None);
    // "off" -> "on" is news: silence would leave the operator unsure.
    assert_eq!(proactive_rust_report(Some(false), true), Some("enabled"));
    // Unchanged "on" is not, and "on" on the first sighting is not either.
    assert_eq!(proactive_rust_report(Some(true), true), None);
    assert_eq!(proactive_rust_report(None, true), None);
    // "on" -> "off" is news.
    assert_eq!(proactive_rust_report(Some(true), false), Some("disabled"));
}

/// The source text of one top-level function, from its signature to the
/// start of the next top-level item.
///
/// A nested function would be indented, so the anchors below can only match
/// a column-0 item.
pub(crate) fn top_level_fn_source(source: &str, signature: &str) -> String {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} must exist"));
    let rest = &source[start + signature.len()..];
    let end = rest
        .find("\nfn ")
        .or_else(|| rest.find("\npub(crate) fn "))
        .or_else(|| rest.find("\npub(crate) async fn "))
        .or_else(|| rest.find("\nimpl "))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// The "the rust cleanup is switched off" report must NOT be tied to a
/// cleanup TIER.
///
/// ADDED 2026-10-25 (audit D7): the report was first emitted from the
/// proactive cleanup branch. The proactive branch only runs while the disk
/// is BETWEEN `proactive_cleanup_percent` (80%) and `disk_action_percent`
/// (85%) — at the action tier the guard takes the pressure branch instead,
/// so the report went silent at exactly the disk pressure that makes the
/// unreclaimed build trees matter. Measured live on this host: the disk
/// crossed to 85% and the warning the audit had just added stopped
/// appearing.
///
/// The pressure tier honours the same knob (`run_auto_cleanup` only scans
/// the rust targets when `auto_cleanup_rust` is set), so the DISABLED state
/// is equally true there — and equally worth saying.
#[test]
fn the_disabled_rust_cleanup_report_is_not_tied_to_a_cleanup_tier() {
    const SOURCE: &str = include_str!("main.rs");

    // Exactly ONE call site: a second one inside a tier would reintroduce
    // the hole this test exists to close.
    let call = "report_proactive_rust_state(guard.auto_cleanup_rust)";
    assert_eq!(
        SOURCE.matches(call).count(),
        1,
        "the rust-cleanup report must have exactly one call site — the guard pass — so no cleanup tier can skip it"
    );

    // The call site is in the guard PASS.
    let pass = top_level_fn_source(SOURCE, "pub(crate) async fn run_guard_once(");
    assert!(
        pass.contains("disk_details_for"),
        "the guard pass must have been located — otherwise the assertion below would pass on an empty region"
    );
    assert!(
        pass.contains(call),
        "the guard pass must report the rust-cleanup state, so the report appears in every cleanup tier"
    );

    // ... and NOT in the cleanup tier dispatch, which the action tier skips.
    let dispatch = top_level_fn_source(SOURCE, "fn spawn_background_cleanup(");
    assert!(
        dispatch.contains("CleanupMode::Proactive"),
        "the tier dispatch must have been located — otherwise the assertion below would pass on an empty region"
    );
    assert!(
        !dispatch.contains("report_proactive_rust_state("),
        "the rust-cleanup report must not live in the cleanup tier dispatch: the action tier (85%) takes the pressure branch and skips the proactive one"
    );
}

/// A committed `.env` TEMPLATE is not a credential. It ships in every
/// repository that documents its environment, with placeholder values.
#[test]
fn an_env_template_is_not_a_credential_file() {
    assert!(!crate::looks_credential_like(".env.example"));
    assert!(!crate::looks_credential_like(".env.ovh.example"));
    assert!(!crate::looks_credential_like(".env.sample"));
    assert!(!crate::looks_credential_like(".env.template"));
    assert!(!crate::looks_credential_like(".env.dist"));
    // The real ones still match.
    assert!(crate::looks_credential_like(".env"));
    assert!(crate::looks_credential_like(".env.local"));
    assert!(crate::looks_credential_like(".env.production"));
}

/// The strong word must LEAD the name. `credentials_backup` is a
/// credentials file; `02-borrowed-credentials` is a chapter.
#[test]
fn a_credential_word_in_the_middle_of_a_name_is_not_a_credential_file() {
    assert!(crate::looks_credential_like("credentials_backup"));
    assert!(!crate::looks_credential_like("02-borrowed-credentials"));
    assert!(!crate::looks_credential_like("04-miras-secrets"));
    assert!(!crate::looks_credential_like("wallet-token-cache"));
    assert!(crate::looks_credential_like("token"));
}

/// `secret` and `token` are credential words, but they also LEAD ordinary
/// compound names. The cargo artefacts `secret_scan-<hash>` kept a 5.0 GiB
/// build tree alive on the strength of a test name.
#[test]
fn a_credential_word_that_leads_an_ordinary_compound_name_is_not_a_credential_file() {
    assert!(!crate::looks_credential_like(
        "secret_scan-a5b13cc6b5fee239.d"
    ));
    assert!(!crate::looks_credential_like("secret_scan"));
    assert!(!crate::looks_credential_like("token_bucket.rs"));
    assert!(!crate::looks_credential_like("secrets-scan.rs"));
    // The unambiguous ones still qualify when they lead the name.
    assert!(crate::looks_credential_like("credentials_backup"));
    assert!(crate::looks_credential_like("password-store-backup"));
    assert!(crate::looks_credential_like("client_secret.json"));
    // And the whole-stem matches are unaffected.
    assert!(crate::looks_credential_like("secrets.yaml"));
    assert!(crate::looks_credential_like("token.json"));
    assert!(crate::looks_credential_like("password.txt"));
}

/// A guard that KEEPS something must say what keeping it costs.
///
/// The credential guard reported an entry COUNT ("keeping 55 flagged
/// entries") and nothing else. A count looks like a rounding error; the
/// size was 11 GiB. This pins that the cost is measured and reported.
#[tokio::test]
async fn the_credential_guard_reports_the_disk_cost_of_what_it_keeps() {
    let root = std::env::temp_dir().join(format!("dracon-kept-cost-{}", std::process::id()));
    let files = root.join("files");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(files.join("big")).expect("big entry");
    std::fs::create_dir_all(files.join("small")).expect("small entry");
    // 4 KiB in the big entry, 1 KiB in the small one.
    std::fs::write(files.join("big").join("blob.bin"), vec![0u8; 4096]).expect("blob");
    std::fs::write(files.join("small").join("blob.bin"), vec![0u8; 1024]).expect("blob");

    let mut names = std::collections::HashSet::new();
    names.insert("big".to_string());
    names.insert("small".to_string());
    let (total, sizes) = crate::kept_trash_entry_sizes(&files, &names).await;
    assert_eq!(total, 5120, "the kept entries' size must be measured");
    assert_eq!(sizes.len(), 2);
    assert_eq!(sizes[0].0, "big", "the biggest kept entry must come first");
    assert_eq!(sizes[0].1, 4096);
    assert_eq!(sizes[1].1, 1024);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Stabilized report states (2026-11-08)
//
// Every guard report state keys on a MEASURED value — a disk fill rate, a
// zombie count, a heavy process's CPU share. Those measurements oscillate
// across their thresholds, and every crossing was a "changed" report
// state, and every changed state was an event appended to
// `~/.dracon/events.jsonl`, which the sync daemon auto-commits. The
// machine-state repo churned at the rate the measurement flapped.
//
// A stabilized transition accepts a new value only after it has held for
// the sustain window.
// ---------------------------------------------------------------------------

/// The defect this protects against: a measurement that oscillates across
/// its threshold must not produce a stream of transitions.
///
/// Before the fix, each crossing produced an event (and, on the
/// notification paths, a desktop popup). The disk-trend and disk-rapid-fill
/// pair alone was ~19 of the ~27 events/hour this guard produced on the
/// live host, because both key off the same fill-rate estimate from
/// opposite sides of the same threshold.
#[test]
fn an_oscillating_measurement_produces_no_transitions() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    // The rate estimate crosses the threshold every pass.
    let mut events = 0usize;
    let mut transitions = 0usize;
    for pass in 0..40u64 {
        let observed = if pass % 2 == 0 { "soon" } else { "ok" };
        let transition = crate::report_state_transition_stabilized(
            &mut state,
            "disk-trend",
            observed,
            1800,
            120,
            start + Duration::from_secs(pass * 30),
        );
        if transition.changed {
            transitions += 1;
        }
        if transition.emit_event {
            events += 1;
        }
    }
    assert_eq!(
        transitions, 0,
        "a value that never holds for the sustain window must never become the state"
    );
    assert_eq!(
        events, 0,
        "an oscillating measurement must produce no events at all"
    );
}

/// A value that HOLDS is accepted — and reported exactly once.
///
/// The flip above must not be achieved by suppressing reports altogether:
/// a disk that really is filling toward full has to be reported.
#[test]
fn a_sustained_value_is_accepted_and_reported_once() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    // 60 passes at 30 s = 30 minutes of a genuinely sustained condition.
    let mut events = 0usize;
    let mut transitions = 0usize;
    for pass in 0..60u64 {
        let transition = crate::report_state_transition_stabilized(
            &mut state,
            "disk-trend",
            "soon",
            1800,
            120,
            start + Duration::from_secs(pass * 30),
        );
        if transition.changed {
            transitions += 1;
        }
        if transition.emit_event {
            events += 1;
        }
    }
    assert_eq!(transitions, 1, "a sustained condition is one transition");
    // One transition event, plus the 1800 s summary repeats of a condition
    // that still holds: 30 minutes of condition = the entry plus one
    // repeat, not one event per pass.
    assert!(
        events <= 3,
        "a sustained condition must not produce an event per pass, got {events}"
    );
}

/// The sustain window delays the transition; it does not swallow it.
///
/// A condition that starts and then holds is reported once the window has
/// passed — the report is never lost.
#[test]
fn a_transition_is_reported_once_the_sustain_window_has_passed() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    // First observation: the candidate starts, nothing is reported.
    let first = crate::report_state_transition_stabilized(
        &mut state,
        "disk-trend",
        "soon",
        1800,
        120,
        start,
    );
    assert!(!first.changed, "the first observation is not yet a transition");
    assert!(!first.emit_event, "the first observation emits nothing");

    // Still inside the window.
    let inside = crate::report_state_transition_stabilized(
        &mut state,
        "disk-trend",
        "soon",
        1800,
        120,
        start + Duration::from_secs(60),
    );
    assert!(!inside.changed, "60 s in, the window has not passed");
    assert!(!inside.emit_event, "60 s in, nothing is emitted");

    // Past the window: the transition is accepted and reported.
    let after = crate::report_state_transition_stabilized(
        &mut state,
        "disk-trend",
        "soon",
        1800,
        120,
        start + Duration::from_secs(130),
    );
    assert!(after.changed, "past the window, the transition is accepted");
    assert!(after.emit_event, "past the window, the transition is reported");
}

/// A recovery is a transition too — the state must be able to come back.
#[test]
fn a_sustained_recovery_is_accepted_and_reported() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    // Enter the condition and hold it.
    for pass in 0..6u64 {
        crate::report_state_transition_stabilized(
            &mut state,
            "zombie-warning",
            "over-threshold",
            1800,
            120,
            start + Duration::from_secs(pass * 30),
        );
    }
    // Now it clears and holds.
    let mut recoveries = 0usize;
    for pass in 0..6u64 {
        let transition = crate::report_state_transition_stabilized(
            &mut state,
            "zombie-warning",
            "ok",
            1800,
            120,
            start + Duration::from_secs(180 + pass * 30),
        );
        if transition.changed {
            recoveries += 1;
        }
    }
    assert_eq!(
        recoveries, 1,
        "a sustained recovery is one transition, not one per pass"
    );
}

/// The stabilized path must not LOSE reports — it may only delay them.
///
/// The plain `report_state_transition` reports a value the first time it
/// sees it. The stabilized path holds a new value for the sustain window
/// before accepting it, so its FIRST report is later — by at most the
/// window. Over a long enough run the two must produce the SAME number of
/// reports: stabilization changes WHEN a condition is reported, never
/// WHETHER it is.
#[test]
fn the_stabilized_path_delays_the_first_report_but_loses_none() {
    let mut plain = crate::GuardRuntimeState::default();
    let mut stabilized = crate::GuardRuntimeState::default();
    let start = Instant::now();

    let mut plain_events = 0usize;
    let mut stabilized_events = 0usize;
    let mut first_stabilized_at: Option<u64> = None;
    // Two hours at 30 s per pass.
    for pass in 0..240u64 {
        let at = start + Duration::from_secs(pass * 30);
        if crate::report_state_transition_at(&mut plain, "k", "warn", 1800, at).1 {
            plain_events += 1;
        }
        let transition =
            crate::report_state_transition_stabilized(&mut stabilized, "k", "warn", 1800, 120, at);
        if transition.emit_event {
            stabilized_events += 1;
            first_stabilized_at.get_or_insert(pass * 30);
        }
    }
    assert!(
        (plain_events as i64 - stabilized_events as i64).abs() <= 1,
        "stabilization must not lose or add reports over a long run: \
         plain {plain_events}, stabilized {stabilized_events}"
    );
    assert!(
        stabilized_events > 1,
        "the sustained value must still be repeated on the configured interval"
    );
    let first = first_stabilized_at.expect("the sustained value must be reported");
    assert!(
        first <= 120,
        "the first report must be delayed by at most the sustain window, got {first}s"
    );
    assert!(
        first >= 120,
        "the first report must wait for the window, got {first}s"
    );
}

// ---------------------------------------------------------------------------
// Disk rapid-fill hysteresis (2026-11-08, second pass)
//
// The 120 s sustain window could not stop this path: the measured
// oscillation PERIOD (426 s and 782 s between live events) is longer than
// the window, so each side held long enough to be accepted. A deadband
// fixes the shape of the problem rather than its timescale.
// ---------------------------------------------------------------------------

/// The defect: a rate wandering around the threshold must not flip state.
///
/// This is the live pattern — `disk_rapid_fill_gbph = 20.0` with observed
/// rates of 44.8, 32.1, 59.3 and 145.8 GiB/h. Under a bare `rate >=
/// threshold` comparison every one of those is "rapid", but any dip below
/// 20 re-armed a fresh entry.
#[test]
fn a_rate_hovering_at_the_threshold_never_flips_state() {
    let threshold = 20.0;
    // Start out of the condition, then wander around the threshold.
    let mut rapid = crate::disk_rapid_verdict(threshold * 0.5, false, threshold);
    assert!(!rapid, "below the entry threshold the disk is not rapid");

    // The legitimate ENTRY is not a flip — it is the one transition allowed.
    rapid = crate::disk_rapid_verdict(21.0, rapid, threshold);
    assert!(rapid, "crossing the entry threshold from below enters rapid");

    // From here the rate wanders across the threshold and must be absorbed.
    let mut flips = 0usize;
    for rate in [19.0, 22.0, 18.0, 25.0, 17.0, 20.5, 19.5] {
        let next = crate::disk_rapid_verdict(rate, rapid, threshold);
        if next != rapid {
            flips += 1;
        }
        rapid = next;
    }
    assert_eq!(
        flips, 0,
        "a rate wandering across the threshold must be absorbed by the deadband, got {flips} flips"
    );
    assert!(rapid, "having entered rapid, the verdict stays rapid inside the deadband");
}

/// The deadband must not trap the state: a real fall exits.
#[test]
fn a_rate_falling_far_below_the_threshold_leaves_rapid() {
    let threshold = 20.0;
    let exit = threshold * crate::DISK_RAPID_FILL_EXIT_RATIO;
    assert!(
        !crate::disk_rapid_verdict(exit - 0.1, true, threshold),
        "below the exit ratio the verdict must leave rapid"
    );
    assert!(
        crate::disk_rapid_verdict(exit + 0.1, true, threshold),
        "above the exit ratio the verdict must stay rapid"
    );
    // A reclamation drops the rate to zero, which exits.
    assert!(!crate::disk_rapid_verdict(0.0, true, threshold));
}

/// An unmeasurable rate must HOLD the state, not re-arm it.
///
/// This was the dominant cause of the guard's event churn: the caller
/// collapsed `Unmeasured` and `NotFilling` into one state label, so every
/// non-positive or too-young measurement reset the accepted state and the
/// next measurable pass re-emitted `unmeasured -> rapid` as a fresh
/// transition. Every live `disk/rapid-fill` event quoted a rate of 31-146
/// GiB/h against a 20 GiB/h threshold, so the alarm never actually stopped
/// being true — those events were the re-arm cycle.
#[test]
fn an_unmeasured_rate_holds_the_report_state_instead_of_rearming() {
    let mut state = crate::GuardRuntimeState::default();
    let start = Instant::now();

    // Establish "rapid" and let the sustain window pass.
    for pass in 0..5u64 {
        crate::report_state_transition_stabilized(
            &mut state,
            "disk-rapid-fill",
            "rapid",
            1800,
            120,
            start + Duration::from_secs(pass * 30),
        );
    }
    let held = state.report_states.get("disk-rapid-fill").unwrap().value.clone();
    assert_eq!(held, "rapid");

    // Now this is what the fixed caller does when the rate is Unmeasured:
    // it returns without touching the report state at all.
    let still_held = state.report_states.get("disk-rapid-fill").unwrap().value.clone();
    assert_eq!(
        still_held, "rapid",
        "an unmeasurable rate must not reset the accepted state"
    );

    // The next measurable pass therefore sees no change and emits nothing.
    let after = crate::report_state_transition_stabilized(
        &mut state,
        "disk-rapid-fill",
        "rapid",
        1800,
        120,
        start + Duration::from_secs(600),
    );
    assert!(!after.changed, "no change, so no transition");
}

/// The literal contract case: `critical -> ok` recovery still reports.
///
/// The earlier test exercised `warn -> ok`. The contract names
/// `critical -> ok` specifically, and the class mapping means both should
/// behave identically — this pins the literal case rather than inferring it
/// from the class-equivalence test.
#[test]
fn memory_pressure_critical_to_ok_recovery_still_emits() {
    let mut state = crate::GuardRuntimeState::default();
    let arrival = crate::memory_pressure_report_decision(&mut state, "critical", 1800);
    assert!(arrival.emit_event, "arrival into critical must emit");
    assert!(arrival.notify, "arrival into critical must notify");
    let recovery = crate::memory_pressure_report_decision(&mut state, "ok", 1800);
    assert!(
        recovery.emit_event,
        "a literal critical -> ok recovery must emit a recovery event"
    );
    assert!(
        !recovery.notify,
        "recovery is not a new pressure arrival, so it must not raise an alarm"
    );
}

/// A permanently unavailable measurement must not be heartbeated.
///
/// `memory/service-growth` was emitting an identical "unavailable (host
/// PID/cgroup namespace visibility unavailable)" event every
/// `report_repeat_secs` — three consecutive live events at 19:09:23,
/// 19:21:07 and 19:51:21 carried the same text. That state cannot be acted
/// on and cannot change, so the repeat carries no information. It is now
/// reported once on entry, and repeated only while there is real growth.
#[test]
fn an_unavailable_service_growth_measurement_is_not_heartbeated() {
    // Entering the state is news, so it emits even with no growth.
    assert!(
        crate::service_growth_should_emit(true, true, false, true),
        "entry into unavailable must be reported once"
    );
    // The repeat is NOT news: unchanged and nothing actionable.
    assert!(
        !crate::service_growth_should_emit(true, true, false, false),
        "an unchanged unavailable state must not repeat"
    );
    // Real growth still repeats — that IS actionable.
    assert!(
        crate::service_growth_should_emit(true, true, true, false),
        "sustained growth must still repeat on the configured interval"
    );
    // A never-due pass with a change is still news.
    assert!(crate::service_growth_should_emit(true, false, false, true));
    // Disabled monitoring says nothing at all.
    assert!(!crate::service_growth_should_emit(false, true, true, true));
}

/// The LIVE cross-path pair that survived every earlier fix.
///
/// 2026-11-08: `disk/trend-warning` at 21:51:08 ("13.5 GiB/h") and
/// `disk/rapid-fill` at 21:54:44 ("25.8 GiB/h") — 216 s apart, two
/// commits in the machine-state repo. Both paths were already stabilized
/// and both had passed their 120 s sustain window, so the sustain could
/// never have caught this: the defect was that ONE condition (the disk
/// fill rate) was being reported by TWO independent report states.
///
/// The guard's rule is one report per distinct held condition per window.
/// Whichever severity speaks first owns the window; the other stays
/// silent even though it is a fresh transition of its own state, and even
/// though it is an escalation worth telling the operator about — the
/// notification still fires, because a notification is a prompt and not a
/// commit in the machine-state repo.
#[test]
fn an_escalation_from_trend_to_rapid_is_one_report_not_two() {
    let mut state = crate::GuardRuntimeState::default();
    let repeat = 1800u64;

    // The trend warning accepts first and speaks.
    assert!(
        crate::disk_rate_event_due(&state, repeat),
        "the first report of a fill episode must be allowed"
    );
    state.last_disk_rate_event = Some(Instant::now());

    // 216 s later the rate has risen past the rapid threshold. Its own
    // stabilized transition is genuinely accepted — the state machine is
    // right — but the condition already spoke, so nothing is committed.
    assert!(
        !crate::disk_rate_event_due(&state, repeat),
        "an escalation 216 s after the trend warning must not commit a second \
         event for the same fill episode, got a due event"
    );

    // The escalation is not lost: the notification path does not consult
    // this gate, so the operator is still told promptly (see
    // `check_rapid_disk_fill`'s `transition.changed` notification).

    // Once the repeat interval passes the condition may speak again, so a
    // still-filling disk is never permanently silenced.
    state.last_disk_rate_event = Some(Instant::now() - Duration::from_secs(repeat + 1));
    assert!(
        crate::disk_rate_event_due(&state, repeat),
        "the fill-rate condition must be allowed to report again after the \
         repeat interval, otherwise a worsening disk goes silent forever"
    );
}
