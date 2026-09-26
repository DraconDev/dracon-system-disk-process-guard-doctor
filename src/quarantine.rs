//! Quarantine — hold-then-delete staging with a TTL.
//!
//! ADDED 2026-09-26 (space tiers, Phase 1): the middle tier between "working"
//! and "trash" for data kept for now but likely deleted later. `quarantine
//! move` relocates a path under the quarantine root with a manifest recording
//! its origin; `restore` moves it back; `expire` deletes entries older than
//! the TTL (dry-run unless `--apply`). A TTL of 0 disables expiry.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    check_safe_to_delete_guard, copy_tree, expand_tilde, human_bytes, load_system_policy,
    unique_backup_path, walk_stats, QuarantineCommands,
};

const MANIFEST_NAME: &str = ".quarantine.json";

/// Manifest stored inside each quarantine entry.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct QuarantineManifest {
    pub(crate) name: String,
    pub(crate) origin: String,
    pub(crate) moved_at_unix: u64,
    pub(crate) bytes: u64,
    pub(crate) files: u64,
}

/// One listed entry (manifest may be missing for hand-placed dirs).
#[derive(Debug, Serialize)]
pub(crate) struct QuarantineEntry {
    pub(crate) name: String,
    pub(crate) origin: String,
    pub(crate) moved_at_unix: Option<u64>,
    pub(crate) age_days: Option<u64>,
    pub(crate) bytes: u64,
    pub(crate) expired: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuarantineList {
    pub(crate) root: String,
    pub(crate) ttl_days: u64,
    pub(crate) entries: Vec<QuarantineEntry>,
    pub(crate) total_bytes: u64,
    pub(crate) expired_bytes: u64,
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn quarantine_root(guard: &crate::GuardPolicy) -> PathBuf {
    let raw = guard.quarantine_dir.trim();
    if raw.is_empty() {
        expand_tilde(&crate::default_quarantine_dir())
    } else {
        expand_tilde(raw)
    }
}

fn read_manifest(entry_dir: &Path) -> Option<QuarantineManifest> {
    let text = fs::read_to_string(entry_dir.join(MANIFEST_NAME)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Entry dir name: `<basename>.<nanos>` with collision suffix, via the same
/// unique-name helper the link backup path uses.
fn entry_dir_for(root: &Path, origin: &Path) -> PathBuf {
    let base = origin
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "entry".to_string());
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    unique_backup_path(root, &format!("{base}.{ts}"))
}

/// Move `origin` into quarantine. Uses rename when possible (same filesystem),
/// else copy + verify + remove. Refuses symlinks and protected paths.
pub(crate) fn quarantine_move(
    origin: &Path,
    root: &Path,
    user_protected: &[String],
) -> Result<QuarantineManifest> {
    let meta = fs::symlink_metadata(origin)
        .map_err(|e| anyhow::anyhow!("cannot inspect {}: {}", origin.display(), e))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!("refusing to quarantine symlink {}", origin.display());
    }
    if !meta.is_dir() {
        anyhow::bail!("quarantine supports directories only: {}", origin.display());
    }
    let canon_origin = check_safe_to_delete_guard(origin, user_protected)?;
    fs::create_dir_all(root)?;
    let canon_root = root.canonicalize()?;
    if canon_origin.starts_with(&canon_root) || canon_root.starts_with(&canon_origin) {
        anyhow::bail!("origin and quarantine root nest — refusing");
    }

    let (files, bytes) = walk_stats(&canon_origin);
    let entry_dir = entry_dir_for(&canon_root, &canon_origin);
    let name = entry_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    match fs::rename(&canon_origin, &entry_dir) {
        Ok(()) => {}
        Err(_) => {
            copy_tree(&canon_origin, &entry_dir)?;
            let (got_files, got_bytes) = walk_stats(&entry_dir);
            if got_files != files || got_bytes != bytes {
                let _ = fs::remove_dir_all(&entry_dir);
                anyhow::bail!("copy verification failed — origin untouched");
            }
            fs::remove_dir_all(&canon_origin)?;
        }
    }

    let manifest = QuarantineManifest {
        name,
        origin: canon_origin.display().to_string(),
        moved_at_unix: now_unix(),
        bytes,
        files,
    };
    fs::write(
        entry_dir.join(MANIFEST_NAME),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

pub(crate) fn quarantine_list(root: &Path, ttl_days: u64) -> Result<QuarantineList> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    let mut expired_bytes = 0u64;
    if fs::symlink_metadata(root).is_ok() {
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        for dir in fs::read_dir(&canon_root)?.flatten() {
            let path = dir.path();
            if !path.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let (_, bytes) = walk_stats(&path);
            let manifest = read_manifest(&path);
            let now = now_unix();
            let (origin, moved_at, age_days, expired) = match &manifest {
                Some(m) => {
                    let age = now.saturating_sub(m.moved_at_unix) / 86_400;
                    let exp = ttl_days > 0 && age > ttl_days;
                    (m.origin.clone(), Some(m.moved_at_unix), Some(age), exp)
                }
                None => ("unknown".to_string(), None, None, false),
            };
            total_bytes += bytes;
            if expired {
                expired_bytes += bytes;
            }
            entries.push(QuarantineEntry {
                name,
                origin,
                moved_at_unix: moved_at,
                age_days,
                bytes,
                expired,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(QuarantineList {
        root: root.display().to_string(),
        ttl_days,
        entries,
        total_bytes,
        expired_bytes,
    })
}

/// Restore an entry to its recorded origin. The origin must not exist.
pub(crate) fn quarantine_restore(root: &Path, name: &str) -> Result<PathBuf> {
    let canon_root = root.canonicalize().map_err(|e| {
        anyhow::anyhow!("cannot canonicalize quarantine root {}: {}", root.display(), e)
    })?;
    let entry_dir = canon_root.join(name);
    if !entry_dir.is_dir() {
        anyhow::bail!("no such quarantine entry: {name}");
    }
    let manifest = read_manifest(&entry_dir)
        .ok_or_else(|| anyhow::anyhow!("entry {name} has no manifest — refusing blind restore"))?;
    let origin = PathBuf::from(&manifest.origin);
    if fs::symlink_metadata(&origin).is_ok() {
        anyhow::bail!("origin {} already exists — refusing", origin.display());
    }
    if let Some(parent) = origin.parent() {
        fs::create_dir_all(parent)?;
    }
    // Drop the manifest before moving back so the restored tree is clean.
    fs::remove_file(entry_dir.join(MANIFEST_NAME))?;
    match fs::rename(&entry_dir, &origin) {
        Ok(()) => Ok(origin),
        Err(_) => {
            copy_tree(&entry_dir, &origin)?;
            let (got_files, got_bytes) = walk_stats(&origin);
            if got_files != manifest.files || got_bytes != manifest.bytes {
                let _ = fs::remove_dir_all(&origin);
                anyhow::bail!("restore verification failed — quarantine entry kept");
            }
            fs::remove_dir_all(&entry_dir)?;
            Ok(origin)
        }
    }
}

/// Delete entries older than the TTL. Entries are validated to sit directly
/// under the quarantine root before removal. TTL 0 disables expiry.
pub(crate) fn quarantine_expire(root: &Path, ttl_days: u64, apply: bool) -> Result<Vec<String>> {
    if ttl_days == 0 {
        return Ok(Vec::new());
    }
    let list = quarantine_list(root, ttl_days)?;
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut removed = Vec::new();
    for entry in list.entries.iter().filter(|e| e.expired) {
        let dir = canon_root.join(&entry.name);
        let canon = dir.canonicalize()?;
        if canon.parent() != Some(canon_root.as_path()) {
            anyhow::bail!("entry {} escaped quarantine root — refusing", entry.name);
        }
        if apply {
            fs::remove_dir_all(&canon)?;
        }
        removed.push(entry.name.clone());
    }
    Ok(removed)
}

pub(crate) fn cmd_quarantine(cmd: QuarantineCommands) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    let (_, policy) = load_system_policy()?;
    let root = quarantine_root(&policy.guard);
    let ttl = policy.guard.quarantine_ttl_days;
    match cmd {
        QuarantineCommands::Move { path, apply, json } => {
            if !apply {
                let meta = fs::symlink_metadata(&path).map_err(|e| {
                    anyhow::anyhow!("cannot inspect {}: {}", path.display(), e)
                })?;
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    anyhow::bail!("quarantine move supports real directories only");
                }
                let (files, bytes) = walk_stats(&path);
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "origin": path.display().to_string(),
                            "root": root.display().to_string(),
                            "files": files,
                            "bytes": bytes,
                        }))?
                    );
                } else {
                    println!(
                        "Would quarantine {} ({} in {} files) → {}",
                        path.display(),
                        human_bytes(bytes),
                        files,
                        root.display()
                    );
                    println!("Dry-run: pass --apply to move.");
                }
                return Ok(());
            }
            let manifest = quarantine_move(&path, &root, &policy.guard.protected_paths)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!(
                    "✅ Quarantined {} ({} in {} files) as {}",
                    manifest.origin,
                    human_bytes(manifest.bytes),
                    manifest.files,
                    manifest.name
                );
            }
        }
        QuarantineCommands::List { json } => {
            let list = quarantine_list(&root, ttl)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.entries.is_empty() {
                println!("Quarantine {} is empty", root.display());
            } else {
                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL_CONDENSED)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec![
                        Cell::new("ENTRY"),
                        Cell::new("ORIGIN"),
                        Cell::new("AGE"),
                        Cell::new("SIZE"),
                        Cell::new("EXPIRED"),
                    ]);
                for e in &list.entries {
                    table.add_row(vec![
                        Cell::new(&e.name),
                        Cell::new(&e.origin),
                        Cell::new(
                            e.age_days
                                .map(|d| format!("{d}d"))
                                .unwrap_or_else(|| "?".to_string()),
                        ),
                        Cell::new(human_bytes(e.bytes)),
                        Cell::new(if e.expired { "yes" } else { "" }),
                    ]);
                }
                println!("{table}");
                println!(
                    "{} entries, {} total ({} expired, TTL {}d)",
                    list.entries.len(),
                    human_bytes(list.total_bytes),
                    human_bytes(list.expired_bytes),
                    ttl
                );
            }
        }
        QuarantineCommands::Restore { name, json } => {
            let origin = quarantine_restore(&root, &name)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "entry": name,
                        "restored_to": origin.display().to_string(),
                    }))?
                );
            } else {
                println!("✅ Restored {name} → {}", origin.display());
            }
        }
        QuarantineCommands::Expire { apply, json } => {
            let expired = quarantine_expire(&root, ttl, apply)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ttl_days": ttl,
                        "apply": apply,
                        "expired": expired,
                    }))?
                );
            } else if expired.is_empty() {
                println!("No expired entries (TTL {}d)", ttl);
            } else if apply {
                println!("🗑 Expired {} entries: {}", expired.len(), expired.join(", "));
            } else {
                println!(
                    "Would expire {} entries: {}",
                    expired.len(),
                    expired.join(", ")
                );
                println!("Dry-run: pass --apply to delete.");
            }
        }
    }
    Ok(())
}
