//! System diagnostics — deterministic health checks for canonical dracon setup.

use anyhow::Result;
use std::path::PathBuf;

use crate::{canonical_system_root, is_user_service_active};

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

    crate::DoctorReport {
        system_root_exists: root.exists(),
        nixos_root_exists: nixos.exists(),
        canonical_libs_exists: libs.exists(),
        canonical_utils_exists: utils.exists(),
        sync_policy_exists: policy.exists(),
        legacy_config_dracon_exists: legacy_cfg.exists(),
        sync_service_active: is_user_service_active("dracon-sync.service").await,
    }
}

/// Handle the `doctor` CLI subcommand.
pub(crate) async fn cmd_doctor(json: bool, strict: bool) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

    let report = build_doctor_report().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        if strict {
            // In strict mode, fail if any check fails
            if !report.all_ok() {
                std::process::exit(1);
            }
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
        ]);

    // (label, ok, remediation hint when failing)
    let checks: Vec<(&str, bool, &str)> = vec![
        (
            "~/.dracon/nixos",
            report.nixos_root_exists,
            "Clone or symlink your NixOS config under ~/.dracon/nixos",
        ),
        (
            "dracon-libs (dev sibling)",
            report.canonical_libs_exists,
            "Optional for installed binaries. Required only for `cargo build` from source: git clone https://github.com/DraconDev/dracon-libs.git ../dracon-libs",
        ),
        (
            "dracon-utilities (self)",
            report.canonical_utils_exists,
            "This binary should live at ~/Dev/dracon-utilities (or its install.sh target)",
        ),
        (
            "sync policy",
            report.sync_policy_exists,
            "Copy dracon-sync.example.toml to ~/.dracon/utilities/sync/dracon-sync.toml",
        ),
        (
            "legacy config absent",
            !report.legacy_config_dracon_exists,
            "Move or remove the legacy ~/dracon configuration",
        ),
        (
            "sync service",
            report.sync_service_active,
            "systemctl --user enable --now dracon-sync.service",
        ),
    ];

    let mut has_failures = false;
    let mut remediation_lines: Vec<String> = Vec::new();
    for (label, ok, hint) in &checks {
        let (icon, color) = if *ok {
            ("\u{2705}", Color::Green)
        } else {
            ("\u{274c}", Color::Red)
        };
        if !ok {
            has_failures = true;
            remediation_lines.push(format!("  \u{274c} {}: {}", label, hint));
        }
        table.add_row(vec![
            Cell::new(icon).fg(color),
            Cell::new(*label),
            Cell::new(if *ok { "ok" } else { "present" }),
        ]);
    }

    println!("{table}");
    if has_failures {
        eprintln!();
        eprintln!("\u{26a0}\u{fe0f}  Some checks failed. Remediation:");
        for line in &remediation_lines {
            eprintln!("{line}");
        }
        eprintln!();
        eprintln!("Run with --json for machine-readable details.");
        if strict {
            std::process::exit(1);
        }
    } else {
        eprintln!("\u{2705}  All checks passed.");
    }
    Ok(())
}
