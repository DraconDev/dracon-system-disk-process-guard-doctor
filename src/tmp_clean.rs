//! `/tmp` cleanup: liveness protection (open files, cwd/exe, live sockets, fresh content), the e2e fast path, and removal.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// Collect the set of paths under any of `roots` that are currently held
/// open by a running process (`/proc/*/fd` readlink targets,
/// `/proc/*/cwd`, and `/proc/*/exe`). Bounded by the process and
/// descriptor tables visible in procfs; entries in this set are skipped
/// by tmp cleanup even when old, because an open file, a process working
/// directory, or a running executable may still be in use.
pub(crate) async fn collect_open_paths_under_from(
    proc_root: &Path,
    roots: &[PathBuf],
) -> Option<std::collections::HashSet<PathBuf>> {
    // FIXED 2026-10-03 (audit R3-L31): `None` when the scan itself
    // fails (unreadable `proc_root`, pid-iteration error) — the caller
    // must skip the pass, because an empty set would silently disable
    // open-file protection. Per-process/per-fd read failures stay
    // skips (other users' fd tables are routinely unreadable to this
    // unprivileged service; failing the pass on those would disable
    // tmp cleanup forever).
    let mut open = std::collections::HashSet::new();
    let mut proc_rd = match tokio::fs::read_dir(proc_root).await {
        Ok(rd) => rd,
        Err(_) => return None,
    };
    loop {
        let pid_entry = match proc_rd.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => return None,
        };
        let name = pid_entry.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let process_dir = pid_entry.path();
        let fd_dir = process_dir.join("fd");
        if let Ok(mut fd_rd) = tokio::fs::read_dir(&fd_dir).await {
            loop {
                match fd_rd.next_entry().await {
                    Ok(Some(fd_entry)) => {
                        if let Ok(target) = tokio::fs::read_link(fd_entry.path()).await {
                            let target: PathBuf = target;
                            if roots.iter().any(|r| target.starts_with(r)) {
                                // Remember the deepest ancestor we saw so cleanup can
                                // check "is any prefix of this entry open" cheaply.
                                open.insert(target);
                            }
                        }
                    }
                    Ok(None) => break,
                    // FIXED 2026-10-03 (audit R4-SYS-11): one unreadable
                    // fd entry (an fd closed mid-iteration — TOCTOU on a
                    // live process) must skip THAT fd, not abandon the
                    // rest of the table. The old `while let Ok(Some(..))`
                    // exited the loop on Err, under-counting that
                    // process's open files — broader than the documented
                    // "per-fd read failures stay skips".
                    Err(_) => continue,
                }
            }
        }

        // A process cwd is a directory reference, not an open fd entry. It
        // must be collected even when the fd table cannot be read, otherwise
        // cleanup can recursively remove an old directory that a live process
        // is still using as its working directory.
        if let Ok(target) = tokio::fs::read_link(process_dir.join("cwd")).await {
            let target: PathBuf = target;
            if roots.iter().any(|r| target.starts_with(r)) {
                open.insert(target);
            }
        }

        // FIXED 2026-10-03 (audit R4-SYS-12): same for the running
        // executable — it is not an fd entry either, so without this
        // a stale binary executing from /tmp is unlinked while
        // running (the process survives on the unlinked inode but
        // the path is gone). One readlink per pid, same cost class
        // as cwd; failures stay per-process skips like cwd.
        if let Ok(target) = tokio::fs::read_link(process_dir.join("exe")).await {
            let target: PathBuf = target;
            if roots.iter().any(|r| target.starts_with(r)) {
                open.insert(target);
            }
        }
    }
    Some(open)
}

/// True when any entry at or under `dir` (including `dir` itself) is
/// newer than `cutoff`. Symlinks are never followed. Short-circuits on
/// the first fresh entry; a fully stale tree costs one walk — the same
/// order as the size accounting that follows for deletion candidates.
pub(crate) async fn tree_has_fresh_content(dir: &Path, cutoff: SystemTime) -> bool {
    // FIXED 2026-10-03 (audit R3-L22): fail CLOSED — any walk error
    // (metadata, readdir, entry iteration, mtime) answers "fresh"
    // (keep) instead of skipping toward a `false` (delete) verdict.
    // A transient I/O error or unreadable subtree must defer cleanup
    // to the next cycle, never authorize deletion of unknown content.
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = match tokio::fs::symlink_metadata(&current).await {
            Ok(m) => m,
            Err(_) => return true,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        match meta.modified() {
            Ok(mtime) if mtime > cutoff => return true,
            Ok(_) => {}
            Err(_) => return true,
        }
        if meta.is_dir() {
            let mut rd = match tokio::fs::read_dir(&current).await {
                Ok(rd) => rd,
                Err(_) => return true,
            };
            loop {
                match rd.next_entry().await {
                    Ok(Some(entry)) => stack.push(entry.path()),
                    Ok(None) => break,
                    Err(_) => return true,
                }
            }
        }
    }
    false
}

