//! Report-only detectors for reclaimable space that must never be auto-deleted.
//!
//! ADDED 2026-10-27 as part of the nitpicky disk cleaner. Everything in this
//! module is deliberately REPORT-ONLY: the guard prints it, sizes it and hands
//! the operator an exact command, but never acts.
//!
//! Two classes live here, for the same underlying reason — the tool cannot
//! prove the item is worthless:
//!
//! 1. **Dead swapfiles.** A swapfile that no longer appears in `/proc/swaps`
//!    looks like pure waste (the 2026-10-26 incident found a 32 GiB
//!    `/var/lib/swapfile` that nothing referenced). But activating a swapfile
//!    is a root operation and removing the wrong one can silently disable
//!    swap on a memory-pressured host, so the guard reports it and prints the
//!    `sudo rm` line instead of guessing. Root-owned paths are never attempted
//!    by an unprivileged tool: `rm` on them would only produce a permission
//!    error, or worse, succeed if the guard ever ran as root.
//!
//! 2. **Stray top-level directories and old backup bundles.** A 19 GiB
//!    `*.bundle` may be the only copy of history that was later rewritten
//!    away; a `dracon-lfs-proof/` may be referenced by an audit. Nothing here
//!    can establish provenance, so these are surfaced with sizes and left
//!    alone until a human approves each path.

use std::path::{Path, PathBuf};

/// A swapfile that exists on disk but is not currently active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadSwapfile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    /// True when the file is not writable by the current user, i.e. removing
    /// it requires root.
    pub(crate) needs_root: bool,
}

impl DeadSwapfile {
    /// The exact command an operator should run to reclaim this file.
    pub(crate) fn removal_command(&self) -> String {
        if self.needs_root {
            format!("sudo rm -f {}", self.path.display())
        } else {
            format!("rm -f {}", self.path.display())
        }
    }
}

/// A directory or backup artefact that looks abandoned but is not provably so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrayItem {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) kind: StrayKind,
}

/// Why a stray item is being reported rather than cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrayKind {
    /// A `*.bundle`/`*.tar.gz`/`*.tar.zst` backup archive.
    BackupArchive,
    /// A bare repository kept as a pre-rewrite safety copy.
    BackupRepo,
    /// A top-level directory whose name marks it as ad-hoc scratch.
    ScratchDir,
}

impl StrayKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::BackupArchive => "backup-archive",
            Self::BackupRepo => "backup-repo",
            Self::ScratchDir => "scratch-dir",
        }
    }
}

/// Markers that make a top-level directory report-worthy.
///
/// Kept as an explicit list rather than a heuristic so the report stays
/// predictable: an operator can grep for these names and know exactly what
/// will and will not appear.
const SCRATCH_DIR_MARKERS: [&str; 6] = [
    "-recovery-",
    "-proof",
    "-audit-new",
    "tmp-shield",
    "-orphans",
    "-pre-filterrepo-backup",
];

const BACKUP_ARCHIVE_SUFFIXES: [&str; 3] = [".bundle", ".tar.gz", ".tar.zst"];

/// One active swap device: its path plus the configured size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SwapEntry {
    pub(crate) path: PathBuf,
    /// The `Size` column of `/proc/swaps`, in KiB.
    pub(crate) size_kib: u64,
}

/// Parse `/proc/swaps` into active swap entries (path + size).
///
/// The first line is a header (`Filename Type Size Used Priority`). A file
/// that is not in this list is inactive — which is exactly the condition
/// that makes it a dead-swapfile candidate.
pub(crate) fn active_swap_table(swaps_contents: &str) -> Vec<SwapEntry> {
    swaps_contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let path = cols.next()?;
            // Skip the `Type` column, take `Size`.
            let size_kib = cols.nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(SwapEntry {
                path: PathBuf::from(path),
                size_kib,
            })
        })
        .collect()
}

