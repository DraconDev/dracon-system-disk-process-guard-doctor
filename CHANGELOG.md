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
## [0.112.36] - 2026-08-10

### Added

- **Memory-pressure limiter** (`auto_renice_on_memory`, default `true`): when
  memory pressure is warn/critical, the top RSS offenders are **reniced**
  (graduated: 4 GiB → nice 5, 8 GiB → nice 10) so interactive apps win CPU
  back during a choke. Reversible: restored to nice 0 after `release_after_secs`
  of recovered pressure. Fixes the "system unresponsive" symptom without
  killing anything. Whitelist via `process_exempt_names`.
- **OOM-killer bias** (`bias_oom_on_pressure`, default `true`): during CRITICAL
  pressure, top offenders get `oom_score_adj` raised to 250 so the kernel's
  last-resort OOM kill picks THEM instead of an innocent process. Writing
  `oom_score_adj` never triggers a kill — it only steers the victim choice IF
  the kernel kills anyway. Restored on recovery. Deliberately protected
  processes (adj ≤ -500, e.g. -1000 unkillable) are never touched.
- **Optional CPUQuota offender caps** (`cap_offenders_cpu_percent`, default
  `0` = off): during CRITICAL pressure, top offenders are moved into a
  transient user systemd unit with `CPUQuota=N%` — hard-throttles a stuck
  busy-loop that nice 19 still lets burn a core. CPU throttling never kills;
  the process is moved back and the unit stopped on recovery. Off by default
  because it needs a user systemd manager and moves processes between
  cgroups; verified live (100% → ~51% at `CPUQuota=50%`).

### Security

- Memory limiter and OOM bias both skip kernel threads and
  `process_exempt_names` entries; OOM bias additionally skips processes with
  `oom_score_adj <= -500` (deliberate unkillable/protected).

### Fixed

- None (behavioral additions only).

## [0.112.35] - 2026-08-10

### Added (2026-08-10, v0.112.35)

- **Memory/swap pressure guard** (`monitor_memory`): reads
  `/proc/meminfo` + PSI (`/proc/pressure/memory`) every guard pass;
  warns when free memory is low, swap usage is high, or the system is
  swap-thrashing (PSI `full avg10`). Notifications include the top-5
  RSS offenders so the operator knows what to kill. Never kills
  anything itself. Knobs: `mem_available_warn_percent` (default 10),
  `swap_used_warn_percent` (default 50), `mem_psi_full_warn`
  (default 10.0). Falls back to a pswpin-rate check when PSI is
  unavailable. This is the failure mode from the 2026-08-09/10
  incidents (RAM exhausted, kswapd thrashing at 86% CPU, 19 GiB swap
  used) that previously had NO guard at all.
- **Sustained-heavy "stuck candidate" escalation**
  (`process_stuck_after_secs`, default 600): a process still heavy
  after the sustain window plus this many seconds is reported as
  "POSSIBLY STUCK" (e.g. the 4 svelte-check processes at ~285% CPU
  holding 6 GiB that never finished). Notification only; no auto-kill.
- **Zombie process detail** (`zombie_details`): zombies are now
  enumerated per-pid with comm, ppid, parent command, whether the
  parent is still alive, and time since first seen in Z state; the
  report and notification include the oldest offenders instead of a
  bare count. Zombies are still not killable — this is diagnostic.
- **Rapid disk-fill alert** (`disk_rapid_fill_gbph`, default 20):
  byte-precise df history (percent deltas are too coarse on large
  disks) alerts "disk filling at X GiB/h" when the sustained fill
  rate crosses the threshold, long before the percent thresholds.
- **Trash credential guard** (`trash_credential_guard`, default
  true): before emptying the trash, a recursive scan checks for
  credential-signal filenames (chrome/credential/password/secret/
  token/*.env/*.pem/*.key/*.age/etc., per
  docs/design/disk-full-credentials-2026-08-10.md). Any match aborts
  the deletion — the 2026-08-10 scan found 665 credential-pattern
  matches in a 56 GiB trash.
- `guard once` report now includes Memory Pressure, Zombies, and
  Disk Fill Rate rows (and the same fields in `--json`).


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
