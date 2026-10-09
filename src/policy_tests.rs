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
    assert!(
        p.process_cpu_percent.is_finite(),
        "NaN CPU threshold survived"
    );
    assert!(
        p.mem_psi_full_warn.is_finite(),
        "NaN PSI threshold survived"
    );
    assert!(p.disk_rapid_fill_gbph.is_finite(), "NaN fill-rate survived");
    for field in [
        "process_cpu_percent",
        "mem_psi_full_warn",
        "disk_rapid_fill_gbph",
    ] {
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
        relocate_min_age_days: 0,
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

/// ADDED 2026-10-08 (audit F117): the reap_report_* 0s stay sentinels
/// for the REPORT, but the DESTRUCTIVE reaper shares the same policy object
/// — a 0 age floor there makes every idle terminal-less allowlisted process
/// a kill candidate. With the destructive opt-in set, normalization must
/// floor min_idle_hours at 1 and record the adjustment.
#[test]
fn destructive_reap_opt_in_floors_the_idle_age_gate() {
    let mut policy = GuardPolicy {
        reap_stale_dev_servers: true,
        reap_report_min_idle_hours: 0,
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut policy);
    assert_eq!(
        policy.reap_report_min_idle_hours, 1,
        "the destructive reaper must never run without an age floor (adjusted: {adjusted:?})"
    );
    assert!(
        adjusted.contains(&"reap_report_min_idle_hours"),
        "the adjustment must be reported so SIGHUP logging names the knob: {adjusted:?}"
    );
}

/// The flip side of the floor above: with the destructive opt-in OFF, 0 is
/// still a legal report-only sentinel and must survive normalization
/// untouched (pinned next to the destructive case so the pair stays honest).
#[test]
fn report_only_reap_keeps_the_zero_idle_sentinel() {
    let mut policy = GuardPolicy {
        reap_stale_dev_servers: false,
        reap_report_min_idle_hours: 0,
        ..Default::default()
    };
    let adjusted = normalize_guard_policy(&mut policy);
    assert_eq!(
        policy.reap_report_min_idle_hours, 0,
        "the report-only path must keep its 0 sentinel (adjusted: {adjusted:?})"
    );
    assert!(!adjusted.contains(&"reap_report_min_idle_hours"));
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
        "relocate_min_age_days",
        "trash_min_age_days",
        "rust_target_action_min_age_days",
        "quarantine_ttl_days",
        "cap_offenders_cpu_percent",
        "reap_report_min_idle_hours",
        "reap_report_max_cpu_seconds",
        "nix_keep_generations",
        "quarantine_expire_interval_secs",
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

/// Go through the SAME entry point the loader uses.
///
/// The earlier version of this test parsed TOML and called `normalize()`
/// itself, so it would still have passed if the loader had stopped
/// normalizing — it pinned the normalizer, not the load path. Calling
/// `parse_system_policy` means dropping the `.normalize()` from that
/// function now fails here, which is the line that actually matters.
#[test]
fn parse_system_policy_is_the_normalizing_entry_point() {
    let toml_src = r#"
[guard]
interval_secs = 0
guard_log_max_mb = 0
renice_value = -20

[storage]
min_size_mb = 0
"#;
    let parsed = crate::parse_system_policy(toml_src, std::path::Path::new("fixture.toml"))
        .expect("fixture must parse");
    assert!(parsed.guard.interval_secs >= 5);
    assert!(parsed.guard.guard_log_max_mb >= 1);
    assert_eq!(parsed.guard.renice_value, 0);
    assert!(parsed.storage.min_size_mb >= 1);
}

// ---------------------------------------------------------------------------
// Report cadence — the guard re-loads the policy every pass
// ---------------------------------------------------------------------------

/// Drive the clamp-report state machine the way the production wrapper does,
/// but over locally-owned state instead of the process-global.
///
/// FIXED 2026-09-29 (auditor, second review round): these tests used to
/// reset and read `LAST_REPORTED_CLAMPS` directly, so libtest's concurrent
/// threads interleaved and the full suite failed ~7% of runs with another
/// test's clamped set. Folding the pure decision over local state is
/// order-independent and needs no mutex.
fn run_report_sequence(steps: &[&[&'static str]]) -> Vec<ClampReport> {
    let mut state: Option<Vec<&'static str>> = None;
    let mut decisions = Vec::with_capacity(steps.len());
    for step in steps {
        let decision = clamp_report_decision(state.as_deref(), step);
        match &decision {
            ClampReport::SilentNoState | ClampReport::Unchanged => {}
            _ => state = Some(step.to_vec()),
        }
        decisions.push(decision);
    }
    decisions
}

const A: &[&str] = &["interval_secs"];
const B: &[&str] = &["disk_warn_percent", "interval_secs"];
const NONE: &[&str] = &[];

#[test]
fn first_out_of_range_policy_is_reported() {
    assert_eq!(
        run_report_sequence(&[A])[0],
        ClampReport::Clamped(vec!["interval_secs"])
    );
}

#[test]
fn an_in_range_policy_on_a_fresh_process_records_nothing() {
    assert_eq!(
        run_report_sequence(&[NONE])[0],
        ClampReport::SilentNoState,
        "an in-range policy must not report, and must not record state — \
         otherwise the first real clamp looks like a transition from a known state"
    );
}

#[test]
fn repeating_the_same_clamped_set_is_silent() {
    let d = run_report_sequence(&[A, A, A, A]);
    assert_eq!(d[0], ClampReport::Clamped(vec!["interval_secs"]));
    for step in &d[1..] {
        assert_eq!(*step, ClampReport::Unchanged, "a repeat must stay silent");
    }
}

#[test]
fn returning_to_in_range_reports_the_recovery_once() {
    let d = run_report_sequence(&[A, A, NONE, NONE]);
    assert_eq!(d[0], ClampReport::Clamped(vec!["interval_secs"]));
    assert_eq!(d[1], ClampReport::Unchanged);
    assert_eq!(
        d[2],
        ClampReport::Recovered(vec!["interval_secs"]),
        "the fix must be confirmed, not inferred from silence"
    );
    assert_eq!(d[3], ClampReport::Unchanged, "recovery must not repeat");
}

#[test]
fn a_changed_clamped_set_reports_again() {
    let d = run_report_sequence(&[A, B]);
    assert_eq!(d[0], ClampReport::Clamped(vec!["interval_secs"]));
    assert_eq!(
        d[1],
        ClampReport::Clamped(vec!["disk_warn_percent", "interval_secs"])
    );
}

#[test]
fn already_clean_stays_silent_across_many_passes() {
    for d in run_report_sequence(&vec![NONE; 50]) {
        assert_eq!(d, ClampReport::SilentNoState);
    }
}

/// The daemon cadence: many passes over one misconfigured file must produce
/// exactly one report, not one per pass.
#[test]
fn repeated_passes_over_a_bad_policy_report_exactly_once() {
    let d = run_report_sequence(&vec![B; 50]);
    assert_eq!(
        d[0],
        ClampReport::Clamped(vec!["disk_warn_percent", "interval_secs"])
    );
    for step in &d[1..] {
        assert_eq!(*step, ClampReport::Unchanged);
    }
}

/// The shipped example template documents the ranges, so it must itself be
/// legal. This is what stops the two from drifting: if a range is tightened in
/// code, the example that violates it fails here rather than being copied onto
/// a machine as the recommended starting config.
#[test]
fn shipped_example_template_is_already_legal() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system.example.toml");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut parsed: SystemPolicy =
        toml::from_str(&content).unwrap_or_else(|e| panic!("example template must parse: {e}"));
    let adjusted = normalize_guard_policy(&mut parsed.guard);
    assert!(
        adjusted.is_empty(),
        "dracon-system.example.toml sets out-of-range values for {adjusted:?}; \
         either fix the example or widen the range in normalize_guard_policy"
    );
}

// ---------------------------------------------------------------------------
// notify_command parsing
// ---------------------------------------------------------------------------

// --- multi-arg values ---

#[test]
fn plain_path_is_a_program_with_no_arguments() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send").expect("must parse");
    assert_eq!(cmd.program, "/usr/bin/notify-send");
    assert!(
        cmd.args.is_empty(),
        "backwards compatibility: no args expected"
    );
}

#[test]
fn notify_send_urgency_flag_parses_into_two_arguments() {
    // The case that previously "silently failed to exec" and forced a
    // wrapper script.
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -u critical").expect("must parse");
    assert_eq!(cmd.program, "/usr/bin/notify-send");
    assert_eq!(cmd.args, vec!["-u", "critical"]);
}

#[test]
fn multiple_flags_and_an_equals_style_value_parse() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -u critical -i /tmp/icon.png -t 5s")
        .expect("must parse");
    assert_eq!(
        cmd.args,
        vec!["-u", "critical", "-i", "/tmp/icon.png", "-t", "5s"]
    );
}

#[test]
fn runs_of_whitespace_separate_words() {
    let cmd =
        NotifyCommand::parse("  /usr/bin/notify-send \t -u   critical  ").expect("must parse");
    assert_eq!(cmd.program, "/usr/bin/notify-send");
    assert_eq!(cmd.args, vec!["-u", "critical"]);
}

#[test]
fn a_quoted_path_containing_spaces_stays_one_program() {
    // Unquoted spaces split (as in any shell); quoting is how a path with
    // a space is expressed.
    let split = NotifyCommand::parse("/opt/my notifier/bin/notify").expect("must parse");
    assert_eq!(split.program, "/opt/my");
    assert_eq!(split.args, vec!["notifier/bin/notify"]);

    let quoted = NotifyCommand::parse("\"/opt/my notifier/bin/notify\"").expect("must parse");
    assert_eq!(quoted.program, "/opt/my notifier/bin/notify");
    assert!(quoted.args.is_empty());
}

// --- quoting ---

#[test]
fn single_quotes_preserve_spaces() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -t 'my title'").expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "my title"]);
}

