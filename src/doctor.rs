//! System diagnostics — deterministic health checks for canonical dracon setup.

use anyhow::Result;
use std::path::PathBuf;

use crate::{
    canonical_system_root, effective_system_policy_path, is_user_service_active, resolve_bin_strict,
};

/// The service this repo ships, and the one a `dracon-system` installation
/// actually depends on.
const GUARD_SERVICE: &str = "dracon-system-guard.service";
const SYNC_SERVICE: &str = "dracon-sync.service";
// ADDED 2026-10-03 (audit R3-L26): the M8 watchdog backstops.
const SYNC_WATCHDOG_TIMER: &str = "dracon-sync-watchdog.timer";
const FREEZE_WATCHDOG_TIMER: &str = "dracon-freeze-watchdog.timer";
const GUARD_WATCHDOG_TIMER: &str = "dracon-system-guard-watchdog.timer";

/// Run the diagnostic check and return a report.
pub(crate) async fn build_doctor_report() -> crate::DoctorReport {
    let root = canonical_system_root();
    let nixos = root.join("nixos");
    let libs = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join("Dev/dracon-libs");
    let utils = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join("Dev/dracon-utilities");
    let policy = root.join("utilities/sync/dracon-sync.toml");
    let legacy_cfg = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join(".config/dracon");

    // ADDED 2026-10-01 (audit): the guard's own policy, resolved the same way
    // the daemon resolves it, so a DRACON_SYSTEM_POLICY override is honoured
    // instead of being reported as "no policy".
    let system_policy = effective_system_policy_path().unwrap_or_else(|_| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/home"))
            .join(".dracon/utilities/system/dracon-system.toml")
    });

    // "Cannot ask" is not "the answer is no": without this the two service
    // checks reported `false` on a host with no systemctl at all, and the
    // remediation told the operator to run systemctl.
    let service_probe_available = resolve_bin_strict("systemctl").is_ok();

    crate::DoctorReport {
        system_root_exists: root.exists(),
        nixos_root_exists: nixos.exists(),
        canonical_libs_exists: libs.exists(),
        canonical_utils_exists: utils.exists(),
        sync_policy_exists: policy.exists(),
        legacy_config_dracon_exists: legacy_cfg.exists(),
        sync_service_active: is_user_service_active(SYNC_SERVICE).await,
        system_policy_exists: system_policy.exists(),
        guard_service_active: is_user_service_active(GUARD_SERVICE).await,
        sync_watchdog_timer_active: is_user_service_active(SYNC_WATCHDOG_TIMER).await,
        freeze_watchdog_timer_active: is_user_service_active(FREEZE_WATCHDOG_TIMER).await,
        guard_watchdog_timer_active: is_user_service_active(GUARD_WATCHDOG_TIMER).await,
        service_probe_available,
    }
}

/// Outcome of one check. `Skipped` is "this host cannot answer the question" —
/// it is neither a pass nor a failure, and it never fails `--strict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckState {
    Ok,
    Fail,
    Skipped,
}

/// One diagnostic row.
///
/// `required` is what `--strict` counts. ADDED 2026-10-01 (audit): strict mode
/// used a second, hand-maintained field list in main.rs which counted
/// `canonical_libs_exists` — whose own remediation text calls it "Optional for
/// installed binaries" — so `doctor --strict` could never pass on a host
/// installed from crates.io. The check table is now the single source of truth
/// for what strict means, and the field list is gone.
#[derive(Debug)]
struct DoctorCheck {
    label: &'static str,
    state: CheckState,
    required: bool,
    hint: &'static str,
}

