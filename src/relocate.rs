//! Cold-data relocation — move a directory to another disk and leave a symlink.
//!
//! ADDED 2026-09-26 (space tiers, Phase 1): deletion-based cleanup cannot fix
//! a disk filled with *kept* data in the wrong place. `relocate` moves a cold
//! directory (media, datasets, old checkouts) to a destination root — typically
//! on the second disk — verifies the copy by file count + bytes, removes the
//! source, and leaves a symlink so existing paths keep working. Dry-run by
//! default; `--apply` performs the move.

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    check_safe_to_delete_guard, expand_tilde, human_bytes, load_system_policy, parse_df_details,
};

/// Finals of a relocation dry-run: what would move, where, and whether it fits.
#[derive(Debug, Serialize)]
pub(crate) struct RelocatePlan {
    pub(crate) source: String,
    pub(crate) dest: String,
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    pub(crate) dest_avail_bytes: Option<u64>,
    pub(crate) fits: bool,
    pub(crate) ready: bool,
    pub(crate) issues: Vec<String>,
}

/// Outcome of an applied relocation.
#[derive(Debug, Serialize)]
pub(crate) struct RelocateReport {
    pub(crate) link: String,
    pub(crate) target: String,
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    pub(crate) skipped_special: u64,
    pub(crate) policy_snippet: String,
}

/// Walk stats: (regular_files, total_bytes). Never follows symlinks.
pub(crate) fn walk_stats(root: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// Recursive copy: dirs recreated, files copied, symlinks re-created as
/// symlinks. Returns the count of skipped special files (sockets, fifos…).
/// Never follows symlinks.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<u64> {
    let mut skipped = 0u64;
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(src).map_err(|e| {
            anyhow::anyhow!("strip prefix {}: {}", entry.path().display(), e)
        })?;
        let target = dst.join(rel);
        let ftype = entry.file_type();
        if ftype.is_dir() {
            fs::create_dir_all(&target)?;
        } else if ftype.is_symlink() {
            let link_target = fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &target)?;
            #[cfg(not(unix))]
            anyhow::bail!("symlink copy is only supported on unix");
        } else if ftype.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        } else {
            skipped += 1;
        }
    }
    Ok(skipped)
}

