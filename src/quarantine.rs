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
    unique_backup_path, walk_stats, walk_stats_strict, QuarantineCommands,
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
    /// True when the manifest is unreadable, so the entry can never age out
    /// and can never be restored to a known location.
    pub(crate) pinned: bool,
    /// Why this entry is pinned, shown to the operator.
    pub(crate) pin_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuarantineList {
    pub(crate) root: String,
    pub(crate) ttl_days: u64,
    pub(crate) entries: Vec<QuarantineEntry>,
    pub(crate) total_bytes: u64,
    pub(crate) expired_bytes: u64,
    /// Names of entries the fail-safe is holding, and the bytes they hold.
    pub(crate) pinned: Vec<String>,
    pub(crate) pinned_bytes: u64,
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Quarantine `origin` instead of deleting it. Returns the manifest plus the
/// bytes actually freed on the origin filesystem (0 for same-filesystem
/// moves — the win there is the TTL expiry later, not immediate space).
pub(crate) fn quarantine_first_remove(
    origin: &Path,
    root: &Path,
    user_protected: &[String],
) -> Result<(QuarantineManifest, u64)> {
    // Capture the origin device BEFORE the move (the path is gone after).
    // FIXED 2026-09-27 (clippy `option_and_then_some`): `.and_then(|m| Some(m.dev()))`
    // is `.map(|m| m.dev())`. Pre-existing lint, newly fatal under the CI
    // gate that now lints `--all-targets`.
    let origin_dev = fs::metadata(origin).ok().map(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            m.dev()
        }
        #[cfg(not(unix))]
        {
            let _ = m;
            0
        }
    });
    let manifest = quarantine_move(origin, root, user_protected)?;
    #[cfg(unix)]
    let same_dev = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(root)
            .ok()
            .is_some_and(|m| Some(m.dev()) == origin_dev)
    };
    #[cfg(not(unix))]
    let same_dev = false;
    // Unknown device comparison claims nothing (fail closed).
    let freed = if same_dev || origin_dev.is_none() {
        0
    } else {
        manifest.bytes
    };
    Ok((manifest, freed))
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

    let (files, bytes) = walk_stats_strict(&canon_origin)?;
    let entry_dir = entry_dir_for(&canon_root, &canon_origin);
    let name = entry_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    match fs::rename(&canon_origin, &entry_dir) {
        Ok(()) => {}
        Err(_) => {
            let skipped = copy_tree(&canon_origin, &entry_dir).inspect_err(|_| {
                let _ = fs::remove_dir_all(&entry_dir);
            })?;
            if skipped > 0 {
                eprintln!(
                    "quarantine move: skipped {} special files (sockets, fifos) — not preserved",
                    skipped
                );
            }
            let (got_files, got_bytes) = walk_stats_strict(&entry_dir)?;
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
    // Serialize before anything is unlinked: if the write below fails we
    // still have the bytes to report exactly where the data sits.
    let manifest_text = serde_json::to_string_pretty(&manifest)?;
    fs::write(entry_dir.join(MANIFEST_NAME), &manifest_text).map_err(|e| {
        // The data moved but has no manifest, which would orphan it (a
        // future restore refuses manifest-less entries). Move it back
        // rather than leave it unrecorded.
        if fs::rename(&entry_dir, &canon_origin).is_ok() {
            anyhow::anyhow!("quarantine manifest write failed — moved back to origin: {e}")
        } else {
            anyhow::anyhow!(
                "quarantine manifest write failed AND move-back failed: data is at {} with no manifest, origin {} is gone",
                entry_dir.display(),
                canon_origin.display()
            )
        }
    })?;
    Ok(manifest)
}

/// Why a fail-safe pin is reported to the operator.
///
/// Re-examined 2026-09-29 (DECIDE #3 follow-up). The fail-safe is KEPT.
/// The manifest is the only record of where the entry came from; without it
/// the entry is permanently unrestorable, so expiring it would convert
/// "recoverable by hand" into "gone". Nothing is gained by taking that risk
/// on a timer: the quarantine root is written only by this daemon, so an
/// unreadable manifest means corruption or outside interference, not a
/// routine condition that a TTL is there to bound — and a genuinely
/// transient read error (EACCES, EIO) resolves itself, after which the entry
/// ages normally.
///
/// The cost of that choice is that bytes can be held indefinitely, so the
/// pin is never silent: it is listed, counted, and explained, and
/// `quarantine purge <name>` is the documented operator escape hatch for a
/// specific entry the operator has inspected.
pub(crate) const PIN_REASON: &str =
    "manifest unreadable — cannot age out or restore; clear with `quarantine purge <name> --apply`";

pub(crate) fn quarantine_list(root: &Path, ttl_days: u64) -> Result<QuarantineList> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    let mut expired_bytes = 0u64;
    let mut pinned = Vec::new();
    let mut pinned_bytes = 0u64;
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
                None => ("unknown".to_string(), None, None, true),
            };
            let is_pinned = manifest.is_none();
            total_bytes += bytes;
            if expired {
                expired_bytes += bytes;
            }
            if is_pinned {
                pinned.push(name.clone());
                pinned_bytes += bytes;
            }
            entries.push(QuarantineEntry {
                name,
                origin,
                moved_at_unix: moved_at,
                age_days,
                bytes,
                expired,
                pinned: is_pinned,
                pin_reason: is_pinned.then(|| PIN_REASON.to_string()),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        pinned.sort();
    }
    Ok(QuarantineList {
        root: root.display().to_string(),
        ttl_days,
        entries,
        total_bytes,
        expired_bytes,
        pinned,
        pinned_bytes,
    })
}