fn doctor_checks(report: &crate::DoctorReport) -> Vec<DoctorCheck> {
    let yes = CheckState::Ok;
    let no = CheckState::Fail;
    // Service checks are unanswerable when systemctl is absent.
    let probe = report.service_probe_available;
    let service_state = |active: bool| {
        if probe {
            if active {
                yes
            } else {
                no
            }
        } else {
            CheckState::Skipped
        }
    };

    vec![
        DoctorCheck {
            label: "~/.dracon",
            state: if report.system_root_exists { yes } else { no },
            required: true,
            hint: "Create the canonical system root at ~/.dracon",
        },
        DoctorCheck {
            label: "~/.dracon/nixos",
            state: if report.nixos_root_exists { yes } else { no },
            required: true,
            hint: "Clone or symlink your NixOS config under ~/.dracon/nixos",
        },
        DoctorCheck {
            label: "dracon-libs (dev sibling)",
            state: if report.canonical_libs_exists { yes } else { no },
            // Optional for an installed binary — it is only needed to build
            // from source, as its own hint says. NOT counted by --strict.
            required: false,
            hint: "Optional for installed binaries. Required only for `cargo build` from source: git clone https://github.com/DraconDev/dracon-libs.git ../dracon-libs",
        },
        DoctorCheck {
            label: "dracon-utilities (self)",
            state: if report.canonical_utils_exists { yes } else { no },
            required: true,
            hint: "This binary should live at ~/Dev/dracon-utilities (or its install.sh target)",
        },
        DoctorCheck {
            label: "sync policy",
            state: if report.sync_policy_exists { yes } else { no },
            required: true,
            hint: "Copy dracon-sync.example.toml to ~/.dracon/utilities/sync/dracon-sync.toml",
        },
        DoctorCheck {
            // The guard's own policy — the setting this binary reads. Missing
            // means the guard runs on built-in defaults with no disk thresholds
            // the operator wrote, and nothing used to say so.
            label: "system policy (guard)",
            state: if report.system_policy_exists { yes } else { no },
            required: true,
            hint: "Copy dracon-system.example.toml to ~/.dracon/utilities/system/dracon-system.toml (or set DRACON_SYSTEM_POLICY)",
        },
        DoctorCheck {
            label: "guard service",
            state: service_state(report.guard_service_active),
            required: true,
            hint: "systemctl --user enable --now dracon-system-guard.service",
        },
        DoctorCheck {
            label: "legacy config absent",
            state: if report.legacy_config_dracon_exists {
                no
            } else {
                yes
            },
            required: true,
            hint: "Move or remove the legacy ~/dracon configuration",
        },
        DoctorCheck {
            label: "sync service",
            state: service_state(report.sync_service_active),
            required: true,
            hint: "systemctl --user enable --now dracon-sync.service",
        },
        // ADDED 2026-10-03 (audit R3-L26): the M8 backstops. A missing
        // timer fails --strict (install.sh and the flake module both
        // enable them); without systemctl they are n/a like services.
        DoctorCheck {
            label: "sync watchdog timer",
            state: service_state(report.sync_watchdog_timer_active),
            required: true,
            hint: "systemctl --user enable --now dracon-sync-watchdog.timer",
        },
        DoctorCheck {
            label: "freeze watchdog timer",
            state: service_state(report.freeze_watchdog_timer_active),
            required: true,
            hint: "systemctl --user enable --now dracon-freeze-watchdog.timer",
        },
        DoctorCheck {
            label: "guard watchdog timer",
            state: service_state(report.guard_watchdog_timer_active),
            required: true,
            hint: "systemctl --user enable --now dracon-system-guard-watchdog.timer",
        },
    ]
}

/// What `--strict` exits non-zero on: any REQUIRED check that failed. Skipped
/// and optional checks never fail it.
fn strict_ok(checks: &[DoctorCheck]) -> bool {
    !checks
        .iter()
        .any(|c| c.required && c.state == CheckState::Fail)
}

fn doctor_status(state: CheckState) -> &'static str {
    match state {
        CheckState::Ok => "ok",
        CheckState::Fail => "fail",
        // "n/a", not "fail": a host without systemd cannot be told its service
        // is down.
        CheckState::Skipped => "n/a",
    }
}

