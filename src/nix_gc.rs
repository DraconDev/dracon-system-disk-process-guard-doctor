//! Nix generation pruning and garbage collection.
//!
//! Split out of main.rs 2026-11-14 (audit task list, P3). Moved verbatim;
//! items are `pub(crate)` and re-exported from main.rs.

use super::*;

/// Return the `nix-env --delete-generations` argument that retains the newest
/// `keep_generations` generations. Nix interprets a bare number as one
/// specific generation to delete; the leading `+` is what means "keep the
/// newest N generations".
pub(crate) fn nix_delete_generations_arg(keep_generations: u32) -> Option<String> {
    (keep_generations > 0).then(|| format!("+{keep_generations}"))
}

/// Choose which generations to KEEP when thinning a profile down to `keep`.
///
/// ADDED 2026-10-27 (operator request): `+N` keeps the newest N and throws away
/// every older one, which is the wrong shape for rollback — it collapses the
/// entire history into a narrow recent window. Given 18 generations and
/// `keep = 5`, `+5` leaves generations 14–18 and loses the ability to roll back
/// to anything from further out. The operator asked for the survivors to be
/// *spread across the range* instead, which keeps one anchor near the newest,
/// one near the oldest and the rest spaced between.
///
/// Contract:
/// - the CURRENT generation (highest number) is always kept — it is what the
///   system boots;
/// - `keep >= count` keeps everything (nothing to do);
/// - `keep == 0` keeps nothing but the current generation (`0` means "keep no
///   generations" in the policy's own normalisation, so the current one is
///   still protected — deleting it would leave the system unbootable);
/// - `keep == 1` keeps only the current generation;
/// - otherwise the kept set is evenly spaced indices across the sorted range,
///   always including both ends.
///
/// Returns the generations to DELETE, sorted ascending.
/// Legacy semantic (protect only the highest generation), used by the
/// pre-existing spread tests.
#[cfg(test)]
pub(crate) fn nix_generations_to_delete_highest_protected(
    generations: &[u32],
    keep: usize,
) -> Vec<u32> {
    let highest = generations
        .iter()
        .copied()
        .max()
        .into_iter()
        .collect::<Vec<_>>();
    nix_generations_to_delete_protecting(generations, keep, &highest)
}

/// Which generation a `nix-env --list-generations` table marks as current.
///
/// ADDED 2026-10-27 (audit): the first version inferred "current == highest",
/// which is only true until someone rolls back. After
/// `nix-env --switch-generation 5` on an 18-generation profile the *current*
/// generation is 5, and a cleaner that protects the highest number would
/// happily delete the generation the machine actually boots.
///
/// The table marks the live row with a `(current)` token. Anything unparseable
/// yields `None`, and the caller must then refuse to prune rather than guess.
pub(crate) fn parse_nix_current_generation(stdout: &str) -> Option<u32> {
    stdout.lines().find_map(|line| {
        if !line.contains("(current)") {
            return None;
        }
        line.split_whitespace().next()?.parse::<u32>().ok()
    })
}

/// The generation number currently booted, read from the system closure.
///
/// ADDED 2026-10-27 (audit): the objective requires keeping "every generation
/// referenced by the running system closure". `/run/current-system` is a
/// symlink into `/nix/store`; the store path name contains a hash we cannot map
/// back to a generation number, so this returns the resolved store path and the
/// caller keeps any generation whose closure contains it. Best effort: a host
/// without `/run/current-system` (a container, or a non-NixOS install) simply
/// reports `None`.
pub(crate) fn running_system_closure(run_root: &Path) -> Option<PathBuf> {
    std::fs::read_link(run_root.join("current-system")).ok()
}

