//! Tests for policy.rs — legal-range enforcement (`normalize_guard_policy`).
//!
//! One `#[test]` per knob, mirroring the zram `memory_percent` bounds test:
//! a range that exists only as prose in the example template is not a range.
//! The cases are split by rule shape, and `sentinel_zero_knobs_are_never_clamped`
//! pins the knobs that are deliberately left alone so a later "just add a
//! floor" change cannot silently re-arm a feature the operator turned off.

use super::*;

// ---------------------------------------------------------------------------
// Generators — one #[test] per knob, per rule shape
// ---------------------------------------------------------------------------

/// Knob with a documented lower bound: below it the daemon misbehaves.
macro_rules! floor_knob_tests {
    ($($name:ident: $field:ident = $bad:expr => min $min:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut p = GuardPolicy { $field: $bad, ..Default::default() };
                let adjusted = normalize_guard_policy(&mut p);
                assert!(
                    p.$field >= $min,
                    concat!(stringify!($field), " = {:?} is below its floor {}"),
                    $bad,
                    $min
                );
                assert!(
                    adjusted.contains(&stringify!($field)),
                    "{} must be reported as clamped",
                    stringify!($field)
                );
            }
        )*
    };
}

/// Knob with a documented upper bound: above it the knob is meaningless or
/// unbounded.
macro_rules! ceil_knob_tests {
    ($($name:ident: $field:ident = $bad:expr => max $max:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut p = GuardPolicy { $field: $bad, ..Default::default() };
                let adjusted = normalize_guard_policy(&mut p);
                assert!(
                    p.$field <= $max,
                    concat!(stringify!($field), " = {:?} is above its ceiling {}"),
                    $bad,
                    $max
                );
                assert!(
                    adjusted.contains(&stringify!($field)),
                    "{} must be reported as clamped",
                    stringify!($field)
                );
            }
        )*
    };
}

/// Knob with meaningful bounds on both ends.
macro_rules! band_knob_tests {
    ($($name:ident: $field:ident low $low:expr => $bad_lo:expr, high $high:expr => $bad_hi:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut low = GuardPolicy { $field: $bad_lo, ..Default::default() };
                let adjusted = normalize_guard_policy(&mut low);
                assert!(
                    low.$field >= $low,
                    concat!(stringify!($field), " = {:?} is below its floor {}"),
                    $bad_lo,
                    $low
                );
                assert!(adjusted.contains(&stringify!($field)));

                let mut high = GuardPolicy { $field: $bad_hi, ..Default::default() };
                let adjusted = normalize_guard_policy(&mut high);
                assert!(
                    high.$field <= $high,
                    concat!(stringify!($field), " = {:?} is above its ceiling {}"),
                    $bad_hi,
                    $high
                );
                assert!(adjusted.contains(&stringify!($field)));
            }
        )*
    };
}

// ---------------------------------------------------------------------------
// Floors
// ---------------------------------------------------------------------------

floor_knob_tests! {
    interval_secs_floor: interval_secs = 0 => min 5;
    auto_cleanup_interval_secs_floor: auto_cleanup_interval_secs = 0 => min 60;
    report_repeat_secs_floor: report_repeat_secs = 0 => min 60;
    proactive_cleanup_interval_cycles_floor: proactive_cleanup_interval_cycles = 0 => min 1;
    process_rss_mb_floor: process_rss_mb = 0 => min 64;
    process_sustain_secs_floor: process_sustain_secs = 0 => min 5;
    release_after_secs_floor: release_after_secs = 0 => min 5;
    notify_cooldown_secs_floor: notify_cooldown_secs = 0 => min 5;
    memory_pressure_sustain_secs_floor: memory_pressure_sustain_secs = 0 => min 30;
    guard_log_max_mb_floor: guard_log_max_mb = 0 => min 1;
    log_max_truncate_mb_floor: log_max_truncate_mb = 0 => min 1;
    cleanup_min_size_mb_floor: cleanup_min_size_mb = 0 => min 1;
    rust_target_max_age_days_floor: rust_target_max_age_days = 0 => min 1;
    relocate_min_size_mb_floor: relocate_min_size_mb = 0 => min 1;
    relocate_max_moves_per_pass_floor: relocate_max_moves_per_pass = 0 => min 1;
}

