//! Rust `target/` discovery and the auto/proactive cleanup passes.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// Find all Rust target directories under the given search roots
pub(crate) async fn find_rust_target_dirs(roots: &[PathBuf]) -> Result<Vec<TargetDirInfo>> {
    use walkdir::WalkDir;

    let mut targets = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root)
            // CHANGED 2026-09-20 (space audit): 5 -> 8. Nested game repos
            // sit at ~/Dev/dracon-platform/web/games/<wip|released>/<name>/
            // so their target dirs are at depth 6 — the old cap made every
            // nested-game build cache invisible to the guard.
            .max_depth(8)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }

            if entry.file_name() != "target" {
                continue;
            }

            let path = entry.path().to_path_buf();

            // Check if there's a Cargo.toml in parent (confirm it's a Rust project)
            let parent = match path.parent() {
                Some(p) => p,
                None => continue,
            };

            if !parent.join("Cargo.toml").exists() {
                continue;
            }

            // Get directory size (in-process walk; unreadable trees measure 0
            // and fall out at the min-size gate below).
            let bytes = match get_dir_size(&path).await {
                Ok(b) => b,
                Err(_) => continue,
            };

            let mtime_secs_ago = match fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(mtime) => SystemTime::now()
                    .duration_since(mtime)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };

            targets.push(TargetDirInfo {
                path,
                bytes,
                mtime_secs_ago,
            });
        }
    }

    // Sort ascending (smallest first); iteration cleans all above threshold so order is arbitrary
    targets.sort_by_key(|a| a.bytes);

    Ok(targets)
}

