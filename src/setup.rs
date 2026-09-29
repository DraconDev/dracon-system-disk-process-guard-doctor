//! Setup readiness — the seed of multi-user onboarding.
//!
//! ADDED 2026-09-27 (space tiers Phase 2): space-tier automation needs
//! per-machine configuration (a cold root on a second disk, a quarantine
//! dir, candidate roots). Nothing is assumed: `dracon-system setup` discovers
//! mounts and reports what is configured, and `setup --apply` creates the
//! missing cold/quarantine directories. Future work grows this into a full
//! guided setup; the readiness contract starts here.

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::Path;

use crate::{expand_tilde, human_bytes, load_system_policy, parse_df_details};

#[derive(Debug, Serialize)]
pub(crate) struct SetupCheck {
    pub(crate) name: String,
    pub(crate) ok: bool,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SetupReport {
    pub(crate) policy_path: String,
    pub(crate) policy_exists: bool,
    pub(crate) checks: Vec<SetupCheck>,
    pub(crate) ready: bool,
}

fn df_avail(path: &Path) -> Option<(u8, u64)> {
    let out = std::process::Command::new("df")
        .args(["-P"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df_details(&String::from_utf8_lossy(&out.stdout)).map(|d| (d.use_percent, d.avail_bytes))
}

/// Ensure `dir` exists and is writable (creates it, including parents).
/// Returns a human detail string; Err only on failure.
///
/// Refuses symlinked paths outright (create_dir_all would build into the
/// link target) and validates the nearest existing ancestor against the
/// protected lists before creating anything new underneath it.
pub(crate) fn ensure_setup_dir(dir: &Path, user_protected: &[String]) -> Result<String> {
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            anyhow::bail!("refusing to set up symlinked dir {}", dir.display());
        }
    }
    let anchor = dir
        .ancestors()
        .find(|a| !a.as_os_str().is_empty() && a.exists())
        .unwrap_or(Path::new("/"));
    // Reuse the delete-guard as a manage-guard: a path it refuses to
    // delete is not a path to build managed roots under either.
    crate::check_safe_to_delete_guard(anchor, user_protected)?;
    fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {}", dir.display(), e))?;
    let probe = dir.join(".dracon-system-write-test");
    fs::write(&probe, b"ok")
        .map_err(|e| anyhow::anyhow!("{} not writable: {}", dir.display(), e))?;
    let _ = fs::remove_file(&probe);
    Ok(match df_avail(dir) {
        Some((used, avail)) => format!("{} free, {}% used", human_bytes(avail), used),
        None => "writable".to_string(),
    })
}

fn check_dir_configured(raw: &str, what: &str) -> SetupCheck {
    if raw.trim().is_empty() {
        return SetupCheck {
            name: what.to_string(),
            ok: false,
            detail: "not configured".to_string(),
        };
    }
    let dir = expand_tilde(raw.trim());
    if !dir.is_dir() {
        return SetupCheck {
            name: what.to_string(),
            ok: false,
            detail: format!("{} missing (run `setup --apply`)", dir.display()),
        };
    }
    match df_avail(&dir) {
        Some((used, avail)) => SetupCheck {
            name: what.to_string(),
            ok: true,
            detail: format!(
                "{} ({} free, {}% used)",
                dir.display(),
                human_bytes(avail),
                used
            ),
        },
        None => SetupCheck {
            name: what.to_string(),
            ok: true,
            detail: dir.display().to_string(),
        },
    }
}

pub(crate) fn collect_setup_report() -> SetupReport {
    let (path, policy) = load_system_policy().unwrap_or((None, crate::SystemPolicy::default()));
    let guard = &policy.guard;
    let mut checks = Vec::new();

    checks.push(match df_avail(Path::new(&guard.disk_mount_path)) {
        Some((used, avail)) => SetupCheck {
            name: "primary mount".to_string(),
            ok: true,
            detail: format!(
                "{} ({} free, {}% used)",
                guard.disk_mount_path,
                human_bytes(avail),
                used
            ),
        },
        None => SetupCheck {
            name: "primary mount".to_string(),
            ok: false,
            detail: format!("{} unreadable", guard.disk_mount_path),
        },
    });

    for mount in guard
        .disk_extra_mounts
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        checks.push(match df_avail(Path::new(mount)) {
            Some((used, avail)) => SetupCheck {
                name: format!("extra mount {mount}"),
                ok: true,
                detail: format!("{} free, {}% used", human_bytes(avail), used),
            },
            None => SetupCheck {
                name: format!("extra mount {mount}"),
                ok: false,
                detail: "unreadable".to_string(),
            },
        });
    }

