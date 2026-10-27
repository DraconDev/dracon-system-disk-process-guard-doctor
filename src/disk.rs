//! The `dracon-system disk` subcommand group.
//!
//! The objective asks for an explicit, nitpicky disk cleaner: one command that
//! brings EVERY trash class it knows about into a single report, with a
//! per-path disposition and a reason a human can act on. `guard clean` grew
//! these detectors piecemeal and each one needs its own flag, so there was no
//! single answer to "what is actually eating this disk?".
//!
//! Dispositions (the vocabulary the output uses):
//!
//! - `APPLY`  — the class is safe and allowed in this mode; it was (audit) or
//!   would be (report) deleted.
//! - `SKIP`   — a safety rule refused it: a live process holds it, it is too
//!   fresh, or a root-owned path needs a privileged command. The reason names
//!   the rule AND the owning pid where one exists.
//! - `REPORT` — never auto-deleted by design (stray dirs, backup bundles, dead
//!   swapfiles). The path and the exact command are printed; a human decides.
//!
//! Mode semantics (objective, audit MED): the DEFAULT is the free-the-space
//! mode, and it applies only the SAFE classes. `--report` is the dry run and
//! frees nothing. `--apply` is accepted as an explicit synonym for the default
//! so a script that spelled it out keeps working.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::disk_refs::{ProcSnapshot, ReferenceCapture};
use crate::disk_tmp::{
    find_oversized_logs, find_temp_cruft, LOG_ROOTS, TEMP_ROOTS,
};
use crate::git_temp::{
    clean_git_temp_files, find_git_temp_files, git_owner_for, live_git_owners,
    repo_root_for_git_path,
};
use crate::policy::{expand_tilde, human_bytes};
use crate::space_report::{
    active_swap_paths, check_swap_health, classify_stray, classify_swapfile, DeadSwapfile,
    StrayItem, UnreferencedSwap, WrongSwap,
};

/// What the cleaner proposes to do with one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Safe class, allowed in this mode.
    Apply,
    /// Refused by a safety rule; `reason` says which.
    Skip,
    /// Never auto-deleted; a human decides.
    Report,
}

impl Disposition {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Apply => "APPLY",
            Self::Skip => "SKIP",
            Self::Report => "REPORT",
        }
    }
}

/// The objective's four trash classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TrashClass {
    /// 1. Orphaned git temp packs.
    GitTemp,
    /// 2. /tmp, /var/tmp, oversized/rotated logs, dead swapfiles.
    TempAndLogs,
    /// 3. Package/browser caches and old Nix generations.
    CachesAndNix,
    /// 4. Stray top-level dirs and old backup bundles.
    Strays,
}

impl TrashClass {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GitTemp => "1 orphaned git temp",
            Self::TempAndLogs => "2 tmp + logs",
            Self::CachesAndNix => "3 caches + nix",
            Self::Strays => "4 stray dirs/bundles",
        }
    }

    pub(crate) const ALL: [TrashClass; 4] = [
        Self::GitTemp,
        Self::TempAndLogs,
        Self::CachesAndNix,
        Self::Strays,
    ];
}

/// One classified finding.
#[derive(Debug, Clone)]
pub(crate) struct Finding {
    pub(crate) class: TrashClass,
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) disposition: Disposition,
    /// Why this disposition, in words a human can act on.
    pub(crate) reason: String,
    /// The `/proc` exe/cwd/fd capture taken BEFORE any deletion.
    pub(crate) references: ReferenceCapture,
    /// The exact command a human must run, for root-owned REPORT findings.
    pub(crate) human_command: Option<String>,
}

impl Finding {
    pub(crate) fn new(
        class: TrashClass,
        path: PathBuf,
        bytes: u64,
        disposition: Disposition,
        reason: String,
    ) -> Self {
        Self {
            class,
            path,
            bytes,
            disposition,
            reason,
            references: ReferenceCapture::default(),
            human_command: None,
        }
    }

    pub(crate) fn with_references(mut self, references: ReferenceCapture) -> Self {
        self.references = references;
        self
    }

    pub(crate) fn with_command(mut self, command: String) -> Self {
        self.human_command = Some(command);
        self
    }

    /// One report line, in the disposition-first shape the objective asks for.
    pub(crate) fn render(&self) -> String {
        let mut line = format!(
            "  • {:<6} {} ({}) — {}",
            self.disposition.label(),
            self.path.display(),
            human_bytes(self.bytes),
            self.reason
        );
        if let Some(command) = &self.human_command {
            line.push_str(&format!("\n           run: {command}"));
        }
        line
    }

    /// True when this finding carries a human action (a command or approval).
    ///
    /// Used to decide whether a per-path line is worth printing: with 33k
    /// aged `/tmp` entries a "delete this stale file" reason repeated 33k times
    /// buries the handful of findings that actually need a decision.
    pub(crate) fn needs_attention(&self) -> bool {
        matches!(self.disposition, Disposition::Report)
            || self.human_command.is_some()
            || self.disposition == Disposition::Apply
    }
}

/// Everything the cleaner found, grouped for reporting.
#[derive(Debug, Default)]
pub(crate) struct DiskReport {
    pub(crate) findings: Vec<Finding>,
    /// Classes that produced nothing, so the report can say so explicitly
    /// rather than leaving a silent gap.
    pub(crate) empty_classes: Vec<TrashClass>,
    /// True when a scan could not be performed at all (as opposed to finding
    /// nothing), which must never read as "clean".
    pub(crate) incomplete: Vec<String>,
}