#[test]
fn double_quotes_preserve_spaces() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -t \"my title\"").expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "my title"]);
}

#[test]
fn quoted_empty_string_is_an_empty_argument_not_a_missing_one() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -a '' -b \"\"").expect("must parse");
    assert_eq!(cmd.args, vec!["-a", "", "-b", ""]);
}

#[test]
fn quotes_concatenate_with_adjacent_unquoted_text() {
    let cmd =
        NotifyCommand::parse("/usr/bin/notify-send -t pre'fix '\"-suf\"fix").expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "prefix -suffix"]);
}

#[test]
fn backslash_escapes_the_next_character() {
    let cmd = NotifyCommand::parse("/usr/bin/notify-send -t my\\ title").expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "my title"]);
}

#[test]
fn backslash_inside_double_quotes_only_escapes_the_posix_set() {
    // "a\"b" -> a"b
    let cmd = NotifyCommand::parse(r#"/usr/bin/notify-send -t "a\"b""#).expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "a\"b"]);
    // POSIX keeps a backslash that does not precede " \ $ ` inside double
    // quotes, so "\n" is two characters.
    let cmd = NotifyCommand::parse(r#"/usr/bin/notify-send -t "a\nb""#).expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "a\\nb"]);
}

#[test]
fn single_quotes_are_literal_including_backslashes() {
    let cmd = NotifyCommand::parse(r#"/usr/bin/notify-send -t 'a\b'"#).expect("must parse");
    assert_eq!(cmd.args, vec!["-t", "a\\b"]);
}

#[test]
fn unterminated_quotes_are_an_error() {
    for bad in [
        "/usr/bin/notify-send -t 'oops",
        "/usr/bin/notify-send -t \"oops",
        "/usr/bin/notify-send -t oops\\",
    ] {
        assert!(
            NotifyCommand::parse(bad).is_err(),
            "{bad} must be rejected rather than silently mis-parsed"
        );
    }
}

// --- no shell: metacharacters are inert argv, never syntax ---

#[test]
fn shell_metacharacters_are_inert_arguments_not_syntax() {
    let cmd = NotifyCommand::parse(
        "/usr/bin/notify-send -t 'a; rm -rf /' -b '$(whoami)' -i `id` && echo pwned",
    )
    .expect("must parse");
    // They are passed through verbatim as argv entries. Nothing is expanded
    // and nothing is executed: Command::new execs the program directly.
    assert_eq!(
        cmd.args,
        vec![
            "-t",
            "a; rm -rf /",
            "-b",
            "$(whoami)",
            "-i",
            "`id`",
            "&&",
            "echo",
            "pwned",
        ]
    );
    assert_eq!(cmd.program, "/usr/bin/notify-send");
}

// --- the security boundary arg support would otherwise have removed ---

#[test]
fn a_relative_program_is_rejected() {
    for bad in ["notify-send -u critical", "./notify-send", "sh -c ls"] {
        let err = NotifyCommand::parse(bad).expect_err("must reject");
        assert!(
            err.contains("absolute path"),
            "{bad} should be rejected for being relative, got: {err}"
        );
    }
}

#[test]
fn shell_and_privilege_programs_are_rejected() {
    for bad in [
        "/bin/sh -c 'curl evil.example'",
        "/usr/bin/bash -c ls",
        "/bin/dash",
        "/usr/bin/zsh -c ls",
        "/usr/bin/fish",
        "/bin/busybox sh",
        "/usr/bin/sudo /usr/bin/notify-send",
        "/bin/su - root",
        "/usr/bin/doas notify-send",
        "/usr/bin/env FOO=1 /usr/bin/notify-send",
        "/usr/bin/xargs notify-send",
        "/usr/bin/systemd-run notify-send",
    ] {
        let err = NotifyCommand::parse(bad).expect_err("must reject");
        assert!(
            err.contains("may not use"),
            "{bad} must be refused as a shell/escalator, got: {err}"
        );
    }
}

#[test]
fn the_forbidden_list_matches_on_the_file_name_not_the_full_path() {
    // The same shell under different prefixes must all be caught.
    for path in [
        "/bin/sh",
        "/usr/bin/sh",
        "/usr/local/bin/bash",
        "/nix/store/x/bin/zsh",
    ] {
        let err = NotifyCommand::parse(path).expect_err("must reject");
        assert!(err.contains("may not use"), "{path} slipped through: {err}");
    }
}

#[test]
fn a_real_notifier_is_not_on_the_forbidden_list() {
    for good in [
        "/usr/bin/notify-send -u critical",
        "/run/current-system/sw/bin/notify-send",
        "/opt/scripts/my-notifier.sh --channel ops",
    ] {
        assert!(
            NotifyCommand::parse(good).is_ok(),
            "{good} is a legitimate notifier and must be accepted"
        );
    }
}

#[test]
fn an_empty_value_is_rejected() {
    assert!(NotifyCommand::parse("   ").is_err());
}

// ---------------------------------------------------------------------------
// log_dirs default vs. explicit disable
// ---------------------------------------------------------------------------

// ADDED 2026-09-29 (DECIDE #4 follow-up). The two cases must stay
// distinguishable: before, `log_dirs` was a `String` defaulting to "", so
// "the operator never mentioned it" and "the operator wrote an empty value"
// were the same thing, which is why no default could be added without
// taking away the only way to switch the check off.

#[test]
fn an_absent_log_dirs_resolves_to_the_default() {
    let raw: Option<String> = None;
    assert_eq!(
        effective_log_dirs(&raw).as_deref(),
        Some(default_log_dirs().as_str()),
        "an unset key must resolve to the default, not to 'disabled'"
    );
}

#[test]
fn an_explicitly_blank_log_dirs_stays_disabled() {
    for blank in ["", "   ", "\t"] {
        let raw = Some(blank.to_string());
        assert_eq!(
            effective_log_dirs(&raw),
            None,
            "{blank:?} is the operator's explicit off-switch and must stay honoured"
        );
    }
}

#[test]
fn a_configured_log_dirs_passes_through() {
    let raw = Some("/var/log,~/.local/state/dracon".to_string());
    assert_eq!(
        effective_log_dirs(&raw).as_deref(),
        Some("/var/log,~/.local/state/dracon")
    );
}

#[test]
fn the_default_log_dirs_is_the_guards_own_state_directory() {
    let default = default_log_dirs();
    assert_eq!(default, "~/.local/state/dracon");
    // It must live where the shipped unit grants write access, otherwise
    // auto_truncate_logs would fail EROFS exactly as /var/log does.
    let unit = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system-guard.service"),
    )
    .expect("guard unit must be readable");
    assert!(
        unit.contains("%h/.local/state/dracon"),
        "the default log dir must be inside the unit's ReadWritePaths"
    );
}

#[test]
fn reap_auto_kill_defaults_off_and_parses_opt_in() {
    // The destructive half of the reap feature must never arm by default:
    // a missing key parses to false, and only an explicit true opts in.
    assert!(!GuardPolicy::default().reap_stale_dev_servers);
    let absent: SystemPolicy =
        toml::from_str("[guard]\n").expect("policy without the knob must parse");
    assert!(!absent.guard.reap_stale_dev_servers);
    let opted_in: SystemPolicy = toml::from_str("[guard]\nreap_stale_dev_servers = true\n")
        .expect("explicit opt-in must parse");
    assert!(opted_in.guard.reap_stale_dev_servers);
    // The pressure-gated sibling defaults off with the same parsing.
    assert!(!GuardPolicy::default().reap_orphans_on_pressure);
    let opted_in: SystemPolicy = toml::from_str("[guard]\nreap_orphans_on_pressure = true\n")
        .expect("orphan opt-in must parse");
    assert!(opted_in.guard.reap_orphans_on_pressure);
}

#[test]
fn absent_and_blank_behave_differently_end_to_end_through_toml() {
    let absent: SystemPolicy =
        toml::from_str("[guard]\nmonitor_logs = true\n").expect("absent must parse");
    let blank: SystemPolicy =
        toml::from_str("[guard]\nmonitor_logs = true\nlog_dirs = \"\"\n").expect("blank parses");
    assert_eq!(absent.guard.log_dirs, None);
    assert_eq!(blank.guard.log_dirs, Some(String::new()));
    assert_eq!(
        effective_log_dirs(&absent.guard.log_dirs),
        Some(default_log_dirs())
    );
    assert_eq!(effective_log_dirs(&blank.guard.log_dirs), None);
}

// ---------------------------------------------------------------------------
// Unknown policy keys
//
// Found 2026-09-29 while implementing range enforcement: a key appended
// under the wrong table header was swallowed with no warning, so the config
// looked applied and was not. These tests pin that such keys are SURFACED.
//
// The contract is that the daemon WARNS rather than fails: a stray key must
// not stop a monitoring daemon from starting. The operator needs to be told,
// not locked out of their machine.
// ---------------------------------------------------------------------------

fn doc_of(toml_src: &str) -> toml::Value {
    toml_src.parse().expect("fixture must be valid TOML")
}

#[test]
fn a_misspelled_knob_is_surfaced() {
    let doc = doc_of("[guard]\ndisk_warn_persent = 80\n");
    assert_eq!(
        unknown_policy_keys(&doc),
        vec!["guard.disk_warn_persent".to_string()],
        "a typo must be reported, not dropped"
    );
}

#[test]
fn a_key_under_the_wrong_table_is_surfaced() {
    // The exact case this item was filed for: `cleanup_min_size_mb` is a
    // [guard] knob, but written after a [storage] header it lands in
    // [storage] and used to vanish with no warning.
    let doc = doc_of("[storage]\nmin_size_mb = 512\ncleanup_min_size_mb = 256\n");
    let unknown = unknown_policy_keys(&doc);
    assert!(
        unknown.contains(&"storage.cleanup_min_size_mb".to_string()),
        "a [guard] knob filed under [storage] must be surfaced, got {unknown:?}"
    );
    // And it must be reported as unknown even though it IS a real knob name
    // — being spelled correctly does not rescue being in the wrong table.
    assert!(!unknown.is_empty());
}

#[test]
fn an_entirely_unknown_section_is_surfaced() {
    let doc = doc_of("[guards]\ninterval_secs = 30\n");
    assert_eq!(unknown_policy_keys(&doc), vec!["guards".to_string()]);
}

#[test]
fn a_section_written_as_a_bare_key_is_surfaced() {
    // `storage = 5` is a shape error, not a valid section.
    let doc = doc_of("storage = 5\n");
    assert_eq!(unknown_policy_keys(&doc), vec!["storage".to_string()]);
}

#[test]
fn a_fully_valid_policy_reports_nothing() {
    let src = "[guard]\ninterval_secs = 30\ndisk_warn_percent = 80\n\n\
               [storage]\nmin_size_mb = 512\nkinds = \"rust-build\"\n\n\
               [[links.entries]]\nlink = \"/a\"\ntarget = \"/b\"\n";
    assert!(
        unknown_policy_keys(&doc_of(src)).is_empty(),
        "a correct policy must not produce warnings"
    );
}

#[test]
fn the_shipped_example_template_has_no_unknown_keys() {
    // Guards the derivation against drift in BOTH directions: a knob added
    // to the struct is accepted automatically, and a key in the template
    // that no longer matches the struct is reported.
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system.example.toml"),
    )
    .expect("example template must be readable");
    let unknown = unknown_policy_keys(&text.parse().expect("template must parse"));
    assert!(
        unknown.is_empty(),
        "dracon-system.example.toml contains keys the parser does not accept: {unknown:?}"
    );
}

/// The accepted-key set is derived from the structs, so a newly added knob
/// is accepted without touching a list. This proves the derivation is not
/// silently returning an empty set, which would disable the whole check.
#[test]
fn the_known_key_set_is_derived_from_the_structs() {
    let known = known_policy_keys();
    assert!(known.contains_key("guard"), "guard section must be known");
    let guard = &known["guard"];
    for knob in [
        "interval_secs",
        "disk_warn_percent",
        "log_dirs",
        "notify_command",
    ] {
        assert!(guard.contains(knob), "{knob} must be an accepted guard key");
    }
    assert!(
        known["storage"].contains("min_size_mb"),
        "storage keys must be derived too"
    );
    assert!(known["links"].contains("entries"));
    // A representative non-key must NOT be accepted, or the set is bogus.
    assert!(!guard.contains("definitely_not_a_knob"));
}

#[test]
fn a_near_miss_spelling_gets_a_suggestion() {
    assert!(hint_for("guard.disk_warn_persent").contains("disk_warn_percent"));
    assert!(hint_for("guard.nonsense_key_xyz").is_empty());
}

#[test]
fn a_correctly_spelled_key_in_the_wrong_table_names_its_table() {
    // The precise confusion this check exists to surface: the name is right,
    // the table is not, and "did you mean <the same word>" would not help.
    let hint = hint_for("storage.cleanup_min_size_mb");
    assert!(
        hint.contains("[guard]"),
        "a misplaced key must be told which table it belongs under, got {hint:?}"
    );
}

/// Tripwire for the one manual part of the key derivation.
///
/// TOML omits `Option` fields when serializing a `None`, so they must be
/// listed in `GUARD_OPTION_FIELDS`. If a new `Option` is added to
/// `GuardPolicy` and not listed there, it would be reported as an unknown
/// key on every load — a false positive that trains operators to ignore the
/// warning. This test fails in that case.
#[test]
fn option_field_keys_cover_every_option_field() {
    let guard = known_policy_keys();
    let guard = &guard["guard"];
    for field in GUARD_OPTION_FIELDS {
        assert!(
            guard.contains(*field),
            "{field} is an Option field and must be listed in GUARD_OPTION_FIELDS, \
             or it will be reported as an unknown key on every load"
        );
    }
    // A guard policy that actually sets log_dirs must not be flagged.
    let doc = doc_of("[guard]\nlog_dirs = \"/var/log\"\n");
    assert!(
        unknown_policy_keys(&doc).is_empty(),
        "a policy that sets the Option field must be clean, got {:?}",
        unknown_policy_keys(&doc)
    );
}

/// The 2026-10-01 audit: `unfreeze_below_percent` and `disk_early_warn_percent`
/// had no lower bound. A legal 0 meant "unfreeze only at 0% used" — which
/// never happens, so the sync freeze marker only ever came back via the
/// external freeze watchdog — and "warn on every single pass" respectively.
/// Every sibling percent knob is banded 1..100 for exactly that reason.
#[test]
fn percent_thresholds_keep_a_one_percent_floor() {
    let mut p = GuardPolicy {
        unfreeze_below_percent: 0,
        disk_early_warn_percent: 0,
        ..GuardPolicy::default()
    };
    let adjusted = normalize_guard_policy(&mut p);
    assert_eq!(
        p.unfreeze_below_percent, 1,
        "0% unfreeze would never lift the freeze; adjusted={adjusted:?}"
    );
    assert_eq!(
        p.disk_early_warn_percent, 1,
        "a 0% early warn fires on every pass; adjusted={adjusted:?}"
    );
    assert!(adjusted.contains(&"unfreeze_below_percent"));
    assert!(adjusted.contains(&"disk_early_warn_percent"));

    // The relative clamp must not push either knob back down to 0.
    let mut pathological = GuardPolicy {
        disk_warn_percent: 1,
        disk_early_warn_percent: 1,
        disk_action_percent: 1,
        disk_critical_percent: 1,
        unfreeze_below_percent: 1,
        ..GuardPolicy::default()
    };
    normalize_guard_policy(&mut pathological);
    assert!(
        pathological.unfreeze_below_percent >= 1,
        "got {}",
        pathological.unfreeze_below_percent
    );
    assert!(
        pathological.proactive_cleanup_percent >= 1,
        "got {}",
        pathological.proactive_cleanup_percent
    );
}

/// The 2026-10-01 audit: `notify_cooldown_secs` had a floor but no ceiling.
/// A huge value reached `cleanup_stale_cooldowns`, which subtracted 2x it from
/// `Instant::now()` — a subtraction std PANICS on once the duration underflows
/// the clock (>= 2^63 seconds), killing the daemon on its first pass. Below
/// that it silently disabled cooldown pruning entirely.
#[test]
fn notify_cooldown_has_a_ceiling() {
    let mut p = GuardPolicy {
        notify_cooldown_secs: u64::MAX,
        ..GuardPolicy::default()
    };
    let adjusted = normalize_guard_policy(&mut p);
    assert!(
        p.notify_cooldown_secs <= 86_400,
        "notify_cooldown_secs must be capped, got {}",
        p.notify_cooldown_secs
    );
    assert!(adjusted.contains(&"notify_cooldown_secs"));
}

/// 2026-10-01 (audit): the unknown-key check stopped at depth 1, so a typo
/// inside a `[[links.entries]]` body was accepted SILENTLY while the identical
/// typo one level up was reported. The template promises an unrecognised key is
/// at least named.
#[test]
fn unknown_keys_inside_link_entries_are_reported() {
    let doc: toml::Value = r#"
[links]
[[links.entries]]
link = "/a"
target = "/b"
targt = "/c"
"#
    .parse()
    .expect("fixture parses");
    let unknown = unknown_policy_keys(&doc);
    assert!(
        unknown.contains(&"links.entries.targt".to_string()),
        "a typo inside a link entry must be named, got {unknown:?}"
    );

    // The accepted shape stays silent.
    let clean: toml::Value = r#"
[links]
[[links.entries]]
link = "/a"
target = "/b"
"#
    .parse()
    .expect("fixture parses");
    assert!(
        unknown_policy_keys(&clean).is_empty(),
        "a valid link entry must not be reported: {:?}",
        unknown_policy_keys(&clean)
    );
}

/// 2026-10-01 (audit): a mistyped SECTION name got no hint at all, because only
/// key names were candidates. `guards` for `[guard]` is the most common config
/// typo and the nearest key is 4 edits away.
#[test]
fn a_mistyped_section_name_is_hinted() {
    let hint = hint_for("guards");
    assert!(
        hint.contains("[guard]"),
        "a mistyped section must be pointed at the real one, got {hint:?}"
    );
}

/// 2026-10-01 (audit): the near-miss hint crossed a semantic family. The
/// rust-target gates are `rust_target_max_age_days` (what counts as a target)
/// and `rust_target_action_min_age_days` (what to clean) — two different policy
/// decisions with confusable names, and plain edit distance pointed a mistyped
/// action-tier key at the max gate. There is no `rust_target_min_age_days` knob
/// at all, so that spelling must not be quietly mapped onto one.
#[test]
fn a_near_miss_hint_stays_inside_the_typed_family() {
    // A typo of the action-tier knob suggests the action-tier knob.
    let hint = hint_for("rust_target_action_min_age_day");
    assert!(
        hint.contains("rust_target_action_min_age_days"),
        "the same-tier key must be suggested, got {hint:?}"
    );
    assert!(
        !hint.contains("rust_target_max_age_days"),
        "the max-age gate is a DIFFERENT setting and must not be suggested here: {hint:?}"
    );

    // A name in the family that does not exist is not silently mapped onto a
    // sibling: the family is listed instead, so the operator picks.
    let unknown_tier = hint_for("rust_target_min_age_days");
    assert!(
        !unknown_tier.contains("did you mean rust_target_max_age_days"),
        "a non-existent tier must not be presented as the max-age knob: {unknown_tier:?}"
    );
    assert!(
        unknown_tier.contains("rust_target_max_age_days")
            && unknown_tier.contains("rust_target_action_min_age_days"),
        "both real family members should be offered: {unknown_tier:?}"
    );

    // Across families, plain near-miss still works.
    let unrelated = hint_for("interval_sec");
    assert!(
        unrelated.contains("interval_secs"),
        "an unrelated typo must still get a plain near miss, got {unrelated:?}"
    );
}

// ---------------------------------------------------------------------------
// Storage roots vs the shipped unit's hardening (2026-10-01)
// ---------------------------------------------------------------------------

/// The `ReadWritePaths` entries of a unit, as raw text, for mutation testing.
fn shipped_unit() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system-guard.service"),
    )
    .expect("guard unit must be readable")
}

