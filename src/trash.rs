//! Trash emptying with the credential guard and aged-entry purge.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// The disk cost of the top-level trash entries the credential guard keeps.
///
/// ADDED 2026-10-24. The guard reported "keeping 55 flagged entries" and
/// nothing else — the entry COUNT looks like a rounding error, and it was
/// not: on this host those 55 entries held **11 GiB**, and they grew every
/// time the CI trashed a repository whose test files are named
/// `…-token-…test.ts` (the operator's own manuscript was in there too,
/// kept because a chapter is called "borrowed credentials").
///
/// A guard that keeps something must say what keeping it COSTS. This
/// measures it: the total, and the biggest few by name.
pub(crate) async fn kept_trash_entry_sizes(
    trash_files: &Path,
    names: &std::collections::HashSet<String>,
) -> (u64, Vec<(String, u64)>) {
    let mut total = 0u64;
    let mut sizes: Vec<(String, u64)> = Vec::with_capacity(names.len());
    for name in names {
        let bytes = get_dir_size(&trash_files.join(name)).await.unwrap_or(0);
        total = total.saturating_add(bytes);
        sizes.push((name.clone(), bytes));
    }
    sizes.sort_by_key(|b| std::cmp::Reverse(b.1));
    (total, sizes)
}

/// Empty trash. When `credential_guard` is set, the trash is first
/// scanned for credential-signal filenames (see
/// looks_credential_like and docs/design/disk-full-credentials-
/// 2026-08-10.md); flagged top-level entries are kept while the rest
/// purges (per-entry since 2026-09-20, not all-or-nothing). The
/// 2026-08-10 scan found 665 credential-pattern matches in the 56 GiB
/// trash, so blind emptying is unsafe by default.
///
/// Unobservable subtrees fail closed per-entry (audit L6): a directory
/// AT the walk depth cap (children unseen) or an unreadable subtree
/// (walk error) keeps its whole top-level entry instead of purging it.
pub(crate) async fn empty_trash(
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
    min_age_days: u64,
) -> Result<(u64, Vec<String>)> {
    let Some(home) = dirs::home_dir() else {
        return Ok((0, Vec::new()));
    };
    empty_trash_at(
        &home,
        apply,
        protected_paths,
        credential_guard,
        min_age_days,
    )
    .await
}

/// Map a trash path to its top-level entry name and add it to the
/// credential-guard keep set. Returns false when the path escapes the
/// trash root or has no nameable top component (nothing kept).
pub(crate) fn trash_keep_top_level(
    skip_names: &mut std::collections::HashSet<String>,
    trash_files: &Path,
    path: &Path,
) -> bool {
    if let Ok(rel) = path.strip_prefix(trash_files) {
        if let Some(top) = rel.components().next().and_then(|c| c.as_os_str().to_str()) {
            skip_names.insert(top.to_string());
            return true;
        }
    }
    false
}