impl DiskReport {
    pub(crate) fn applyable(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.disposition == Disposition::Apply)
    }

    pub(crate) fn applyable_bytes(&self) -> u64 {
        self.applyable().map(|f| f.bytes).sum()
    }

    pub(crate) fn report_only_bytes(&self) -> u64 {
        self.findings
            .iter()
            .filter(|f| f.disposition == Disposition::Report)
            .map(|f| f.bytes)
            .sum()
    }

    /// Render the report.
    ///
    /// `detail_limit` bounds how many per-path lines each class prints; the
    /// aggregate totals are always complete. 0 means "all".
    pub(crate) fn render(&self, mode: &str, detail_limit: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!("dracon-system disk — {mode}\n"));
        let total: u64 = self.findings.iter().map(|f| f.bytes).sum();
        out.push_str(&format!(
            "  {} finding(s), {} total\n",
            self.findings.len(),
            human_bytes(total)
        ));
        for class in TrashClass::ALL {
            let mut class_findings: Vec<&Finding> =
                self.findings.iter().filter(|f| f.class == class).collect();
            class_findings.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
            let class_bytes: u64 = class_findings.iter().map(|f| f.bytes).sum();
            let apply_bytes: u64 = class_findings
                .iter()
                .filter(|f| f.disposition == Disposition::Apply)
                .map(|f| f.bytes)
                .sum();
            let skip_count = class_findings
                .iter()
                .filter(|f| f.disposition == Disposition::Skip)
                .count();
            let report_count = class_findings
                .iter()
                .filter(|f| f.disposition == Disposition::Report)
                .count();
            out.push_str(&format!("\n### {}\n", class.label()));
            if class_findings.is_empty() {
                if self.empty_classes.contains(&class) {
                    out.push_str("  (nothing found)\n");
                } else {
                    out.push_str("  (not scanned in this run)\n");
                }
                continue;
            }
            out.push_str(&format!(
                "  {} path(s), {} — {} SAFE/reclaimable, {} needing a decision, {} skipped\n",
                class_findings.len(),
                human_bytes(class_bytes),
                human_bytes(apply_bytes),
                report_count,
                skip_count
            ));
            // Findings that need a human decision first, then the largest
            // reclaimable paths, so the actionable end of the list is on top.
            class_findings.sort_by(|a, b| {
                b.needs_attention()
                    .cmp(&a.needs_attention())
                    .then_with(|| b.bytes.cmp(&a.bytes))
            });
            let shown = if detail_limit == 0 {
                class_findings.len()
            } else {
                detail_limit.min(class_findings.len())
            };
            for finding in class_findings.iter().take(shown) {
                out.push_str(&finding.render());
                out.push('\n');
            }
            if shown < class_findings.len() {
                let rest: u64 = class_findings
                    .iter()
                    .skip(shown)
                    .map(|f| f.bytes)
                    .sum();
                out.push_str(&format!(
                    "  … {} more path(s), {} (use --detail 0, or --json for all)\n",
                    class_findings.len() - shown,
                    human_bytes(rest)
                ));
            }
        }
        out.push_str(&format!(
            "\nSAFE classes reclaimable now: {}\n",
            human_bytes(self.applyable_bytes())
        ));
        out.push_str(&format!(
            "REPORT-ONLY (needs a human decision): {}\n",
            human_bytes(self.report_only_bytes())
        ));
        if !self.incomplete.is_empty() {
            out.push_str("\n⚠️ INCOMPLETE SCAN (a clean result is not implied):\n");
            for note in &self.incomplete {
                out.push_str(&format!("  ! {note}\n"));
            }
        }
        out
    }
}

/// Cache families the objective names, with the directory each one lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CacheFamily {
    CargoRegistry,
    Npm,
    Bun,
    Pip,
    GoModules,
    Playwright,
    Puppeteer,
    Chrome,
    /// ADDED 2026-10-27 (leftover-audit follow-up): agent-tool caches are
    /// scoped to their cache SUBDIRECTORIES, never the whole dotdir —
    /// `.codex` holds auth and plugin state, `.hermes` holds backups, cron
    /// and hooks. Unowned families REPORT with the manager's own purge
    /// command, exactly like the other families; live-held paths SKIP.
    CodexCache,
    CodexPluginsCache,
    HermesCache,
    /// ADDED 2026-10-27 (audit MED): the hermes audio/image caches are
    /// regeneratable media caches that grow to hundreds of MiB and are
    /// already separate from `.hermes/cache`. Pointing at them is the whole
    /// value of the family — they hold the bytes, the dotdir does not.
    HermesAudioCache,
    HermesImageCache,
}

impl CacheFamily {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CargoRegistry => "cargo registry",
            Self::Npm => "npm",
            Self::Bun => "bun",
            Self::Pip => "pip",
            Self::GoModules => "go modules",
            Self::Playwright => "playwright",
            Self::Puppeteer => "puppeteer",
            Self::Chrome => "chrome",
            // Labels stay cache-FREE: the report message already appends
            // " cache; no owning process", so `hermes cache` here would
            // render as "hermes cache cache".
            Self::CodexCache => "codex",
            Self::CodexPluginsCache => "codex plugins",
            Self::HermesCache => "hermes",
            Self::HermesAudioCache => "hermes audio",
            Self::HermesImageCache => "hermes image",
        }
    }

    /// Relative to the user's home directory.
    pub(crate) fn relative_path(self) -> &'static str {
        match self {
            Self::CargoRegistry => ".cargo/registry/cache",
            Self::Npm => ".npm",
            Self::Bun => ".bun/install/cache",
            Self::Pip => ".cache/pip",
            Self::GoModules => "go/pkg/mod/cache/download",
            Self::Playwright => ".cache/ms-playwright",
            Self::Puppeteer => ".cache/puppeteer",
            Self::Chrome => ".cache/google-chrome",
            Self::CodexCache => ".codex/cache",
            Self::CodexPluginsCache => ".codex/plugins/cache",
            Self::HermesCache => ".hermes/cache",
            Self::HermesAudioCache => ".hermes/audio_cache",
            Self::HermesImageCache => ".hermes/image_cache",
        }
    }

    /// Process names that use this cache family.
    pub(crate) fn process_names(self) -> &'static [&'static str] {
        match self {
            Self::CargoRegistry => &["cargo", "rustc", "cargo-build", "cargo-clippy"],
            Self::Npm => &["npm", "node", "npx", "pnpm", "yarn"],
            Self::Bun => &["bun", "bunx"],
            Self::Pip => &["pip", "pip3", "python", "python3", "uv"],
            Self::GoModules => &["go", "gopls"],
            Self::Playwright => &["playwright", "node", "chrome", "chromium"],
            Self::Puppeteer => &["puppeteer", "node", "chrome", "chromium"],
            Self::Chrome => &["chrome", "chromium", "google-chrome"],
            // A process literally named `codex` (the TUI/daemon binary) or
            // `hermes`/`hermes-agent`. A python-hosted agent shows comm
            // `python` and will NOT match — the family then reports as
            // unowned (still protected by the live-fd SKIP), which is the
            // safe degradation, not a false "in use".
            Self::CodexCache | Self::CodexPluginsCache => &["codex"],
            Self::HermesCache => &["hermes", "hermes-agent"],
            Self::HermesAudioCache | Self::HermesImageCache => &["hermes", "hermes-agent"],
        }
    }

    pub(crate) const ALL: [CacheFamily; 13] = [
        Self::CargoRegistry,
        Self::Npm,
        Self::Bun,
        Self::Pip,
        Self::GoModules,
        Self::Playwright,
        Self::Puppeteer,
        Self::Chrome,
        Self::CodexCache,
        Self::CodexPluginsCache,
        Self::HermesCache,
        Self::HermesAudioCache,
        Self::HermesImageCache,
    ];
}

/// A process that uses a cache family, with the pid the report must show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheUser {
    pub(crate) pid: u32,
    pub(crate) name: String,
}