/// The example config teaches operators these two storage paths; if the shipped
/// unit cannot write to them, every quarantine move and every expiry fails
/// EROFS under `ProtectSystem=strict`.
///
/// This is the same regression the default log dir has been pinned against
/// (`the_default_log_dirs_is_the_guards_own_state_directory`): a path the docs
/// recommend must be a path the unit grants. It FAILS on the unit as it stood
/// before 2026-10-01, which is the point — `/mnt/data/cold` was documented and
/// unreachable.
#[test]
fn the_example_configs_storage_roots_are_inside_the_units_readwritepaths() {
    let example = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dracon-system.example.toml"),
    )
    .expect("example config must be readable");
    let unit = shipped_unit();
    let home = std::path::Path::new("/home/tester");

    let mut checked = 0;
    for line in example.lines() {
        let line = line.trim_start_matches('#').trim();
        let Some(rest) = line
            .strip_prefix("quarantine_dir")
            .or_else(|| line.strip_prefix("relocate_cold_root"))
        else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        // An empty value means the feature is off; nothing will be written.
        if value.is_empty() {
            continue;
        }
        // Inline tilde expansion against a FIXED home: the module-private
        // helper is not reachable from here, and the test needs a home it
        // controls so the assertion does not depend on who runs it.
        let path = match value.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => std::path::PathBuf::from(value),
        };
        assert!(
            crate::safety::unit_grants_write(&unit, home, &path),
            "the example config documents {value} ({}), but the shipped unit's \
             ReadWritePaths does not grant it — writes there fail EROFS under \
             ProtectSystem=strict",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected both documented storage roots to be checked, saw {checked}"
    );
}