// A 0 log cap is the floor case that motivated the rule: `rotate_guard_log_if_oversized`
// returns early on 0, so it does not mean "unlimited" — it means rotation is
// off and the event log grows until the disk guard catches it.
#[test]
fn guard_log_max_mb_zero_would_silently_disable_rotation() {
    let mut p = GuardPolicy {
        guard_log_max_mb: 0,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.guard_log_max_mb >= 1);
}

// ---------------------------------------------------------------------------
// Ceilings
// ---------------------------------------------------------------------------

ceil_knob_tests! {
    cap_offenders_cpu_percent_ceiling: cap_offenders_cpu_percent = 500 => max 100;
    log_preserve_header_lines_ceiling: log_preserve_header_lines = 50_000_000 => max LOG_PRESERVE_HEADER_LINES_MAX;
}

#[test]
fn renice_value_is_clamped_into_the_posix_nice_range() {
    // Negative would let the guard RAISE an offender's priority, inverting
    // the tier-floor contract the example template documents.
    let mut low = GuardPolicy {
        renice_value: -20,
        ..Default::default()
    };
    normalize_guard_policy(&mut low);
    assert_eq!(low.renice_value, RENICE_VALUE_MIN);

    // Above 19 the renice exec fails outright.
    let mut high = GuardPolicy {
        renice_value: 40,
        ..Default::default()
    };
    normalize_guard_policy(&mut high);
    assert_eq!(high.renice_value, RENICE_VALUE_MAX);
}

// ---------------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------------

band_knob_tests! {
    disk_warn_percent_band: disk_warn_percent low 1 => 0, high 100 => 200;
    inode_warn_percent_band: inode_warn_percent low 1 => 0, high 100 => 200;
    mem_available_warn_percent_band: mem_available_warn_percent low 1 => 0, high 100 => 200;
    swap_used_warn_percent_band: swap_used_warn_percent low 1 => 0, high 200 => 250;
}

// ---------------------------------------------------------------------------
// Floats — NaN and infinity
// ---------------------------------------------------------------------------

/// TOML accepts `nan` and `inf` literals and serde maps them straight
/// through. A NaN fails every comparison, so a `>`-guarded clamp would let it
/// past and silently disable the threshold it guards — hence the
/// unconditional assign in the fband macro.
#[test]
fn float_knobs_scrub_nan_and_infinity() {
    let mut p = GuardPolicy {
        process_cpu_percent: f32::NAN,
        mem_psi_full_warn: f64::NAN,
        disk_rapid_fill_gbph: f64::NAN,
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut p);
    assert!(p.process_cpu_percent.is_finite(), "NaN CPU threshold survived");
    assert!(p.mem_psi_full_warn.is_finite(), "NaN PSI threshold survived");
    assert!(
        p.disk_rapid_fill_gbph.is_finite(),
        "NaN fill-rate survived"
    );
    for field in ["process_cpu_percent", "mem_psi_full_warn", "disk_rapid_fill_gbph"] {
        assert!(adjusted.contains(&field), "{field} must be reported");
    }
}

#[test]
fn float_knobs_clamp_infinity() {
    let mut p = GuardPolicy {
        process_cpu_percent: f32::INFINITY,
        mem_psi_full_warn: f64::INFINITY,
        disk_rapid_fill_gbph: f64::INFINITY,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.process_cpu_percent.is_finite());
    assert!(p.mem_psi_full_warn.is_finite());
    assert!(p.disk_rapid_fill_gbph.is_finite());
}

#[test]
fn float_knobs_clamp_negative_infinity() {
    let mut p = GuardPolicy {
        process_cpu_percent: f32::NEG_INFINITY,
        mem_psi_full_warn: f64::NEG_INFINITY,
        disk_rapid_fill_gbph: f64::NEG_INFINITY,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.process_cpu_percent >= 1.0);
    assert!(p.mem_psi_full_warn >= 0.0);
    assert!(p.disk_rapid_fill_gbph >= 0.5);
}

// Per-process CPU is a percentage of ONE core, so a 32-core process
// legitimately reads 3200%. The ceiling must not squash that.
#[test]
fn process_cpu_percent_ceiling_allows_multicore_readings() {
    let mut p = GuardPolicy {
        process_cpu_percent: 3200.0,
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut p);
    assert_eq!(p.process_cpu_percent, 3200.0);
    assert!(!adjusted.contains(&"process_cpu_percent"));
}

