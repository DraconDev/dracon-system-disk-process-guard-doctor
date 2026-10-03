//! Safety checks — protect system paths from accidental deletion.

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// User-protected entries already reported as unresolvable, so the warning is
/// emitted once per entry rather than once per candidate.
///
/// `check_safe_to_delete_guard` runs per cleanup candidate, so a config typo
/// would otherwise reprint the same line on every candidate of every 30-second
/// pass — a warning nobody reads because it is the only thing in the log.
/// Deduped, it is one line naming the entry that is not protecting anything.
static WARNED_UNRESOLVABLE_PROTECTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Record that `entry` is unresolvable, returning true the first time it is
/// seen and false afterwards. Split out from the `eprintln` so the
/// once-per-entry behaviour is testable rather than trapped inside a stream
/// write.
pub(crate) fn note_unresolvable_protected(entry: &str) -> bool {
    let seen = WARNED_UNRESOLVABLE_PROTECTED.get_or_init(|| Mutex::new(HashSet::new()));
    seen.lock()
        .map(|mut s| s.insert(entry.to_string()))
        .unwrap_or(true)
}

/// System directories always protected from deletion.
pub(crate) const SYSTEM_PROTECTED: &[&str] = &[
    "/", "/home", "/etc", "/usr", "/var", "/boot", "/nix", "/run", "/sys", "/dev", "/proc",
];

/// Verifies that `path` is safe to delete (not a protected system path).
///
/// **Security note:** This function canonicalizes the path and returns it for
/// the caller to delete separately. There is a TOCTOU window between
/// canonicalization and deletion where a symlink could be planted.
/// This is mitigated by the systemd service hardening:
/// `NoNewPrivileges=true`, `ProtectSystem=strict`, `ProtectHome=read-only`.
///
/// Kept as the STRICT classifier on purpose. The guard's own delete paths use
/// `check_safe_to_delete_guard`, which permits a `$HOME` descendant (the guard
/// legitimately deletes `~/Dev/*/target`, `~/.cache/*`); this one refuses it.
/// The 2026-10-01 audit found `link apply --force-replace` calling THIS
/// variant, which made the flag dead for every link under `$HOME` — having two
/// near-identical safety functions is exactly how that happened, so the
/// difference is pinned by tests rather than left to memory. Dead-code allowed
/// outside test builds, where only those tests use it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn check_safe_to_delete(path: &Path, user_protected: &[String]) -> Result<PathBuf> {
    let canon = match path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.to_path_buf());
        }
        Err(e) => anyhow::bail!(
            "cannot canonicalize {}: {} — refusing to delete",
            path.display(),
            e
        ),
    };

    // Reject symlinks to mitigate TOCTOU
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to delete symlink {} — use target directly",
                path.display()
            );
        }
    }

    let canon_str = canon.display().to_string();

    for prot in SYSTEM_PROTECTED {
        if is_protected_ancestor(&canon_str, prot) {
            anyhow::bail!(
                "refusing to delete protected path {} (under system root {})",
                canon.display(),
                prot
            );
        }
    }

    for user_prot in user_protected {
        let prot_canon = match Path::new(user_prot).canonicalize() {
            Ok(p) => p.display().to_string(),
            // FIXED 2026-10-02 (audit HIGH): this arm was a bare `continue` with
            // no diagnostic — the same fail-open-silently bug as in
            // `check_safe_to_delete_guard`, found here while proving the fix.
            // An entry that cannot resolve protects nothing, and the operator
            // has no way to learn that. Warn once per entry, then skip: still
            // fail OPEN, because refusing every candidate over one typo would
            // fill the disk, which is worse than the typo.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if note_unresolvable_protected(user_prot) {
                    eprintln!(
                        "⚠️ protected_paths entry does not resolve, so it is protecting NOTHING: {}",
                        user_prot
                    );
                }
                continue;
            }
            Err(e) => anyhow::bail!(
                "cannot canonicalize user-protected path {}: {} — refusing",
                user_prot,
                e
            ),
        };
        if is_protected_ancestor(&canon_str, &prot_canon) {
            anyhow::bail!(
                "refusing to delete protected path {} (under user-protected path {})",
                canon.display(),
                user_prot
            );
        }
    }

    Ok(canon)
}