/// Get directory size using du command
pub(crate) async fn get_dir_size(path: &Path) -> Result<u64> {
    // CHANGED 2026-09-20 (space audit): in-process WalkDir sum. The old
    // body shelled out to `du -sb` once PER CALL — with 580k top-level
    // /tmp entries that was 580k forks (~34/s under the service's 20%
    // CPUQuota), turning one tmp pass into a multi-hour window with the
    // whole daemon loop (memory mitigation included) blocked behind it.
    // File bytes only (directory entries themselves excluded): this is the
    // established contract — `active_package_operations_...` asserts a
    // 5-byte fixture measures exactly 5 — and the useful "reclaimable
    // bytes" semantic for cleanup accounting. Links not followed.
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// Action-level lingering gate for Rust target cleanup (2026-09-14).
///
/// A target dir touched within `min_age_days` is an active project, not a
/// cleanup candidate — even when disk pressure is at action/critical level.
/// `min_age_days == 0` disables the gate (the pre-fix delete-anything
/// posture). Pure so the guard loop and unit tests share one definition.
pub(crate) fn rust_target_old_enough_for_action_cleanup(
    mtime_secs_ago: u64,
    min_age_days: u64,
) -> bool {
    min_age_days == 0 || mtime_secs_ago >= min_age_days.saturating_mul(24).saturating_mul(3600)
}

/// Perform automatic cleanup of Rust target directories.
///
/// `bypass_age_gate` is true only on the daemon's critical tier (disk at or
/// above `disk_critical_percent`): at that point a full disk breaks the
/// machine, so rebuildable targets are candidates even when freshly
/// touched. Active-build, 60s-mtime and protected-path checks still apply —
/// only the lingering age gate is lifted.
pub(crate) async fn auto_cleanup_rust_targets(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    apply: bool,
    bypass_age_gate: bool,
) -> Result<AutoCleanupResult> {
    let mut result = AutoCleanupResult {
        cleaned_count: 0,
        reclaimed_bytes: 0,
        cleaned_paths: Vec::new(),
        protected_paths: Vec::new(),
    };

    // Parse search roots
    let roots: Vec<PathBuf> = guard
        .rust_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if roots.is_empty() {
        return Ok(result);
    }

    // Find all target directories
    let targets = find_rust_target_dirs(&roots).await?;

    // Detect active builds — one of TWO protection mechanisms (the other is
    // the lingering age gate below). We protect target dirs where
    // cargo/rustc is actively running.
    let active_builds = detect_active_rust_builds().await?;
    state.active_build_pids = active_builds.clone();

    // Get CWDs of active builds to protect their target dirs
    let mut protected_project_dirs: Vec<PathBuf> = Vec::new();
    for pid in &active_builds {
        if let Some(cwd) = get_process_cwd(*pid).await {
            // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7): walk
            // ALL ancestors collecting EVERY dir with a Cargo.toml,
            // not just the FIRST. In a cargo workspace, running
            // `cargo build` from a member crate (`ws/crates/foo`)
            // stops at the member's Cargo.toml, but the shared
            // target dir lives at the WORKSPACE root (`ws/target`) —
            // the pre-fix exact-equality check then failed and
            // `ws/target` was deleted mid-build.
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir.clone());
                }
                dir = parent.to_path_buf();
            }
        }
    }

    let min_size_bytes = guard
        .cleanup_min_size_mb
        .saturating_mul(1024)
        .saturating_mul(1024);

    // ADDED 2026-10-12 (stall guard): cap cross-device stash-move bytes
    // per pass. 2026-10-04 disabled this whole path after repeated 10-30 GiB
    // copies to /mnt/data/stash stalled the desktop. The budget is
    // reserved at gate time in BOTH dry-run and apply so preview == apply;
    // a failed move wasting budget only defers others — safe direction.
    // Direct deletes (stash-first off) are uncapped: no copy, no stall.
    let stash_budget = stash_budget_bytes(guard.stash_max_bytes_per_pass_mb);
    let mut stashed_bytes = 0u64;

    for target in targets {
        // Skip if too small
        if target.bytes < min_size_bytes {
            continue;
        }

        // CHANGED 2026-09-14: lingering gate — a target touched within
        // rust_target_action_min_age_days (default 7) belongs to an active
        // project and is never an action-level candidate, no matter how big
        // it is. The old code deleted anything above min_size (modulo the
        // 60s backstop), which thrashed daily drivers: each deleted target
        // was rebuilt within hours, pushing disk straight back over the
        // action line for the next cycle. Bonus: a just-rebuilt target is
        // fresh by definition, so the gate doubles as a re-clean cooldown.
        // CHANGED 2026-09-20 (space audit): at the critical tier a full
        // disk is worse than a rebuild — the gate is lifted (a 79 GiB
        // freshly-built workspace target held a 100% disk hostage because
        // every reclaim path deferred to freshness).
        if !bypass_age_gate
            && !rust_target_old_enough_for_action_cleanup(
                target.mtime_secs_ago,
                guard.rust_target_action_min_age_days,
            )
        {
            result.protected_paths.push(format!(
                "{} (touched {}d ago, under action min-age {}d)",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                guard.rust_target_action_min_age_days
            ));
            continue;
        }

        // Only skip if there's an ACTIVELY RUNNING cargo/rustc in this project
        let target_project = target.path.parent().unwrap_or(&target.path);
        // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7):
        // ancestor-aware protection — a target dir is protected when
        // a protected project is EQUAL to its project dir, is an
        // ANCESTOR of it (workspace root building a nested member),
        // or is a DESCENDANT of it (member crate building into the
        // workspace-root target). The pre-fix exact-equality check
        // missed the workspace-member case entirely.
        let has_active_build = protected_project_dirs.iter().any(|proj| {
            proj == target_project
                || proj.starts_with(target_project)
                || target_project.starts_with(proj)
        });

        // ADDED 2026-07-21 (v0.112.33, audit M33/F4.7): cheap mtime
        // backstop — a target dir modified in the last 60s is almost
        // certainly being written by a build RIGHT NOW (builds touch
        // it constantly), regardless of process detection (which
        // misses `cargo build --manifest-path ...` run from an
        // unrelated CWD).
        let recently_active = std::fs::metadata(&target.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() < 60);

        if has_active_build || recently_active {
            result.protected_paths.push(format!(
                "{} ({})",
                target.path.display(),
                if has_active_build {
                    "active cargo/rustc process"
                } else {
                    "modified <60s ago (active build)"
                }
            ));
            continue;
        }

        // FIXED 2026-10-02: the user-protected-path refusal used to live INSIDE
        // the `if apply` block. In a dry run that block is skipped entirely, so
        // the preview listed every protected tree as reclaimable and only
        // refused it at apply time — overstating reclaimable space by however
        // large the protected workspace is (~66 GiB on this host), which misled
        // the operator twice in one session.
        //
        // Hoisted so preview and apply go through ONE check and cannot
        // disagree, rather than adding a second filter that would drift. The
        // reason travels with the verdict, matching the convention the
        // active-build protections above already use.
        if let Err(e) = check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
            if apply {
                eprintln!("⚠️ skipping {}: {:#}", target.path.display(), e);
            }
            result.protected_paths.push(format!(
                "{} ({})",
                target.path.display(),
                one_line(&e.to_string())
            ));
            continue;
        }

        // ADDED 2026-09-27 (space tiers Phase 2): stash-first routing —
        // move to stash (TTL'd, restorable) instead of deleting.
        // ADDED 2026-10-12 (stall guard): over-budget entries defer with an
        // explicit operator note instead of stalling the desktop mid-pass.
        if guard.clean_stash_first && stash_over_budget(stashed_bytes, target.bytes, stash_budget) {
            result.protected_paths.push(format!(
                "{} ({}, over per-pass stash budget {}, deferred — needs operator)",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(stash_budget)
            ));
            continue;
        }
        if guard.clean_stash_first {
            stashed_bytes = stashed_bytes.saturating_add(target.bytes);
        }
        let mut stashed = false;
        let mut freed = target.bytes;
        if apply {
            if guard.clean_stash_first {
                let qdir = stash_root(guard);
                match stash_first_remove(&target.path, &qdir, &guard.protected_paths) {
                    Ok((manifest, f)) => {
                        stashed = true;
                        freed = f;
                        eprintln!(
                            "📦 Rust stashed: {} as {}",
                            target.path.display(),
                            manifest.name
                        );
                    }
                    Err(e) => {
                        eprintln!("⚠️ failed to stash {}: {}", target.path.display(), e);
                        continue;
                    }
                }
            } else {
                // `check_safe_to_delete_guard` already ran above for BOTH paths,
                // so the canonical path is re-derivable without a second
                // refusal branch that could drift from it.
                let safe_path = target.path.canonicalize().unwrap_or(target.path.clone());
                if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                    eprintln!("⚠️ failed to remove {}: {}", target.path.display(), e);
                    continue;
                }
            }
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += freed;
        if stashed {
            result.cleaned_paths.push(format!(
                "{} (stashed, {}, freed {})",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(freed)
            ));
        } else {
            result.cleaned_paths.push(format!(
                "{} ({})",
                target.path.display(),
                human_bytes(target.bytes)
            ));
        }
    }

    Ok(result)
}