/// Resolve a caller-supplied entry name to a real directory inside the
/// quarantine root.
///
/// The name is a single path component minted by `entry_dir_for`;
/// separators, parent components, and absolute paths are refused before they
/// can escape the quarantine root, and a smuggled symlink is refused rather
/// than followed. Shared by `restore` and `purge` so the escape hatch
/// inherits exactly the same containment checks as the safe path — a purge
/// that validated names differently would be the weakest link in the module.
pub(crate) fn resolve_entry_dir(root: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        anyhow::bail!("invalid quarantine entry name: {name}");
    }
    let canon_root = root.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot canonicalize quarantine root {}: {}",
            root.display(),
            e
        )
    })?;
    let entry_dir = canon_root.join(name);
    // symlink_metadata does not follow links: a smuggled symlink into an
    // entry-named path is refused here instead of being operated through.
    let meta = fs::symlink_metadata(&entry_dir)
        .map_err(|_| anyhow::anyhow!("no such quarantine entry: {name}"))?;
    if !meta.file_type().is_dir() {
        if meta.file_type().is_symlink() {
            anyhow::bail!("refusing to operate on symlink entry {name}");
        }
        anyhow::bail!("no such quarantine entry: {name}");
    }
    Ok(entry_dir)
}

/// Delete one named quarantine entry, whatever state its manifest is in.
///
/// This is the documented escape hatch for an entry the fail-safe is pinning
/// (see `PIN_REASON`): the operator names ONE entry they have inspected and
/// accepts that it cannot be restored. It is deliberately not a bulk
/// "delete everything unreadable" switch — that would reintroduce exactly the
/// timer-driven, unaccountable deletion the fail-safe exists to prevent, and
/// a manifest that is unreadable for one entry is often unreadable because
/// something is wrong with the whole root.
///
/// Dry-run unless `apply`, and it reports the size it would reclaim so the
/// operator sees what they are about to give up before giving it up.
pub(crate) fn quarantine_purge(
    root: &Path,
    name: &str,
    apply: bool,
) -> Result<(String, u64, bool)> {
    let entry_dir = resolve_entry_dir(root, name)?;
    let (files, bytes) = walk_stats(&entry_dir);
    let pinned = read_manifest(&entry_dir).is_none();
    if apply {
        fs::remove_dir_all(&entry_dir)?;
    }
    Ok((
        format!("{files} files, {}", human_bytes(bytes)),
        bytes,
        pinned,
    ))
}

