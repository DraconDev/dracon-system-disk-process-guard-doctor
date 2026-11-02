# Guard repair deployment and observational validation — 2026-11-02

Plan only. No binary has been installed and no service has been restarted
under this plan. Deployment (step 3) requires explicit operator approval
in addition to approving this document.

## 1. What is qualified and in scope

The 2026-11-01 performance repairs passed targeted regressions and all four
parent-workspace gates on the frozen source state recorded in
[the qualification report](../evidence/performance-2026-11-01/README.md)
(dracon-system `f6ad34189e6b988eaae01e8125a50059abe3a8ae`).

Current-tree check (2026-11-02): `f6ad341` is an ancestor of HEAD and
`git diff f6ad341..HEAD -- src/ Cargo.toml Cargo.lock` is empty, so the
build inputs at HEAD are identical to the qualified snapshot; only evidence
docs and session exports changed since. A build from HEAD therefore carries
the same source qualification. Any newer source edit re-opens qualification.

Scope is the **user guard instance only**: the incident-era capture shows a
live user unit (`dracon-system-guard.service`, PID 2798562, started
2026-10-25) and no system instance (`FragmentPath` empty, `MainPID=0`).
The unit runs `%h/.local/bin/dracon-system guard daemon`.

Out of scope (unchanged by this plan): zram sizing/tuning, epoch-spool
reclamation (still disabled), live policy values, Nix pins, browser
cleanup, and any automatic recovery behavior — the new diagnostics are
report-only.

## 2. Pre-deploy checks (read-only, no approval needed)

1. `cargo build --release --locked` from HEAD; confirm the tree is still
   the qualified source (`git diff f6ad341..HEAD -- src/ Cargo.toml
   Cargo.lock` empty).
2. `scripts/verify-install.sh <fresh-binary> <version>` — version string
   and read-only status JSON schema on the new artifact.
3. `scripts/check-unit-deployment.sh` — repo unit vs
   `~/.config/systemd/user/dracon-system-guard.service`. If stale, copy
   the repo unit over and `systemctl --user daemon-reload` before
   restarting (unit drift has silently shipped twice before).
4. Record pre-deploy state: `systemctl --user show
   dracon-system-guard.service` (PID/start timestamp),
   `dracon-system --version`, and the active policy file location.
5. Back up the running binary:
   `cp ~/.local/bin/dracon-system ~/.local/bin/dracon-system.pre-20261102`.

## 3. Deploy (requires operator approval)

1. Install: `cp <fresh-binary> ~/.local/bin/dracon-system`.
2. `systemctl --user restart dracon-system-guard.service`.
3. Confirm: new `MainPID`/`ExecMainStartTimestamp` in
   `systemctl --user show`, `ActiveState=active`, and the new version
   from `dracon-system --version`.
4. If the service fails to stay active, roll back immediately (step 6)
   instead of debugging forward on the live unit.

## 4. Observational validation (24 hours, report-only)

Cover at least one full growth window (default 600 s) and several tmux
autosave intervals under normal workload. No synthetic pressure tests,
no induced OOM, no config changes during the window.

Watch, with timestamped journal evidence:

- Guard pressure detection and safety refusals still operate (same
  signals as the incident record: warnings, mitigation attempts,
  refusals — no new crash loops).
- `service_memory_growth` observations/events for the default targets
  (`system:dracon-sync-operator.service`,
  `user:dracon-system-guard.service`,
  `user:xdg-desktop-portal.service`): warmup → bounded history →
  sustained-growth verdicts or explicit unknowns. Growth is not a leak
  diagnosis; charged memory includes cache/kernel.
- Swap-aware offender selection: low-RSS/high-swap processes can now
  appear as offenders where the old shortlist-only path hid them;
  unknown swap stays null and disclosed.
- `dracon-system doctor` (human and `--json`): per-server tmux
  producer/snapshot/launch-marker observations; stale vs fresh
  snapshots; no shared-evidence borrowing across servers.

Capture as evidence: post-deploy `systemctl --user show` output,
relevant `journalctl --user -u dracon-system-guard.service` extracts,
and `doctor --json` output, each with the capture timestamp.

## 5. Success criteria

- User guard runs the new binary for 24 h with no crash loop and no
  restart beyond the planned one.
- Growth/tmux observations appear as designed or as explicit unknowns;
  no unknown is silently treated as healthy.
- No mitigation-eligibility, restart, cap, quota, or admission action
  fires from the new code paths (they add none; confirm by absence in
  journal plus code boundary, not by assumption).
- New `[guard]` knobs (`monitor_service_memory_growth`,
  `service_memory_growth_window_secs`, `service_memory_growth_min_mb`,
  `service_memory_growth_targets`, `tmux_persistence_sockets`,
  `tmux_persistence_max_save_age_secs`) keep shipped defaults unless the
  operator later opts into TOML/custom-policy values. Array knobs stay
  out of the Nix scalar override map.

## 6. Rollback triggers and procedure

Roll back on: crash loop or failed restarts, sustained new error spam in
the journal, missing baseline pressure detection, or any unexpected
mitigation/recovery action.

1. `cp ~/.local/bin/dracon-system.pre-20261102 ~/.local/bin/dracon-system`.
2. `systemctl --user restart dracon-system-guard.service`.
3. Confirm old version string, `ActiveState=active`, and baseline
   journal behavior; keep the backup binary until the next successful
   deployment.