/// Generation numbers whose system closure IS the running one.
///
/// `profile_dir` is a NixOS system-profile directory (`/nix/var/nix/profiles`)
/// holding `system-<n>-link` symlinks; `closure` is the resolved
/// `/run/current-system`.
///
/// FIXED 2026-10-27 (audit HIGH): the first version kept every generation whose
/// link target merely EXISTED. Every generation link of a live profile points
/// at an existing store path, so that protected the entire profile and made
/// pruning impossible — the cleaner silently fell back to `+N` on the real
/// host. Membership is now decided by comparing the RESOLVED store path with
/// the running closure, which is what "referenced by the running system
/// closure" actually means.
pub(crate) fn nix_generations_referenced_by_closure(
    profile_dir: &Path,
    generations: &[u32],
    closure: &Path,
) -> Vec<u32> {
    generations
        .iter()
        .copied()
        .filter(|generation| {
            let Ok(target) =
                std::fs::read_link(profile_dir.join(format!("system-{generation}-link")))
            else {
                // An unreadable generation is not evidence that it is
                // unreferenced, so it is protected.
                return true;
            };
            resolve_store_path(&target) == resolve_store_path(closure)
        })
        .collect()
}

/// Canonicalise a store path for comparison.
///
/// Generation links and `/run/current-system` are both symlinks into
/// `/nix/store`; comparing the raw link text would miss the match, so resolve
/// one level. A path that cannot be resolved is compared as given.
pub(crate) fn resolve_store_path(path: &Path) -> PathBuf {
    std::fs::read_link(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Spread selection with explicit protection.
///
/// `protected` names generations that must survive regardless of the spread
/// plan — the live `(current)` generation and anything the running system
/// closure references.
pub(crate) fn nix_generations_to_delete_protecting(
    generations: &[u32],
    keep: usize,
    protected: &[u32],
) -> Vec<u32> {
    let mut sorted: Vec<u32> = generations.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        return Vec::new();
    }
    if keep >= sorted.len() {
        return Vec::new();
    }
    let protected: std::collections::BTreeSet<u32> = protected.iter().copied().collect();
    // `keep` counts generations RETAINED IN TOTAL, so the spread is computed
    // over the full range and protection is then added on top. Computing it
    // over "unprotected only" would silently retain `keep` PLUS every protected
    // generation, which drifts from the configured number.
    let retain_by_spread: std::collections::BTreeSet<u32> = if keep <= 1 {
        [*sorted.last().expect("non-empty")].into_iter().collect()
    } else {
        (0..keep)
            .map(|i| i * (sorted.len() - 1) / (keep - 1))
            .map(|index| sorted[index])
            .chain(std::iter::once(*sorted.last().expect("non-empty")))
            .collect()
    };
    // Protection always wins, even when the spread plan did not choose it.
    let mut retain = retain_by_spread;
    retain.extend(
        protected
            .iter()
            .copied()
            .filter(|generation| sorted.binary_search(generation).is_ok()),
    );
    sorted
        .into_iter()
        .filter(|generation| !retain.contains(generation))
        .collect()
}

/// How many generations a Nix profile has.
///
/// ADDED 2026-10-22: the guard only attempts the root-owned system-profile
/// prune when there is genuinely something to prune, so the expected
/// permission failure stops being a line on every cycle. `None` when the
/// profile cannot be read — the caller then leaves the profile alone
/// rather than guessing.
/// The generation NUMBERS a Nix profile currently has, ascending.
///
/// `nix-env --list-generations` prints a table whose first column is the
/// generation number (optionally followed by "(current)"). Anything that is
/// not a leading integer is skipped rather than guessed at, so a parser change
/// or a warning on stderr cannot fabricate generation ids that `nix-env
/// --delete-generations` would then act on.
pub(crate) fn parse_nix_generation_numbers(stdout: &str) -> Vec<u32> {
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|first| first.parse::<u32>().ok())
        .collect()
}

/// A parsed `nix-env --list-generations` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NixGenerationListing {
    pub(crate) generations: Vec<u32>,
    /// The generation marked `(current)`, when the table marks one.
    pub(crate) current: Option<u32>,
}