/// Every process using each cache family, keyed by family.
///
/// ADDED 2026-10-27 (audit MED): the existing detection answered only "is SOME
/// npm operation running", which the report could render as
/// "active npm operation detected" with no pid. The objective requires the
/// owning PID, which is also what a human needs to decide whether to wait or
/// stop the process.
pub(crate) fn cache_users(proc_root: &Path) -> std::collections::BTreeMap<CacheFamily, Vec<CacheUser>> {
    let mut users: std::collections::BTreeMap<CacheFamily, Vec<CacheUser>> =
        std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return users;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim().to_string();
        for family in CacheFamily::ALL {
            if family
                .process_names()
                .iter()
                .any(|name| comm == *name || comm.starts_with(&format!("{name}-")))
            {
                users.entry(family).or_default().push(CacheUser {
                    pid,
                    name: comm.clone(),
                });
            }
        }
    }
    for list in users.values_mut() {
        list.sort_by_key(|user| user.pid);
        list.dedup();
    }
    users
}

/// Render the owning processes for a cache family, or `None` when it is idle.
pub(crate) fn describe_cache_users(users: &[CacheUser]) -> Option<String> {
    if users.is_empty() {
        return None;
    }
    // Group by process name: a browser cache is held by dozens of renderer
    // processes and listing each pid buries the actual finding. Every name
    // still shows a pid, so the owning PID is always present.
    let mut by_name: std::collections::BTreeMap<&str, Vec<u32>> = std::collections::BTreeMap::new();
    for user in users {
        by_name.entry(user.name.as_str()).or_default().push(user.pid);
    }
    let rendered: Vec<String> = by_name
        .iter()
        .map(|(name, pids)| {
            if pids.len() == 1 {
                format!("{name} (pid {})", pids[0])
            } else {
                let head: Vec<String> = pids.iter().take(2).map(|p| format!("pid {p}")).collect();
                format!("{name} ({} processes; {})", pids.len(), head.join(", "))
            }
        })
        .collect();
    Some(rendered.join("; "))
}

/// A dot-prefixed `/tmp` entry is reported (never auto-deleted) above this
/// size, so a real space hog cannot hide behind a leading dot.
pub(crate) const HIDDEN_TMP_REPORT_BYTES: u64 = 256 * 1024 * 1024;

/// Knobs the disk cleaner reads from the guard policy.
#[derive(Debug, Clone)]
pub(crate) struct DiskOptions {
    pub(crate) rust_search_roots: String,
    pub(crate) stray_scan_roots: String,
    pub(crate) dead_swapfile_candidates: String,
    pub(crate) expected_swap_prefixes: String,
    pub(crate) nixos_swap_config: String,
    pub(crate) git_temp_min_age_hours: u64,
    pub(crate) tmp_min_age_hours: u64,
    /// ADDED 2026-10-27 (leftover-audit follow-up): the one-shot audit
    /// judges e2e/test-litter names against this SHORT floor so the sweep
    /// REPORTs them instead of staying blind until the general floor.
    pub(crate) tmp_e2e_patterns: String,
    pub(crate) tmp_e2e_min_age_hours: u64,
}

/// Which age floor governs a `/tmp` entry: the short e2e floor when the
/// basename matches a fast-path pattern, otherwise the general floor.
/// Returns the governing floor when the entry is at least that old (`None`
/// = not yet cruft). An empty pattern list disables the fast path — the
/// caller passes empty patterns when the e2e floor is 0, mirroring the
/// guard daemon's `e2e_min_age_hours == 0` early return.
pub(crate) fn tmp_cruft_floor(
    file_name: &str,
    age_hours: u64,
    e2e_patterns: &[String],
    general_floor_hours: u64,
    e2e_floor_hours: u64,
) -> Option<u64> {
    let floor = if e2e_patterns.is_empty() {
        general_floor_hours
    } else if crate::tmp_e2e_match(file_name, e2e_patterns) {
        e2e_floor_hours
    } else {
        general_floor_hours
    };
    (age_hours >= floor).then_some(floor)
}

/// Whether a tree is under live churn, judged by the age of its NEWEST
/// content rather than the directory's own mtime.
///
/// This is the one-shot mirror of the guard daemon's
/// `tree_has_fresh_content` check. It exists because a top-level directory
/// mtime does not move when its contents are rewritten: measured 2026-10-27,
/// `/tmp/node-compile-cache` had a 17h-old top mtime while 317 files inside
/// were newer than 6h and the newest was seconds old. Deleting on the top
/// mtime would remove a tree a live process is writing to right now.
///
/// `newest_content_age_hours == None` means the walk could not prove the tree
/// is old, so it counts as FRESH (fail closed) — never as deletable.
pub(crate) fn tree_is_live_churn(
    newest_content_age_hours: Option<u64>,
    threshold_hours: u64,
) -> bool {
    match newest_content_age_hours {
        Some(age) => age < threshold_hours,
        None => true,
    }
}

