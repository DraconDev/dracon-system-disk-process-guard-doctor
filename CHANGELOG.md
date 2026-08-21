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

- **Active-build detection now recognizes long-lived Rust tooling**
  (detection-gap fix, 2026-08-21): `detect_active_rust_builds` classified
  only cargo/rustc/clippy-driver processes as active builds, so a
  disk-pressure cleanup could delete a target dir out from under a live
  `rust-analyzer` or `cargo-watch` session (which hold the dir but were
  invisible to the cwd-based protection). Classification moved into a pure
  `is_rust_build_process` helper with unit-test coverage of new and
  pre-existing classes plus negative cases.

- **Guard cleanup target selection is now explicit** (audit M21, 2026-08-14):
  `guard clean` with no target flags no longer silently means “all”, and
  `--all` now wins over individual target flags as documented.

- **Workspace cleanup continues after individual deletion failures** (audit
  M23, 2026-08-14): `storage --cleanup --apply` now reports failed paths and
  still attempts the remaining selected paths instead of aborting on the
  first protected or I/O error.
## [0.112.37] - 2026-08-14

### Added

- **State-aware guard reporting** (`report_state_transition`): repeated
  unchanged observations are coalesced into a per-condition state machine
  instead of firing every guard cycle. Entry, escalation, and recovery
  transitions notify immediately; unchanged non-`ok` conditions repeat at
  most every `report_repeat_secs` (default 1800 = 30 min). Structured events
  and desktop notifications share the same backoff.
- **Memory-pressure classification with hysteresis**: swap occupancy alone is
  no longer treated as pressure. An observation only counts when free memory
  is low (`mem_available_warn_percent`) and/or PSI/swap-in thrash is active,
  and a candidate state must persist for `memory_pressure_sustain_secs`
  (default 120 s) before any notification or process mitigation fires.
  `guard once --json` now reports both `observed_pressure` and the stabilized
  `pressure`. Added regression coverage for classification and persistence.
- **Heavy-process alert keying**: per-process alert state is keyed by
  `pid + /proc starttime`, so a process incarnation gets at most one
  notification on entry (and one on stuck-candidate escalation), and a
  recycled PID cannot inherit silence from a previous incarnation. Alert
  state is dropped when the incarnation is no longer heavy.
- **Action-level cleanup scan cadence** (`auto_cleanup_interval_secs`,
  default 1800): the action/critical-level Rust/Trash/Nix/node_modules
  scan is bounded even in report-only mode, so persistent disk pressure no
  longer re-walks large trees every guard cycle. The timestamp is set before
  the scan so a filesystem error cannot retry-loop.

### Changed

- **Proactive cleanup now starts at 80% disk usage** (was 50%): stale
  `target/` scans only run when the disk actually needs attention. Codified
  in the example policy with the 120-cycle hourly cadence.
- **Release pipeline hardening**: `scripts/release.sh` now runs the locked
  test/build/clippy/deny gates before mutation, verifies the packaged binary
  through an install fixture, handles already-published/committed/tagged/
  released reruns, and prints exact mirror-tag push reminders. The standalone
  repository also ships a synchronized `Cargo.lock` and `deny.toml`, with a
  clean-clone regression covering all release gates so the parent workspace
  cannot mask failures. Dry-runs now update Cargo.toml/Cargo.lock/changelog/
  release notes for inspection, and `--abort` rolls back that complete set.

### Fixed

- **Disk state-change notification on restart**: starting the guard while
  disk usage is in the ordinary `warn` band no longer fires a spurious
  "state changed to warn" notification. Only a transition from a previous
  in-process state notifies; an initial `critical` reading is still
  actionable and notifies.
- **Heavy-process notification spam**: the per-process notification now
  honors the process-incarnation state machine instead of re-notifying every
  `notify_cooldown_secs` while the same heavy process persists. Escalation to
  `stuck` is still reported once.

- **Swap fallback now retains `pswpout`**: the previous swap-counter sample
  stores both parsed `/proc/vmstat` counters instead of replacing `pswpout`
  with zero, matching the runtime-state contract and regression coverage.
- **CPUQuota offender caps now clamp to 100%**: values above one CPU are
  normalized before the critical-pressure loop, preventing an invalid
  configuration from producing a failed cap warning for every offender on
  every guard interval. Added regression coverage for oversized values.
- **OOM bias no longer strands forked descendants at 250**: critical-pressure
  biasing records the existing descendant incarnations, then sweeps newly
  forked descendants on each guard pass and restores them to the nearest
  biased parent's pre-bias `oom_score_adj`. Failed or unreadable child writes
  remain pending so the root adjustment is retained and retried. The sweep is
  ancestry-, identity-, exemption-, and kernel-aware, with nested-root and
  retry regression coverage.
- **Process-management documentation now matches v0.112.36 behavior**: the
  guard's comments and README describe reversible renice, optional
  `oom_score_adj` biasing, and optional CPUQuota throttling instead of
  incorrectly claiming renice is the only process action.
- **Doctor status output now reports failures correctly**: failing checks are
  labeled `fail` instead of the inverted `present`, and the canonical
  `~/.dracon` system-root check is included in the table. Added regression
  coverage for the root row and status mapping.
- **SIGHUP policy reload and graceful shutdown now restore every active
  process limiter** before discarding runtime state: legacy and memory renice
  adjustments, OOM bias, and transient CPUQuota cgroups are all restored or
  retained for retry. Restorations use `/proc` starttime identities rather
  than cmdline argv0, treat unavailable proc/cgroup trees as indeterminate,
  avoid stopping transient services until cgroup state is readable, and report
  unverified systemd or cgroup operations instead of dropping the tracking
  entry.
- **Memory-pressure renice now preserves the original nice value**: runtime
  state stores both the pre-pressure and applied nice values, and release
  restores the captured pre-pressure value instead of hardcoding nice 0.
- **Overlapping limiter cleanup now restores the pre-limiter nice value once**:
  legacy and memory renice state compose for the same process incarnation,
  stale PID-reuse entries cannot discard a current entry, failed restoration
  remains retryable, and `guard once` restores process limiters on success,
  JSON output, and error paths instead of dropping its local runtime state.
- **Trash credential scanning now applies to dry-run estimates**: a
  credential-like filename blocks both deletion and the reported reclaim
  estimate, with the same warning/event path used by apply mode.

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