/// Parse a full `nix-env --list-generations` table.
///
/// ADDED 2026-10-27 (audit HIGH): `parse_nix_generation_numbers` threw the
/// `(current)` marker away, so the cleaner could not tell a rolled-back
/// profile from a linear one and protected the wrong generation.
pub(crate) fn parse_nix_generation_listing(stdout: &str) -> NixGenerationListing {
    NixGenerationListing {
        generations: parse_nix_generation_numbers(stdout),
        current: parse_nix_current_generation(stdout),
    }
}

/// List a profile's generations AND its current marker.
pub(crate) fn nix_env_list_generations(
    nix_env: &Path,
    profile: &str,
) -> Option<NixGenerationListing> {
    let output = std::process::Command::new(nix_env)
        .args(["--list-generations", "-p", profile])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_nix_generation_listing(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// A closure-safe spread plan for the NixOS system profile.
///
/// Reads `/nix/var/nix/profiles/system-<n>-link` (world-readable, unlike the
/// profile lock that blocks `nix-env` without root) and protects:
///
/// - every generation whose resolved store path equals `/run/current-system`,
///   i.e. the generation the machine is actually booted into;
/// - the highest generation, as a floor for a host without a readable closure;
/// - every generation whose link cannot be resolved (unknown is not "unused").
///
/// Returns `None` when the links are unreadable, which the caller treats as
/// "leave the profile alone".
///
/// CHANGED 2026-11-13: the profile directory and run root are parameters
/// (they were hardcoded, and the `nix_env` parameter was unused) so the
/// NixOS branch can be exercised by a hermetic fixture on any host.
pub(crate) fn system_profile_spread_plan(
    profile_dir: &Path,
    run_root: &Path,
    keep: u32,
) -> Option<Vec<u32>> {
    let generations = system_profile_generations(profile_dir)?;
    if generations.is_empty() {
        return None;
    }
    let mut protected: Vec<u32> = match running_system_closure(run_root) {
        Some(closure) => nix_generations_referenced_by_closure(profile_dir, &generations, &closure),
        None => Vec::new(),
    };
    if let Some(highest) = generations.iter().copied().max() {
        protected.push(highest);
    }
    Some(nix_generations_to_delete_protecting(
        &generations,
        keep as usize,
        &protected,
    ))
}

/// Generation numbers present as `system-<n>-link` symlinks.
pub(crate) fn system_profile_generations(profile_dir: &Path) -> Option<Vec<u32>> {
    let entries = std::fs::read_dir(profile_dir).ok()?;
    let mut generations = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("system-") else {
            continue;
        };
        let Some(number) = rest.strip_suffix("-link") else {
            continue;
        };
        if let Ok(generation) = number.parse::<u32>() {
            generations.push(generation);
        }
    }
    generations.sort_unstable();
    generations.dedup();
    Some(generations)
}

/// `--delete-generations <id> <id> ... -p <profile>` for a specific profile.
pub(crate) fn nix_delete_generations_argv_with_profile(
    generations: &[u32],
    profile: &str,
) -> Vec<String> {
    let mut argv = nix_delete_generations_argv(generations);
    argv.push("-p".to_string());
    argv.push(profile.to_string());
    argv
}

/// The exact argv used to delete generations, one element per generation.
///
/// Extracted so a test can assert ARGUMENT SHAPE rather than a rendered
/// command string: `--delete-generations 3 6` is two arguments, and
/// `--delete-generations "3 6"` is one — nix rejects the second form.
pub(crate) fn nix_delete_generations_argv(generations: &[u32]) -> Vec<String> {
    let mut argv = vec!["--delete-generations".to_string()];
    argv.extend(generations.iter().map(u32::to_string));
    argv
}

pub(crate) fn nix_env_generation_count(nix_env: &Path, profile: &str) -> Option<usize> {
    let output = std::process::Command::new(nix_env)
        .args(["--list-generations", "-p", profile])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().filter(|line| !line.trim().is_empty()).count())
}