/// Run the full audit and classify every finding.
///
/// `proc_root` and `run_root` are parameters so the whole classification can be
/// tested against fixtures instead of the live `/proc`.
pub(crate) fn audit(
    options: &DiskOptions,
    proc_root: &Path,
    now: SystemTime,
    report_only: bool,
    selection: Option<&[TrashClass]>,
) -> DiskReport {
    // Only walk the classes actually asked for. Filtering findings AFTER a full
    // scan made `--class git` slower than the whole audit, because the three
    // skipped classes were still walked.
    let wanted = |class: TrashClass| {
        selection.is_none_or(|selected| selected.contains(&class))
    };
    let mut report = DiskReport::default();
    // One /proc pass for the whole run: per-finding walks took >15 minutes on
    // a host with thousands of /tmp entries.
    let snapshot = ProcSnapshot::read(proc_root);

    // --- class 1: orphaned git temp -----------------------------------------
    let git_roots: Vec<PathBuf> = split_roots(&options.rust_search_roots);
    let owners = live_git_owners(proc_root);
    let mut git_found = 0usize;
    for root in git_roots.iter().filter(|_| wanted(TrashClass::GitTemp)) {
        for candidate in find_git_temp_files(root, 4) {
            git_found += 1;
            let references = snapshot.capture(&candidate.path);
            let owner = repo_root_for_git_path(&candidate.path)
                .and_then(|repo| git_owner_for(&owners, &repo).map(|o| format!("{}(pid {})", o.name, o.pid)));
            let finding = if let Some(reason) = references.blocking_reason(&candidate.path) {
                Finding::new(
                    TrashClass::GitTemp,
                    candidate.path.clone(),
                    candidate.bytes,
                    Disposition::Skip,
                    format!("in use — {reason}"),
                )
                .with_references(references)
            } else if let Some(owner) = owner {
                Finding::new(
                    TrashClass::GitTemp,
                    candidate.path.clone(),
                    candidate.bytes,
                    Disposition::Skip,
                    format!("live {owner} is packing this repository"),
                )
                .with_references(references)
            } else if !candidate.is_expired(options.git_temp_min_age_hours) {
                Finding::new(
                    TrashClass::GitTemp,
                    candidate.path.clone(),
                    candidate.bytes,
                    Disposition::Skip,
                    format!(
                        "younger than git_temp_min_age_hours={}h (may be an in-flight pack)",
                        options.git_temp_min_age_hours
                    ),
                )
                .with_references(references)
            } else {
                Finding::new(
                    TrashClass::GitTemp,
                    candidate.path.clone(),
                    candidate.bytes,
                    Disposition::Apply,
                    format!(
                        "orphaned scratch, older than {}h, no owning git process",
                        options.git_temp_min_age_hours
                    ),
                )
                .with_references(references)
            };
            report.findings.push(finding);
        }
    }
    // The Apply set is only meaningful in the freeing mode.
    if report_only {
        for finding in &mut report.findings {
            if finding.disposition == Disposition::Apply {
                finding.reason.push_str(" — report mode frees nothing");
            }
        }
    }
    if git_found == 0 {
        report.empty_classes.push(TrashClass::GitTemp);
    }

    // --- class 2: /tmp, /var/tmp, logs, dead swapfiles ----------------------
    let mut tmp_found = 0usize;
    // ADDED 2026-10-27 (leftover-audit follow-up): e2e/test-litter names
    // are judged against the SHORT floor even here, so the one-shot sweep
    // REPORTs them instead of staying blind until the general floor. The
    // walk uses the lower of the two floors; entries that clear neither
    // floor are dropped below. Empty patterns = fast path disabled
    // (mirrors the guard daemon's e2e_min_age_hours == 0 early return).
    let e2e_patterns: Vec<String> = if options.tmp_e2e_min_age_hours == 0 {
        Vec::new()
    } else {
        crate::parse_tmp_e2e_patterns(&options.tmp_e2e_patterns)
    };
    let walk_floor = if e2e_patterns.is_empty() {
        options.tmp_min_age_hours
    } else {
        options
            .tmp_min_age_hours
            .min(options.tmp_e2e_min_age_hours)
    };
    let tmp_entries = if wanted(TrashClass::TempAndLogs) {
        find_temp_cruft(&TEMP_ROOTS, walk_floor, now)
    } else {
        Vec::new()
    };
    for entry in tmp_entries {
        // A dot-prefixed top-level entry in /tmp is an application's private
        // runtime state (`/tmp/.X11-unix`, sockets, lock dirs), not build
        // litter. Deleting it breaks the owning application, so it is excluded
        // by default and only reported when it is genuinely large.
        let hidden = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with('.'))
            .unwrap_or(false);
        if hidden && entry.bytes < HIDDEN_TMP_REPORT_BYTES {
            continue;
        }
        // Fast-path floor decision: a machine-generated test-copy name only
        // needs the short floor; anything clearing neither floor was picked
        // up by the lowered walk floor and is dropped here, uncounted.
        let basename = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let floor = match tmp_cruft_floor(
            &basename,
            entry.age_hours,
            &e2e_patterns,
            options.tmp_min_age_hours,
            options.tmp_e2e_min_age_hours,
        ) {
            Some(floor) => floor,
            None => continue,
        };
        let fast_path = floor == options.tmp_e2e_min_age_hours
            && floor != options.tmp_min_age_hours
            && !e2e_patterns.is_empty();
        tmp_found += 1;
        let references = snapshot.capture(&entry.path);
        // Live-churn gate (2026-10-27, audit HIGH): a directory whose NESTED
        // content was written inside the governing floor is still in use, no
        // matter how old its own mtime is. The daemon refuses to touch such a
        // tree (`tree_has_fresh_content`); the one-shot audit must not be a
        // weaker path to deletion, so it downgrades APPLY to REPORT and says
        // why. Files are their own content, so the gate only applies to dirs.
        let live_churn = entry.is_dir
            && tree_is_live_churn(entry.newest_content_age_hours, floor);
        let finding = if let Some(reason) = references.blocking_reason(&entry.path) {
            Finding::new(
                TrashClass::TempAndLogs,
                entry.path.clone(),
                entry.bytes,
                Disposition::Skip,
                format!("in use — {reason}"),
            )
        } else {
            Finding::new(
                TrashClass::TempAndLogs,
                entry.path.clone(),
                entry.bytes,
                if entry
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().starts_with('.'))
                    .unwrap_or(false)
                    || live_churn
                {
                    Disposition::Report
                } else {
                    Disposition::Apply
                },
                if live_churn {
                    // State the content age, not the directory mtime: that
                    // difference IS the reason it is not deletable.
                    let content = entry
                        .newest_content_age_hours
                        .map(|age| format!("{age}h"))
                        .unwrap_or_else(|| "unreadable".to_string());
                    format!(
                        "{} top-level mtime {}h old but newest nested content is {} old — live churn, under the {}h floor; never auto-deleted",
                        entry.label(), entry.age_hours, content, floor
                    )
                } else if fast_path {
                    format!(
                        "{} untouched for {}h (e2e fast-path floor tmp_e2e_min_age_hours={})",
                        entry.label(),
                        entry.age_hours,
                        options.tmp_e2e_min_age_hours
                    )
                } else {
                    format!(
                        "{} untouched for {}h (tmp_min_age_hours={})",
                        entry.label(),
                        entry.age_hours,
                        options.tmp_min_age_hours
                    )
                },
            )
        };
        report.findings.push(finding.with_references(references));
    }
    let logs = if wanted(TrashClass::TempAndLogs) {
        find_oversized_logs(&LOG_ROOTS, options.tmp_min_age_hours, now)
    } else {
        Vec::new()
    };
    for log in logs {
        tmp_found += 1;
        let references = snapshot.capture(&log.path);
        let finding = if let Some(reason) = references.blocking_reason(&log.path) {
            Finding::new(
                TrashClass::TempAndLogs,
                log.path.clone(),
                log.bytes,
                Disposition::Skip,
                format!("in use — {reason}"),
            )
        } else {
            Finding::new(
                TrashClass::TempAndLogs,
                log.path.clone(),
                log.bytes,
                Disposition::Apply,
                log.reason(),
            )
        };
        report.findings.push(finding.with_references(references));
    }
    let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
    let active = active_swap_paths(&swaps);
    let swap_candidates = if wanted(TrashClass::TempAndLogs) {
        split_roots(&options.dead_swapfile_candidates)
    } else {
        Vec::new()
    };
    // Paths already reported as dead swapfiles: the unreferenced check
    // below must not double-report them.
    let mut dead_reported: Vec<PathBuf> = Vec::new();
    for path in swap_candidates {
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if let Some(dead) = classify_swapfile(&path, metadata.len(), &active) {
            tmp_found += 1;
            dead_reported.push(path.clone());
            report.findings.push(dead_swapfile_finding(dead));
        }
    }
    // --- wrong-swap placement + NixOS references (REPORT-only) ------------
    // An ACTIVE swapfile on the wrong device is the incident in reverse:
    // the dead-swapfile scan above deliberately ignores active files, so
    // without this check a swapfile on the nvme root would be invisible
    // until something deleted it.
    if wanted(TrashClass::TempAndLogs) {
        let expected = split_prefixes(&options.expected_swap_prefixes);
        let config_path = expand_tilde(&options.nixos_swap_config);
        let outcome = check_swap_health(&expected, &dead_reported, &config_path);
        for wrong in outcome.wrong {
            tmp_found += 1;
            report
                .findings
                .push(wrong_swap_finding(wrong, &expected));
        }
        for unref in outcome.unreferenced {
            // Already reported as a dead swapfile above: one finding per
            // path, the dead one (it carries the removal command).
            if dead_reported.iter().any(|p| p == &unref.path) {
                continue;
            }
            tmp_found += 1;
            report
                .findings
                .push(unreferenced_swapfile_finding(unref, &config_path));
        }
        if !outcome.config_known {
            // UNKNOWN is not a finding, but silence would read as "clean".
            let note = match std::fs::read_to_string(&config_path) {
                Err(_) => format!(
                    "NixOS swap config unreadable; swap reference check skipped: {}",
                    config_path.display()
                ),
                Ok(_) => format!(
                    "no swapDevices block in {}; swap reference check skipped",
                    config_path.display()
                ),
            };
            report.incomplete.push(note);
        }
    }
    if tmp_found == 0 {
        report.empty_classes.push(TrashClass::TempAndLogs);
    }

    // --- class 3: caches and nix generations -------------------------------
    let mut cache_found = 0usize;
    if !wanted(TrashClass::CachesAndNix) {
        // skipped entirely
    } else if let Some(home) = dirs::home_dir() {
        let users = cache_users(proc_root);
        for family in CacheFamily::ALL {
            let path = home.join(family.relative_path());
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            cache_found += 1;
            let bytes = crate::disk_tmp::directory_bytes(&path);
            let references = snapshot.capture(&path);
            let in_use = describe_cache_users(users.get(&family).map(Vec::as_slice).unwrap_or(&[]));
            let finding = match (references.blocking_reason(&path), in_use) {
                (Some(reason), _) => Finding::new(
                    TrashClass::CachesAndNix,
                    path.clone(),
                    bytes,
                    Disposition::Skip,
                    format!("in use — {reason}"),
                ),
                (None, Some(owners)) => Finding::new(
                    TrashClass::CachesAndNix,
                    path.clone(),
                    bytes,
                    Disposition::Skip,
                    format!("owned by {owners}; purge from within {} after it exits", family.label()),
                ),
                (None, None) => Finding::new(
                    TrashClass::CachesAndNix,
                    path.clone(),
                    bytes,
                    Disposition::Report,
                    format!(
                        "{} cache; no owning process. Purge with the manager's own command so the cache index stays consistent (no --apply path)",
                        family.label()
                    ),
                ),
            };
            report.findings.push(finding.with_references(references));
        }
    } else {
        report.incomplete.push("home directory unknown; cache scan skipped".to_string());
    }
    if cache_found == 0 {
        report.empty_classes.push(TrashClass::CachesAndNix);
    }

    // --- class 4: stray dirs and backup bundles ----------------------------
    let stray_roots = if wanted(TrashClass::Strays) {
        split_roots(&options.stray_scan_roots)
    } else {
        Vec::new()
    };
    let mut stray_found = 0usize;
    for root in &stray_roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            report
                .incomplete
                .push(format!("stray scan root unreadable: {}", root.display()));
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Classify FIRST: sizing every entry under `~` walked `~/Dev`
            // (hundreds of thousands of files) just to discard it, which made
            // the stray scan slower than the whole rest of the audit.
            let Some(stray) = classify_stray(&path, &name, 0, metadata.is_dir()) else {
                continue;
            };
            let bytes = if metadata.is_dir() {
                crate::disk_tmp::directory_bytes(&path)
            } else {
                metadata.len()
            };
            let stray = StrayItem { bytes, ..stray };
            stray_found += 1;
            let references = snapshot.capture(&path);
            report
                .findings
                .push(stray_finding(stray).with_references(references));
        }
    }
    if stray_found == 0 {
        report.empty_classes.push(TrashClass::Strays);
    }

    report
}

