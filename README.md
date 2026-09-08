# Dracon System

Disk, process, guard, doctor — local machine diagnostics and watchdog for Dracon workspaces.

![`dracon-system status` output](docs/status-output.png)

This page is the standalone guide for `dracon-system` (also rendered on
crates.io). The canonical source is the `dracon-system/` directory of the
[`dracon-utilities`](https://github.com/DraconDev/dracon-utilities) monorepo
on `main`; the standalone GitHub/GitLab/Codeberg repos are frozen mirrors.
You can build and install this utility directly from either checkout.

## Quick start (standalone build)

```bash
# Clone this repo
git clone https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git
cd dracon-system-disk-process-guard-doctor

# Build (locked: workspace discipline requires --locked)
cargo build --release --locked -p dracon-system

# Install where the shipped guard unit looks
# (dracon-system-guard.service runs %h/.local/bin/dracon-system);
# or run ./install.sh at the monorepo root to install everything.
install -d "$HOME/.local/bin"
install -m 0755 target/release/dracon-system "$HOME/.local/bin/dracon-system"
```

## What is in this repo

- `src/` — utility source code
- `tests/` — integration tests (if present)
- `Cargo.toml` — standalone build manifest with registry dependencies
- `README.md` — this utility's user guide
- `BLUEPRINT.md` — design notes
- `dracon-system.example.toml` — example config
- `dracon-system-guard.service` — systemd user-service unit
- `LICENSE`, `SECURITY.md`, `.gitignore`, `.github/` — repo metadata
- `docs/SOURCE_OF_TRUTH.md` — architecture + invariants

## Relationship to the monorepo

| Boundary | Decision |
|----------|----------|
| Source code | The `dracon-system/` directory of the `dracon-utilities` monorepo (`main` branch) |
| Source of truth | The `dracon-utilities` monorepo; the standalone repos are frozen mirrors |
| Workspace integration | Included by the `dracon-utilities` meta workspace when checked out under `dracon-system/` |
| Shared libraries | Published `dracon-system-lib` crate from crates.io |
| Operational policy | `~/.dracon/utilities/` TOML files |

## Why this name?

The descriptive name is a deliberate choice for Codeberg/Forgejo, where
descriptive repo names get upvotes and free attention because readers
immediately know what the project does. The full word list (no fillers, no
audience/UX claims) is documented in
[`docs/design/github-feature-repos.md`](https://github.com/DraconDev/dracon-utilities/blob/main/docs/design/github-feature-repos.md).

## Purpose

Protects machines from disk/process pressure and provides deterministic diagnostics for storage, links, zram, events, and the guard daemon.

## Runtime

- Binary: `dracon-system`
- Service: dracon-system-guard.service (`systemctl --user enable --now dracon-system-guard.service`)
- Example policy: `dracon-system.example.toml` in this repo
  (`dracon-system/dracon-system.example.toml` from the `dracon-utilities` monorepo root);
  the live config lives at `~/.dracon/utilities/system/dracon-system.toml`
  (override with `DRACON_SYSTEM_POLICY`)
- Common commands: `dracon-system status · dracon-system doctor · dracon-system storage · dracon-system guard daemon`;
  also `events`, `link` (`status`/`doctor`/`apply`), `symlinks`, `zram`,
  `guard once` (one pass, `--json` for machines), `guard prune`, `guard clean`
  (dry-run unless `--apply`) — full list at `dracon-system --help`.
  Destructive flags (`storage --cleanup --apply`, `guard clean --apply`,
  `link apply --force-replace`) only act when the operator opts in.

## Guard behavior (observation-first)

`dracon-system guard daemon` monitors disk, memory, CPU, zombies, inodes,
logs, and cleanup candidates every `interval_secs`. Defaults are deliberately
quiet and non-destructive:

- **Report, don't act**: CPU-heavy processes are reported, not reniced
  (`auto_renice = false`); cleanup is dry-run/report-first
  (`auto_cleanup_apply = false`); disk pressure never pauses `dracon-sync`
  (`freeze_sync_at_action = false`). No process is killed, reniced, or moved
  and no file is deleted unless the operator opts in.
- **Memory-pressure limiting is reversible and gated**: `auto_renice_on_memory`
  and `bias_oom_on_pressure` (both default `true`) act only after a
  multi-signal pressure state persists `memory_pressure_sustain_secs`
  (120 s). Swap occupancy alone is **not** pressure — low available memory
  and/or PSI/swap-in thrash is required. Hard CPU caps
  (`cap_offenders_cpu_percent`) are off by default.
- **Stateful, rate-limited alerts**: entry/escalation/recovery notify once;
  unchanged conditions emit at most every `report_repeat_secs` (30 min).
  Heavy-process alerts are keyed by pid + process start time, so a persistent
  process cannot nag and a recycled PID cannot be silenced.
- **Bounded scans**: action-level cleanup scans run at most every
  `auto_cleanup_interval_secs` (30 min) even in report-only mode; proactive
  stale-`target/` scans start at `proactive_cleanup_percent = 80`.

Everything is configurable in the `[guard]` table of the policy file; see
`dracon-system.example.toml`. `dracon-system guard once --json` prints a full
machine-readable snapshot (disk state, memory `observed` vs stabilized
`pressure`, zombies, offenders).

## Maintenance

Changes are made in the `dracon-utilities` monorepo (`dracon-system/` on `main`).
The standalone repos are frozen mirrors of that tree.

## License

AGPL-3.0-only — see [LICENSE](LICENSE).

---

*Part of the [Dracon](https://dracon.uk) developer workspace.*