pub(crate) async fn empty_trash_at(
    home: &Path,
    apply: bool,
    protected_paths: &[String],
    credential_guard: bool,
    min_age_days: u64,
) -> Result<(u64, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();

    {
        let trash_files = home.join(".local/share/Trash/files");
        let trash_info = home.join(".local/share/Trash/info");
        // Credential-flagged top-level entries to keep (2026-09-20 space
        // audit: per-entry skip replaced the all-or-nothing abort).
        let mut skip_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        if trash_files.exists() {
            let size = get_dir_size(&trash_files).await.unwrap_or(0);
            if size > 0 {
                // CHANGED 2026-09-20 (space audit): the old code aborted the
                // ENTIRE trash purge when a single credential-signal filename
                // matched anywhere inside — one trashed `target/` dir with
                // cargo fingerprint files named `*mint_dev_token*` blocked
                // 46 GiB of unrelated trash every cycle at 100% disk. The
                // guard now skips only the flagged top-level entries and
                // purges the rest. The scan must be exhaustive (no early
                // break): a capped match list would purge entries the cap
                // cut off.
                if credential_guard {
                    let mut samples = Vec::new();
                    let mut match_count = 0u64;
                    // FIXED 2026-10-02 (audit L6): the scan failed OPEN
                    // twice — credential files deeper than max_depth(8)
                    // were never seen, and walk errors (EPERM, I/O) were
                    // swallowed by `_ => {}`. Both now fail closed
                    // PER-ENTRY (keeping the 2026-09-20 design: skip the
                    // unobservable top-level entries, purge the rest —
                    // not the old all-or-nothing abort).
                    let mut depth_capped = 0u64;
                    let mut walk_errors = 0u64;
                    for entry in walkdir::WalkDir::new(&trash_files).max_depth(TRASH_SCAN_MAX_DEPTH)
                    {
                        match entry {
                            Ok(e) if e.file_type().is_file() => {
                                if let Some(name) = e.file_name().to_str() {
                                    if looks_credential_like(name) {
                                        match_count += 1;
                                        trash_keep_top_level(
                                            &mut skip_names,
                                            &trash_files,
                                            e.path(),
                                        );
                                        if samples.len() < 3 {
                                            samples.push(e.path().display().to_string());
                                        }
                                    }
                                }
                            }
                            Ok(e)
                                if e.file_type().is_dir() && e.depth() == TRASH_SCAN_MAX_DEPTH =>
                            {
                                // AT the cap: children unobservable (files
                                // AT depth 8 are still yielded and scanned
                                // above — only descent stops). Keep the
                                // whole top-level entry.
                                depth_capped += 1;
                                trash_keep_top_level(&mut skip_names, &trash_files, e.path());
                            }
                            Err(e) => {
                                // Unreadable subtree: purging it would
                                // destroy credential files the guard never
                                // saw. Keep the top-level entry; a pathless
                                // error (unreachable in practice — an
                                // unreadable trash root sizes to 0 and skips
                                // the block) is counted loud with no keep.
                                walk_errors += 1;
                                if let Some(path) = e.path() {
                                    trash_keep_top_level(&mut skip_names, &trash_files, path);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !skip_names.is_empty() {
                        // The guard must not hide the COST of what it keeps.
                        //
                        // "55 flagged entries" says nothing about how much
                        // disk that is, and the answer here was 11 GiB that
                        // grew every time the CI trashed a repository whose
                        // test files are named `…-token-…test.ts` — and the
                        // operator's own manuscript, kept because a chapter
                        // is called "borrowed credentials". The entry count
                        // looks like a rounding error; the size does not.
                        //
                        // Reporting only: no behaviour changes here.
                        let (kept_bytes, kept_sizes) =
                            kept_trash_entry_sizes(&trash_files, &skip_names).await;
                        let biggest: Vec<String> = kept_sizes
                            .iter()
                            .take(3)
                            .map(|(name, bytes)| format!("{} ({})", name, human_bytes(*bytes)))
                            .collect();
                        let mut why = format!(
                            "{} credential-like file(s), e.g. {}",
                            match_count,
                            if samples.is_empty() {
                                "(none sampled)".to_string()
                            } else {
                                samples.join(", ")
                            }
                        );
                        if depth_capped > 0 {
                            why.push_str(&format!(
                                "; {} unobservable capped subtrees",
                                depth_capped
                            ));
                        }
                        if walk_errors > 0 {
                            why.push_str(&format!("; {} unreadable subtrees", walk_errors));
                        }
                        if kept_bytes > 0 {
                            why.push_str(&format!(
                                "; they hold {} (biggest: {})",
                                human_bytes(kept_bytes),
                                if biggest.is_empty() {
                                    "(none)".to_string()
                                } else {
                                    biggest.join(", ")
                                }
                            ));
                        }
                        eprintln!(
                            "🛡️ trash: keeping {} flagged entr{} ({}), purging the rest",
                            skip_names.len(),
                            if skip_names.len() == 1 { "y" } else { "ies" },
                            why
                        );
                        emit_event(&DraconEvent::new(
                            "system",
                            EventSeverity::Warn,
                            "trash/credential-guard",
                            format!(
                                "trash purge skipping {} flagged entries ({})",
                                skip_names.len(),
                                why
                            ),
                        ));
                    }
                }
                let mut succeeded = true;
                // Flagged entries force the per-entry path even when
                // min_age_days == 0: the whole-dir wipe below cannot spare
                // individual entries.
                let per_entry = min_age_days > 0 || !skip_names.is_empty();
                if per_entry {
                    // CHANGED 2026-08-25 (v0.112.39): age-based purge — only
                    // entries whose mtime is older than the cutoff are
                    // removed, preserving a recovery window instead of
                    // emptying everything at once. Handles both dry-run
                    // measurement and deletion, including paired .trashinfo
                    // metadata. min_age_days = 0 preserves the old behavior.
                    match purge_aged_trash_entries(
                        &trash_files,
                        &trash_info,
                        min_age_days,
                        protected_paths,
                        apply,
                        &skip_names,
                    )
                    .await
                    {
                        Ok((bytes, count)) if bytes > 0 => {
                            reclaimed += bytes;
                            cleaned.push(if min_age_days > 0 {
                                format!(
                                    "{} trash entries older than {}d ({})",
                                    count,
                                    min_age_days,
                                    human_bytes(bytes)
                                )
                            } else {
                                format!(
                                    "{} trash entries ({}, flagged entries kept)",
                                    count,
                                    human_bytes(bytes)
                                )
                            });
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("⚠️ aged trash purge failed: {}", e);
                            succeeded = false;
                        }
                    }
                } else if apply {
                    match check_safe_to_delete_guard(&trash_files, protected_paths) {
                        Ok(ref safe_path) => {
                            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                                eprintln!("⚠️ failed to remove trash files: {}", e);
                                succeeded = false;
                            } else if let Err(e) = tokio::fs::create_dir_all(&trash_files).await {
                                eprintln!("⚠️ failed to recreate trash dir: {}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("⚠️ skipping trash files: {}", e);
                            succeeded = false;
                        }
                    }
                }
                // Skip-set runs already counted per-entry via purge_aged above;
                // the whole-size accounting below is only for the wipe path.
                if min_age_days == 0 && skip_names.is_empty() && (!apply || succeeded) {
                    cleaned.push(format!("trash files ({})", human_bytes(size)));
                    reclaimed += size;
                }
            }
        }

        // In aged mode (min_age_days > 0) the .trashinfo files are removed
        // alongside their entries by purge_aged_trash_entries; only the old
        // empty-everything path wipes the whole info directory. A non-empty
        // skip set also takes the per-entry path (whole-dir wipe would
        // orphan the kept entries' .trashinfo).
        if min_age_days == 0 && skip_names.is_empty() && trash_info.exists() {
            let info_size = get_dir_size(&trash_info).await.unwrap_or(0);
            if info_size > 0 {
                let mut succeeded = true;
                if apply {
                    match check_safe_to_delete_guard(&trash_info, protected_paths) {
                        Ok(ref safe_path) => {
                            if let Err(e) = tokio::fs::remove_dir_all(safe_path).await {
                                eprintln!("⚠️ failed to remove trash info: {}", e);
                                succeeded = false;
                            } else if let Err(e) = tokio::fs::create_dir_all(&trash_info).await {
                                eprintln!("⚠️ failed to recreate trash info dir: {}", e);
                                // Note: we still count this as success since the files were removed
                            }
                        }
                        Err(e) => {
                            eprintln!("⚠️ skipping trash info: {}", e);
                            succeeded = false;
                        }
                    }
                }
                if !apply || succeeded {
                    cleaned.push(format!("trash info ({})", human_bytes(info_size)));
                    reclaimed += info_size;
                }
            }
        }
    }

    Ok((reclaimed, cleaned))
}

/// Remove top-level trash entries whose mtime is older than
/// `min_age_days`, together with their paired `.trashinfo` metadata.
/// Works for both apply and dry-run (dry-run only measures). Protected
/// paths are skipped with a note; removal failures keep the entry and
/// do not abort the pass. `skip_names` holds top-level entry names the
/// credential guard flagged: they are kept unconditionally.
pub(crate) async fn purge_aged_trash_entries(
    trash_files: &Path,
    trash_info: &Path,
    min_age_days: u64,
    protected_paths: &[String],
    apply: bool,
    skip_names: &std::collections::HashSet<String>,
) -> Result<(u64, u64)> {
    // CHANGED 2026-09-09 (audit F41): `now - age` panicked (debug) on
    // absurd min_age configs. checked_sub falls back to the epoch, so an
    // older-than-epoch cutoff matches nothing (every real mtime is newer)
    // instead of crashing the pass — the safe direction.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let age = Duration::from_secs(min_age_days.saturating_mul(86_400));
    let cutoff = UNIX_EPOCH + now.checked_sub(age).unwrap_or(Duration::ZERO);
    let mut reclaimed = 0u64;
    let mut count = 0u64;
    let mut rd = tokio::fs::read_dir(trash_files).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if skip_names.contains(name) {
                eprintln!(
                    "🛡️ keeping credential-flagged trash entry {}",
                    path.display()
                );
                continue;
            }
        }
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = match meta.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if modified > cutoff {
            continue;
        }
        if let Err(e) = check_safe_to_delete_guard(&path, protected_paths) {
            eprintln!("🛡️ keeping protected trash entry {}: {}", path.display(), e);
            continue;
        }
        let entry_size = if meta.is_dir() {
            get_dir_size(&path).await.unwrap_or(0)
        } else {
            meta.len()
        };
        if !apply {
            reclaimed += entry_size;
            count += 1;
            continue;
        }
        let removed = if meta.is_dir() {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        if let Err(e) = removed {
            eprintln!("⚠️ failed to remove trash entry {}: {}", path.display(), e);
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let info = trash_info.join(format!("{}.trashinfo", name));
            let _ = tokio::fs::remove_file(info).await;
        }
        reclaimed += entry_size;
        count += 1;
    }
    Ok((reclaimed, count))
}

pub(crate) static RESOLVE_BIN_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();