// ---------------------------------------------------------------------------
// Cross-field invariants
// ---------------------------------------------------------------------------

#[test]
fn disk_bands_are_ordered() {
    let mut p = GuardPolicy {
        disk_early_warn_percent: 99,
        disk_warn_percent: 80,
        disk_action_percent: 40,
        disk_critical_percent: 10,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.disk_early_warn_percent <= p.disk_warn_percent);
    assert!(p.disk_action_percent >= p.disk_warn_percent);
    assert!(p.disk_critical_percent >= p.disk_action_percent);
    assert!(p.disk_action_percent <= 100);
    assert!(p.disk_critical_percent <= 100);
}

#[test]
fn pre_action_gates_stay_below_the_action_level() {
    let mut p = GuardPolicy {
        disk_action_percent: 90,
        proactive_cleanup_percent: 95,
        unfreeze_below_percent: 90,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.proactive_cleanup_percent < p.disk_action_percent);
    assert!(p.unfreeze_below_percent < p.disk_action_percent);
}

#[test]
fn stuck_threshold_never_drops_below_sustain() {
    let mut p = GuardPolicy {
        process_sustain_secs: 300,
        process_stuck_after_secs: 30,
        ..Default::default()
    };
    normalize_guard_policy(&mut p);
    assert!(p.process_stuck_after_secs >= p.process_sustain_secs);
}

// ---------------------------------------------------------------------------
// Rule 2 — sentinel-zero knobs are never clamped
// ---------------------------------------------------------------------------

/// Every knob where 0 is a documented "off" sentinel. A floor here would
/// silently RE-ARM a feature the operator deliberately disabled, which is a
/// worse failure than the one the clamp was meant to prevent.
#[test]
fn sentinel_zero_knobs_are_never_clamped() {
    let mut sentinel = GuardPolicy {
        trend_warn_hours: 0,
        zombie_threshold: 0,
        log_size_mb: 0,
        node_modules_max_age_days: 0,
        tmp_min_age_hours: 0,
        trash_min_age_days: 0,
        rust_target_action_min_age_days: 0,
        quarantine_ttl_days: 0,
        cap_offenders_cpu_percent: 0,
        reap_report_min_idle_hours: 0,
        reap_report_max_cpu_seconds: 0,
        nix_keep_generations: 0,
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut sentinel);
    for knob in SENTINEL_ZERO_KNOBS {
        assert!(
            !adjusted.contains(knob),
            "{knob} is a documented 0-sentinel and must not be clamped (adjusted: {adjusted:?})"
        );
    }
    // Spot-check the values actually survived, so the test is not vacuous.
    assert_eq!(sentinel.trend_warn_hours, 0);
    assert_eq!(sentinel.quarantine_ttl_days, 0);
    assert_eq!(sentinel.zombie_threshold, 0);
    assert_eq!(sentinel.nix_keep_generations, 0);
    assert_eq!(sentinel.rust_target_action_min_age_days, 0);
}

/// The sentinel list is a contract, not documentation: if a knob is added to
/// it, the list and the policy struct must still agree, and if a listed knob
/// is removed from the struct the list must not drift silently.
#[test]
fn sentinel_zero_knob_list_matches_the_policy_fields() {
    let known = [
        "trend_warn_hours",
        "zombie_threshold",
        "log_size_mb",
        "node_modules_max_age_days",
        "tmp_min_age_hours",
        "trash_min_age_days",
        "rust_target_action_min_age_days",
        "quarantine_ttl_days",
        "cap_offenders_cpu_percent",
        "reap_report_min_idle_hours",
        "reap_report_max_cpu_seconds",
        "nix_keep_generations",
    ];
    let mut listed: Vec<&&str> = SENTINEL_ZERO_KNOBS.iter().collect();
    listed.sort();
    let mut expected: Vec<&&str> = known.iter().collect();
    expected.sort();
    assert_eq!(listed, expected, "SENTINEL_ZERO_KNOBS drifted");
}

// ---------------------------------------------------------------------------
// Empty-string fallbacks
// ---------------------------------------------------------------------------