fn split_roots(raw: &str) -> Vec<PathBuf> {
    raw.split(',')
        .filter_map(|s| {
            let s = s.trim();
            (!s.is_empty()).then(|| expand_tilde(s))
        })
        .collect()
}

/// Comma-separated policy prefixes (no tilde: these are system paths).
fn split_prefixes(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// REPORT an ACTIVE swap device outside the expected prefixes. Never
/// auto-deleted: the fix is a human decision (move the swap, or extend
/// `expected_swap_prefixes` for a legitimate device).
pub(crate) fn wrong_swap_finding(wrong: WrongSwap, expected: &[String]) -> Finding {
    Finding::new(
        TrashClass::TempAndLogs,
        wrong.path,
        wrong.bytes,
        Disposition::Report,
        format!(
            "ACTIVE swap on an unexpected device (expected under: {}); fix the placement or extend expected_swap_prefixes, never delete it blind",
            expected.join(", ")
        ),
    )
}

/// REPORT a swapfile with no NixOS `swapDevices` entry. Inactive ones carry
/// the exact removal command like dead swapfiles do; ACTIVE ones never do —
/// suggesting `rm` on live swap would be the incident all over again.
pub(crate) fn unreferenced_swapfile_finding(found: UnreferencedSwap, config: &Path) -> Finding {
    let reason = if found.is_active {
        format!(
            "ACTIVE swap with no NixOS swapDevices entry in {}: a rebuild/reboot may lose it — add a device entry, never delete it while active",
            config.display()
        )
    } else {
        format!(
            "not referenced by any NixOS swapDevices entry in {}: stale or unmanaged, delete only by explicit approval",
            config.display()
        )
    };
    let finding = Finding::new(
        TrashClass::TempAndLogs,
        found.path.clone(),
        found.bytes,
        Disposition::Report,
        reason,
    );
    if found.is_active {
        return finding;
    }
    let command = if crate::space_report::is_writable_by_current_user(&found.path) {
        format!("rm -f {}", found.path.display())
    } else {
        format!("sudo rm -f {}", found.path.display())
    };
    finding.with_command(command)
}

pub(crate) fn dead_swapfile_finding(dead: DeadSwapfile) -> Finding {
    let command = dead.removal_command();
    Finding::new(
        TrashClass::TempAndLogs,
        dead.path,
        dead.bytes,
        Disposition::Report,
        "inactive swapfile (not in /proc/swaps): deleting it is a privileged decision, never automatic"
            .to_string(),
    )
    .with_command(command)
}

pub(crate) fn stray_finding(stray: StrayItem) -> Finding {
    Finding::new(
        TrashClass::Strays,
        stray.path,
        stray.bytes,
        Disposition::Report,
        format!(
            "{}: never auto-deleted, needs explicit per-path approval",
            stray.kind.label()
        ),
    )
}

/// Delete the APPLY-class findings and return `(bytes freed, records)`.
///
/// Every deleted path carries the pre-delete reference capture and the owner
/// check the objective requires, so the evidence file can record it verbatim.
pub(crate) fn apply(
    report: &mut DiskReport,
    proc_root: &Path,
    git_temp_min_age_hours: u64,
) -> (u64, Vec<DeletedRecord>) {
    let mut freed = 0u64;
    let mut records = Vec::new();
    let owners = live_git_owners(proc_root);
    let snapshot = ProcSnapshot::read(proc_root);

    // Group git-temp findings so the battle-tested cleaner (age floor, symlink
    // refusal, per-repo ownership) keeps doing the actual unlink.
    let mut git_candidates = Vec::new();
    let mut others: Vec<Finding> = Vec::new();
    for finding in report.findings.drain(..) {
        if finding.disposition != Disposition::Apply {
            others.push(finding);
            continue;
        }
        if finding.class == TrashClass::GitTemp {
            // The audit already ran the age floor; re-read the mtime so the
            // cleaner re-checks it rather than trusting the audit's snapshot.
            let age_hours = std::fs::metadata(&finding.path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| {
                    SystemTime::now()
                        .duration_since(modified)
                        .ok()
                        .map(|delta| delta.as_secs() / 3600)
                });
            git_candidates.push(crate::git_temp::GitTempFile {
                path: finding.path.clone(),
                bytes: finding.bytes,
                age_hours,
            });
            others.push(finding);
        } else {
            others.push(finding);
        }
    }
    report.findings = others;

    let owner_of = |candidate: &Path| -> Option<String> {
        let repo = repo_root_for_git_path(candidate)?;
        git_owner_for(&owners, &repo).map(|o| format!("{}(pid {})", o.name, o.pid))
    };
    let outcome = clean_git_temp_files(git_candidates, git_temp_min_age_hours, owner_of, true);
    for cleaned in outcome.cleaned {
        if let Some(finding) = report
            .findings
            .iter()
            .find(|f| f.path == cleaned.path)
            .cloned()
        {
            freed += cleaned.bytes;
            records.push(DeletedRecord::from_finding(&finding));
            report.findings.retain(|f| f.path != cleaned.path);
        }
    }

    // Non-git-temp APPLY findings: re-capture references immediately before the
    // unlink, so the recorded capture is provably pre-delete.
    let mut remaining = Vec::new();
    for finding in report.findings.drain(..) {
        if finding.disposition != Disposition::Apply {
            remaining.push(finding);
            continue;
        }
        let fresh = snapshot.capture(&finding.path);
        if let Some(reason) = fresh.blocking_reason(&finding.path) {
            let mut skipped = finding.clone();
            skipped.disposition = Disposition::Skip;
            skipped.reason = format!("in use at delete time — {reason}");
            skipped.references = fresh;
            remaining.push(skipped);
            continue;
        }
        let record = DeletedRecord::from_finding(&finding).with_references(&fresh);
        let bytes = finding.bytes;
        let result = if std::fs::symlink_metadata(&finding.path)
            .is_ok_and(|m| m.is_dir())
        {
            std::fs::remove_dir_all(&finding.path)
        } else {
            std::fs::remove_file(&finding.path)
        };
        match result {
            Ok(()) => {
                freed += bytes;
                records.push(record);
            }
            Err(error) => {
                let mut failed = finding;
                failed.reason = format!("delete failed: {error}");
                failed.disposition = Disposition::Skip;
                remaining.push(failed);
            }
        }
    }
    report.findings = remaining;
    (freed, records)
}

/// One path the cleaner actually deleted, with its pre-delete evidence.
#[derive(Debug, Clone)]
pub(crate) struct DeletedRecord {
    pub(crate) class: TrashClass,
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) references: ReferenceCapture,
}