/// Temporary roots accepted by the age-based `clean_tmp` cleanup.
///
/// This is deliberately an explicit allowlist rather than a generic
/// "anything below the current user's home" rule. Paths below `/tmp` are
/// allowed, but the configured root must resolve there and may not itself be
/// a symlink.
pub(crate) const SAFE_TMP_ROOTS: &[&str] = &["/tmp"];

/// Validate a configured `clean_tmp` search root and return its canonical path.
///
/// Missing, non-directory, symlink, and non-temporary roots are rejected so a
/// bad configuration cannot turn the top-level age scan into home-directory
/// cleanup. Callers should use the returned canonical path for both scanning
/// and containment checks on deletion candidates.
pub(crate) fn check_safe_tmp_root(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| {
        anyhow::anyhow!(
            "cannot inspect configured tmp root {}: {} — refusing to scan",
            path.display(),
            e
        )
    })?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "refusing configured tmp root {} because it is a symlink",
            path.display()
        );
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "refusing configured tmp root {} because it is not a directory",
            path.display()
        );
    }

    let canon = path.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot canonicalize configured tmp root {}: {} — refusing to scan",
            path.display(),
            e
        )
    })?;
    if !SAFE_TMP_ROOTS
        .iter()
        .any(|root| canon.starts_with(Path::new(root)))
    {
        anyhow::bail!(
            "refusing configured tmp root {}: it must be /tmp (or a descendant)",
            canon.display()
        );
    }

    Ok(canon)
}

/// Guard-specific safety check — skips descendant checks for SYSTEM_PROTECTED
/// because the guard only deletes known artifact/cache directories (~/Dev/*/target,
/// ~/.cache/*, ~/.local/share/Trash/*) which are legitimately under /home.
/// Still rejects exact system roots, user-protected paths, symlinks, and
/// canonicalization failures.
pub(crate) fn check_safe_to_delete_guard(
    path: &Path,
    user_protected: &[String],
) -> Result<PathBuf> {
    let canon = match path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.to_path_buf());
        }
        Err(e) => anyhow::bail!(
            "cannot canonicalize {}: {} — refusing to delete",
            path.display(),
            e
        ),
    };

    // Reject symlinks to mitigate TOCTOU
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to delete symlink {} — use target directly",
                path.display()
            );
        }
    }

    let canon_str = canon.display().to_string();

    for prot in SYSTEM_PROTECTED {
        if canon_str == *prot {
            anyhow::bail!(
                "refusing guard cleanup of protected system path {}",
                canon.display()
            );
        }
    }

    // Only check user-protected paths, not SYSTEM_PROTECTED descendants
    for user_prot in user_protected {
        let prot_canon = match Path::new(user_prot).canonicalize() {
            Ok(p) => p.display().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // FIXED 2026-10-02 (audit HIGH): this used to `continue` with
                // no diagnostic at all. That is what made the `~` form of
                // `protected_paths` fail invisibly for four days: the config
                // parsed, the guard listed the protected tree as a reclaim
                // candidate, and the only symptom was a tree the operator
                // believed was protected not being protected.
                //
                // Warn, then skip — deliberately NOT a hard refusal. A single
                // typo must not make every cleanup candidate refuse, because
                // that converts a typo into a disk that fills and never
                // reclaims, which is worse than the typo. The point is to make
                // the failure visible so it gets fixed.
                let first = note_unresolvable_protected(user_prot);
                if first {
                    eprintln!(
                        "⚠️ protected_paths entry does not resolve, so it is protecting NOTHING: {}",
                        user_prot
                    );
                }
                continue;
            }
            Err(e) => anyhow::bail!(
                "cannot canonicalize user-protected path {}: {} — refusing",
                user_prot,
                e
            ),
        };
        if is_protected_ancestor(&canon_str, &prot_canon) {
            anyhow::bail!(
                "refusing to delete protected path {} (under user-protected path {})",
                canon.display(),
                user_prot
            );
        }
    }

    Ok(canon)
}