#[test]
fn blank_strings_fall_back_to_their_defaults() {
    let mut p = GuardPolicy {
        sync_freeze_marker: "   ".to_string(),
        quarantine_dir: String::new(),
        notify_command: "  ".to_string(),
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut p);
    assert_eq!(p.sync_freeze_marker, default_sync_freeze_marker());
    assert_eq!(p.quarantine_dir, default_quarantine_dir());
    assert_eq!(p.notify_command, default_notify_command());
    for field in ["sync_freeze_marker", "quarantine_dir", "notify_command"] {
        assert!(adjusted.contains(&field), "{field} must be reported");
    }
}

// ---------------------------------------------------------------------------
// Storage policy
// ---------------------------------------------------------------------------

#[test]
fn storage_min_size_mb_floor() {
    let mut s = StoragePolicy {
        min_size_mb: 0,
        ..Default::default()
    };
    let adjusted = normalize_storage_policy(&mut s);
    assert!(s.min_size_mb >= 1);
    assert!(adjusted.contains(&"storage.min_size_mb"));
}

#[test]
fn system_policy_normalize_covers_both_sub_policies() {
    let mut p = SystemPolicy {
        storage: StoragePolicy {
            min_size_mb: 0,
            ..Default::default()
        },
        guard: GuardPolicy {
            interval_secs: 0,
            guard_log_max_mb: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    p.normalize();
    assert!(p.storage.min_size_mb >= 1);
    assert!(p.guard.interval_secs >= 5);
    assert!(p.guard.guard_log_max_mb >= 1);
}

// ---------------------------------------------------------------------------
// Idempotency — the property that lets the load boundary AND the CLI
// entry points both normalize without double-applying anything.
// ---------------------------------------------------------------------------

#[test]
fn normalization_is_idempotent() {
    let mut p = GuardPolicy {
        disk_early_warn_percent: 200,
        disk_warn_percent: 10,
        disk_action_percent: 5,
        disk_critical_percent: 1,
        proactive_cleanup_percent: 200,
        unfreeze_below_percent: 200,
        interval_secs: 0,
        renice_value: -99,
        process_cpu_percent: f32::NAN,
        guard_log_max_mb: 0,
        log_preserve_header_lines: usize::MAX,
        process_sustain_secs: 900,
        process_stuck_after_secs: 1,
        ..Default::default()
    };
    let first = normalize_guard_policy(&mut p);
    assert!(!first.is_empty(), "the fixture must actually be clamped");

    let after_first = p.clone();
    let second = normalize_guard_policy(&mut p);
    assert!(
        second.is_empty(),
        "second pass re-clamped {second:?}; normalization is not idempotent"
    );
    assert!(
        format!("{p:?}") == format!("{after_first:?}"),
        "second pass mutated the policy"
    );
}

#[test]
fn already_legal_policy_is_left_untouched() {
    let mut p = GuardPolicy::default();
    assert!(
        normalize_guard_policy(&mut p).is_empty(),
        "the shipped defaults must already satisfy every documented range"
    );
}

// ---------------------------------------------------------------------------
// The load boundary — the reason this module exists
// ---------------------------------------------------------------------------

/// A parsed TOML carrying out-of-range values must come out normalized, so
/// every consumer gets in-range values by construction. Nine of the eleven
/// `load_system_policy` call sites never normalized, which is exactly how a
/// bad `quarantine_ttl_days` or `storage.min_size_mb` used to reach relocate,
/// quarantine and storage RAW.
#[test]
fn parsed_policy_is_normalized_at_the_load_boundary() {
    let toml_src = r#"
[guard]
interval_secs = 0
guard_log_max_mb = 0
renice_value = -20
process_cpu_percent = nan
quarantine_ttl_days = 0
disk_warn_percent = 200
process_sustain_secs = 900
process_stuck_after_secs = 1

[storage]
min_size_mb = 0
"#;
    let mut parsed: SystemPolicy = toml::from_str(toml_src).expect("fixture must parse");
    parsed.normalize();
    assert!(parsed.guard.interval_secs >= 5);
    assert!(parsed.guard.guard_log_max_mb >= 1);
    assert_eq!(parsed.guard.renice_value, 0);
    assert!(parsed.guard.process_cpu_percent.is_finite());
    // 0 is a documented sentinel, so it survives.
    assert_eq!(parsed.guard.quarantine_ttl_days, 0);
    assert!(parsed.guard.disk_warn_percent <= 100);
    assert!(parsed.guard.process_stuck_after_secs >= parsed.guard.process_sustain_secs);
    assert!(parsed.storage.min_size_mb >= 1);
}