impl DeletedRecord {
    pub(crate) fn from_finding(finding: &Finding) -> Self {
        Self {
            class: finding.class,
            path: finding.path.clone(),
            bytes: finding.bytes,
            references: finding.references.clone(),
        }
    }

    pub(crate) fn with_references(mut self, references: &ReferenceCapture) -> Self {
        self.references = references.clone();
        self
    }

    /// Markdown table row: path, pre-delete bytes, pre-delete /proc refs.
    pub(crate) fn evidence_row(&self) -> String {
        format!(
            "| `{}` | {} ({}) | {} | {} |",
            self.path.display(),
            human_bytes(self.bytes),
            self.bytes,
            self.class.label(),
            self.references.describe().replace('|', "\\|")
        )
    }
}

#[cfg(test)]
mod disk_tests {
    use super::*;

    #[test]
    fn every_objective_trash_class_has_a_label() {
        let labels: Vec<&str> = TrashClass::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels.len(), 4);
        assert!(labels[0].starts_with('1'));
        assert!(labels[3].starts_with('4'));
    }

    #[test]
    fn the_three_dispositions_render_distinctly() {
        assert_eq!(Disposition::Apply.label(), "APPLY");
        assert_eq!(Disposition::Skip.label(), "SKIP");
        assert_eq!(Disposition::Report.label(), "REPORT");
    }

    #[test]
    fn a_report_names_all_four_classes_even_when_nothing_is_found() {
        let report = DiskReport {
            empty_classes: TrashClass::ALL.to_vec(),
            ..DiskReport::default()
        };
        let text = report.render("audit", 0);
        for class in TrashClass::ALL {
            assert!(
                text.contains(class.label()),
                "report must name class {}",
                class.label()
            );
        }
        assert!(text.contains("(nothing found)"));
    }

    #[test]
    fn an_incomplete_scan_never_reads_as_clean() {
        let mut report = DiskReport::default();
        report
            .incomplete
            .push("stray scan root unreadable: /nope".to_string());
        let text = report.render("audit", 0);
        assert!(text.contains("INCOMPLETE SCAN"));
        assert!(text.contains("a clean result is not implied"));
    }

    #[test]
    fn a_dead_swapfile_is_report_only_with_the_exact_command() {
        let finding = dead_swapfile_finding(DeadSwapfile {
            path: PathBuf::from("/var/lib/swapfile"),
            bytes: 32 * 1024 * 1024 * 1024,
            needs_root: true,
        });
        assert_eq!(finding.disposition, Disposition::Report);
        let text = finding.render();
        assert!(text.contains("REPORT"));
        assert!(text.contains("run: sudo rm -f /var/lib/swapfile"));
    }

    /// 2026-10-27 (wrong-swap check): an ACTIVE swapfile on the wrong
    /// device is REPORT-only and never carries a removal command.
    #[test]
    fn a_wrong_swap_is_report_only_without_a_command() {
        let finding = wrong_swap_finding(
            WrongSwap {
                path: PathBuf::from("/var/lib/swapfile"),
                bytes: 32 * 1024 * 1024 * 1024,
            },
            &["/dev/zram".to_string(), "/mnt/data".to_string()],
        );
        assert_eq!(finding.disposition, Disposition::Report);
        assert_eq!(finding.class, TrashClass::TempAndLogs);
        let text = finding.render();
        assert!(text.contains("REPORT"));
        assert!(text.contains("ACTIVE swap on an unexpected device"));
        assert!(text.contains("/dev/zram"));
        assert!(
            !text.contains("run:"),
            "live swap must never get a removal command: {text}"
        );
    }

    /// 2026-10-27 (wrong-swap check): an inactive unreferenced swapfile
    /// carries the exact command; an ACTIVE one never does.
    #[test]
    fn an_unreferenced_swapfile_commands_only_when_inactive() {
        let config = Path::new("/etc/nixos/configuration.nix");
        let inactive = unreferenced_swapfile_finding(
            UnreferencedSwap {
                path: PathBuf::from("/var/lib/swapfile"),
                bytes: 1024,
                is_active: false,
            },
            config,
        );
        assert_eq!(inactive.disposition, Disposition::Report);
        assert!(inactive.render().contains("swapDevices"));
        assert!(
            inactive.human_command.is_some(),
            "inactive unreferenced swapfile gets the removal command"
        );
        let live = unreferenced_swapfile_finding(
            UnreferencedSwap {
                path: PathBuf::from("/mnt/data/swapfile"),
                bytes: 2048,
                is_active: true,
            },
            config,
        );
        let text = live.render();
        assert!(text.contains("ACTIVE swap with no NixOS swapDevices entry"));
        assert!(
            live.human_command.is_none(),
            "active swap must never get a removal command: {text}"
        );
    }

    #[test]
    fn cache_users_carry_the_owning_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_dir = dir.path().join("4242");
        std::fs::create_dir_all(&pid_dir).expect("mkdir");
        std::fs::write(pid_dir.join("comm"), "npm\n").expect("write comm");

        let users = cache_users(dir.path());
        let npm = users
            .get(&CacheFamily::Npm)
            .expect("npm must be detected as a cache user");
        assert_eq!(npm[0].pid, 4242);
        assert_eq!(npm[0].name, "npm");
        let described = describe_cache_users(npm).expect("description");
        assert!(
            described.contains("pid 4242"),
            "the owning PID must appear: {described}"
        );
    }

    #[test]
    fn an_idle_cache_has_no_owning_process_description() {
        assert_eq!(describe_cache_users(&[]), None);
    }

    #[test]
    fn every_named_cache_family_has_a_path_and_process_list() {
        assert_eq!(CacheFamily::ALL.len(), 13);
        for family in CacheFamily::ALL {
            assert!(!family.relative_path().is_empty());
            assert!(
                !family.process_names().is_empty(),
                "{} needs processes to detect",
                family.label()
            );
        }
    }

    #[test]
    fn bun_playwright_puppeteer_and_chrome_are_covered() {
        let labels: Vec<&str> = CacheFamily::ALL.iter().map(|c| c.label()).collect();
        for wanted in ["bun", "playwright", "puppeteer", "chrome"] {
            assert!(
                labels.contains(&wanted),
                "objective class 3 names {wanted}; covered families: {labels:?}"
            );
        }
    }

    #[test]
    fn e2e_names_use_the_short_floor_while_plain_names_use_the_general_floor() {
        let patterns =
            crate::parse_tmp_e2e_patterns(&crate::policy::default_tmp_e2e_patterns());
        // e2e litter between the two floors is cruft under the short floor.
        assert_eq!(
            tmp_cruft_floor("ai-tts-pro-eval-InAhom", 7, &patterns, 24, 6),
            Some(6)
        );
        assert_eq!(tmp_cruft_floor("iso-route", 7, &patterns, 24, 6), Some(6));
        // Younger than even the short floor: not yet cruft.
        assert_eq!(tmp_cruft_floor("iso-route", 5, &patterns, 24, 6), None);
        // A plain name between the floors is held back by the general floor.
        assert_eq!(tmp_cruft_floor("my-notes.md", 7, &patterns, 24, 6), None);
        assert_eq!(
            tmp_cruft_floor("my-notes.md", 25, &patterns, 24, 6),
            Some(24)
        );
        // Disabled fast path (empty patterns): the general floor governs all.
        assert_eq!(tmp_cruft_floor("iso-route", 7, &[], 24, 6), None);
        assert_eq!(tmp_cruft_floor("iso-route", 25, &[], 24, 6), Some(24));
    }

    #[test]
    fn an_e2e_floor_above_the_general_floor_is_not_treated_as_a_fast_path() {
        // Operator corner: general lowered below the e2e floor. A pattern
        // match then governs with the LONGER e2e floor, exactly as the daemon
        // does — the classifier reports that floor so the reason string can
        // never claim "fast path" for an entry that was actually held back.
        let patterns = crate::parse_tmp_e2e_patterns("iso-route");
        assert_eq!(tmp_cruft_floor("iso-route", 7, &patterns, 6, 24), None);
        assert_eq!(
            tmp_cruft_floor("iso-route", 25, &patterns, 6, 24),
            Some(24)
        );
        // A non-matching name still follows the (lower) general floor.
        assert_eq!(tmp_cruft_floor("plain", 7, &patterns, 6, 24), Some(6));
        // And equal floors never render as a distinct fast path.
        assert_eq!(
            tmp_cruft_floor("iso-route", 7, &patterns, 6, 6),
            Some(6)
        );
    }

    #[test]
    fn the_audit_observed_families_match_the_fast_path_patterns() {
        let patterns =
            crate::parse_tmp_e2e_patterns(&crate::policy::default_tmp_e2e_patterns());
        // Every family the 2026-10-27 whole-system sweep found in /tmp.
        for name in [
            "ai-tts-decide-D6HJnh",
            "ai-tts-pro-eval-InAhom",
            "iso-route",
            "base-copy",
            "synth4",
            "junk-runner-e2e.qVEdrH",
            "node-compile-cache",
            "cw-browser-test-UUCxMS",
        ] {
            assert!(
                crate::tmp_e2e_match(name, &patterns),
                "{name} must take the fast path"
            );
        }
        // The DOT in junk-runner-e2e.* is load-bearing: a dash suffix must
        // not match, and unrelated names stay on the general floor.
        assert!(!crate::tmp_e2e_match("junk-runner-e2e-qVEdrH", &patterns));
        assert!(!crate::tmp_e2e_match("my-notes.md", &patterns));
        assert!(!crate::tmp_e2e_match("node_modules", &patterns));
    }

    #[test]
    fn agent_tool_caches_are_scoped_to_cache_subdirs_never_the_dotdir() {
        assert_eq!(
            CacheFamily::CodexCache.relative_path(),
            ".codex/cache"
        );
        assert_eq!(
            CacheFamily::CodexPluginsCache.relative_path(),
            ".codex/plugins/cache"
        );
        assert_eq!(
            CacheFamily::HermesCache.relative_path(),
            ".hermes/cache"
        );
        // Scoping proof: none of the new families may point at a bare
        // dotdir, which would put auth, backups, cron and hooks in range.
        for family in [
            CacheFamily::CodexCache,
            CacheFamily::CodexPluginsCache,
            CacheFamily::HermesCache,
            CacheFamily::HermesAudioCache,
            CacheFamily::HermesImageCache,
        ] {
            assert!(
                family.relative_path().contains('/'),
                "{} must stay inside a cache subdir",
                family.label()
            );
        }
    }

    #[test]
    fn a_tree_with_fresh_nested_content_is_never_apply() {
        // Regression for the 2026-10-27 audit HIGH: `/tmp/node-compile-cache`
        // had a 17h-old top mtime while 317 files inside were newer than 6h.
        // Aging a directory by its own mtime made that tree APPLY, so `disk
        // clean` would have deleted a directory under live writes.
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("node-compile-cache");
        std::fs::create_dir_all(&tree).expect("mkdir");
        // Old top-level mtime...
        let old = SystemTime::now() - std::time::Duration::from_secs(17 * 3600);
        std::fs::File::options()
            .read(true)
            .open(&tree)
            .expect("open dir")
            .set_modified(old)
            .expect("set dir mtime");
        // ...but content written just now.
        std::fs::write(tree.join("fresh.bin"), b"live write").expect("write");

        let (bytes, newest) =
            crate::disk_tmp::directory_bytes_and_age(&tree, SystemTime::now());
        assert!(bytes > 0);
        assert_eq!(
            newest,
            Some(0),
            "the walk must report the NEWEST nested mtime, not the dir mtime"
        );
        // The gate that decides disposition: 0h content under a 6h floor is
        // live churn, therefore REPORT and never APPLY.
        assert!(tree_is_live_churn(newest, 6));

        // And an untouched tree of the same shape is genuinely deletable.
        let stale = dir.path().join("stale-tree");
        std::fs::create_dir_all(&stale).expect("mkdir");
        std::fs::write(stale.join("old.bin"), b"stale").expect("write");
        let old_file = SystemTime::now() - std::time::Duration::from_secs(17 * 3600);
        std::fs::File::options()
            .write(true)
            .open(stale.join("old.bin"))
            .expect("open")
            .set_modified(old_file)
            .expect("set mtime");
        let (_, stale_newest) =
            crate::disk_tmp::directory_bytes_and_age(&stale, SystemTime::now());
        assert_eq!(stale_newest, Some(17));
        assert!(!tree_is_live_churn(stale_newest, 6));
    }

    #[test]
    fn an_unreadable_tree_counts_as_live_churn_not_as_deletable() {
        // Fail-closed contract, mirroring the daemon's walk-error behavior.
        assert!(
            tree_is_live_churn(None, 6),
            "unknown content age must keep the tree, never authorize deletion"
        );
        assert!(!tree_is_live_churn(Some(6), 6), "exactly at the floor is old");
        assert!(tree_is_live_churn(Some(5), 6));
        assert!(!tree_is_live_churn(Some(0), 0), "a zero floor disables the gate");
    }

    #[test]
    fn apply_only_touches_apply_class_findings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let doomed = dir.path().join("doomed.bin");
        std::fs::write(&doomed, b"x").expect("write");
        let kept = dir.path().join("kept.bin");
        std::fs::write(&kept, b"y").expect("write");

        let mut report = DiskReport::default();
        report.findings.push(Finding::new(
            TrashClass::TempAndLogs,
            doomed.clone(),
            1,
            Disposition::Apply,
            "test".to_string(),
        ));
        report.findings.push(Finding::new(
            TrashClass::Strays,
            kept.clone(),
            1,
            Disposition::Report,
            "test".to_string(),
        ));

        let (freed, records) = apply(&mut report, Path::new("/proc"), 6);
        assert_eq!(freed, 1);
        assert_eq!(records.len(), 1);
        assert!(!doomed.exists(), "APPLY finding is deleted");
        assert!(kept.exists(), "REPORT finding is never auto-deleted");
    }

    #[test]
    fn a_path_that_becomes_used_after_the_audit_is_skipped_at_delete_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("freshly-used.bin");
        std::fs::write(&path, b"z").expect("write");

        // Simulate the race: the audit says Apply, but /proc now reports an fd.
        let mut report = DiskReport::default();
        report.findings.push(
            Finding::new(
                TrashClass::TempAndLogs,
                path.clone(),
                1,
                Disposition::Apply,
                "test".to_string(),
            )
            .with_references(crate::disk_refs::ReferenceCapture {
                refs: Vec::new(),
                scan_failed: false,
            }),
        );
        // A capture path that cannot read /proc forces the fail-closed branch.
        let (freed, records) = apply(&mut report, Path::new("/definitely/not/proc"), 6);
        assert_eq!(freed, 0, "an unreadable /proc must not authorise a delete");
        assert!(records.is_empty());
        assert!(path.exists());
        assert_eq!(report.findings[0].disposition, Disposition::Skip);
    }

    #[test]
    fn evidence_rows_carry_bytes_and_the_pre_delete_capture() {
        let record = DeletedRecord {
            class: TrashClass::GitTemp,
            path: PathBuf::from("/home/u/Dev/r/.git/objects/pack/tmp_pack_a"),
            bytes: 1234,
            references: crate::disk_refs::ReferenceCapture::default(),
        };
        let row = record.evidence_row();
        assert!(row.contains("tmp_pack_a"));
        assert!(row.contains("1234"));
        assert!(row.contains("no /proc exe, cwd or fd reference"));
    }
}
