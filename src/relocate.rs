//! Cold-data relocation — move a directory to another disk and leave a symlink.
//!
//! ADDED 2026-09-26 (space tiers, Phase 1): deletion-based cleanup cannot fix
//! a disk filled with *kept* data in the wrong place. `relocate` moves a cold
//! directory (media, datasets, old checkouts) to a destination root — typically
//! on the second disk — verifies the copy by file count + bytes, removes the
//! source, and leaves a symlink so existing paths keep working. Dry-run by
//! default; `--apply` performs the move.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
///
/// Lenient by design: unreadable entries are skipped. Use for reporting
/// and sizing only — never to verify a copy whose origin is about to be
/// deleted (see `walk_stats_strict`).
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

/// Walk stats, strict variant: any unreadable entry is an error, not a
/// silent undercount. Use at verification points where the origin is
/// about to be deleted — an entry neither side can read would otherwise
/// vanish from both tallies and the verification would still pass.
pub(crate) fn walk_stats_strict(root: &Path) -> Result<(u64, u64)> {
    fn walk_err(root: &Path, e: &walkdir::Error) -> anyhow::Error {
        match e.path() {
            Some(p) => anyhow::anyhow!("cannot inspect {}: {}", p.display(), e),
            None => anyhow::anyhow!("directory walk failed under {}: {}", root.display(), e),
        }
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter() {
        let entry = entry.map_err(|e| walk_err(root, &e))?;
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry
                .metadata()
                .map(|m| m.len())
                .map_err(|e| anyhow::anyhow!("cannot stat {}: {}", entry.path().display(), e))?;
        }
    }
    Ok((files, bytes))
}

/// Recursive copy: dirs recreated, files copied, symlinks re-created as
/// symlinks. Returns the count of skipped special files (sockets, fifos…).
/// Never follows symlinks. Bails on the first unreadable entry rather
/// than silently dropping data the verification could never see.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<u64> {
    let mut skipped = 0u64;
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
    {
        let entry = entry.map_err(|e| match e.path() {
            Some(p) => anyhow::anyhow!("cannot copy {}: {}", p.display(), e),
            None => anyhow::anyhow!("copy walk failed under {}: {}", src.display(), e),
        })?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| anyhow::anyhow!("strip prefix {}: {}", entry.path().display(), e))?;
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