/// Restore an entry to its recorded origin. The origin must not exist.
///
/// Uses the shared `resolve_entry_dir` containment check, and the manifest
/// is kept until the move succeeds, so a failed restore leaves the entry
/// restorable instead of orphaning it.
pub(crate) fn quarantine_restore(root: &Path, name: &str) -> Result<PathBuf> {
    let entry_dir = resolve_entry_dir(root, name)?;
    let manifest = read_manifest(&entry_dir)
        .ok_or_else(|| anyhow::anyhow!("entry {name} has no manifest — refusing blind restore"))?;
    let origin = PathBuf::from(&manifest.origin);
    if fs::symlink_metadata(&origin).is_ok() {
        anyhow::bail!("origin {} already exists — refusing", origin.display());
    }
    if let Some(parent) = origin.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(&entry_dir, &origin) {
        Ok(()) => {
            // Drop the manifest only after the move, so the restored tree
            // is clean; a failure here is cosmetic, the data is home.
            let _ = fs::remove_file(origin.join(MANIFEST_NAME));
            Ok(origin)
        }
        Err(_) => {
            copy_tree(&entry_dir, &origin).inspect_err(|_| {
                let _ = fs::remove_dir_all(&origin);
            })?;
            // The copy carries the manifest along; drop it from the
            // restored tree BEFORE verifying, or its file+bytes break the
            // tally (the manifest was written after the source tally) and
            // a stray manifest leaks into the restored tree.
            let _ = fs::remove_file(origin.join(MANIFEST_NAME));
            let (got_files, got_bytes) = walk_stats_strict(&origin)?;
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
        // A directory that vanished between the list and the removal is
        // already gone; skip it rather than aborting the batch and losing
        // the removals already collected.
        let Ok(canon) = dir.canonicalize() else {
            eprintln!("quarantine expire: entry {} vanished, skipping", entry.name);
            continue;
        };
        if canon.parent() != Some(canon_root.as_path()) {
            // An escaped entry is a security signal, but aborting the
            // batch discards the removals already collected. Skip it
            // loudly instead; the warning names the entry for the operator.
            eprintln!(
                "quarantine expire: entry {} escaped quarantine root — refusing it, continuing batch",
                entry.name
            );
            continue;
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
                let meta = fs::symlink_metadata(&path)
                    .map_err(|e| anyhow::anyhow!("cannot inspect {}: {}", path.display(), e))?;
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
                        Cell::new("NOTE"),
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
                        Cell::new(if e.pinned { "PINNED" } else { "" }),
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
                // The fail-safe is not allowed to be silent: an entry held
                // forever because its manifest is unreadable would otherwise
                // look exactly like a quarantine that has nothing to expire.
                if !list.pinned.is_empty() {
                    println!(
                        "⚠ {} entr{} pinned by the fail-safe ({}): {PIN_REASON}",
                        list.pinned.len(),
                        if list.pinned.len() == 1 {
                            "y is"
                        } else {
                            "ies are"
                        },
                        human_bytes(list.pinned_bytes)
                    );
                    println!("   {}", list.pinned.join(", "));
                }
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
            // Count what the fail-safe is holding so "No expired entries"
            // cannot be read as "nothing is being kept".
            let pinned = quarantine_list(&root, ttl)?.pinned;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ttl_days": ttl,
                        "apply": apply,
                        "expired": expired,
                        "pinned": pinned,
                    }))?
                );
            } else if expired.is_empty() {
                println!("No expired entries (TTL {}d)", ttl);
            } else if apply {
                println!(
                    "🗑 Expired {} entries: {}",
                    expired.len(),
                    expired.join(", ")
                );
            } else {
                println!(
                    "Would expire {} entries: {}",
                    expired.len(),
                    expired.join(", ")
                );
                println!("Dry-run: pass --apply to delete.");
            }
            // Reported whether or not anything expired. An operator who
            // just ran a successful expire must still learn that data is
            // being HELD, or the run looks like the queue is now empty.
            if !pinned.is_empty() {
                println!(
                    "⚠ {} entr{} held by the fail-safe (unreadable manifest) and NOT expired: {}",
                    pinned.len(),
                    if pinned.len() == 1 { "y is" } else { "ies are" },
                    pinned.join(", ")
                );
                println!("   clear one with: quarantine purge <name> --apply");
            }
        }
        QuarantineCommands::Purge { name, apply, json } => {
            let (contents, bytes, was_pinned) = quarantine_purge(&root, &name, apply)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "entry": name,
                        "contents": contents,
                        "bytes": bytes,
                        "pinned_by_fail_safe": was_pinned,
                        "applied": apply,
                    }))?
                );
            } else if apply {
                println!("🗑 Purged {name} ({contents})");
                if was_pinned {
                    println!(
                        "   This entry had an unreadable manifest, so it could not be \
                         restored. It is now gone for good."
                    );
                }
            } else {
                println!("Would purge {name} ({contents})");
                if was_pinned {
                    println!(
                        "   Its manifest is unreadable, so it cannot be restored — \
                         purging discards it permanently."
                    );
                }
                println!("Dry-run: pass --apply to delete.");
            }
        }
    }
    Ok(())
}
