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
}

#[test]
fn guard_runtime_state_insert_and_retrieve_heavy_process() {
    let mut state = crate::GuardRuntimeState::default();
    state.heavy_since.insert(1234, Instant::now());
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
    let output = "12345  1  50.0  1024  cargo\n  23456  12345  25.0  2048  rustc";
    let samples = crate::parse_ps_output(output);
    assert_eq!(samples.len(), 2);

    assert_eq!(samples[0].pid, 12345);
    assert_eq!(samples[0].ppid, 1);
    assert_eq!(samples[0].cpu_percent, 50.0);
    // RSS is in KB, converted to MB via /1024
    assert_eq!(samples[0].rss_mb, 1024 / 1024); // 1024 KB = 1 MB
    assert_eq!(samples[0].command, "cargo");

    assert_eq!(samples[1].pid, 23456);
    assert_eq!(samples[1].ppid, 12345);
    assert_eq!(samples[1].cpu_percent, 25.0);
    assert_eq!(samples[1].rss_mb, 2048 / 1024); // 2048 KB = 2 MB
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
    let output = "not_valid\n12345  1  75.0  512  cargo";
    let samples = crate::parse_ps_output(output);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].pid, 12345);
    assert_eq!(samples[0].cpu_percent, 75.0);
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