/// Parse `/proc/swaps` into the set of active swap paths.
///
/// The first line is a header. A file that is not in this list is inactive —
/// which is exactly the condition that makes it a candidate.
pub(crate) fn active_swap_paths(swaps_contents: &str) -> Vec<PathBuf> {
    active_swap_table(swaps_contents)
        .into_iter()
        .map(|entry| entry.path)
        .collect()
}

/// An ACTIVE swap device that lives on an unexpected device.
///
/// This is the dangerous direction: the 2026-10-26 incident deleted a 32 GiB
/// `/var/lib/swapfile` only after proving it was inactive. An active swapfile
/// on the wrong device must never be touched automatically — but it must be
/// LOUD, because the next cleanup pass (or operator) could mistake it for
/// the dead one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrongSwap {
    pub(crate) path: PathBuf,
    /// Configured size in bytes (`size_kib * 1024` from `/proc/swaps`).
    pub(crate) bytes: u64,
}

/// True when `path` lives under `prefix`.
///
/// Plain string-prefix matching, deliberately: the expected entries are
/// device families (`/dev/zram` must accept `/dev/zram0`) as well as
/// directories (`/mnt/data` must accept `/mnt/data/swapfile`), and no single
/// component-wise rule covers both. Documented so the operator can reason
/// about it: a prefix also accepts a sibling whose name merely starts the
/// same way (`/mnt/database` would satisfy `/mnt/data`). That fails OPEN,
/// so keep prefixes specific.
fn under_prefix(path: &Path, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    path.to_string_lossy().starts_with(prefix)
}

/// Every active swap device must live under one of `expected` (comma-split
/// policy prefixes, default `/dev/zram` + `/mnt/data`). Returns the active
/// devices that do not, with their configured sizes.
pub(crate) fn check_active_swap_placement(
    active: &[SwapEntry],
    expected: &[String],
) -> Vec<WrongSwap> {
    active
        .iter()
        .filter(|entry| !expected.iter().any(|p| under_prefix(&entry.path, p)))
        .map(|entry| WrongSwap {
            path: entry.path.clone(),
            bytes: entry.size_kib.saturating_mul(1024),
        })
        .collect()
}

/// Best-effort extraction of `swapDevices` device paths from NixOS
/// configuration text.
///
/// Finds `swapDevices = [ ... ];` with bracket matching (so nested attribute
/// sets don't cut the block short) and collects every `device = "...";`
/// inside it. Anything fancier — conditionals, `lib.mkIf`, generated configs
/// — is out of scope: an unparsed config yields an empty list, and the
/// caller treats the reference check as UNKNOWN (skipped), never as
/// "everything is unreferenced".
/// The config with comments and string CONTENTS blanked (same length, so
/// offsets transfer to the original). Structural parsing — key search and
/// bracket matching — runs on this, so a `swapDevices` mentioned in a
/// comment (the 2026-10-27 false positive: `# ... swapDevices below` latched
/// onto `boot.kernelParams = [ ... ]`) or brackets inside a string can never
/// unbalance it. Handles `#`, `/* */`, `"..."` and `''...''` strings.
fn code_only(config: &str) -> String {
    let chars: Vec<char> = config.chars().collect();
    let mut out = String::with_capacity(config.len());
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '#' {
            while k < chars.len() && chars[k] != '\n' {
                out.push(' ');
                k += 1;
            }
            continue;
        }
        if c == '/' && k + 1 < chars.len() && chars[k + 1] == '*' {
            out.push(' ');
            out.push(' ');
            k += 2;
            while k + 1 < chars.len() && !(chars[k] == '*' && chars[k + 1] == '/') {
                out.push(if chars[k] == '\n' { '\n' } else { ' ' });
                k += 1;
            }
            out.push(' ');
            out.push(' ');
            k += 2;
            continue;
        }
        if c == '"' {
            out.push(c);
            k += 1;
            while k < chars.len() && chars[k] != '"' {
                if chars[k] == '\\' && k + 1 < chars.len() {
                    out.push(' ');
                    out.push(' ');
                    k += 2;
                } else {
                    out.push(if chars[k] == '\n' { '\n' } else { ' ' });
                    k += 1;
                }
            }
            if k < chars.len() {
                out.push('"');
                k += 1;
            }
            continue;
        }
        // Nix indented string: ends at `''` except for the `'''` / `''$` /
        // `''\` / `'''` escapes, which are consumed as content.
        if c == '\'' && k + 1 < chars.len() && chars[k + 1] == '\'' {
            out.push(' ');
            out.push(' ');
            k += 2;
            loop {
                if k >= chars.len() {
                    break;
                }
                if chars[k] == '\''
                    && k + 1 < chars.len()
                    && chars[k + 1] == '\''
                    && k + 2 < chars.len()
                    && (chars[k + 2] == '\'' || chars[k + 2] == '$' || chars[k + 2] == '\\')
                {
                    out.push(' ');
                    out.push(' ');
                    out.push(' ');
                    k += 3;
                    continue;
                }
                if chars[k] == '\'' && k + 1 < chars.len() && chars[k + 1] == '\'' {
                    out.push(' ');
                    out.push(' ');
                    k += 2;
                    break;
                }
                out.push(if chars[k] == '\n' { '\n' } else { ' ' });
                k += 1;
            }
            continue;
        }
        out.push(c);
        k += 1;
    }
    out
}