/// Validate a tmp cleanup candidate against the already validated tmp root in
/// addition to the guard's normal protected-path and symlink checks. The
/// containment check prevents a root-directory redirection from making a
/// candidate outside the approved temporary namespace deletable.
pub(crate) fn check_safe_to_delete_tmp_entry(
    path: &Path,
    tmp_root: &Path,
    user_protected: &[String],
) -> Result<PathBuf> {
    let canon = check_safe_to_delete_guard(path, user_protected)?;
    if !canon.starts_with(tmp_root) {
        anyhow::bail!(
            "refusing to delete tmp entry {}: resolved path is outside validated tmp root {}",
            canon.display(),
            tmp_root.display()
        );
    }
    Ok(canon)
}

/// Check if `path` is equal to or a descendant of `protected`.
/// Both must be canonicalized absolute paths.
pub(crate) fn is_protected_ancestor(path: &str, protected: &str) -> bool {
    if path == protected {
        return true;
    }
    // Root '/' is special: every path is a descendant, so only match exact.
    if protected == "/" {
        return path == "/";
    }
    // Ensure protected ends with '/' so '/home' doesn't match '/homefoo'
    let prefix = if protected.ends_with('/') {
        protected.to_string()
    } else {
        format!("{}/", protected)
    };
    path.starts_with(&prefix)
}

// ---------------------------------------------------------------------------
// Storage roots vs the shipped unit's hardening (2026-10-01)
// ---------------------------------------------------------------------------

/// The guard unit this binary ships with, compiled in.
///
/// `include_str!` rather than reading the deployed copy: this check must
/// describe the unit THIS release would install, and stay testable on a host
/// where nothing is deployed. `scripts/check-unit-deployment.sh` is what keeps
/// the deployed copy identical to it.
pub(crate) const SHIPPED_GUARD_UNIT: &str = include_str!("../dracon-system-guard.service");

/// The paths `ReadWritePaths=` in `unit` grants write access to, with `%h`
/// expanded and the `-` ("ignore if missing") prefix stripped.
///
/// A leading `-` does NOT narrow what is granted — it only stops systemd from
/// refusing to START when the path is absent. On a host where it exists, the
/// path is still made writable, so it must count as covered here. Multiple
/// `ReadWritePaths=` lines accumulate, exactly as systemd accumulates them.
pub(crate) fn unit_readwrite_paths(unit: &str, home: &Path) -> Vec<PathBuf> {
    let mut granted = Vec::new();
    for line in unit.lines() {
        let line = line.trim();
        // Comments and non-directives are skipped; a directive is `Key=value`.
        let Some(value) = line.strip_prefix("ReadWritePaths=") else {
            continue;
        };
        for token in value.split_whitespace() {
            let token = token.strip_prefix('-').unwrap_or(token);
            granted.push(expand_unit_path(token, home));
        }
    }
    granted
}

/// Expand one `ReadWritePaths=` token against `home`. Absolute paths pass
/// through; `%h` is the unit specifier for the user's home.
fn expand_unit_path(token: &str, home: &Path) -> PathBuf {
    if let Some(rest) = token.strip_prefix("%h") {
        return home.join(rest.trim_start_matches('/'));
    }
    PathBuf::from(token)
}

/// Whether the unit's `ReadWritePaths=` makes `path` writable.
///
/// `ProtectSystem=strict` remounts the whole hierarchy read-only inside the
/// service's namespace, and `ProtectHome=read-only` does the same for `$HOME`.
/// Only these paths are exempted, so a root that is not under one of them fails
/// every write with EROFS — which is exactly how quarantine silently stopped
/// working on 2026-10-01.
pub(crate) fn unit_grants_write(unit: &str, home: &Path, path: &Path) -> bool {
    // FIXED 2026-10-03 (audit R4-SYS-14): compare canonical paths so
    // a symlinked storage root matches the grant on its target — the
    // checker (R3-L23) already matches canonical, and the split warned
    // here while passing there.
    let canon_path = canonical_or_literal(path);
    unit_readwrite_paths(unit, home)
        .iter()
        .any(|granted| canon_path.starts_with(canonical_or_literal(granted)))
}

