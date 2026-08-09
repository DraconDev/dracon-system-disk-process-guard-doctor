# Changelog

All notable changes to `dracon-system` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: prior to 0.112.12, `dracon-system` was developed inside the
> [`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
> monorepo. Releases 0.0.0–0.112.11 are recorded in
> [`dracon-utilities/CHANGELOG.md`](https://github.com/DraconDev/dracon-utilities/blob/main/CHANGELOG.md)
> under the `dracon-system` heading. From 0.112.12 onward, this CHANGELOG
> is the canonical record.

## [Unreleased]

### Fixed

- **`evaluate_link` accepts equivalent non-canonicalized targets**
  (audit LOW, 2026-08-10): `normalize_path` fell back to RAW path
  strings when `canonicalize` failed, so a link whose actual target is
  written `~/a/../b` (with the intermediate `a` missing/broken) was
  reported `link_target_mismatch` against a configured `~/b` even
  though it points at the same file. The fallback is now
  `lexical_normalize`, which collapses `.`/`..` components without
  touching the filesystem (never dropping a leading `..` and never
  climbing above the root). Tests: lexical collapse cases (incl.
  `..`-above-root preserved and `a/b/../../c` → `c`) plus an
  `evaluate_link` regression test with a real `..`-form symlink whose
  intermediate is missing — now in-sync, while a genuinely different
  target still reports mismatch.

- **`scan_broken_symlinks` comment corrected** (audit LOW, 2026-08-10):
  the note claimed `fs::metadata` "doesn't follow symlinks" — it does
  (that is `symlink_metadata`). The call itself is correct: metadata
  resolves the whole chain, so a chain (L → T → missing) fails and L
  is reported broken. The corrected comment documents why a future
  "simplification" to `symlink_metadata` must NOT happen (it would
  break chain-following detection), and a new test pins the behavior:
  a broken chain (leaf → mid → missing) reports BOTH as broken, while
  a healthy chain (→ real file) is not.

- **force_replace backups can no longer collide within the same second**
  (audit LOW, 2026-08-10): `backup_path_for` used a second-resolution
  timestamp (`as_secs()`), so two backups of the same basename in one
  directory within the same second (same link listed twice, or a daemon
  pulse plus a manual `link apply`) produced the SAME backup path and
  `fs::rename` silently overwrote the earlier backup. The timestamp is
  now nanosecond-resolution and a new `unique_backup_path` helper bumps
  a `-1`, `-2`, … suffix until the name is free (`symlink_metadata`,
  so leftover broken symlinks count as occupied too). Tests:
  suffix-bump helper (incl. broken-symlink occupancy), never-reuse of
  an occupied backup name, and a force_replace behavioral test that
  pre-places a file at the exact second-resolution name and asserts both
  backups survive.

## [0.112.34] — 2026-07-26 — full-audit remediation batch 4 (2 HIGH fixes)

From `AUDIT_FULL_2026-07-26.md`:

- **SYS-H1 — guard daemon busy-looped forever after the first
  interval**: `elapsed` was declared once before the outer daemon
  loop, so after the first full interval the inner 1-second sleep
  loop never ran again — `run_guard_once` executed back-to-back
  continuously (df/ps/du + walkdir scans every pass). `elapsed` is
  now reset inside the outer loop, every pass.
- **SYS-H2 — `link apply` could never fix a drifted symlink**:
  existing symlinks were routed through `check_safe_to_delete`,
  which ALWAYS refuses symlinks — so `apply` errored on every
  existing symlink, including the drifted ones it exists to repair
  (and even in-sync ones, since there was no short-circuit). Now
  in-sync entries are skipped and drifted symlinks are unlinked
  directly (unlinking a symlink never touches its target) before
  re-creation. Regression tests added (`links_tests.rs`).

## [0.112.12] - 2026-06-21

### Changed
- **Standalone repo**: `dracon-system` is now a first-class standalone git
  repository at
  [`DraconDev/dracon-system-disk-process-guard-doctor`](https://github.com/DraconDev/dracon-system-disk-process-guard-doctor).
  Previously this code lived in
  [`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
  as a workspace member. Source-of-truth has moved to the standalone repo;
  future releases are cut from there via `scripts/release.sh`.
- **`scripts/release.sh`**: new per-repo release script. Same interface as
  the parent monorepo's `release.sh` (`<version> --yes [--dry-run] [--abort]`),
  scoped to the standalone repo's Cargo.toml, CHANGELOG, crates.io publish,
  and GitHub release. Each utility now releases independently on its own
  cadence.
- **Push-protected remotes**: the verbose repo name
  (`dracon-system-disk-process-guard-doctor`) is the public-facing
  identity. Local directory is `dracon-system/` for ergonomics. The
  4-keyword description in the repo metadata ("disk, process, guard,
  doctor") is the canonical public description.

### Verified
- `cargo info dracon-system` confirms version 0.112.12 on crates.io
- `gh release view v0.112.12` (verbose repo) shows the github release
- Daemon's `dracon-sync repos` continues to see this repo and pushes to
  the 3 remotes (github + gitlab + codeberg) on its own cycle

[Unreleased]: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/v0.112.12...HEAD
[0.112.12]: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/releases/tag/v0.112.12