pub(crate) fn path_has_open_ancestor(path: &Path, open: &std::collections::HashSet<PathBuf>) -> bool {
    open.iter()
        .any(|o| o.starts_with(path) || path.starts_with(o))
}

pub(crate) fn resolve_safe_tmp_roots(roots: &[String]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for configured in roots {
        let configured = configured.trim();
        if configured.is_empty() {
            continue;
        }
        let requested = expand_tilde(configured);
        let safe_root = check_safe_tmp_root(&requested)
            .with_context(|| format!("invalid tmp_search_paths entry {:?}", configured))?;
        if !resolved.contains(&safe_root) {
            resolved.push(safe_root);
        }
    }
    Ok(resolved)
}

/// e2e-litter fast path for /tmp (ADDED 2026-10-12, obvious-fat increment).
///
/// Machine-generated test-copy namespaces (`reconcile-*`, `*-shield-*`,
/// `cw-browser-test-*`, ...) are never user documents, so they are judged
/// against a SHORTER age gate (`e2e_min_age_hours`, default 6) than the
/// general tmp gate. The 2026-10-11 incident left ~29 GiB of reconcile-e2e
/// copies 20h old — under the general 24h gate the guard would never touch
/// them before the disk went critical.
///
/// The fast path owns ONLY the age window the general pass skips: entries
/// whose whole tree is older than `general_cutoff` are left for the general
/// pass, so dry-run accounting can never double-count an entry. Every other
/// protection is identical to the general pass (open fd / cwd / exe scan,
/// symlink skip, protected paths, fail-closed tree walk). Deletion is DIRECT,
/// never stash: /tmp is already disposable, and copying tens of GiB to
/// stash just to delete it later defeats the purpose.
///
/// Returns `(reclaimed_bytes, cleaned, denied)` where `denied` holds entries
/// that could not be removed (typically root-owned leftovers needing sudo).
pub(crate) fn parse_tmp_e2e_patterns(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Minimal `*` glob over a tmp entry basename: exact, `prefix*`, `*suffix`,
/// `*infix*`, and bare `*`. Case-sensitive; tmp litter names are
/// machine-generated lowercase. No dependency, no regex — the pattern list
/// is operator config, not user input under test.
pub(crate) fn tmp_e2e_match(file_name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        let p = p.trim();
        if p.is_empty() {
            return false;
        }
        if p == "*" {
            return true;
        }
        let starts_star = p.starts_with('*');
        let ends_star = p.ends_with('*');
        let core = p.trim_matches('*');
        match (starts_star, ends_star) {
            (false, false) => file_name == p,
            (true, false) => file_name.ends_with(core),
            (false, true) => file_name.starts_with(core),
            (true, true) => file_name.contains(core),
        }
    })
}

/// Stash-move budget in bytes. 0 = unlimited (`u64::MAX`, never hit by
/// a saturating add). Pure so the boundary is unit-testable.
pub(crate) fn stash_budget_bytes(max_mb: u64) -> u64 {
    if max_mb == 0 {
        u64::MAX
    } else {
        max_mb.saturating_mul(1024).saturating_mul(1024)
    }
}

/// True when moving `entry_bytes` would exceed the per-pass stash
/// budget. Pure boundary helper: exactly-at-budget is allowed, one byte
/// over defers.
pub(crate) fn stash_over_budget(moved_bytes: u64, entry_bytes: u64, budget_bytes: u64) -> bool {
    moved_bytes.saturating_add(entry_bytes) > budget_bytes
}