/// A root the operator repointed outside `ReadWritePaths` must be REPORTED,
/// not merely tolerated — and reporting must actually stop when the entry is
/// present. The stripped-unit half is the mutation proof: with the entry
/// removed, the check catches it.
#[test]
fn a_repointed_storage_root_outside_readwritepaths_is_reported() {
    let guard = GuardPolicy {
        relocate_cold_root: "/mnt/data/cold".to_string(),
        ..Default::default()
    };
    // The shipped unit grants the documented roots: nothing to report.
    assert!(
        crate::safety::uncovered_storage_roots(&shipped_unit(), &guard).is_empty(),
        "the shipped unit grants every configured storage root"
    );

    // Strip the entry (the state before this fix) and the root must surface.
    let stripped = shipped_unit().replace(" -/mnt/data/quarantine -/mnt/data/cold", "");
    assert!(
        !stripped.contains("/mnt/data/cold"),
        "the mutation must actually remove the entry"
    );
    let reported = crate::safety::uncovered_storage_roots(&stripped, &guard);
    assert_eq!(
        reported.len(),
        1,
        "expected exactly the cold root to be reported, got {reported:?}"
    );
    assert_eq!(reported[0].0, "relocate_cold_root");
    assert_eq!(reported[0].1, std::path::Path::new("/mnt/data/cold"));
}