    for root in guard
        .relocate_candidate_roots
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let dir = expand_tilde(root);
        checks.push(SetupCheck {
            name: format!("candidates {root}"),
            ok: dir.is_dir(),
            detail: if dir.is_dir() {
                "scannable".to_string()
            } else {
                "missing".to_string()
            },
        });
    }

    checks.push(check_dir_configured(&guard.relocate_cold_root, "cold root"));
    let qdir = if guard.quarantine_dir.trim().is_empty() {
        crate::default_quarantine_dir()
    } else {
        guard.quarantine_dir.trim().to_string()
    };
    checks.push(check_dir_configured(&qdir, "quarantine"));

    checks.push(SetupCheck {
        name: "auto mode".to_string(),
        ok: true,
        detail: format!(
            "relocate scan={}, apply={}, quarantine-first={}",
            guard.auto_relocate, guard.auto_relocate_apply, guard.clean_quarantine_first
        ),
    });

    // Ready = cold root + quarantine both usable. Mounts/candidates inform
    // but do not gate: a single-disk user can still quarantine locally.
    let ready = checks
        .iter()
        .filter(|c| c.name == "cold root" || c.name == "quarantine")
        .all(|c| c.ok);
    SetupReport {
        policy_path: path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none — built-in defaults)".to_string()),
        policy_exists: load_system_policy()
            .map(|(p, _)| p.is_some())
            .unwrap_or(false),
        checks,
        ready,
    }
}

pub(crate) fn cmd_setup(apply: bool, json: bool) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

    if apply {
        let (_, policy) = load_system_policy()?;
        let cold_raw = policy.guard.relocate_cold_root.trim();
        if cold_raw.is_empty() {
            anyhow::bail!("cannot apply: relocate_cold_root is not configured");
        }
        let qdir = crate::quarantine_root(&policy.guard);
        // The two managed roots must not nest: a cold root inside the
        // quarantine root (or vice versa) would make cleanup and expiry
        // operate on each other's trees.
        let cold_path = expand_tilde(cold_raw);
        if cold_path != qdir
            && (cold_path.starts_with(&qdir) || qdir.starts_with(&cold_path))
        {
            anyhow::bail!(
                "cannot apply: cold root {} nests inside quarantine root {} (or vice versa)",
                cold_path.display(),
                qdir.display()
            );
        }
        let protected = &policy.guard.protected_paths;
        println!(
            "cold root: {}",
            ensure_setup_dir(&cold_path, protected)?
        );
        println!("quarantine: {}", ensure_setup_dir(&qdir, protected)?);
    }

    let report = collect_setup_report();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("STATUS"),
                Cell::new("CHECK"),
                Cell::new("DETAIL"),
            ]);
        for c in &report.checks {
            let (icon, color) = if c.ok {
                ("✅", Color::Green)
            } else {
                ("❌", Color::Red)
            };
            table.add_row(vec![
                Cell::new(icon).fg(color),
                Cell::new(&c.name),
                Cell::new(&c.detail),
            ]);
        }
        println!("{table}");
        println!("policy: {}", report.policy_path);
        if report.ready {
            println!("✅ Space-tier automation is ready.");
        } else {
            println!("❌ Not ready: configure relocate_cold_root, then `setup --apply`.");
        }
    }
    Ok(())
}
