//! Symlink ownership management — deterministic link reconciliation.

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    check_safe_to_delete, expand_tilde, LinkCommands, LinkEntry, LinkEntryStatus, LinkStatusReport,
    SystemPolicy,
};

/// Evaluate a single link entry: check if symlink exists and points to the correct target.
pub(crate) fn evaluate_link(entry: &LinkEntry) -> LinkEntryStatus {
    let link = expand_tilde(&entry.link);
    let target = expand_tilde(&entry.target);
    let target_exists = target.exists();
    let meta = fs::symlink_metadata(&link).ok();
    let exists = meta.is_some();
    let is_symlink = meta
        .as_ref()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    let mut points_to = String::new();
    let mut in_sync = false;
    let issue = if !target_exists {
        "target_missing".to_string()
    } else if !exists {
        "link_missing".to_string()
    } else if !is_symlink {
        "path_not_symlink".to_string()
    } else {
        match fs::read_link(&link) {
            Ok(actual) => {
                let actual_abs = if actual.is_absolute() {
                    actual
                } else {
                    link.parent().unwrap_or_else(|| Path::new("/")).join(actual)
                };
                points_to = path_display(&actual_abs);
                if normalize_path(&actual_abs) == normalize_path(&target) {
                    in_sync = true;
                    "ok".to_string()
                } else {
                    "link_target_mismatch".to_string()
                }
            }
            Err(_) => "readlink_failed".to_string(),
        }
    };

    LinkEntryStatus {
        link: path_display(&link),
        target: path_display(&target),
        exists,
        is_symlink,
        target_exists,
        points_to,
        in_sync,
        issue,
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Build a full link status report from policy.
pub(crate) fn build_link_report(policy: &SystemPolicy) -> LinkStatusReport {
    let mut entries = Vec::with_capacity(policy.links.entries.len());
    let mut healthy = 0usize;
    let mut drifted = 0usize;
    let mut missing_target = 0usize;
    let mut missing_link = 0usize;

    for entry in &policy.links.entries {
        let status = evaluate_link(entry);
        match status.issue.as_str() {
            "ok" => healthy += 1,
            "target_missing" => {
                drifted += 1;
                missing_target += 1;
            }
            "link_missing" => {
                drifted += 1;
                missing_link += 1;
            }
            _ => drifted += 1,
        }
        entries.push(status);
    }

    LinkStatusReport {
        total: entries.len(),
        entries,
        healthy,
        drifted,
        missing_target,
        missing_link,
    }
}

fn backup_path_for(link: &Path) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = link
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "link".to_string());
    let backup_name = format!("{name}.dracon-system-backup-{ts}");
    link.with_file_name(backup_name)
}

/// Apply link policy: create or fix symlinks according to the configuration.
fn apply_link_policy(policy: &SystemPolicy, force_replace: bool) -> Result<LinkStatusReport> {
    for entry in &policy.links.entries {
        let link = expand_tilde(&entry.link);
        let target = expand_tilde(&entry.target);

        if !target.exists() {
            continue;
        }

        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }

        let meta = fs::symlink_metadata(&link).ok();
        if let Some(meta) = meta {
            if meta.file_type().is_symlink() {
                let safe_link = check_safe_to_delete(&link, &[])?;
                fs::remove_file(&safe_link)?;
            } else if force_replace {
                let safe_link = check_safe_to_delete(&link, &[])?;
                let backup = backup_path_for(&link);
                fs::rename(&safe_link, backup)?;
            } else {
                continue;
            }
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &link)?;
        }
        #[cfg(not(unix))]
        {
            return Err(anyhow::anyhow!("link apply is only supported on unix"));
        }
    }

    Ok(build_link_report(policy))
}

/// Display path relative to home if possible.
fn path_display(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub(crate) fn cmd_link(cmd: LinkCommands) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

    let (_, policy) = crate::load_system_policy()?;
    match cmd {
        LinkCommands::Status { json } | LinkCommands::Doctor { json } => {
            let report = build_link_report(&policy);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL_CONDENSED)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec![
                        Cell::new("STATUS"),
                        Cell::new("LINK"),
                        Cell::new("TARGET"),
                        Cell::new("ISSUE"),
                    ]);

                for item in &report.entries {
                    let (icon, color) = if item.issue == "ok" {
                        ("\u{2705}", Color::Green)
                    } else if item.issue.contains("missing") || item.issue.contains("not exist") {
                        ("\u{274c}", Color::Red)
                    } else {
                        ("\u{26a0}\u{fe0f}", Color::Yellow)
                    };

                    let issue = if item.issue == "ok" {
                        String::new()
                    } else {
                        format!("\u{2192} {}", item.issue)
                    };

                    table.add_row(vec![
                        Cell::new(icon).fg(color),
                        Cell::new(&item.link),
                        Cell::new(&item.target),
                        Cell::new(issue),
                    ]);
                }

                if report.entries.is_empty() {
                    println!("No configured links");
                } else {
                    println!("{table}");
                }
                println!(
                    "{} links: {} ok, {} drifted, {} missing target, {} missing link",
                    report.total,
                    report.healthy,
                    report.drifted,
                    report.missing_target,
                    report.missing_link
                );
            }
        }
        LinkCommands::Apply {
            json,
            force_replace,
        } => {
            let report = apply_link_policy(&policy, force_replace)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Applied link policy.");
                println!(
                    "{} links: {} ok, {} drifted",
                    report.total, report.healthy, report.drifted
                );
            }
        }
    }
    Ok(())
}