/// Canonicalize for comparison, with literal fallback (mirrors the
/// checker's `readlink -f ... || literal`, R3-L23). Missing paths
/// (granted `-` entries for not-yet-created dirs, unconfigured
/// roots) keep comparing literally. Relative paths are NEVER
/// resolved: canonicalize would anchor them to THIS process's CWD,
/// not the service's WorkingDirectory, so resolving them would flip
/// verdicts depending on where the CLI was invoked.
fn canonical_or_literal(path: &Path) -> PathBuf {
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The storage roots this policy writes to that the shipped unit would leave
/// read-only, as `(policy key, resolved path)` pairs.
///
/// Only roots that are actually configured are reported: an unset
/// `relocate_cold_root` means relocation is off, and naming a path nothing will
/// ever write to would be noise.
pub(crate) fn uncovered_storage_roots(
    unit: &str,
    guard: &crate::GuardPolicy,
) -> Vec<(&'static str, PathBuf)> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let cold_root = guard.relocate_cold_root.trim();
    let mut roots: Vec<(&'static str, PathBuf)> = vec![
        ("quarantine_dir", crate::quarantine_root(guard)),
        ("relocate_cold_root", crate::expand_tilde(cold_root)),
    ];
    // FIXED 2026-10-03 (audit R4-SYS-06): the guard also WRITES the
    // event log, the (truncated) log dirs, and the sync freeze marker.
    // A repointed one outside ReadWritePaths failed EROFS on every
    // pass with no startup warning — the silent-reclaim-death class
    // this check exists to catch. Each arm mirrors the write site's
    // own resolution so the check tests the path actually written.
    if let Some(log_file) = crate::resolve_guard_log_path(&guard.guard_log_file) {
        roots.push(("guard_log_file", log_file));
    }
    if let Some(configured) = crate::effective_log_dirs(&guard.log_dirs) {
        for entry in configured.split(',') {
            let entry = entry.trim();
            if !entry.is_empty() {
                roots.push(("log_dirs", crate::expand_tilde(entry)));
            }
        }
    }
    let marker = guard.sync_freeze_marker.trim();
    if marker.is_empty() {
        // Blank normalizes to the default (policy.rs), exactly as
        // quarantine_root does for its blank — check the default.
        roots.push((
            "sync_freeze_marker",
            PathBuf::from(crate::default_sync_freeze_marker()),
        ));
    } else {
        // Mirrors sync_freeze_marker_path: no tilde expansion there,
        // so none here — a `~` value would be written literally.
        roots.push(("sync_freeze_marker", PathBuf::from(marker)));
    }
    roots
        .into_iter()
        .filter(|(_, path)| !path.as_os_str().is_empty() && path != &PathBuf::from("."))
        .filter(|(_, path)| !unit_grants_write(unit, &home, path))
        .collect()
}

/// Say ONCE, on stderr, which configured storage roots the shipped unit would
/// leave read-only, naming the exact entry to add.
///
/// Printed to stderr so `guard once --json` stays machine-parseable. Emitted
/// once per process from the command entry point, NOT per pass: the old
/// behaviour was one "failed to quarantine … Read-only file system" line per
/// candidate per pass, which buried the single actionable fact under thousands
/// of identical errors.
pub(crate) fn warn_uncovered_storage_roots(guard: &crate::GuardPolicy) {
    for (key, path) in uncovered_storage_roots(SHIPPED_GUARD_UNIT, guard) {
        eprintln!(
            "⚠️ {key} = {} is not in the guard unit's ReadWritePaths — writes there \
             fail EROFS under ProtectSystem=strict. Add: -{} to ReadWritePaths= in \
             dracon-system-guard.service, then reinstall the unit.",
            path.display(),
            path.display()
        );
    }
}
