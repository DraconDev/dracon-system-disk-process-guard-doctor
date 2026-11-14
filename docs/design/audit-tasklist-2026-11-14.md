# dracon-system audit — task list (2026-11-14)

Scope: kill paths (reap, pressure-reap, forgotten trees, session retire),
mitigations (renice, OOM bias, CPU cap), disk paths (tmp, trash, stash,
relocate, browser profiles), service/deploy plumbing, code health.
Evidence: guard journal (7d/14d), live `/proc`, `cargo test`/`clippy`/`deny`.

## Done (deployed locally 2026-11-14, uncommitted release)

- [x] **Pressure-reap killed live work** (5,227 kills / 48h). `parent == systemd`
      was treated as proof of orphanhood. Victims: `wave11-micro1-sync.service`
      (killed ~3,000× — matched its `NRestarts`), agents' background
      `timeout … bun run test` / `vite` jobs, crashpad of live test browsers.
      Fix: `owner_provably_dead` (never a `*.service` member, all reap paths) +
      `session_still_anchored` (pressure path: launching pane/agent still alive
      ⇒ keep). Both re-checked at kill time; unreadable cgroup fails closed.
      Result: 0 kills since deploy; wave11 stable.
- [x] **Exempt desktop processes were OOM-biased / CPU-capped.** With the
      session lifecycle on, `keeps_memory_exemption` narrowed EVERY exempt name
      to "active session pids", so user `systemd` (pid 1224), `plasmashell`,
      `chrome`, … got `oom_score_adj` 250. Fix: `SessionExemption { names, pids }`
      — narrowing applies only to `agent_session_names`.

## P1 — DONE 2026-11-14

- [x] **Operator decision: `retire_idle_agent_sessions = true` is now LIVE.**
      The pre-deploy binary ignored the session keys ("5 unknown keys"), so this
      kill path never ran before today. It SIGTERMs `pi` sessions with zero CPU
      and no transcript write for 6h while the fleet exceeds 8 (currently 31).
      Earliest possible fire: ~17:51 2026-11-14. Keep, disable, or add a
      notify-first / report-only period.
- [x] **Stale-binary detection.** A restart raced an install and the guard kept
      running the replaced binary while looking healthy. Add to `doctor` /
      `guard once`: warn when the service MainPID's `/proc/<pid>/exe` is
      `(deleted)` or differs from the installed binary.
- [x] **Local deploy script** (`scripts/deploy-local.sh`): build `--release
      --locked` → back up → atomic install → `systemctl --user restart` →
      verify exe is not `(deleted)` → tail journal. No publish/tag (that stays
      `release.sh`).
- [x] **Reap dry-run CLI** (`dracon-system guard reap --dry-run`): print what
      each pass (stale, pressure, forgotten, session-retire) WOULD signal right
      now, with the gate that admitted each. Today the only way is a temporary
      `#[ignore]` test; `guard once` can really kill.
- [x] **Watchdog can be switched off silently.** 2026-11-14 11:30 the guard AND
      its watchdog timer were stopped together; 15 min with no guard. Options:
      guard unit `Wants=dracon-system-guard-watchdog.timer`; dracon-sync's
      watchdog cross-checks the system timer; `doctor` flags an inactive timer.
- [x] **Cut a real release** (v0.112.53) of today's two fixes via `release.sh`
      (gates: test/clippy/deny/build all green on 2026-11-14).

## P2 — DONE 2026-11-14

- [x] **Renice is a no-op that logs ~1,000 lines/week.** A user service cannot
      hold `CAP_SYS_NICE`, so every pid hits "renice mitigation skipped".
      Either log once per pass (AGENTS.md already claims "one diagnostic") or set
      `auto_renice_on_memory = false` in the live config.
- [x] **`oom-descendant-restore … defunct` ~2,900 lines/week** — aggregate per
      pass or demote; it is bookkeeping, not an event.
- [x] **Guard memory budget.** Unit says `MemoryMax=250M`; a runtime
      `set-property` drop-in raised it to 2G and the guard peaked at 2G
      (steady-state RSS is ~6 MB, so the peak is children: nix gc / docker
      prune / cleanup scans). Run heavy cleanup children in their own transient
      unit/slice (like dracon-sync-push.slice), then drop the 2G drop-in.
- [x] **service-growth is blind** — logs "host PID/cgroup namespace visibility
      unavailable" for all 3 watched services on every start. Fix visibility
      under the unit's sandboxing or remove the feature.