/// Proactive cleanup: remove stale Rust target dirs (older than max_age_days)
/// even when disk is not at action/critical level. Only cleans targets that
/// haven't been touched in a while, skipping actively-built projects.
pub(crate) async fn proactive_cleanup_rust_targets(
    guard: &GuardPolicy,
    state: &mut GuardRuntimeState,
    apply: bool,
) -> Result<AutoCleanupResult> {
    let mut result = AutoCleanupResult {
        cleaned_count: 0,
        reclaimed_bytes: 0,
        cleaned_paths: Vec::new(),
        protected_paths: Vec::new(),
    };

    let roots: Vec<PathBuf> = guard
        .rust_search_roots
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let p = expand_tilde(s);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if roots.is_empty() {
        return Ok(result);
    }

    let targets = find_rust_target_dirs(&roots).await?;
    let active_builds = detect_active_rust_builds().await?;
    state.active_build_pids = active_builds.clone();

    let mut protected_project_dirs: Vec<PathBuf> = Vec::new();
    for pid in &active_builds {
        if let Some(cwd) = get_process_cwd(*pid).await {
            // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7): walk
            // ALL ancestors collecting EVERY dir with a Cargo.toml
            // (workspace-member case — see the proactive path above).
            let mut dir = cwd.clone();
            while let Some(parent) = dir.parent() {
                if dir.join("Cargo.toml").exists() {
                    protected_project_dirs.push(dir.clone());
                }
                dir = parent.to_path_buf();
            }
        }
    }

    let min_size_bytes = guard
        .cleanup_min_size_mb
        .saturating_mul(1024)
        .saturating_mul(1024);
    let max_age_secs = guard
        .rust_target_max_age_days
        .saturating_mul(24)
        .saturating_mul(3600);

    // ADDED 2026-10-12 (stall guard): same per-pass stash-move budget
    // as the action path — a 14d-stale forgotten target is exactly the huge
    // cross-device copy that stalled the desktop in 2026-10-04.
    let stash_budget = stash_budget_bytes(guard.stash_max_bytes_per_pass_mb);
    let mut stashed_bytes = 0u64;

    for target in targets {
        if target.bytes < min_size_bytes {
            continue;
        }

        if target.mtime_secs_ago < max_age_secs {
            continue;
        }

        let target_project = target.path.parent().unwrap_or(&target.path);
        // CHANGED 2026-07-21 (v0.112.33, audit M33/F4.7):
        // ancestor-aware protection (workspace-member case — see the
        // proactive path above).
        let has_active_build = protected_project_dirs.iter().any(|proj| {
            proj == target_project
                || proj.starts_with(target_project)
                || target_project.starts_with(proj)
        });

        if has_active_build {
            result.protected_paths.push(format!(
                "{} (active cargo/rustc process)",
                target.path.display()
            ));
            continue;
        }

        // ADDED 2026-09-27 (space tiers Phase 2): stash-first routing.
        // ADDED 2026-10-12 (stall guard): over-budget entries defer with an
        // explicit operator note instead of stalling the desktop mid-pass.
        if guard.clean_stash_first && stash_over_budget(stashed_bytes, target.bytes, stash_budget) {
            result.protected_paths.push(format!(
                "{} ({}, over per-pass stash budget {}, deferred — needs operator)",
                target.path.display(),
                human_bytes(target.bytes),
                human_bytes(stash_budget)
            ));
            continue;
        }
        if guard.clean_stash_first {
            stashed_bytes = stashed_bytes.saturating_add(target.bytes);
        }
        let mut stashed = false;
        let mut freed = target.bytes;
        // FIXED 2026-10-24 (audit): the user-protected-path refusal used to
        // live INSIDE the `if apply` block — exactly the defect that was
        // fixed in the ACTION tier on 2026-10-02 ("overstating reclaimable
        // space ... misled the operator twice in one session"). In a dry run
        // that block is skipped entirely, so the preview listed every
        // protected tree as reclaimable AND counted its bytes: a protected
        // workspace of any size inflated the proactive tier's reclaimable
        // figure by the size of the workspace it was protecting.
        //
        // Hoisted so preview and apply go through ONE check and cannot
        // disagree. It covers the stash-first branch too: the stash move
        // runs the SAME `check_safe_to_delete_guard` internally
        // (`stash_move_inner`), so a protected tree is not reclaimable
        // either way and the preview must say so.
        match check_safe_to_delete_guard(&target.path, &guard.protected_paths) {
            Ok(safe_path) => {
                if apply {
                    if guard.clean_stash_first {
                        let qdir = stash_root(guard);
                        match stash_first_remove(&target.path, &qdir, &guard.protected_paths) {
                            Ok((manifest, f)) => {
                                stashed = true;
                                freed = f;
                                eprintln!(
                                    "📦 proactive stashed: {} as {}",
                                    target.path.display(),
                                    manifest.name
                                );
                            }
                            Err(e) => {
                                eprintln!(
                                    "⚠️ proactive: failed to stash {}: {}",
                                    target.path.display(),
                                    e
                                );
                                continue;
                            }
                        }
                    } else if let Err(e) = tokio::fs::remove_dir_all(&safe_path).await {
                        eprintln!(
                            "⚠️ proactive: failed to remove {}: {}",
                            target.path.display(),
                            e
                        );
                        continue;
                    }
                }
            }
            Err(e) => {
                if apply {
                    eprintln!("⚠️ proactive: skipping {}: {}", target.path.display(), e);
                }
                result.protected_paths.push(format!(
                    "{} ({})",
                    target.path.display(),
                    one_line(&e.to_string())
                ));
                continue;
            }
        }

        result.cleaned_count += 1;
        result.reclaimed_bytes += freed;
        if stashed {
            result.cleaned_paths.push(format!(
                "{} (stashed, {} days stale, {}, freed {})",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                human_bytes(target.bytes),
                human_bytes(freed)
            ));
        } else {
            result.cleaned_paths.push(format!(
                "{} ({} days stale, {})",
                target.path.display(),
                target.mtime_secs_ago / 86400,
                human_bytes(target.bytes)
            ));
        }
    }

    Ok(result)
}