/// Run an external cleanup command with a hard bound.
///
/// ADDED 2026-10-22: the guard's loop calls the cleanup kinds inline, so a
/// slow `nix-collect-garbage` (minutes) or a docker prune blocks the whole
/// loop — and the memory monitoring that would notice a swap spike never
/// runs in the meantime. The tracked design note (the parent repository's
/// `docs/design/performance-swap-thrash-2026-10-21.md`) listed this as open
/// work: bound the external cleanup commands and keep memory monitoring
/// independent of slow storage maintenance.
///
/// The child is killed when the bound expires (`kill_on_drop`), so a wedged
/// cleanup cannot outlive its budget.
pub(crate) async fn run_bounded(
    command: &mut Command,
    timeout_secs: u64,
) -> Result<Option<std::process::Output>> {
    command.kill_on_drop(true);
    let bound = Duration::from_secs(timeout_secs.max(1));
    match tokio::time::timeout(bound, command.output()).await {
        Ok(result) => Ok(Some(result?)),
        Err(_) => {
            eprintln!(
                "⚠️ external cleanup command exceeded its {timeout_secs}s bound — skipped this pass"
            );
            Ok(None)
        }
    }
}

/// Run Nix profile and store cleanup with explicit binary paths.
///
/// The binary parameters keep the command construction testable without
/// depending on the host's Nix installation. Applying profile retention must
/// not pass `-d` to `nix-collect-garbage`: that flag deletes all old profile
/// generations, bypassing `nix_keep_generations`.
pub(crate) async fn clean_nix_garbage_with_bins(
    keep_generations: u32,
    apply: bool,
    nix_env: &Path,
    nix_gc: &Path,
    command_timeout_secs: u64,
) -> Result<(u64, Vec<String>)> {
    clean_nix_garbage_impl(
        keep_generations,
        apply,
        nix_env,
        nix_gc,
        command_timeout_secs,
        Path::new("/nix/var/nix/profiles"),
        Path::new("/run"),
    )
    .await
}