/// Default roots to scan for broken symlinks.
fn default_symlink_scan_roots() -> Vec<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let candidates = [
        home.join("Dev"),
        home.join(".dracon"),
        home.join(".local/bin"),
        home.join(".config"),
    ];
    candidates.into_iter().filter(|p| p.exists()).collect()
}

/// Information about a single broken symlink.
#[derive(Debug, Serialize)]
pub(crate) struct BrokenSymlink {
    pub(crate) path: String,
    pub(crate) target: String,
    pub(crate) target_exists: bool,
}

/// Report of broken symlinks found during scan.
#[derive(Debug, Serialize)]
pub(crate) struct BrokenSymlinkReport {
    pub(crate) roots_scanned: Vec<String>,
    pub(crate) total_scanned: usize,
    pub(crate) broken: Vec<BrokenSymlink>,
}

/// Recursively scan a directory for broken symlinks.
fn scan_broken_symlinks(root: &Path, max_depth: usize) -> (usize, Vec<BrokenSymlink>) {
    let mut count = 0usize;
    let mut broken = Vec::new();

    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return (0, Vec::new()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            count += 1;
            // For symlinks, check if the target exists using stat (not following the link)
            let target = match fs::read_link(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Resolve target relative to symlink's parent
            let resolved = if target.is_absolute() {
                target.clone()
            } else {
                path.parent()
                    .map(|p| p.join(&target))
                    .unwrap_or(target.clone())
            };
            // Use stat to check existence (doesn't follow symlinks)
            let target_exists = fs::metadata(&resolved).is_ok();
            if !target_exists {
                broken.push(BrokenSymlink {
                    path: path.display().to_string(),
                    target: target.display().to_string(),
                    target_exists: false,
                });
            }
        } else if meta.is_dir() && max_depth > 0 {
            // Skip system dirs that are noisy / dangerous to walk
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                name,
                "target"
                    | "node_modules"
                    | ".git"
                    | ".cache"
                    | ".venv"
                    | "dist"
                    | "build"
                    | "archives"
            ) {
                continue;
            }
            let (sub_count, sub_broken) = scan_broken_symlinks(&path, max_depth - 1);
            count += sub_count;
            broken.extend(sub_broken);
        }
    }
    (count, broken)
}

/// Command handler for `dracon-system symlinks`.
pub(crate) fn cmd_symlinks(roots: Vec<PathBuf>, json: bool, max_depth: usize) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};

    let roots = if roots.is_empty() {
        default_symlink_scan_roots()
    } else {
        roots
    };

    let mut total_scanned = 0usize;
    let mut all_broken: Vec<BrokenSymlink> = Vec::new();
    let mut root_strings: Vec<String> = Vec::new();

    for root in &roots {
        root_strings.push(path_display(root));
        let (count, broken) = scan_broken_symlinks(root, max_depth);
        total_scanned += count;
        all_broken.extend(broken);
    }

    if json {
        let report = BrokenSymlinkReport {
            roots_scanned: root_strings,
            total_scanned,
            broken: all_broken,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if all_broken.is_empty() {
            println!(
                "\u{2705} Scanned {} symlinks across {} root(s) — no broken links found",
                total_scanned,
                root_strings.len()
            );
        } else {
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![
                    Cell::new("STATUS"),
                    Cell::new("BROKEN LINK"),
                    Cell::new("TARGET"),
                ]);
            for item in &all_broken {
                table.add_row(vec![
                    Cell::new("\u{274c}").fg(Color::Red),
                    Cell::new(&item.path),
                    Cell::new(&item.target),
                ]);
            }
            println!("{table}");
            println!(
                "\u{274c} Found {} broken symlink(s) out of {} scanned across {} root(s)",
                all_broken.len(),
                total_scanned,
                root_strings.len()
            );
        }
    }
    Ok(())
}