/// Handle the `doctor` CLI subcommand.
pub(crate) async fn cmd_doctor(json: bool, strict: bool) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

    let report = build_doctor_report().await;
    let checks = doctor_checks(&report);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        if strict && !strict_ok(&checks) {
            std::process::exit(1);
        }
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new(" "),
            Cell::new("CHECK"),
            Cell::new("STATUS"),
            Cell::new("REQUIRED"),
        ]);

    let mut failures: Vec<&DoctorCheck> = Vec::new();
    for check in &checks {
        let (icon, color) = match check.state {
            CheckState::Ok => ("\u{2705}", Color::Green),
            CheckState::Fail => ("\u{274c}", Color::Red),
            CheckState::Skipped => ("\u{2013}", Color::DarkGrey),
        };
        if check.required && check.state == CheckState::Fail {
            failures.push(check);
        }
        table.add_row(vec![
            Cell::new(icon).fg(color),
            Cell::new(check.label),
            Cell::new(doctor_status(check.state)),
            Cell::new(if check.required { "yes" } else { "no" }),
        ]);
    }

    println!("{table}");
    if failures.is_empty() {
        eprintln!("\u{2705}  All required checks passed.");
    } else {
        eprintln!();
        eprintln!("\u{26a0}\u{fe0f}  Some required checks failed. Remediation:");
        for check in &failures {
            eprintln!("  \u{274c} {}: {}", check.label, check.hint);
        }
        for check in &checks {
            if !check.required && check.state == CheckState::Fail {
                eprintln!(
                    "  \u{2139}\u{fe0f} {} (optional): {}",
                    check.label, check.hint
                );
            }
        }
        eprintln!();
        eprintln!("Run with --json for machine-readable details.");
        if strict {
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DoctorReport;

    fn report() -> DoctorReport {
        DoctorReport {
            system_root_exists: true,
            nixos_root_exists: true,
            canonical_libs_exists: true,
            canonical_utils_exists: true,
            sync_policy_exists: true,
            legacy_config_dracon_exists: false,
            sync_service_active: true,
            system_policy_exists: true,
            guard_service_active: true,
            service_probe_available: true,
        }
    }

    #[test]
    fn doctor_checks_show_system_root_and_failure_status() {
        let mut r = report();
        r.system_root_exists = false;
        let checks = doctor_checks(&r);
        let root_check = checks
            .iter()
            .find(|c| c.label == "~/.dracon")
            .expect("system root check should be displayed");
        assert_eq!(root_check.state, CheckState::Fail);
        assert_eq!(doctor_status(CheckState::Fail), "fail");
        assert_eq!(doctor_status(CheckState::Ok), "ok");
    }

    /// 2026-10-01 (audit): the guard's own policy and service are the things
    /// THIS binary is responsible for, and `doctor` used to check neither.
    #[test]
    fn doctor_covers_the_guard_itself() {
        let checks = doctor_checks(&report());
        for expected in ["system policy (guard)", "guard service"] {
            assert!(
                checks.iter().any(|c| c.label == expected),
                "doctor must check '{expected}' — it is the guard utility's own diagnostic"
            );
        }
    }

    /// 2026-10-01 (audit): `doctor --strict` counted `canonical_libs_exists`,
    /// whose own hint says "Optional for installed binaries", so strict mode
    /// could never pass on a host installed from crates.io.
    #[test]
    fn strict_ignores_optional_checks() {
        let mut r = report();
        r.canonical_libs_exists = false; // optional, missing
        let checks = doctor_checks(&r);
        assert!(
            strict_ok(&checks),
            "a missing optional sibling must not fail --strict: {checks:?}"
        );

        // A missing REQUIRED check still fails it.
        let mut r = report();
        r.system_policy_exists = false;
        assert!(
            !strict_ok(&doctor_checks(&r)),
            "a missing system policy must fail --strict"
        );
    }

    /// 2026-10-01 (audit): without systemctl, "cannot ask" was reported as
    /// "the service is down" plus `systemctl --user enable` advice.
    #[test]
    fn an_absent_systemctl_is_skipped_not_failed() {
        let mut r = report();
        r.service_probe_available = false;
        r.sync_service_active = false;
        r.guard_service_active = false;
        let checks = doctor_checks(&r);
        for label in ["guard service", "sync service"] {
            let c = checks.iter().find(|c| c.label == label).expect(label);
            assert_eq!(
                c.state,
                CheckState::Skipped,
                "'{label}' must be n/a when systemctl is absent, not fail"
            );
        }
        assert!(
            strict_ok(&checks),
            "an unanswerable service check must not fail --strict"
        );
        assert_eq!(doctor_status(CheckState::Skipped), "n/a");
    }
}