- [x] **`/tmp` 2h age floor** (`tmp_min_age_hours = 2`). Held-open/cwd/socket
      and newest-mtime-in-tree checks are solid, but a finished command's output
      an agent reads later can go after 2h. Consider 6h, or exempt known
      agent-scratch prefixes.
- [x] **cpu-cap noise**: "scope contains exempt process pi" every pass for the
      same scopes — remember the refusal per scope instead of re-logging.

## P3 — DONE 2026-11-14

- [x] Split `src/main.rs` (14,350 lines, ~296 fns): memory/mitigation, cleanup
      passes, session lifecycle, CLI wiring into modules.
- [x] Docs drift: update `docs/design/guard-auto-reap-2026-10-04.md` and
      AGENTS.md "Guard auto-reap" for the new service/anchor rules; fix the
      renice "one diagnostic" claim.
- [x] Repo hygiene: move root `release-notes-v0.112.41…52.md` to the parent's
      `docs/archive/release-notes/dracon-system/`; untrack agent state
      (`.pi/goals/`, `.opencode/glla/state.json`, `.pi-glla/`, `note.md`) or
      document why it is tracked.
- [x] `git fsck`: unreachable tree `ff416f7` has 2 missing blobs (old
      `conversation-*.txt` / `pi-session-*.html`, now gitignored). Reachable
      history is intact; clears on gc. No action unless fsck is gated.
- [x] `cargo update -p async-executor` (yanked, flagged by `cargo deny`).

## Checked, no action

- `dracon-process-reaper` (24h floor, pane-gone proof) and
  `dracon-memory-guard.sh` (freezes sync, never kills) — conservative.
- Trash credential guard, stash-before-delete for `node_modules`, tmp
  held-open/socket protections — working as designed.
- Production `unwrap`/`expect`: 4; `unsafe`: thin libc calls only.

## Resolution notes (2026-11-14)

- Session retirement: operator chose **report-only first** →
  `retire_idle_agent_sessions_dry_run` (default true; live config sets it
  explicitly). Logs `session-retire (dry run) … WOULD retire`.
- Stale binary: `doctor` "guard runs installed binary" + the daemon warns
  and notifies once when `/proc/self/exe` is `(deleted)`.
- Deploy: `scripts/deploy-local.sh` (used for every deploy since).
- Reap dry-run: `dracon-system guard reap-preview [--json]`.
- Watchdog: guard unit `Wants=` its timer (shipped unit + parent flake);
  the daemon checks every 10 min and warns once per outage (no hold marker).
  Cross-repo cross-check from dracon-sync's watchdog NOT done (other repo).
- Renice: one diagnostic per daemon lifetime; live config
  `auto_renice_on_memory = false` (operator delegated the choice).
- Defunct OOM-descendant drops: one `-v` summary per sweep. cpu-cap refusals
  were already per-pid deduped; the map is now pruned to live pids.
- Memory: the 2G peaks were the guard's own page cache (anon ~1 MiB,
  `memory.events max=0`), so cleanup children were NOT moved out of the
  sandbox. Unit is now `MemoryHigh=256M` + `MemoryMax=1G`; the hidden
  runtime 2G drop-in was removed.
- service-growth: `/proc/1/ns` and the user manager's ns links are both
  unreadable to the sandboxed guard (ptrace caps); host visibility is now
  proven from `self/status` NSpid + root-relative `self/cgroup`. Measuring.
- `/tmp` floor: live config `tmp_min_age_hours = 6`.
  `tmp_large_min_age_hours = 1` left as the operator's deliberate setting —
  `check-guard-knob-parity.sh` reports it as drift (default 2).
- main.rs split: 14,350 → 9,267 lines into `guard_reap`, `mitigation`,
  `rust_targets`, `trash`, `nix_gc`, `memory_pressure`, `tmp_clean`,
  `cli_guard` (verbatim moves). Further splits (status/storage CLI,
  heavy-process checks) are optional follow-ups.
- Hygiene: v0.112.41–51 notes archived to the parent; the parent
  `.gitignore` nested-repo patterns were anchored (they were silently
  ignoring `docs/archive/release-notes/<utility>/`). Agent state kept.
- Found while doing #16: the standalone `Cargo.lock` recorded 0.112.45;
  fixed, and `release.sh` now keeps it in sync.
- Docs: AGENTS.md "Guard auto-reap" + renice notes; addendum in
  `docs/design/guard-auto-reap-2026-10-04.md`; CHANGELOG [Unreleased].