#[cfg(unix)]
fn make_symlink(dest: &Path, source: &Path) -> Result<()> {
    std::os::unix::fs::symlink(dest, source)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_symlink(_dest: &Path, _source: &Path) -> Result<()> {
    anyhow::bail!("relocate is only supported on unix");
}

/// Whether `bytes` fits in `avail`. Unknown space (df failure) does
/// NOT fit (R3-L28) — "cannot ask" must not authorize a copy that can
/// strand a partial destination.
pub(crate) fn fits_in_avail(avail: Option<u64>, bytes: u64) -> bool {
    avail.is_some_and(|a| a >= bytes)
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
///
/// A repository root is tracked content even when its parent is not a work
/// tree, so the toplevel of `path` itself is probed first and compared
/// canonically (symlinks and trailing slashes cannot dodge it). Anything
/// else falls through to the parent probe below.
pub(crate) fn is_git_tracked(path: &Path) -> Result<bool> {
    is_git_tracked_with(path, "git")
}

/// `git_bin` is injectable so tests can force the spawn-failure arm with
/// a nonexistent binary — no process-global `PATH` swap, so the test is
/// race-free under the parallel harness.
pub(crate) fn is_git_tracked_with(path: &Path, git_bin: &str) -> Result<bool> {
    if let Ok(canon) = fs::canonicalize(path) {
        if let Ok(top_out) = std::process::Command::new(git_bin)
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
        {
            if top_out.status.success() {
                let top = String::from_utf8_lossy(&top_out.stdout).trim().to_string();
                if !top.is_empty() {
                    let matches = fs::canonicalize(&top)
                        .map(|canon_top| canon_top == canon)
                        .unwrap_or_else(|_| Path::new(&top) == canon.as_path());
                    if matches {
                        return Ok(true);
                    }
                }
            }
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::process::Command::new(git_bin)
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        // FIXED 2026-10-03 (audit R4-SYS-04): git itself unrunnable
        // (missing binary, EACCES, ...) is UNKNOWN, not "untracked" —
        // the old arm returned Ok(false) here too, so with no git on
        // PATH every tracked dir read untracked and relocate/cold-scan
        // proceeded onto repo content. Callers already fail closed on
        // Err (plan_relocate `?`, cold-scan `unwrap_or(true)`).
        Err(e) => {
            return Err(anyhow::anyhow!(
                "cannot run git rev-parse for {}: {e}",
                parent.display()
            ));
        }
        Ok(o) if o.status.success() => {}
        // rev-parse RAN and failed → not a work tree → untracked.
        _ => return Ok(false),
    }
    let out = std::process::Command::new(git_bin)
        .arg("-C")
        .arg(parent)
        .args(["ls-files", "--", &name])
        .output()
        .map_err(|e| anyhow::anyhow!("cannot run git ls-files for {}: {e}", path.display()))?;
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
    let meta = fs::symlink_metadata(source)
        .map_err(|e| anyhow::anyhow!("cannot inspect source {}: {}", source.display(), e))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to relocate symlink {} (already relocated?)",
            source.display()
        );
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
    let canon_root = dest_root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot canonicalize {}: {}", dest_root.display(), e))?;

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

    let (files, bytes) = walk_stats_strict(&canon_src)?;
    let avail = avail_bytes_for(&canon_root);
    let fits = fits_in_avail(avail, bytes);
    if !fits {
        issues.push(match avail {
            Some(a) => format!(
                "destination has {} free but source needs {}",
                human_bytes(a),
                human_bytes(bytes)
            ),
            // FIXED 2026-10-03 (audit R3-L28): unknown space (df
            // failure) is NOT fits — the old `unwrap_or(true)`
            // treated "cannot ask" as fits, so ENOSPC mid-copy
            // bailed with the source untouched but left a partial
            // dest blocking retries ("already exists") until manual
            // cleanup. Not-ready plans refuse in `apply_relocate`.
            None => "destination free space unknown (df failed) — refusing to relocate blind"
                .to_string(),
        });
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

/// Suffix of the aside-staging dir. Single source of truth for the
/// name: the pre-flight guard, the rename site, and the setup scan
/// must all agree (R4-SYS-09).
pub(crate) const RELOCATE_STAGING_SUFFIX: &str = ".dracon-relocate-staging";

/// Staging path for a source: `<parent>/<name>.dracon-relocate-staging`.
fn staging_path_for(source: &Path) -> PathBuf {
    source.with_file_name(format!(
        "{}{RELOCATE_STAGING_SUFFIX}",
        source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "relocated".to_string())
    ))
}

/// Build the stale-staging refusal (R4-SYS-09). The recovery depends
/// on what the SOURCE looks like, so the message names the case and
/// the exact command instead of "clear it manually".
fn stale_staging_refusal(source: &Path, dest: &Path, staging: &Path) -> String {
    match fs::symlink_metadata(source) {
        Err(_) => format!(
            "stale staging dir {} from an interrupted run — and {} is MISSING (crash between the staging rename and the symlink). Data is intact in staging (plus the verified copy at {}). Recover with: mv {} {} — then re-run relocate. Refusing.",
            staging.display(),
            source.display(),
            dest.display(),
            staging.display(),
            source.display()
        ),
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = fs::read_link(source)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "(unreadable)".to_string());
            format!(
                "stale staging dir {} from a previous run — but {} is already a symlink to {} (the move completed; only staging cleanup failed). Staging is a redundant duplicate: verify the symlink, then delete staging by hand: rm -rf {}. Refusing.",
                staging.display(),
                source.display(),
                target,
                staging.display()
            )
        }
        Ok(_) => format!(
            "stale staging dir {} from a previous run — while {} is still a real directory. Staging holds the pre-move tree; inspect both before clearing staging by hand (the source may have been recreated after an interrupted run). Refusing.",
            staging.display(),
            source.display()
        ),
    }
}

/// Best-effort stale-staging scan (R4-SYS-09): one level under each
/// root, entries ending in [`RELOCATE_STAGING_SUFFIX`]. Deeper
/// nesting is missed by design (staging sits next to the source, and
/// the scan must stay cheap). Unreadable roots are skipped, never
/// fatal — this feeds an informational setup check.
pub(crate) fn find_stale_staging_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(RELOCATE_STAGING_SUFFIX))
            {
                stale.push(entry.path());
            }
        }
    }
    stale.sort();
    stale
}

