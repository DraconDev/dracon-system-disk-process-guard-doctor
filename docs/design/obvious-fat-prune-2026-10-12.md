# Obvious-fat prune — 2026-10-12 incident + guard increment

## What happened

The box (30 GiB RAM, 907 GiB `/`) went into a swap-thrash death spiral across
2026-10-11/12: load 60-78, ~200 MiB RAM free, 64-70 GiB swap in use, PSI
memory-full 65-76%, disk at 92% (76 GiB free). Interactive use was unusable.

**Primary root cause (operator-found): a ~50 GiB memory task sitting in swap.**
Killing it fixed the box instantly: swap 70G → 15G, 8G available, load → 8,
PSI-full → 0%. Every other number below is secondary — real fat, but not
the cause.

**Diagnostic blind spot (ours):** all triage sorted processes by RSS, which
excludes swapped-out memory. A task with 50G in swap and a small resident
footprint is invisible to `ps --sort=-%mem`. The correct lens was per-process
`VmSwap` (`grep VmSwap /proc/*/status`), which nobody checked. The guard has
the same blind spot baked in: memory-pressure events report top-RSS offenders
only. Follow-up: report top-`VmSwap` holders alongside RSS (not implemented).

## Secondary fat freed (manual, ~35 GiB disk)

- `/tmp/reconcile*` — 71 e2e-test copies, ~29 GiB, ~20h old. No live cwd, no
  open fds. Under the general 24h tmp gate, so the guard would never touch
  them before critical.
- `/tmp/pi-subagents-uid-1000/async-subagent-runs` — 658 finished runs, 2.5 GiB
  (kept the 2 with live open logs).
- `/tmp/jr-audit` (953M), `/tmp/nb-run` + `/tmp/nb-fresh` (~1.5G, full repo
  copies), `dr-audit-*`, `deathrun-shield-*`, `hh-base`, stale
  `cw-browser-test-*`. Today's live `cw-*` runs were spared.
- Root-owned leftovers (`/tmp/nb-fresh` 554M, `/tmp/nb-run` 423M,
  docker-spawned) needed `sudo rm -rf` — the unprivileged guard can never
  remove these; it now says so explicitly instead of burying per-file errors.
- 34 stale dev procs (5-15d old, tty-less, idle, signature-matching
  `serve-build`/`e2e-harness`/`stub-companion`/static servers) — SIGTERM, all
  reaped, ~200M RSS total (process-table hygiene, not memory relief).
- Orphaned endless-td puppeteer browser (main + renderer, 225% CPU for 3h,
  driver dead, reparented to systemd) + its 162M `/tmp` profile.

Disk: 92% (76G free) → 88% (112G free). `/tmp`: 47G → 13G.

Deliberately untouched: `target/` (only `db-gateway` 12G was genuinely stale;
the rest was rebuilt within days — deleting active build caches just forces
rebuilds under load), `.git` (48G, sanctioned-slimming-only), `Backups`
(operator data), 30 live `pi` sessions (operator triages its own fleet).

## Guard increment (shipped)

1. **`tmp_e2e_patterns` / `tmp_e2e_min_age_hours`** (default 6h): e2e/test
   litter gets a shorter gate; lifts entirely at the critical tier. Exclusive
   window with the general pass (no double-count); all open/cwd/exe, symlink,
   and protected-path checks unchanged; direct delete, never stash.
2. **`stash_max_bytes_per_pass_mb`** (default 10240, 0 = unlimited):
   caps cross-device stash-copy bytes per pass in the action + proactive
   rust-target paths. Over-budget candidates defer with an explicit note in
   both dry-run and apply. This is the re-arm path for `auto_cleanup_rust`,
   disabled since the 2026-10-04 stall — the operator flips it back when
   ready (note: `db-gateway`'s 12G target exceeds the 10G default and will
   report as deferred until the budget is raised or it is handled by hand).
3. **Root-needed summary**: the fast path reports `N entries need root to
   remove (e.g. …)`.

Tests: 6 new (glob shapes incl. puppeteer/playwright profiles, exclusive
window, 0-sentinel, critical bypass, open-held, budget boundary). Full suite
502 passed; clippy, deny, fmt clean.

## Follow-ups (not done)

- Top-`VmSwap` reporters in memory-pressure events (the actual lesson).
- `auto_cleanup_rust = true` re-arm in the live config (operator decision).
- Stash budget for the node_modules path (same stall class, unwired).
- Release: `scripts/release.sh 0.112.45 --dry-run` when the operator wants it
  (version bump + publish stay in the pipeline, never hand-edited).