/// Audit R4-SYS-06: the guard's OTHER write roots — event log, truncated
/// log dirs, freeze marker — must be covered by the same startup check.
/// A repointed one outside ReadWritePaths failed EROFS every pass with
/// no warning; disabled ones (blank log file / blank log_dirs) stay silent.
#[test]
fn repointed_log_and_freeze_roots_outside_readwritepaths_are_reported() {
    let guard = GuardPolicy {
        guard_log_file: "/mnt/data/logs/guard.log".to_string(),
        log_dirs: Some("/mnt/data/logs, ~/.local/state/dracon".to_string()),
        sync_freeze_marker: "/mnt/data/freeze/marker".to_string(),
        ..Default::default()
    };
    let reported = crate::safety::uncovered_storage_roots(&shipped_unit(), &guard);
    let keys: Vec<&str> = reported.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        vec!["guard_log_file", "log_dirs", "sync_freeze_marker"],
        "all three repointed write roots must surface, got {reported:?}"
    );
    // The second log dir is under a granted root — only the repointed
    // entry is reported, not the whole key.
    assert_eq!(
        reported[1].1,
        std::path::Path::new("/mnt/data/logs"),
        "only the uncovered log dir entry must surface, got {reported:?}"
    );

    // Disabled stays silent: blank log file, blank log_dirs, and the
    // default (covered) marker.
    let quiet = GuardPolicy {
        guard_log_file: String::new(),
        log_dirs: Some("   ".to_string()),
        ..Default::default()
    };
    assert!(
        crate::safety::uncovered_storage_roots(&shipped_unit(), &quiet).is_empty(),
        "disabled log roots must not warn"
    );
}