/// Execute a validated plan: copy, verify, remove source, leave symlink.
///
/// Crash window (DOCUMENTED 2026-10-03, audit R4-SYS-09): between
/// `rename(source, staging)` and `symlink(dest, source)` a crash
/// (power loss, SIGKILL — a symlink Err is handled, a crash is not)
/// leaves SOURCE MISSING with the data only in
/// `<name>.dracon-relocate-staging` (plus the verified copy at
/// `dest`, which completed before the rename). Recovery by case —
/// the stale-staging refusal below names the case and the command:
/// - source missing → `mv <staging> <source>`, then re-run relocate.
/// - source is a symlink → move completed, staging is a redundant
///   duplicate: verify the link, then `rm -rf <staging>`.
/// - source is a real dir → ambiguous (recreated after the crash?):
///   inspect both trees before clearing staging by hand.
///
/// `setup` best-effort reports `*.dracon-relocate-staging` dirs one
/// level under the candidate roots so the state is visible.
pub(crate) fn apply_relocate(plan: &RelocatePlan) -> Result<RelocateReport> {
    #[cfg(not(unix))]
    {
        anyhow::bail!("relocate is only supported on unix");
    }
    if !plan.ready {
        anyhow::bail!("plan is not ready: {}", plan.issues.join("; "));
    }
    let source = Path::new(&plan.source);
    let dest = Path::new(&plan.dest);
    // MOVED FIRST 2026-10-03 (audit R4-SYS-09): the stale-staging
    // guard used to sit AFTER the copy block, so a refused run still
    // copied gigabytes first — and the crash-window arm (source
    // missing) was unreachable behind the source-vanished check
    // below. Refuse before any IO, with recovery-aware messages.
    let staging = staging_path_for(source);
    if fs::symlink_metadata(&staging).is_ok() {
        anyhow::bail!("{}", stale_staging_refusal(source, dest, &staging));
    }
    if fs::symlink_metadata(dest).is_ok() {
        anyhow::bail!(
            "destination {} appeared since planning — refusing",
            dest.display()
        );
    }
    let meta = fs::symlink_metadata(source)
        .map_err(|e| anyhow::anyhow!("source {} vanished or changed: {}", source.display(), e))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        anyhow::bail!(
            "source {} is no longer a real directory — refusing",
            source.display()
        );
    }
    // FIXED 2026-10-03 (audit R4-SYS-05): the plan's space verdict is
    // stale by apply time — a plan that fit at dry-run can ENOSPC
    // mid-copy and strand every retry behind a partial dest ("already
    // exists"). Re-check availability now (`dest` itself was just
    // verified absent, so `df` runs against its parent root), with
    // the same fail-closed unknown-space rule as plan time (R3-L28).
    let dest_parent = dest.parent().unwrap_or(dest);
    let avail = avail_bytes_for(dest_parent);
    if !fits_in_avail(avail, plan.bytes) {
        match avail {
            Some(a) => anyhow::bail!(
                "destination has {} free but plan needs {} — refusing (re-plan to retry)",
                human_bytes(a),
                human_bytes(plan.bytes)
            ),
            None => anyhow::bail!(
                "destination free space unknown (df failed) — refusing to relocate blind"
            ),
        }
    }

    // `dest` was verified absent above, so anything there on failure
    // is our own partial copy — remove it best-effort so a retry is
    // not stranded behind "appeared since planning".
    let copy_outcome: Result<u64> = (|| {
        let skipped_special = copy_tree(source, dest)?;
        let (dest_files, dest_bytes) = walk_stats_strict(dest)?;
        if dest_files != plan.files || dest_bytes != plan.bytes {
            anyhow::bail!(
                "copy verification failed: expected {} files / {} bytes, got {} / {} — source untouched",
                plan.files,
                plan.bytes,
                dest_files,
                dest_bytes
            );
        }
        Ok(skipped_special)
    })();
    let skipped_special = match copy_outcome {
        Ok(s) => s,
        Err(e) => {
            if let Err(rm_err) = fs::remove_dir_all(dest) {
                if fs::symlink_metadata(dest).is_ok() {
                    eprintln!(
                        "relocate: copy failed ({e:#}); cleanup of partial {} also failed: {rm_err:#}",
                        dest.display()
                    );
                }
            }
            return Err(e);
        }
    };

    // Stage the source aside instead of deleting it: if the symlink step
    // fails, the original path is restored rather than left broken.
    // (`staging` was bound + guarded pre-copy at the top of this fn,
    // R4-SYS-09 — the old guard here ran AFTER the copy and was
    // unreachable for the crash-window case.)
    fs::rename(source, &staging).map_err(|e| {
        anyhow::anyhow!(
            "cannot stage {} aside: {} — source untouched",
            source.display(),
            e
        )
    })?;
    if let Err(e) = make_symlink(dest, source) {
        fs::rename(&staging, source).map_err(|restore_err| {
            anyhow::anyhow!(
                "symlink failed ({e:#}) AND restore failed ({restore_err:#}) — data is intact at {}",
                staging.display()
            )
        })?;
        anyhow::bail!("symlink failed ({e:#}) — source restored");
    }
    // FIXED 2026-10-01 (audit): the staging copy is removed AFTER the symlink is
    // already in place, and the `?` made a removal failure report the whole move
    // as failed. The move had in fact succeeded: the source is a correct symlink
    // to the destination AND a full duplicate of the tree was still sitting in
    // the staging path, consuming the bytes the relocate was made to reclaim,
    // while the caller never recorded the relocation (so `link status` and the
    // doctor drift check never saw it). A failed cleanup is now reported as a
    // warning on the report instead of failing the move, because the data is
    // safe and re-running the move would be the wrong remedy.
    if let Err(cleanup_err) = fs::remove_dir_all(&staging) {
        eprintln!(
            "relocate: moved {} -> {}, but the staging copy at {} could not be removed: {cleanup_err:#}\n  \
             the move is complete; delete that directory by hand when convenient",
            plan.source,
            plan.dest,
            staging.display()
        );
    }

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
    let plan = plan_relocate(
        &path,
        &dest_root,
        &policy.guard.protected_paths,
        allow_tracked,
    )?;

    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![Cell::new("FIELD"), Cell::new("VALUE")]);
            table.add_row(vec![
                Cell::new("Source"),
                Cell::new(display_home(&plan.source)),
            ]);
            table.add_row(vec![Cell::new("Dest"), Cell::new(display_home(&plan.dest))]);
            table.add_row(vec![
                Cell::new("Size"),
                Cell::new(format!(
                    "{} in {} files",
                    human_bytes(plan.bytes),
                    plan.files
                )),
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
        println!(
            "Add to policy to keep the link managed:\n{}",
            report.policy_snippet
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Automatic cold-candidate scanning (space tiers Phase 2)
// ---------------------------------------------------------------------------

/// A directory eligible for automatic relocation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ColdCandidate {
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) files: u64,
    pub(crate) age_days: u64,
}

const SCAN_MAX_DEPTH: usize = 8;
const SCAN_MAX_CANDIDATES: usize = 20;
/// Subtrees never counted as relocate candidates: build outputs are
/// delete-managed, `.git` is history. Pruned before sizing so parents are
/// not inflated by bytes that can never move with them.
const SCAN_SKIP_NAMES: &[&str] = &["target", "node_modules", ".git"];

/// Single-pass bottom-up sizing: every file's bytes and mtime accumulate
/// into each ancestor dir up to `root`. Returns bytes/files/newest-mtime.
fn dir_sizes_bottom_up(root: &Path) -> HashMap<PathBuf, (u64, u64, u64)> {
    let mut sizes: HashMap<PathBuf, (u64, u64, u64)> = HashMap::new();
    // NOTE: no contents_first — it yields children before parents, which
    // defeats filter_entry pruning (rejected dirs are pruned only after
    // their children were already visited). Accumulation via ancestors()
    // is order-independent, so plain pre-order is correct here.
    let iter = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(SCAN_MAX_DEPTH)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_symlink() {
                return false;
            }
            if e.depth() > 0 {
                if let Some(name) = e.file_name().to_str() {
                    if SCAN_SKIP_NAMES.contains(&name) {
                        return false;
                    }
                }
            }
            true
        })
        .filter_map(|e| e.ok());
    for entry in iter {
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for ancestor in entry.path().ancestors() {
            if !ancestor.starts_with(root) {
                break;
            }
            let slot = sizes.entry(ancestor.to_path_buf()).or_insert((0, 0, 0));
            slot.0 += meta.len();
            slot.1 += 1;
            slot.2 = slot.2.max(mtime);
        }
    }
    sizes
}

