//! Safety checks — protect system paths from accidental deletion.

use anyhow::Result;
use std::path::{Path, PathBuf};

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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
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
