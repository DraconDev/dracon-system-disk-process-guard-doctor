# Dracon System

Disk, process, guard, doctor — local machine diagnostics and watchdog for Dracon workspaces.

This repository is the **canonical standalone source** for `dracon-system` on
GitHub, GitLab, and Codeberg. It contains the source code, `Cargo.toml`, tests,
examples, and release metadata.
You can build and install this utility directly from this repo.

## Quick start (standalone build)

```bash
# Clone this repo
git clone https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git
cd dracon-system-disk-process-guard-doctor

# Build
cargo build --release

# Install (binary lands in target/release/)
sudo cp target/release/dracon-system /usr/local/bin/
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
| Source code | This repository's `main` branch |
| Source of truth | This standalone repository |
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
- Service: dracon-system-guard.service
- Example policy: `dracon-system/dracon-system.example.toml`
- Common commands: `dracon-system status · dracon-system doctor · dracon-system storage · dracon-system guard daemon`

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

Changes are made in this standalone repository. The `dracon-system` tooling
runs locally on each node; the parent meta workspace does not mirror source
files into it.

## License

AGPL-3.0-only — see [LICENSE](LICENSE).