/// Find cold relocation candidates under `roots`: big enough, idle long
/// enough, not symlinked, not git-tracked. Nested qualifiers collapse to
/// the outermost dir. Sorted biggest-first, capped.
pub(crate) fn find_cold_candidates(
    roots: &[PathBuf],
    min_bytes: u64,
    min_age_days: u64,
) -> Vec<ColdCandidate> {
    let now = crate::now_unix();
    let mut qualified: Vec<(PathBuf, u64, u64, u64)> = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        for (dir, (bytes, files, newest)) in dir_sizes_bottom_up(&canon_root) {
            if dir == canon_root || bytes < min_bytes {
                continue;
            }
            let age_days = now.saturating_sub(newest) / 86_400;
            if age_days < min_age_days {
                continue;
            }
            // Auto mode never touches tracked content (no override).
            if is_git_tracked(&dir).unwrap_or(true) {
                continue;
            }
            qualified.push((dir, bytes, files, age_days));
        }
    }
    // Collapse nesting: keep outermost qualifiers only.
    qualified.sort_by_key(|(p, _, _, _)| p.components().count());
    let mut kept: Vec<(PathBuf, u64, u64, u64)> = Vec::new();
    for item in qualified {
        if kept.iter().any(|(k, _, _, _)| item.0.starts_with(k)) {
            continue;
        }
        kept.push(item);
    }
    let mut out: Vec<ColdCandidate> = kept
        .into_iter()
        .map(|(path, bytes, files, age_days)| ColdCandidate {
            path: path.display().to_string(),
            bytes,
            files,
            age_days,
        })
        .collect();
    // FIXED 2026-09-27 (clippy `unnecessary_sort_by`): sorting by a single
    // `u64` key is `sort_by_key`. Pre-existing lint, newly fatal under the
    // CI gate that now lints `--all-targets`.
    out.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    out.truncate(SCAN_MAX_CANDIDATES);
    out
}