/// Parsing semantics, pinned directly: `%h` expands, the `-` "ignore if missing"
/// prefix does not narrow what is granted (on a host where the path EXISTS it
/// is writable), repeated directives accumulate, and a path nobody listed is
/// not covered.
#[test]
fn unit_readwrite_paths_expands_home_and_keeps_optional_entries() {
    let unit = "\
ReadWritePaths=%h/.dracon /tmp
ReadWritePaths=-%h/Dev -/mnt/data/quarantine
";
    let home = std::path::Path::new("/home/tester");
    let granted = crate::safety::unit_readwrite_paths(unit, home);
    assert!(
        granted.contains(&"/home/tester/.dracon".into()),
        "{granted:?}"
    );
    assert!(granted.contains(&"/home/tester/Dev".into()), "{granted:?}");
    assert!(
        granted.contains(&"/mnt/data/quarantine".into()),
        "a '-' prefix means 'ignore if missing', not 'never granted': {granted:?}"
    );
    assert!(granted.contains(&"/tmp".into()), "{granted:?}");

    assert!(crate::safety::unit_grants_write(
        unit,
        home,
        std::path::Path::new("/home/tester/.dracon")
    ));
    assert!(crate::safety::unit_grants_write(
        unit,
        home,
        std::path::Path::new("/home/tester/Dev/a-repo/target")
    ));
    assert!(crate::safety::unit_grants_write(
        unit,
        home,
        std::path::Path::new("/tmp/nested/file")
    ));
    assert!(
        !crate::safety::unit_grants_write(unit, home, std::path::Path::new("/mnt/data/cold")),
        "an unlisted path is not covered"
    );
    assert!(
        !crate::safety::unit_grants_write(
            unit,
            home,
            std::path::Path::new("/home/tester/.local/state/dracon")
        ),
        "a sibling of a granted path is not covered"
    );
}