fn avail_bytes_for(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .args(["-P"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df_details(&String::from_utf8_lossy(&out.stdout)).map(|d| d.avail_bytes)
}

/// True when `path` holds git-tracked content. Outside a work tree → false.
/// Inside a work tree, inspection failures bail (fail closed).
pub(crate) fn is_git_tracked(path: &Path) -> Result<bool> {
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::process::Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        Ok(o) if o.status.success() => {}
        _ => return Ok(false),
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["ls-files", "--", &name])
        .output()
        .map_err(|e| anyhow::anyhow!("git ls-files failed: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("git ls-files failed for {}", path.display());
    }
    Ok(!out.stdout.iter().all(|b| b.is_ascii_whitespace()))
}

/// Build a relocation plan. Pure except for the `df`/git probes; never mutates.
pub(crate) fn plan_relocate(
    source: &Path,
    dest_root: &Path,
    user_protected: &[String],
    allow_tracked: bool,
) -> Result<RelocatePlan> {
    let mut issues = Vec::new();
    let meta = fs::symlink_metadata(source).map_err(|e| {
        anyhow::anyhow!("cannot inspect source {}: {}", source.display(), e)
    })?;
    if meta.file_type().is_symlink() {
        anyhow::bail!("refusing to relocate symlink {} (already relocated?)", source.display());
    }
    if !meta.is_dir() {
        anyhow::bail!("relocate supports directories only: {}", source.display());
    }
    let canon_src = check_safe_to_delete_guard(source, user_protected)?;
    // Moving a tracked dir replaces it with a symlink and breaks the repo.
    if !allow_tracked && is_git_tracked(&canon_src)? {
        anyhow::bail!(
            "refusing to relocate git-tracked {} — untrack it first or pass --allow-tracked",
            source.display()
        );
    }

    let dest_meta = fs::symlink_metadata(dest_root)
        .map_err(|e| anyhow::anyhow!("destination root {}: {}", dest_root.display(), e))?;
    if !dest_meta.is_dir() || dest_meta.file_type().is_symlink() {
        anyhow::bail!(
            "destination root {} must be an existing real directory",
            dest_root.display()
        );
    }
    let canon_root = dest_root.canonicalize().map_err(|e| {
        anyhow::anyhow!("cannot canonicalize {}: {}", dest_root.display(), e)
    })?;

    let name = canon_src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("source has no file name"))?;
    let dest = canon_root.join(name);
    if fs::symlink_metadata(&dest).is_ok() {
        anyhow::bail!("destination {} already exists — refusing", dest.display());
    }
    if dest.starts_with(&canon_src) || canon_src.starts_with(&dest) {
        anyhow::bail!("source and destination nest — refusing");
    }

    let (files, bytes) = walk_stats(&canon_src);
    let avail = avail_bytes_for(&canon_root);
    let fits = avail.map(|a| a >= bytes).unwrap_or(true);
    if !fits {
        issues.push(format!(
            "destination has {} free but source needs {}",
            human_bytes(avail.unwrap_or(0)),
            human_bytes(bytes)
        ));
    }

    Ok(RelocatePlan {
        source: canon_src.display().to_string(),
        dest: dest.display().to_string(),
        files,
        bytes,
        dest_avail_bytes: avail,
        fits,
        ready: fits,
        issues,
    })
}

/// Execute a validated plan: copy, verify, remove source, leave symlink.
pub(crate) fn apply_relocate(plan: &RelocatePlan) -> Result<RelocateReport> {
    if !plan.ready {
        anyhow::bail!("plan is not ready: {}", plan.issues.join("; "));
    }
    let source = Path::new(&plan.source);
    let dest = Path::new(&plan.dest);
    if fs::symlink_metadata(dest).is_ok() {
        anyhow::bail!("destination {} appeared since planning — refusing", dest.display());
    }
    let meta = fs::symlink_metadata(source).map_err(|e| {
        anyhow::anyhow!("source {} vanished or changed: {}", source.display(), e)
    })?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        anyhow::bail!("source {} is no longer a real directory — refusing", source.display());
    }

    let skipped_special = copy_tree(source, dest)?;
    let (dest_files, dest_bytes) = walk_stats(dest);
    if dest_files != plan.files || dest_bytes != plan.bytes {
        anyhow::bail!(
            "copy verification failed: expected {} files / {} bytes, got {} / {} — source untouched",
            plan.files,
            plan.bytes,
            dest_files,
            dest_bytes
        );
    }

    fs::remove_dir_all(source)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(dest, source)?;
    #[cfg(not(unix))]
    anyhow::bail!("relocate is only supported on unix");

    let snippet = format!(
        "[[links.entries]]\nlink = \"{}\"\ntarget = \"{}\"\n",
        plan.source, plan.dest
    );
    Ok(RelocateReport {
        link: plan.source.clone(),
        target: plan.dest.clone(),
        files: plan.files,
        bytes: plan.bytes,
        skipped_special,
        policy_snippet: snippet,
    })
}

fn display_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.display().to_string();
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

pub(crate) fn cmd_relocate(
    path: PathBuf,
    to: String,
    apply: bool,
    allow_tracked: bool,
    json: bool,
) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    let (_, policy) = load_system_policy()?;
    let dest_root = expand_tilde(&to);
    let plan = plan_relocate(&path, &dest_root, &policy.guard.protected_paths, allow_tracked)?;

    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![Cell::new("FIELD"), Cell::new("VALUE")]);
            table.add_row(vec![Cell::new("Source"), Cell::new(display_home(&plan.source))]);
            table.add_row(vec![Cell::new("Dest"), Cell::new(display_home(&plan.dest))]);
            table.add_row(vec![
                Cell::new("Size"),
                Cell::new(format!("{} in {} files", human_bytes(plan.bytes), plan.files)),
            ]);
            table.add_row(vec![
                Cell::new("Dest free"),
                Cell::new(
                    plan.dest_avail_bytes
                        .map(human_bytes)
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
            ]);
            println!("{table}");
            for issue in &plan.issues {
                println!("⚠️ {issue}");
            }
            if plan.ready {
                println!("Dry-run: pass --apply to move and link.");
            } else {
                println!("❌ Plan is not ready.");
            }
        }
        return Ok(());
    }

    let report = apply_relocate(&plan)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "✅ Relocated {} ({}) → {} (symlink left behind)",
            display_home(&report.link),
            human_bytes(report.bytes),
            display_home(&report.target)
        );
        if report.skipped_special > 0 {
            println!("⚠️ skipped {} special files", report.skipped_special);
        }
        println!("Add to policy to keep the link managed:\n{}", report.policy_snippet);
    }
    Ok(())
}