/// Cutoff `SystemTime` for an age gate expressed in hours. Saturating: an
/// absurd value collapses to the epoch ("delete nothing") instead of
/// panicking — same hardening as the inline computation in
/// `clean_tmp_paths_with_proc`.
pub(crate) fn tmp_age_cutoff(min_age_hours: u64) -> Result<SystemTime> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let age = Duration::from_secs(min_age_hours.saturating_mul(3_600));
    Ok(UNIX_EPOCH + now.checked_sub(age).unwrap_or(Duration::ZERO))
}

pub(crate) async fn clean_tmp_e2e_fast_path(
    apply: bool,
    roots: &[String],
    general_cutoff: SystemTime,
    e2e_patterns: &[String],
    e2e_min_age_hours: u64,
    bypass_age_gate: bool,
    protected_paths: &[String],
) -> Result<(u64, Vec<String>, Vec<String>)> {
    let live_sockets = live_unix_socket_paths();
    clean_tmp_e2e_fast_path_with_proc(
        apply,
        roots,
        general_cutoff,
        e2e_patterns,
        e2e_min_age_hours,
        bypass_age_gate,
        protected_paths,
        Path::new("/proc"),
        &live_sockets,
    )
    .await
}

pub(crate) async fn clean_tmp_e2e_fast_path_with_proc(
    apply: bool,
    roots: &[String],
    general_cutoff: SystemTime,
    e2e_patterns: &[String],
    e2e_min_age_hours: u64,
    bypass_age_gate: bool,
    protected_paths: &[String],
    proc_root: &Path,
    live_sockets: &HashSet<PathBuf>,
) -> Result<(u64, Vec<String>, Vec<String>)> {
    let mut reclaimed = 0u64;
    let mut cleaned = Vec::new();
    let mut denied: Vec<String> = Vec::new();
    if roots.is_empty() || e2e_patterns.is_empty() || e2e_min_age_hours == 0 {
        return Ok((0, cleaned, denied));
    }
    let root_paths = resolve_safe_tmp_roots(roots)?;
    if root_paths.is_empty() {
        return Ok((0, cleaned, denied));
    }
    let open_paths = match collect_open_paths_under_from(proc_root, &root_paths).await {
        Some(set) => set,
        None => anyhow::bail!(
            "open-file scan failed for {} — refusing tmp fast-path blind (cannot verify nothing is held open)",
            proc_root.display()
        ),
    };
    // At the critical tier the e2e age gate is lifted (mirrors the rust-target
    // critical bypass: test copies are regeneratable, a full disk is not).
    // Open/cwd/exe + symlink + protected checks below still apply, so a copy
    // a live test run is writing to is never touched.
    let fast_cutoff = if bypass_age_gate {
        SystemTime::now()
    } else {
        tmp_age_cutoff(e2e_min_age_hours)?
    };
    for root in &root_paths {
        let mut rd = match tokio::fs::read_dir(root).await {
            Ok(rd) => rd,
            Err(e) => {
                eprintln!(
                    "⚠️ Tmp fast-path: cannot list {} — skipping ({e})",
                    root.display()
                );
                continue;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            match entry.file_type().await {
                Ok(t) if t.is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !tmp_e2e_match(&name, e2e_patterns) {
                continue;
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                _ => continue,
            };
            // Exclusive window with the general pass: a tree entirely older
            // than the general cutoff is a general-pass candidate — leave it
            // so dry-run accounting never counts an entry twice.
            if meta.is_dir() {
                if !tree_has_fresh_content(&path, general_cutoff).await {
                    continue;
                }
                if tree_has_fresh_content(&path, fast_cutoff).await {
                    continue;
                }
            } else {
                let modified = match meta.modified() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if modified <= general_cutoff {
                    continue;
                }
                if modified > fast_cutoff {
                    continue;
                }
            }
            // FIXED 2026-10-24 (audit): all three tmp removal paths (the e2e
            // fast path, the general floor and the size-aware floor) now ask
            // ONE helper which gate, if any, stops the removal. The three
            // used to be spelled out separately, and the size-aware floor
            // drifted past all of them.
            if let Some(block) =
                tmp_entry_block(&path, root, protected_paths, &open_paths, live_sockets)
            {
                eprintln!(
                    "🛡️ keeping tmp entry {}: {}",
                    path.display(),
                    block.reason()
                );
                continue;
            }
            let entry_size = if meta.is_dir() {
                get_dir_size(&path).await.unwrap_or(0)
            } else {
                meta.len()
            };
            if !apply {
                reclaimed += entry_size;
                cleaned.push(format!(
                    "{} ({}, dry-run)",
                    path.display(),
                    human_bytes(entry_size)
                ));
                continue;
            }
            let removed = if meta.is_dir() {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            match removed {
                Ok(()) => {
                    reclaimed += entry_size;
                    cleaned.push(format!("{} ({})", path.display(), human_bytes(entry_size)));
                }
                Err(e) => {
                    // Typically root-owned test leftovers (e.g. docker-spawned
                    // build outputs) the unprivileged guard cannot unlink.
                    eprintln!("⚠️ failed to remove tmp entry {}: {}", path.display(), e);
                    denied.push(path.display().to_string());
                }
            }
        }
    }
    Ok((reclaimed, cleaned, denied))
}

/// The age a floor expresses, saturating instead of overflowing.
///
/// CHANGED 2026-10-22: shared by the general tmp floor and the size-aware
/// one, so both saturate the same way (audit F41's underflow hardening:
/// an absurd floor must mean "delete nothing", never a panic).
pub(crate) fn age_of(hours: u64) -> Duration {
    Duration::from_secs(hours.saturating_mul(3_600))
}

/// Why a tmp entry must not be deleted, if it must not be deleted.
///
/// FIXED 2026-10-24 (audit): the size-aware floor used to remove a large
/// entry WITHOUT consulting any of these gates, because it ran before them.
/// `remove_tmp_entry` performs no checks of its own, so the order of the
/// checks in the caller was the entire defence. This helper makes the three
/// gates a single named thing that every removal path must pass, so a second
/// path cannot forget one of them.
///
/// The gates, in order:
///   1. an open file/cwd/exe under the entry — a live process is using it;
///   2. a bound unix socket inside it — deleting the socket PATH strands a
///      live daemon (codex app-server, 2026-10-21);
///   3. the protected-path and tmp-root containment checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TmpEntryBlock {
    /// A live process holds a file, cwd or exe under the entry.
    HeldOpen,
    /// The entry carries a bound unix socket.
    LiveSocket,
    /// The entry is under a protected path, or outside the validated tmp root.
    Protected,
}

impl TmpEntryBlock {
    /// The line to print when this block stops a removal.
    fn reason(self) -> &'static str {
        match self {
            TmpEntryBlock::HeldOpen => "held open by a live process",
            TmpEntryBlock::LiveSocket => "holds a live unix socket",
            TmpEntryBlock::Protected => "protected path",
        }
    }
}

/// Which gate, if any, stops this tmp entry from being deleted.
pub(crate) fn tmp_entry_block(
    path: &Path,
    root: &Path,
    protected_paths: &[String],
    open_paths: &HashSet<PathBuf>,
    live_sockets: &HashSet<PathBuf>,
) -> Option<TmpEntryBlock> {
    if path_has_open_ancestor(path, open_paths) {
        return Some(TmpEntryBlock::HeldOpen);
    }
    if path_holds_live_unix_socket(path, live_sockets) {
        return Some(TmpEntryBlock::LiveSocket);
    }
    if check_safe_to_delete_tmp_entry(path, root, protected_paths).is_err() {
        return Some(TmpEntryBlock::Protected);
    }
    None
}

/// What one tmp cleanup pass found.
#[derive(Debug, Default)]
pub(crate) struct TmpCleanupOutcome {
    /// Bytes removed — or that would be removed, in dry-run.
    pub(crate) reclaimed: u64,
    /// Per-entry notes for the report.
    pub(crate) cleaned: Vec<String>,
    /// Entries that could not be removed: they belong to another user.
    /// ADDED 2026-10-22: aggregated. The per-entry failure line fired once
    /// per root-owned entry per cycle (a dozen lines a cycle on this host),
    /// which buried the lines that matter.
    pub(crate) denied: Vec<String>,
    /// Bytes held back ONLY by the age floor: stale by content, unheld and
    /// unprotected, but too young for the floor that applies to them.
    /// ADDED 2026-10-22: the disk alarms are percent-used and rapid fill;
    /// neither of them sees the scratch that is sitting there waiting for
    /// its floor to expire.
    pub(crate) age_blocked_bytes: u64,
    /// How many entries make up `age_blocked_bytes`.
    pub(crate) age_blocked_entries: usize,
    /// The subset of `age_blocked_bytes` held by the SIZE-AWARE floor: large
    /// entries that are old enough to be scratch but not yet old enough for
    /// the short floor.
    ///
    /// ADDED 2026-10-24 (audit): the two floors are unlocked by DIFFERENT
    /// knobs, so one total cannot give one piece of advice. Telling the
    /// operator to lower `tmp_min_age_hours` when the bytes are held by
    /// `tmp_large_min_age_hours` points at the knob that frees nothing.
    pub(crate) large_blocked_bytes: u64,
    /// How many entries make up `large_blocked_bytes`.
    pub(crate) large_blocked_entries: usize,
}

/// Age-based top-level /tmp cleanup (ADDED 2026-08-25, v0.112.39).
/// Removes TOP-LEVEL entries of each root whose mtime is older than
/// `min_age_hours`, skipping symlinks, protected paths, and anything
/// currently held open by a process. Dry-run only measures.
///
/// ADDED 2026-10-22: a size-aware floor. An entry of at least
/// `large_entry_bytes` is judged against `large_min_age_hours` instead of the
/// general floor, so a bulk scratch clone does not have to wait the general
/// floor out. It is only consulted when `use_large_floor` is set (the
/// proactive/action tier), because deciding it costs a size measurement per
/// entry and that walk is only worth paying when the disk is filling.
/// Whether the scratch the age floor is holding back is worth a warning.
///
/// ADDED 2026-10-22: the disk alarms are percent-used and rapid fill;
/// neither of them sees the scratch that is sitting there waiting for its
/// floor. 0 disables the warning.
pub(crate) fn scratch_warning_threshold_exceeded(age_blocked_bytes: u64, warn_gib: u64) -> bool {
    warn_gib > 0 && age_blocked_bytes >= warn_gib.saturating_mul(1024 * 1024 * 1024)
}

/// The general-floor form: no size-aware floor.
///
/// Kept as the short form for the callers that want exactly the
/// pre-2026-10-22 behaviour (and for the tests, which exercise the general
/// floor on its own). The production path uses the full form below.
#[cfg(test)]
pub(crate) async fn clean_tmp_paths(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    protected_paths: &[String],
) -> Result<TmpCleanupOutcome> {
    clean_tmp_paths_with_floor(apply, roots, min_age_hours, 0, 0, false, protected_paths).await
}

/// The full form, with the size-aware floor.
///
/// ADDED 2026-10-22: see `clean_tmp_paths` for the size-aware floor's
/// rationale — a bulk scratch clone is judged against a short floor instead
/// of the general one, but only when the caller says the disk is filling.
pub(crate) async fn clean_tmp_paths_with_floor(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    large_entry_bytes: u64,
    large_min_age_hours: u64,
    use_large_floor: bool,
    protected_paths: &[String],
) -> Result<TmpCleanupOutcome> {
    let live_sockets = live_unix_socket_paths();
    clean_tmp_paths_with_proc(
        apply,
        roots,
        min_age_hours,
        large_entry_bytes,
        large_min_age_hours,
        use_large_floor,
        protected_paths,
        Path::new("/proc"),
        &live_sockets,
    )
    .await
}

pub(crate) async fn clean_tmp_paths_with_proc(
    apply: bool,
    roots: &[String],
    min_age_hours: u64,
    large_entry_bytes: u64,
    large_min_age_hours: u64,
    use_large_floor: bool,
    protected_paths: &[String],
    proc_root: &Path,
    live_sockets: &HashSet<PathBuf>,
) -> Result<TmpCleanupOutcome> {
    let mut out = TmpCleanupOutcome::default();
    if roots.is_empty() || min_age_hours == 0 {
        return Ok(out);
    }
    let root_paths = resolve_safe_tmp_roots(roots)?;
    if root_paths.is_empty() {
        return Ok(out);
    }
    let open_paths = match collect_open_paths_under_from(proc_root, &root_paths).await {
        Some(set) => set,
        // R3-L31: without the open-file scan, cleanup would delete
        // files held open by live processes with no warning. Skip the
        // pass LOUD in both modes (dry-run would otherwise over-report
        // reclaimable); the caller prints the error.
        None => anyhow::bail!(
            "open-file scan failed for {} — refusing tmp cleanup blind (cannot verify nothing is held open)",
            proc_root.display()
        ),
    };
    // CHANGED 2026-09-09 (audit F41): same underflow hardening as the
    // trash cutoff above — absurd min_age_hours saturates to "delete
    // nothing" instead of panicking.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let cutoff = UNIX_EPOCH
        + now
            .checked_sub(age_of(min_age_hours))
            .unwrap_or(Duration::ZERO);
    // The size-aware floor. A SHORTER age means a LATER cutoff, and a later
    // cutoff judges MORE content old enough to delete — so the short floor
    // is the permissive one for deletion, and its freshness check is also
    // the cheaper walk (it stops at the first recent entry).
    let large_cutoff = UNIX_EPOCH
        + now
            .checked_sub(age_of(large_min_age_hours))
            .unwrap_or(Duration::ZERO);
    // The size-aware floor only applies when its floor is SHORTER than the
    // general one: that is what makes it a shortcut to reclaiming bulk
    // scratch sooner rather than a second, stricter gate.
    //
    // FIXED 2026-10-24 (audit): this used to be decided by comparing the two
    // computed cutoffs, which is the same test in the normal case but
    // silently degraded to "off" when either age saturated. A configured
    // `tmp_large_min_age_hours` that is NOT shorter than the general floor
    // cannot do what its name says — large entries would still be judged
    // against the general floor — so it is reported as a misconfiguration
    // when the policy is loaded rather than ignored here.
    let large_floor_active = use_large_floor
        && large_min_age_hours > 0
        && large_entry_bytes > 0
        && large_min_age_hours < min_age_hours;
    for root in &root_paths {
        let mut rd = match tokio::fs::read_dir(root).await {
            Ok(rd) => rd,
            // FIXED 2026-10-03 (audit R4-SYS-13): skipping stays
            // deletion-safe, but silence under-reports dry-run —
            // name the skipped root like the proc-scan refusal does.
            Err(e) => {
                eprintln!("⚠️ Tmp: cannot list {} — skipping ({e})", root.display());
                continue;
            }
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            // Never follow or remove symlinks in tmp roots.
            // CHANGED 2026-09-09 (audit F40): `entry.metadata()`
            // FOLLOWS symlinks, so `is_symlink()` was never true and
            // every symlink fell through to the target-typed path.
            // `file_type()` does not follow — skip links up front.
            // (The downstream guard check also refuses symlinks, so this
            // was defense-in-depth theater plus scary log spam, not a
            // live deletion hole.)
            match entry.file_type().await {
                Ok(t) if t.is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                _ => continue,
            };
            let modified = match meta.modified() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Files judge by their own mtime. Directories cannot: dir
            // mtime only bumps on direct-child changes, so a stale-looking
            // top dir can hide freshly written nested files. Judge the
            // tree by its newest entry instead.
            let fresh = if meta.is_dir() {
                tree_has_fresh_content(&path, cutoff).await
            } else {
                modified > cutoff
            };
            if fresh {
                // Fresh content against the GENERAL floor.
                //
                // FIXED 2026-10-24 (audit): the size is measured for EVERY
                // fresh entry, not only when the size-aware floor is
                // consulted. Two reasons:
                //
                //  1. the size-aware floor needs it, and
                //  2. the held-back report needs it — and that report must
                //     not depend on the size-aware floor being active. It
                //     used to: below the proactive threshold, and with the
                //     size-aware floor disabled, the guard said NOTHING
                //     about the scratch it was holding back.
                //
                // The sweep runs on the proactive/action cadence (every
                // 30-60 minutes), not once per guard pass, so the extra
                // walk is affordable here.
                let size = tmp_entry_size(&path, &meta).await;
                // The size-aware floor: a large entry may still be
                // reclaimable — measure it and re-check against the short
                // floor.
                if large_floor_active && size >= large_entry_bytes {
                    let fresh_short = if meta.is_dir() {
                        tree_has_fresh_content(&path, large_cutoff).await
                    } else {
                        modified > large_cutoff
                    };
                    if !fresh_short {
                        // FIXED 2026-10-24 (audit): the size-aware floor
                        // removed the entry HERE, before the three
                        // liveness/protection gates below — so a large
                        // entry could be deleted while a live process
                        // held it open, while it carried a bound unix
                        // socket (the 2026-10-21 codex-daemon failure
                        // mode), or while it sat under a protected path.
                        // The size-aware floor only makes the AGE floor
                        // shorter; it must never make the safety gates
                        // weaker. `remove_tmp_entry` has no gates of its
                        // own, so the order here was the whole defence.
                        //
                        // This path IS the one that would have deleted the
                        // entry, so it says why it did not.
                        match tmp_entry_block(
                            &path,
                            root,
                            protected_paths,
                            &open_paths,
                            live_sockets,
                        ) {
                            Some(block) => {
                                eprintln!(
                                    "🛡️ keeping tmp entry {}: {}",
                                    path.display(),
                                    block.reason()
                                );
                            }
                            None => {
                                remove_tmp_entry(&path, &meta, size, apply, &mut out).await;
                            }
                        }
                        continue;
                    }
                    // Held by the SIZE-AWARE floor: large enough for it,
                    // but still fresh against the short floor. Counted
                    // below and named separately, so the advice says which
                    // knob actually frees these bytes.
                    if tmp_entry_block(&path, root, protected_paths, &open_paths, live_sockets)
                        .is_none()
                    {
                        out.large_blocked_bytes += size;
                        out.large_blocked_entries += 1;
                        out.age_blocked_bytes += size;
                        out.age_blocked_entries += 1;
                    }
                    continue;
                }
                // Held back by the age rule: its content is fresh
                // against the general floor. Counted whatever its size —
                // the long tail of SMALL entries is where the incident's
                // 62.7 GiB actually sat.
                //
                // FIXED 2026-10-24 (audit): only count the entry when the
                // age floor is what is actually holding it back. An entry
                // that a live process holds open, that carries a bound
                // unix socket, or that sits under a protected path would
                // NOT be reclaimed by lowering the floor, so counting it
                // here told the operator "lower tmp_min_age_hours to free
                // 30 GiB" when lowering it frees nothing.
                //
                // The gate is asked QUIETLY here: a fresh entry that is
                // protected is not news — it is simply not a candidate,
                // and the same entry would print the same line every
                // pass otherwise.
                if tmp_entry_block(&path, root, protected_paths, &open_paths, live_sockets)
                    .is_none()
                {
                    out.age_blocked_bytes += size;
                    out.age_blocked_entries += 1;
                }
                continue; // fresh content
            }
            // FIXED 2026-10-24 (audit): the general floor and the size-aware
            // floor share ONE gate helper, so neither can drift past the
            // open-file / live-socket / protected-path checks again.
            if let Some(block) =
                tmp_entry_block(&path, root, protected_paths, &open_paths, live_sockets)
            {
                eprintln!(
                    "🛡️ keeping tmp entry {}: {}",
                    path.display(),
                    block.reason()
                );
                continue;
            }
            let entry_size = tmp_entry_size(&path, &meta).await;
            if !apply {
                out.reclaimed += entry_size;
                out.cleaned.push(format!(
                    "{} ({}, dry-run)",
                    path.display(),
                    human_bytes(entry_size)
                ));
                continue;
            }
            remove_tmp_entry(&path, &meta, entry_size, apply, &mut out).await;
        }
    }
    Ok(out)
}

/// The size of a tmp entry: a directory is measured by its tree, a file by
/// its own length.
pub(crate) async fn tmp_entry_size(path: &Path, meta: &std::fs::Metadata) -> u64 {
    if meta.is_dir() {
        get_dir_size(path).await.unwrap_or(0)
    } else {
        meta.len()
    }
}

/// Remove one tmp entry, recording the outcome.
///
/// ADDED 2026-10-22: a removal that fails because the entry belongs to
/// another user is aggregated instead of logged per entry per cycle, and an
/// entry that is held back only by the age floor is counted so the guard can
/// report the scratch it is not reclaiming yet.
pub(crate) async fn remove_tmp_entry(
    path: &Path,
    meta: &std::fs::Metadata,
    entry_size: u64,
    apply: bool,
    out: &mut TmpCleanupOutcome,
) {
    if !apply {
        out.reclaimed += entry_size;
        out.cleaned.push(format!(
            "{} ({}, dry-run)",
            path.display(),
            human_bytes(entry_size)
        ));
        return;
    }
    let removed = if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    match removed {
        Ok(()) => {
            out.reclaimed += entry_size;
            out.cleaned
                .push(format!("{} ({})", path.display(), human_bytes(entry_size)));
        }
        Err(e) => {
            out.denied.push(format!("{}: {}", path.display(), e));
        }
    }
}