pub(crate) fn nixos_swap_devices(config: &str) -> Vec<PathBuf> {
    // Structure on the blanked text, values on the original: same length,
    // so the block offsets transfer directly.
    let code = code_only(config);
    let (start, end) = match swap_devices_block(&code) {
        Some(range) => range,
        None => return Vec::new(),
    };
    let block = &config[start..end];
    let mut devices = Vec::new();
    let bytes = block.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Skip quoted strings so a `device = "..."` inside a string
            // literal cannot match.
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        // Skip comments so a commented-out device entry cannot match.
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if block[i..].starts_with("device")
            && {
                let before = block[..i].chars().next_back();
                before.map(|c| !(c.is_alphanumeric() || c == '_' || c == '.')).unwrap_or(true)
            }
        {
            let mut j = i + "device".len();
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    j += 1;
                    let start = j;
                    while j < bytes.len() && bytes[j] != b'"' {
                        j += 1;
                    }
                    devices.push(PathBuf::from(&block[start..j]));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    devices
}

/// True when the config has a parseable `swapDevices = [ ... ]` block.
/// Operates on the blanked text (see [`code_only`]); a bare mention in a
/// comment or string is not an assignment.
pub(crate) fn has_swap_devices_block(config: &str) -> bool {
    swap_devices_block(&code_only(config)).is_some()
}

/// The byte range of the `[ ... ]` value of a `swapDevices = ...` list
/// assignment in BLANKED text ([`code_only`]) — offsets transfer to the
/// original because blanking preserves length. `None` when there is no
/// such assignment or the brackets never balance.
fn swap_devices_block(code: &str) -> Option<(usize, usize)> {
    let key = "swapDevices";
    // Try every occurrence: the first may be a longer identifier or an
    // attribute path (`x.swapDevices`), a later one the real assignment.
    // (Comment/string occurrences are already blanked away by `code_only`.)
    let mut search_from = 0;
    while let Some(rel) = code[search_from..].find(key) {
        let key_pos = search_from + rel;
        search_from = key_pos + key.len();
        // The character before the key must not be part of a longer identifier.
        if let Some(before) = code[..key_pos].chars().next_back() {
            if before.is_alphanumeric() || before == '_' || before == '.' {
                continue;
            }
        }
        if let Some(range) = block_after(code, key_pos + key.len()) {
            return Some(range);
        }
    }
    None
}

/// The byte range of the bracket-matched `[ ... ]` list value after the
/// `=` following `key_end` (offsets relative to `config`, which must be
/// BLANKED text). A `;` before the `[` means this `=` belongs to a different
/// construct (not a list assignment) — bail instead of grabbing an
/// unrelated later bracket.
fn block_after(config: &str, key_end: usize) -> Option<(usize, usize)> {
    let after_key = &config[key_end..];
    let eq = after_key.find('=')?;
    let after_eq = &after_key[eq + 1..];
    let base = key_end + eq + 1;
    let open_rel = after_eq.find('[')?;
    if after_eq[..open_rel].contains(';') {
        return None;
    }
    // Bracket-match from the opening `[`. The text is already blanked, so
    // no string/comment state is needed — every bracket here is structural.
    let mut depth = 0;
    for (offset, c) in after_eq[open_rel..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((base + open_rel, base + open_rel + offset + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// A swapfile that exists on disk (candidate, or active file swap) but has
/// no entry in the NixOS `swapDevices` list: nothing will recreate or
/// activate it after a rebuild/reboot, so it is either stale or unmanaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnreferencedSwap {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    /// True when the file is ACTIVE swap right now — losing it on reboot
    /// is the sharper edge, so the reason text says so.
    pub(crate) is_active: bool,
}

/// Flag on-disk file-swap paths missing from the parsed NixOS `swapDevices`.
///
/// `on_disk` is `(path, bytes)` for every candidate that exists plus every
/// active swap entry. `/dev/*` entries (zram, partitions) are skipped:
/// they are not files and NixOS manages them through `zramSwap` /
/// hardware config, not `swapDevices`. Only call this when the config
/// actually parsed — an unreadable config is UNKNOWN, not "unreferenced".
pub(crate) fn check_swap_references(
    on_disk: &[(PathBuf, u64)],
    config_devices: &[PathBuf],
    active: &[PathBuf],
) -> Vec<UnreferencedSwap> {
    on_disk
        .iter()
        .filter(|(path, _)| {
            // Device nodes are managed outside swapDevices.
            !under_prefix(path, "/dev/")
                && !config_devices.iter().any(|d| d == path)
        })
        .map(|(path, bytes)| UnreferencedSwap {
            path: path.clone(),
            bytes: *bytes,
            is_active: active.iter().any(|a| a == path),
        })
        .collect()
}

/// The outcome of one wrong-swap pass, shared by `doctor`, the disk audit
/// REPORT section, and the guard's periodic pass so all three agree.
#[derive(Debug, Clone, Default)]
pub(crate) struct SwapCheckOutcome {
    /// False when `/proc/swaps` could not be read: nothing below is known.
    pub(crate) probe_ok: bool,
    /// Active swap on unexpected devices.
    pub(crate) wrong: Vec<WrongSwap>,
    /// On-disk swapfiles with no NixOS `swapDevices` entry.
    pub(crate) unreferenced: Vec<UnreferencedSwap>,
    /// False when the NixOS config was unreadable or had no parseable
    /// `swapDevices` block: `unreferenced` is then empty AND meaningless.
    /// (Callers keep the attempted path for their own reason text.)
    pub(crate) config_known: bool,
}

impl SwapCheckOutcome {
    /// True when the pass proved something is wrong (never when the probe
    /// or the config was unanswerable — unknown is not failure).
    pub(crate) fn has_findings(&self) -> bool {
        !self.wrong.is_empty() || !self.unreferenced.is_empty()
    }

    /// One human-readable line per finding, for guard-once/doctor detail.
    pub(crate) fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for w in &self.wrong {
            lines.push(format!(
                "ACTIVE swap on unexpected device: {} ({} configured)",
                w.path.display(),
                crate::policy::human_bytes(w.bytes),
            ));
        }
        for u in &self.unreferenced {
            lines.push(format!(
                "swapfile not in NixOS swapDevices: {} ({}){}",
                u.path.display(),
                crate::policy::human_bytes(u.bytes),
                if u.is_active {
                    " — ACTIVE now, will not survive a rebuild/reboot"
                } else {
                    ""
                },
            ));
        }
        lines
    }
}

/// Run the full wrong-swap pass: placement of active swap plus NixOS
/// references for every on-disk candidate. Pure filesystem reads, no
/// deletes, safe on every guard pass.
pub(crate) fn check_swap_health(
    expected_prefixes: &[String],
    candidates: &[PathBuf],
    config_path: &Path,
) -> SwapCheckOutcome {
    let swaps = std::fs::read_to_string("/proc/swaps").ok();
    // On-disk universe for the reference check: every candidate that
    // exists, plus every active swap entry (a file active now but absent
    // from the config is the sharpest unreferenced case).
    let table = swaps.as_deref().map(active_swap_table).unwrap_or_default();
    let mut on_disk: Vec<(PathBuf, u64)> = Vec::new();
    for candidate in candidates {
        if let Ok(metadata) = std::fs::metadata(candidate) {
            if !metadata.is_dir() && !on_disk.iter().any(|(p, _)| p == candidate) {
                on_disk.push((candidate.clone(), metadata.len()));
            }
        }
    }
    for entry in &table {
        if !under_prefix(&entry.path, "/dev/")
            && !on_disk.iter().any(|(p, _)| p == &entry.path)
        {
            let bytes = std::fs::metadata(&entry.path)
                .map(|m| m.len())
                .unwrap_or_else(|_| entry.size_kib.saturating_mul(1024));
            on_disk.push((entry.path.clone(), bytes));
        }
    }
    let config = std::fs::read_to_string(config_path).ok();
    evaluate_swap_health(
        swaps.as_deref(),
        config.as_deref(),
        expected_prefixes,
        &on_disk,
    )
}

/// The pure core of [`check_swap_health`]: `/proc/swaps` contents (or `None
/// when unreadable), the NixOS config text (or `None`), the already-statted
/// on-disk universe. Fully fixture-testable; the fs wrapper above feeds it.
pub(crate) fn evaluate_swap_health(
    swaps_contents: Option<&str>,
    config_text: Option<&str>,
    expected_prefixes: &[String],
    on_disk: &[(PathBuf, u64)],
) -> SwapCheckOutcome {
    let mut outcome = SwapCheckOutcome::default();
    let Some(swaps) = swaps_contents else {
        return outcome;
    };
    outcome.probe_ok = true;
    let table = active_swap_table(swaps);
    outcome.wrong = check_active_swap_placement(&table, expected_prefixes);
    let active_paths: Vec<PathBuf> = table.iter().map(|e| e.path.clone()).collect();
    let Some(config) = config_text else {
        return outcome;
    };
    // A config with no swapDevices block at all is UNKNOWN: flagging every
    // swapfile as unreferenced would be pure noise. An explicitly EMPTY
    // block (`swapDevices = [ ];`) is KNOWN — the operator declared no
    // swapfiles, so everything on disk is unreferenced.
    if !has_swap_devices_block(config) {
        return outcome;
    }
    outcome.config_known = true;
    let devices = nixos_swap_devices(config);
    outcome.unreferenced = check_swap_references(on_disk, &devices, &active_paths);
    outcome
}

/// Decide whether `path` is a dead swapfile worth reporting.
///
/// `active` is the parsed `/proc/swaps` list. A file already listed there is
/// live swap and is never reported, regardless of how it was named.
pub(crate) fn classify_swapfile(path: &Path, bytes: u64, active: &[PathBuf]) -> Option<DeadSwapfile> {
    if active.iter().any(|a| a == path) {
        return None;
    }
    let needs_root = !is_writable_by_current_user(path);
    Some(DeadSwapfile {
        path: path.to_path_buf(),
        bytes,
        needs_root,
    })
}

/// True when the current process may unlink inside this file's directory.
///
/// Uses the parent directory rather than the file itself: unlinking requires
/// write permission on the directory, which is the operation that would fail.
pub(crate) fn is_writable_by_current_user(path: &Path) -> bool {
    path.parent()
        .map(|parent| {
            // A path we cannot even inspect is treated as root-owned: the
            // report then errs toward showing `sudo`.
            std::fs::metadata(parent)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = m.permissions().mode();
                    let uid = unsafe_geteuid();
                    // Owner/group/other write bits against our identity.
                    if uid == 0 {
                        return true;
                    }
                    let owner = std::os::unix::fs::MetadataExt::uid(&m);
                    let group = std::os::unix::fs::MetadataExt::gid(&m);
                    let in_group = group_in_current_groups(group);
                    if owner == uid {
                        mode & 0o200 != 0
                    } else if in_group {
                        mode & 0o020 != 0
                    } else {
                        mode & 0o002 != 0
                    }
                })
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

extern "C" {
    fn geteuid() -> u32;
}

/// The effective uid, which is what permission checks actually use.
fn unsafe_geteuid() -> u32 {
    // SAFETY: `geteuid` is a pure syscall wrapper with no memory safety
    // requirements and cannot fail.
    unsafe { geteuid() }
}

/// Whether `gid` is one of the current process's supplementary groups.
fn group_in_current_groups(gid: u32) -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Groups:"))
                .map(|line| {
                    line.trim_start_matches("Groups:")
                        .split_whitespace()
                        .filter_map(|g| g.parse::<u32>().ok())
                        .any(|g| g == gid)
                })
        })
        .unwrap_or(false)
}

/// Report a stray backup/scratch item, if `name` matches a known marker.
///
/// `bytes` is the caller's already-measured allocated size, so a report can be
/// produced from one bounded `du` walk rather than re-statting every file.
pub(crate) fn classify_stray(path: &Path, name: &str, bytes: u64, is_dir: bool) -> Option<StrayItem> {
    if !is_dir {
        if BACKUP_ARCHIVE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            return Some(StrayItem {
                path: path.to_path_buf(),
                bytes,
                kind: StrayKind::BackupArchive,
            });
        }
        return None;
    }
    if name.ends_with(".git") && name.contains("backup") {
        return Some(StrayItem {
            path: path.to_path_buf(),
            bytes,
            kind: StrayKind::BackupRepo,
        });
    }
    if SCRATCH_DIR_MARKERS.iter().any(|marker| name.contains(marker)) {
        return Some(StrayItem {
            path: path.to_path_buf(),
            bytes,
            kind: StrayKind::ScratchDir,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_swaps_header_and_entries() {
        let contents = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                        /dev/zram0                              partition\t63897596\t0\t5\n\
                        /mnt/data/swapfile                      file\t\t67108860\t131704\t-1\n";
        let active = active_swap_paths(contents);
        assert_eq!(
            active,
            vec![PathBuf::from("/dev/zram0"), PathBuf::from("/mnt/data/swapfile")]
        );
    }

    #[test]
    fn empty_swaps_file_has_no_active_paths() {
        assert!(active_swap_paths("").is_empty());
        assert!(active_swap_paths("Filename Type Size Used Priority\n").is_empty());
    }

    #[test]
    fn an_active_swapfile_is_never_reported_as_dead() {
        let active = vec![PathBuf::from("/mnt/data/swapfile")];
        assert!(
            classify_swapfile(Path::new("/mnt/data/swapfile"), 1024, &active).is_none(),
            "live swap must not be reported"
        );
    }

    #[test]
    fn an_inactive_swapfile_is_reported() {
        let active = vec![PathBuf::from("/dev/zram0")];
        let dead = classify_swapfile(Path::new("/var/lib/swapfile"), 34359738368, &active)
            .expect("inactive swapfile should be reported");
        assert_eq!(dead.bytes, 34359738368);
        assert!(dead.needs_root, "/var/lib is root-owned");
        assert_eq!(dead.removal_command(), "sudo rm -f /var/lib/swapfile");
    }

    #[test]
    fn a_user_owned_swapfile_needs_no_sudo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("swapfile");
        std::fs::write(&path, b"x").expect("write");
        let dead = classify_swapfile(&path, 1, &[]).expect("reported");
        assert!(!dead.needs_root);
        assert!(dead.removal_command().starts_with("rm -f "));
    }

    #[test]
    fn backup_archives_are_recognised() {
        for name in [
            "dracon-platform-pre-slim-20260930.bundle",
            "junk-runner-current-tree.tar.gz",
            "holdout.tar.zst",
        ] {
            let item = classify_stray(Path::new(name), name, 10, false)
                .unwrap_or_else(|| panic!("{name} should be a backup archive"));
            assert_eq!(item.kind, StrayKind::BackupArchive);
        }
    }

    #[test]
    fn ordinary_files_are_not_strays() {
        for name in ["README.md", "Cargo.toml", "main.rs"] {
            assert!(classify_stray(Path::new(name), name, 10, false).is_none());
        }
    }

    #[test]
    fn scratch_and_backup_directories_are_recognised() {
        let cases = [
            ("hegemon-pre-filterrepo-backup-20260706.git", StrayKind::BackupRepo),
            ("dracon-platform-orphans", StrayKind::ScratchDir),
            ("dracon-lfs-proof", StrayKind::ScratchDir),
            ("eve-audit-new", StrayKind::ScratchDir),
            ("tmp-shield", StrayKind::ScratchDir),
            (".dracon-git-recovery-20260831-1610", StrayKind::ScratchDir),
        ];
        for (name, expected) in cases {
            let item = classify_stray(Path::new(name), name, 10, true)
                .unwrap_or_else(|| panic!("{name} should be a stray"));
            assert_eq!(item.kind, expected, "wrong kind for {name}");
        }
    }

    #[test]
    fn swap_table_parses_size_column() {
        let contents = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                        /dev/zram0                              partition\t63897596\t0\t5\n\
                        /mnt/data/swapfile                      file\t\t67108860\t131704\t-1\n";
        let table = active_swap_table(contents);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].path, PathBuf::from("/dev/zram0"));
        assert_eq!(table[0].size_kib, 63897596);
        assert_eq!(table[1].path, PathBuf::from("/mnt/data/swapfile"));
        assert_eq!(table[1].size_kib, 67108860);
        // active_swap_paths still returns just the paths.
        assert_eq!(
            active_swap_paths(contents),
            vec![PathBuf::from("/dev/zram0"), PathBuf::from("/mnt/data/swapfile")]
        );
    }

    #[test]
    fn misplaced_active_swap_is_flagged() {
        let table = active_swap_table(
            "Filename Type Size Used Priority\n\
             /dev/zram0 partition 63897596 0 5\n\
             /var/lib/swapfile file 33554432 0 -1\n",
        );
        let expected = vec!["/dev/zram".to_string(), "/mnt/data".to_string()];
        let wrong = check_active_swap_placement(&table, &expected);
        assert_eq!(wrong.len(), 1);
        assert_eq!(wrong[0].path, PathBuf::from("/var/lib/swapfile"));
        assert_eq!(wrong[0].bytes, 33554432 * 1024);
    }

    #[test]
    fn expected_active_swap_is_quiet() {
        let table = active_swap_table(
            "Filename Type Size Used Priority\n\
             /dev/zram0 partition 63897596 0 5\n\
             /mnt/data/swapfile file 67108860 0 -1\n",
        );
        let expected = vec!["/dev/zram".to_string(), "/mnt/data".to_string()];
        assert!(check_active_swap_placement(&table, &expected).is_empty());
        // No active swap at all is vacuously fine.
        assert!(check_active_swap_placement(&[], &expected).is_empty());
    }

    #[test]
    fn swapdevices_block_parses_devices_only() {
        let config = r#"
          fileSystems."/" = { device = "/dev/disk/by-label/nixos"; };
          swapDevices = [
            { device = "/mnt/data/swapfile"; size = 64 * 1024; }
            # { device = "/old/swapfile"; }  -- commented out, must not match
          ];
        "#;
        let devices = nixos_swap_devices(config);
        assert_eq!(devices, vec![PathBuf::from("/mnt/data/swapfile")]);
    }

    /// 2026-10-27 false positive: a COMMENT mentioning swapDevices
    /// (`# ... swapDevices below`) preceded the real assignment, and the
    /// parser latched onto `boot.kernelParams = [ ... ]` — concluding the
    /// device list was empty and flagging every swapfile as unreferenced.
    /// Also covers the `lib.mkForce [ ... ]` wrapper the live config uses.
    #[test]
    fn swapdevices_ignores_comment_mentions() {
        let config = "# Swap managed explicitly via zram and swapDevices below.\n\
          boot.kernelParams = [ \"systemd.swap=0\" ];\n\
          swapDevices = lib.mkForce [\n\
            {\n\
              device = \"/dev/disk/by-uuid/5021c67f-4e1f-4a52-94a4-52ca9325d4ef\";\n\
              priority = 1;\n\
            }\n\
            {\n\
              device = \"/mnt/data/swapfile\";\n\
              size = 65536;\n\
            }\n\
          ];\n";
        let devices = nixos_swap_devices(config);
        assert_eq!(
            devices,
            vec![
                PathBuf::from("/dev/disk/by-uuid/5021c67f-4e1f-4a52-94a4-52ca9325d4ef"),
                PathBuf::from("/mnt/data/swapfile"),
            ]
        );
        assert!(has_swap_devices_block(config));
    }

    #[test]
    fn swapdevices_without_block_is_empty() {
        assert!(nixos_swap_devices("fileSystems = {};\n").is_empty());
        // Unbalanced brackets never parse: unknown, not "everything".
        assert!(nixos_swap_devices("swapDevices = [ { device = \"/x\"; };").is_empty());
    }

    #[test]
    fn unreferenced_swapfiles_are_flagged() {
        let on_disk = vec![
            (PathBuf::from("/var/lib/swapfile"), 1),
            (PathBuf::from("/mnt/data/swapfile"), 2),
            // Device nodes are managed via zramSwap/hardware config, never
            // via swapDevices: they must not be flagged.
            (PathBuf::from("/dev/zram0"), 3),
        ];
        let config = vec![PathBuf::from("/mnt/data/swapfile")];
        let active = vec![PathBuf::from("/var/lib/swapfile")];
        let flagged = check_swap_references(&on_disk, &config, &active);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].path, PathBuf::from("/var/lib/swapfile"));
        assert!(flagged[0].is_active, "active-but-unreferenced must be marked");
    }

    #[test]
    fn evaluator_combines_placement_and_references() {
        let swaps = "Filename Type Size Used Priority\n\
                       /dev/zram0 partition 63897596 0 5\n\
                       /var/lib/swapfile file 33554432 0 -1\n";
        let config = "swapDevices = [ { device = \"/mnt/data/swapfile\"; } ];\n";
        let expected = vec!["/dev/zram".to_string(), "/mnt/data".to_string()];
        let on_disk = vec![(PathBuf::from("/var/lib/swapfile"), 42)];
        let outcome = evaluate_swap_health(Some(swaps), Some(config), &expected, &on_disk);
        assert!(outcome.probe_ok);
        assert!(outcome.config_known);
        assert!(outcome.has_findings());
        assert_eq!(outcome.wrong.len(), 1);
        assert_eq!(outcome.unreferenced.len(), 1);
        assert!(outcome.unreferenced[0].is_active);
        assert_eq!(outcome.describe().len(), 2);
    }

    #[test]
    fn evaluator_unknown_means_no_findings() {
        let expected = vec!["/dev/zram".to_string()];
        // Unreadable /proc/swaps: nothing is known.
        let outcome = evaluate_swap_health(None, Some("swapDevices = [ ];"), &expected, &[]);
        assert!(!outcome.probe_ok);
        assert!(!outcome.has_findings());
        // Readable swaps but a config with no swapDevices block: the
        // reference half is unknown, the placement half still works.
        let swaps = "Filename Type Size Used Priority\n/dev/zram0 partition 1 0 5\n";
        let outcome = evaluate_swap_health(
            Some(swaps),
            Some("fileSystems = {};"),
            &expected,
            &[(PathBuf::from("/var/lib/swapfile"), 9)],
        );
        assert!(outcome.probe_ok);
        assert!(!outcome.config_known);
        assert!(!outcome.has_findings());
    }

    #[test]
    fn ordinary_directories_are_not_strays() {
        for name in ["Dev", "Documents", "Pictures", "Videos"] {
            assert!(
                classify_stray(Path::new(name), name, 10, true).is_none(),
                "{name} must never be reported as stray"
            );
        }
    }
}