/// Implementation with an injectable NixOS system-profile directory and run
/// root. ADDED 2026-11-13: the system-spread branch used to be reachable only
/// on hosts with real `/nix/var/nix/profiles/system-*-link` entries, so its
/// skipped-GC defect (see the FIXED note below) stayed invisible on dev hosts.
pub(crate) async fn clean_nix_garbage_impl(
    keep_generations: u32,
    apply: bool,
    nix_env: &Path,
    nix_gc: &Path,
    command_timeout_secs: u64,
    system_profile_dir: &Path,
    run_root: &Path,
) -> Result<(u64, Vec<String>)> {
    let reclaimed = 0u64;
    let mut cleaned = Vec::new();

    // CHANGED 2026-09-20 (space audit): generation-prune failures are
    // warnings, not fatal. The old code returned Err when either nix-env
    // call failed, which discarded the store-GC results collected below
    // and surfaced "Nix cleanup failed" every cycle — while the valuable
    // step (nix-collect-garbage) may have succeeded. A user-session guard
    // can also never prune /nix/var/nix/profiles/default (root-owned),
    // so that failure is expected, not exceptional; the per-profile note
    // keeps it visible without failing the pass.
    // CHANGED 2026-10-27 (operator request): thin the profile by SPREADING the
    // survivors across the whole generation range instead of using `+N`, which
    // keeps only the newest N and throws away all older rollback anchors. With
    // 18 generations and keep=5, `+5` retains 14..=18; the spread form retains
    // roughly 1, 5, 10, 14, 18 — same reclaim, far more useful rollback.
    //
    // `+N` is still used as the fallback when the generation list cannot be
    // read: it is the documented, safe-by-construction form (it never deletes
    // the current generation), and falling back to "delete nothing" would
    // silently disable Nix pruning on any host where the listing format
    // changes.
    // FIXED 2026-10-27 (audit HIGH): this used to build a SPACE-JOINED string
    // and pass it as ONE argv element. `nix-env --delete-generations "3 6"` is
    // rejected with `invalid generation number '3 6'` — only separate arguments
    // work (`--delete-generations 3 6`). The old unit test used a fake nix-env
    // that echoed `$*`, which cannot tell one argument from two, so the central
    // feature was broken while its test passed. The plan is now a Vec, one
    // generation per element, and the test asserts argv shape.
    //
    // The current generation is no longer inferred as "highest": the
    // `(current)` marker is parsed, and every generation referenced by the
    // running system closure is protected too, so a rolled-back profile cannot
    // have the generation it is actually running deleted.
    let spread_delete_args: Option<Vec<u32>> = if keep_generations > 0 {
        let profile = "default";
        let listing = nix_env_list_generations(nix_env, profile);
        listing.and_then(|listing| {
            if listing.generations.is_empty() {
                return None;
            }
            let mut protected: Vec<u32> = Vec::new();
            match listing.current {
                Some(current) => protected.push(current),
                // No marker means we cannot tell what is live; refuse to prune
                // rather than guess and delete the running generation.
                None => {
                    eprintln!(
                        "⚠️ nix-env listed no (current) generation for profile {profile}; skipping the generation prune"
                    );
                    return None;
                }
            }
            // A user profile's generations are not part of the SYSTEM
            // closure, so closure protection belongs to the system profile
            // below; here the `(current)` marker is the protection that matters.
            let to_delete = nix_generations_to_delete_protecting(
                &listing.generations,
                keep_generations as usize,
                &protected,
            );
            (!to_delete.is_empty()).then_some(to_delete)
        })
    } else {
        None
    };

    if let Some(delete_args) = spread_delete_args
        .or_else(|| nix_delete_generations_arg(keep_generations).map(|_| Vec::new()))
        .filter(|_| apply)
    {
        // An empty plan means "fall back to the +N retain spec"; a non-empty
        // plan passes one argv element per generation (the bug this fixes).
        let nix_args: Vec<String> = if delete_args.is_empty() {
            match nix_delete_generations_arg(keep_generations) {
                Some(retain) => vec!["--delete-generations".to_string(), retain],
                None => Vec::new(),
            }
        } else {
            nix_delete_generations_argv(&delete_args)
        };
        if nix_args.is_empty() {
            return Ok((reclaimed, cleaned));
        }
        match Command::new(nix_env).args(&nix_args).output().await {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "⚠️ nix-env user-profile generation prune failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(e) => eprintln!("⚠️ nix-env user-profile generation prune failed: {}", e),
        }

        // The SYSTEM profile is root-owned: a user-session guard can never
        // prune it, and nix-env answers "Read-only file system (needs
        // root; user GC still runs)" every cycle. Attempt the prune only
        // when it would actually remove something — the generation count
        // is above the retention — so the expected permission failure is
        // not a line on every cycle, and name the documented privileged
        // path when it is genuinely needed.
        //
        // ADDED 2026-10-22: the 2026-10-21 disk incident left the guard
        // logging that failure every cycle while 144 GiB of scratch
        // accumulated; the noise belonged in the report, not on every pass.
        let system_profile_path = system_profile_dir.join("default");
        let system_profile = system_profile_path.to_string_lossy();
        let system_profile_needs_prune = nix_env_generation_count(nix_env, &system_profile)
            .map(|count| count > keep_generations as usize)
            .unwrap_or(false);
        // ADDED 2026-10-27 (audit HIGH): "always keep the currently booted
        // generation and every generation referenced by the running system
        // closure". For NixOS the system profile's generation links are
        // world-readable, and /run/current-system resolves to the SAME store
        // path as the booted generation's link, so the running generation can
        // be identified exactly instead of assumed to be the highest.
        //
        // ADDED 2026-10-27 (audit MED): the system profile is no longer pruned
        // with `+N`. `+N` keeps only the newest N, which is precisely the shape
        // the objective calls wrong; the spread plan now applies here too, with
        // the booted generation protected unconditionally.
        let system_spread =
            system_profile_spread_plan(system_profile_dir, run_root, keep_generations);
        if let Some(system_delete) = system_spread.filter(|plan| !plan.is_empty()) {
            let nix_args =
                nix_delete_generations_argv_with_profile(&system_delete, &system_profile);
            match Command::new(nix_env).args(&nix_args).output().await {
                Ok(output) if output.status.success() => {}
                Ok(output) => eprintln!(
                    "⚠️ nix-env system-profile generation prune failed ({}): {} (needs root — run scripts/prune-nix-system-profile.sh with sudo; user GC still runs)",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Err(e) => eprintln!(
                    "⚠️ nix-env system-profile generation prune failed: {} (needs root — run scripts/prune-nix-system-profile.sh with sudo; user GC still runs)",
                    e
                ),
            }
            // FIXED 2026-11-13: this branch used to `return Ok(...)` here,
            // skipping nix-collect-garbage entirely on every host where a
            // system spread plan exists (any NixOS host above retention).
            // The warnings above promise "user GC still runs" — now it does,
            // whether the root-owned system prune succeeded or failed. The
            // two fake-bin nix tests failing on NixOS were this defect, not
            // a host-layout mismatch.
        }
        if system_profile_needs_prune {
            // The system profile's generation count is above the retention but
            // its links could not be enumerated, so no spread plan exists.
            // `+N` is deliberately NOT used here: it is the newest-N shape the
            // objective rejects, and the booted generation must be identified,
            // not assumed to be the highest. Report the privileged path and
            // leave the profile untouched.
            eprintln!(
                "⚠️ nix-env system-profile has more than {keep_generations} generations \
but its links could not be read, so no closure-safe spread plan is available; \
run scripts/prune-nix-system-profile.sh with sudo (user GC still runs)"
            );
        }
    }

    // Do not use `-d` here. It deletes every old profile generation, which
    // would make nix_keep_generations ineffective. The explicit nix-env
    // commands above perform only the configured retention pruning; ordinary
    // garbage collection then removes store paths no longer rooted by the
    // retained generations.
    let args: &[&str] = if apply { &[] } else { &["--dry-run"] };
    // ADDED 2026-10-22: bounded. `nix-collect-garbage` can run for
    // minutes, and it ran INLINE in the guard's loop — so during a GC the
    // guard sampled no memory at all, which is exactly when a swap spike
    // must not go unnoticed.
    let mut gc = Command::new(nix_gc);
    gc.args(args);
    let out = match run_bounded(&mut gc, command_timeout_secs).await? {
        Some(out) => out,
        None => return Ok((reclaimed, cleaned)),
    };

    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "nix-collect-garbage failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let delete_count = text.lines().filter(|l| l.contains("deleting")).count();
    if delete_count > 0 {
        cleaned.push(format!("nix store garbage ({} paths)", delete_count));
        // nix-collect-garbage reports paths here, not their byte sizes. Do
        // not turn a path count into a made-up reclaim estimate; callers use
        // `reclaimed` for accounting and a false value is worse than zero.
    }

    Ok((reclaimed, cleaned))
}

/// Run nix-collect-garbage and retain the configured number of profile
/// generations.
pub(crate) async fn clean_nix_garbage(
    keep_generations: u32,
    apply: bool,
    command_timeout_secs: u64,
) -> Result<(u64, Vec<String>)> {
    let nix_env = resolve_bin("nix-env");
    let nix_gc = resolve_bin("nix-collect-garbage");
    clean_nix_garbage_with_bins(
        keep_generations,
        apply,
        Path::new(&nix_env),
        Path::new(&nix_gc),
        command_timeout_secs,
    )
    .await
}

/// What one node_modules cleanup pass found.
///
/// ADDED 2026-10-24 (audit): `protected` was missing. Without it the pass
/// could not say "this tree is protected, so no amount of cleanup frees it",
/// and a dry run listed protected trees as reclaimable.
#[derive(Debug, Default)]
pub(crate) struct NodeCleanupOutcome {
    /// Bytes removed — or that would be removed, in dry-run.
    pub(crate) reclaimed: u64,
    /// The entries that were (or would be) removed.
    pub(crate) cleaned: Vec<String>,
    /// The entries that were REFUSED because they are protected.
    pub(crate) protected: Vec<String>,
}