// ---------------------------------------------------------------------------
// Daemon-owned relocation state (space tiers Phase 2)
// ---------------------------------------------------------------------------

/// Record of one automatic relocation. Lives in a daemon-owned JSON state
/// file — human policy stays hand-edited, `link status` merges both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RelocationRecord {
    pub(crate) link: String,
    pub(crate) target: String,
    pub(crate) moved_at_unix: u64,
    pub(crate) bytes: u64,
}

pub(crate) fn relocation_state_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home"))
        .join(".local/state/dracon/dracon-system-relocations.json")
}

pub(crate) fn load_relocation_records() -> Vec<RelocationRecord> {
    fs::read_to_string(relocation_state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub(crate) fn record_relocation(link: &str, target: &str, bytes: u64) -> Result<()> {
    let path = relocation_state_path();
    let mut records = load_relocation_records();
    records.retain(|r| r.link != link);
    records.push(RelocationRecord {
        link: link.to_string(),
        target: target.to_string(),
        moved_at_unix: crate::now_unix(),
        bytes,
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&records)?)?;
    Ok(())
}

/// Auto-relocation records as link entries for `link status` merging.
pub(crate) fn relocation_link_entries() -> Vec<crate::LinkEntry> {
    load_relocation_records()
        .into_iter()
        .map(|r| crate::LinkEntry {
            link: r.link,
            target: r.target,
        })
        .collect()
}