/// 2026-10-03 (audit R4-SYS-14): daemon/checker parity — a symlinked
/// storage root must match the grant on its canonical target (the
/// checker has matched canonical since R3-L23; the daemon warned
/// while the checker passed). Missing paths keep the literal
/// fallback, and relative paths are never CWD-resolved.
#[cfg(unix)]
#[test]
fn unit_grants_write_matches_symlinked_root_to_target_grant() {
    let base = std::env::temp_dir().join(format!("dracon-sys14-{}", std::process::id()));
    let real = base.join("real-cold");
    let link = base.join("cold-link");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let unit = format!("ReadWritePaths={}\n", real.display());
    let home = std::path::Path::new("/home/tester");
    assert!(
        crate::safety::unit_grants_write(&unit, home, &link),
        "a symlinked root must match the grant on its target"
    );
    // Literal fallback: a missing path compares literally, so an
    // exact (nonexistent) grant still covers.
    let missing = base.join("not-created-yet");
    let unit_missing = format!("ReadWritePaths={}\n", missing.display());
    assert!(
        crate::safety::unit_grants_write(&unit_missing, home, &missing),
        "missing paths must keep the literal fallback"
    );
    // Relative paths stay literal: Cargo.toml exists under the test
    // CWD, but resolving it there would anchor the verdict to the
    // invoker's CWD instead of the service's WorkingDirectory.
    let cwd_manifest = std::env::current_dir().unwrap().join("Cargo.toml");
    assert!(
        cwd_manifest.exists(),
        "test premise: cargo test runs in the package root"
    );
    let unit_abs = format!("ReadWritePaths={}\n", cwd_manifest.display());
    assert!(
        !crate::safety::unit_grants_write(&unit_abs, home, std::path::Path::new("Cargo.toml")),
        "relative roots must not be resolved against the invoker CWD"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// protected_paths `~` expansion (2026-10-02, audit HIGH)
// ---------------------------------------------------------------------------

/// The auditor's finding, restated as a test: a `~` in `protected_paths` parsed
/// fine, was reported as configured, and protected NOTHING — `canonicalize`
/// resolves `~` against the process CWD, fails with NotFound, and the failure
/// was skipped silently. The absolute-path test stayed green the whole time.
///
/// This drives the real path — parse TOML, normalize, then ask the actual
/// safety classifier — so the config can no longer be inert while a test that
/// hardcodes an absolute path still passes.
#[test]
fn protected_paths_tilde_entry_actually_protects() {
    let root = std::env::temp_dir().join(format!(
        "dracon-protected-tilde-{}-{}",
        std::process::id(),
        "tilde"
    ));
    let _ = std::fs::remove_dir_all(&root);
    let fake_home = root.join("home");

    // The tree that must survive, and a sibling that must not be affected.
    let protected_tree = fake_home.join("Dev/dracon-utilities/target");
    std::fs::create_dir_all(&protected_tree).unwrap();
    std::fs::write(protected_tree.join("a.txt"), b"keep").unwrap();
    let other_tree = fake_home.join("Dev/some-other-repo/target");
    std::fs::create_dir_all(&other_tree).unwrap();
    std::fs::write(other_tree.join("b.txt"), b"reclaimable").unwrap();

    let toml_src = r#"
[guard]
protected_paths = ["~/Dev/dracon-utilities"]
"#;
    let mut parsed: SystemPolicy = toml::from_str(toml_src).expect("fixture must parse");
    assert_eq!(
        parsed.guard.protected_paths,
        vec!["~/Dev/dracon-utilities".to_string()],
        "precondition: the raw policy really does carry the tilde form"
    );

    normalize_guard_policy_with_home(&mut parsed.guard, Some(&fake_home));
    assert_eq!(
        parsed.guard.protected_paths,
        vec![fake_home.join("Dev/dracon-utilities").display().to_string()],
        "the tilde form must be expanded to an absolute path"
    );
    // Expansion must NOT be reported as a clamped out-of-range value: the knob
    // was perfectly legal, and warning about it on every load would train the
    // operator to ignore policy warnings.
    assert!(
        !normalize_guard_policy_with_home(&mut parsed.guard, Some(&fake_home))
            .contains(&"protected_paths"),
        "a legal tilde form must not be reported as clamped"
    );

    // The real classifier must now refuse the protected tree…
    let err = crate::check_safe_to_delete_guard(&protected_tree, &parsed.guard.protected_paths)
        .expect_err("a ~ entry must protect, not silently no-op");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("refusing to delete protected path"),
        "expected a protection refusal, got: {msg}"
    );

    // …and must NOT over-protect an unrelated sibling.
    assert!(
        crate::check_safe_to_delete_guard(&other_tree, &parsed.guard.protected_paths).is_ok(),
        "protection must stay scoped to the configured ancestor"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Idempotence: normalizing twice must be a no-op, and an already-absolute entry
/// must survive byte-identical.
#[test]
fn protected_paths_expansion_is_idempotent() {
    let home = Path::new("/tmp/fake-home-for-idempotence");
    let mut p = GuardPolicy {
        protected_paths: vec![
            "~/Dev/dracon-utilities".to_string(),
            "/already/absolute".to_string(),
        ],
        ..Default::default()
    };
    normalize_guard_policy_with_home(&mut p, Some(home));
    let snapshot = p.protected_paths.clone();
    let second = normalize_guard_policy_with_home(&mut p, Some(home));
    assert!(
        !second.contains(&"protected_paths"),
        "a second pass must find nothing to adjust"
    );
    assert_eq!(p.protected_paths, snapshot);
    assert_eq!(
        p.protected_paths[1], "/already/absolute",
        "an absolute entry must not be rewritten"
    );
}

/// An unresolvable `protected_paths` entry must warn once per entry, not once
/// per candidate — otherwise a config typo floods every 30-second pass and the
/// one line that matters goes unread. And it must fail OPEN: a typo must not
/// make every cleanup candidate refuse, which would turn a typo into a disk
/// that fills and never reclaims.
///
/// Each classifier gets its OWN entry. Sharing one across all three checks made
/// the later assertions vacuous: the first call registered the string, so the
/// classifier arms could have been silent and the test still passed. That is
/// the same class of vacuous test the tilde bug produced.
#[test]
fn unresolvable_protected_entry_warns_once_and_does_not_refuse() {
    let pid = std::process::id();
    let missing = |which: &str| format!("/tmp/dracon-missing-protected-{pid}-{which}");
    let probe = |which: &str| {
        // /tmp-pinned: the candidate is handed to the guard classifier, which
        // validates candidates against the `/tmp`-rooted policy. `$TMPDIR`
        // is `/build` in the nix sandbox, so honoring it failed the test
        // there for reasons unrelated to the behavior under test.
        let p = std::path::Path::new("/tmp")
            .join(format!("dracon-failopen-probe-{pid}-{which}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    };

    // 1. The helper itself: once per entry, not once per call.
    let a = missing("helper");
    assert!(
        !std::path::Path::new(&a).exists(),
        "precondition: the entry must not resolve"
    );
    assert!(
        crate::safety::note_unresolvable_protected(&a),
        "first sighting warns"
    );
    assert!(
        !crate::safety::note_unresolvable_protected(&a),
        "a second sighting must be silent"
    );

    // 2. The GUARD classifier registers an unresolvable entry — its arm really
    //    calls the helper. Fresh entry, so nothing pre-registered it.
    let b = missing("guard-arm");
    let cand_b = probe("guard-arm");
    assert!(
        crate::check_safe_to_delete_guard(&cand_b, std::slice::from_ref(&b)).is_ok(),
        "a typo in protected_paths must not block all cleanup"
    );
    assert!(
        !crate::safety::note_unresolvable_protected(&b),
        "the guard classifier must register an unresolvable protected entry"
    );
    let _ = std::fs::remove_dir_all(&cand_b);

    // 3. The STRICT classifier (`check_safe_to_delete`, used by `link apply`)
    //    had the identical silent skip and was found while proving the first
    //    fix. The helper returning false afterwards is the observable proof
    //    that it called the helper too, since the warning text lives in an
    //    eprintln this binary cannot capture.
    let c = missing("strict-arm");
    let cand_c = probe("strict-arm");
    let _ = crate::check_safe_to_delete(&cand_c, std::slice::from_ref(&c));
    assert!(
        !crate::safety::note_unresolvable_protected(&c),
        "the strict classifier must register an unresolvable protected entry"
    );
    let _ = std::fs::remove_dir_all(&cand_c);
}
