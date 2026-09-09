# Dracon System

**Proactive disk space monitoring and automatic cleanup.** Disk, process, guard, doctor — local machine diagnostics and watchdog. Prevents "disk full" emergencies on development machines and servers.

![`dracon-system status` output](https://raw.githubusercontent.com/DraconDev/dracon-utilities/main/dracon-system/docs/status-output.png)

This page is the user guide for `dracon-system` (also rendered on
crates.io). The canonical source is the `dracon-system/` directory of the
[`dracon-utilities`](https://github.com/DraconDev/dracon-utilities) monorepo
on `main`; the standalone GitHub/GitLab/Codeberg repos are frozen mirrors.

## Install

```bash
cargo install dracon-system
```

The binary lands at `~/.cargo/bin/dracon-system` (version 0.112.40 on
crates.io). The shipped guard unit runs `%h/.local/bin/dracon-system`, so
for service use either copy it there or install via the monorepo:

```bash
# Clone the monorepo
git clone https://github.com/DraconDev/dracon-utilities.git
cd dracon-utilities

# Build (locked: workspace discipline requires --locked)
cargo build --release --locked -p dracon-system

# Install where the guard unit looks
install -d "$HOME/.local/bin"
install -m 0755 target/release/dracon-system "$HOME/.local/bin/dracon-system"
```

Or run `./install.sh` at the monorepo root to install all three utilities
plus services and hooks in one pass.

## Features

### Disk Space Monitoring
- **Early Warning** — proactive notification before space becomes critical
- **Warning** — state change notification
- **Action** — automatic cleanup triggers
- **Critical** — aggressive mitigation

(Thresholds are policy; the shipped example uses 65/75/85/92, the compiled
defaults are 70/80/90/95 — see Configuration.)

### Automatic Rust Target Cleanup
- Automatically cleans `target/` directories when disk hits action level
- Smart protection for active builds:
  - Detects running `cargo`, `rustc`, `clippy-driver` processes
  - Protects target dirs in their working directories
  - Protects recently modified target dirs (configurable)
- Configurable minimum size threshold

### Process Monitoring & Graduated Renice
- Monitors processes using excessive CPU (default: >50% for >30s) or RSS
  (default: >4 GiB)
- Graduated renice based on severity:
  - ≥180% CPU → nice 5 (gentle deprio)
  - ≥300% CPU → nice 10 (moderate deprio)
  - ≥500% CPU → nice 15 (strong deprio)
  - RSS ≥4 GiB → nice 5 (memory hog deprio)
  - RSS ≥8 GiB → nice 10 (heavy memory deprio)
  - Targets are floors: an already nicer process is never raised to a
    higher priority by a smaller tier value
- **Never directly kills processes** — mitigation can renice heavy jobs,
  bias `oom_score_adj` during critical pressure, and optionally throttle
  CPU with a reversible `CPUQuota`; OOM bias only influences the kernel's
  last-resort choice
- Auto-releases reversible process adjustments after pressure recovers

### Build-Aware Monitoring
- Detects active Rust build processes
- Protects their target directories from cleanup
- Detects active cargo/npm/pip/go operations (including common wrappers) for
  diagnostics and dry-run protection
- Refuses recursive package-cache deletion during apply because external
  package managers provide no shared lock with the guard
- Fails closed when process metadata cannot be inspected
- Prevents breaking active compilation or cache writes by never deleting
  package caches without lifecycle-safe coordination

### Disk Space Trend Prediction
- Tracks disk usage history over time
- Predicts when disk will fill based on usage rate
- Warns if disk predicted to fill within configurable hours

### Inode Monitoring
- Monitors inode usage on root filesystem
- Warns when inode usage exceeds threshold (default 85%)
- Critical for systems with many small files

### Zombie Process Detection
- Detects accumulated zombie processes
- Alerts when zombie count exceeds threshold (default 20)
- Helps identify parent processes not reaping children

### Large Log File Detection
- Scans configured directories for large log files
- Alerts on files exceeding size threshold (default 100 MiB)
- Helps identify runaway logging

## Installation

### Quick Install (User Service)

Run the repository installer from the repository root:

```bash
cd dracon-utilities
./install.sh
```

This will:
1. Build the release binary
2. Install to `~/.local/bin/dracon-system`
3. Set up and start the systemd user service

The per-utility directories do not contain standalone installers; use the root `install.sh` for all utilities.

### Manual Install

```bash
# Build
cargo build --release --locked -p dracon-system

# Copy binary
cp target/release/dracon-system ~/.local/bin/

# (Optional) Install systemd service
mkdir -p ~/.config/systemd/user
cp dracon-system-guard.service ~/.config/systemd/user/
systemctl --user daemon-reload
```

### Server Deployment (System-wide)

For servers, you may want to run as a system service:

```bash
# Build
cargo build --release --locked -p dracon-system

# Copy binary
sudo cp target/release/dracon-system /usr/local/bin/

# Create dedicated user (optional but recommended)
sudo useradd -r -s /bin/false dracon-guard

# Create system service file
sudo cat > /etc/systemd/system/dracon-system-guard.service << 'EOF'
[Unit]
Description=Dracon System Guard - Proactive disk space monitoring
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dracon-system guard daemon
# Do not restart clean policy disablement; restart crashes/failures.
Restart=on-failure
RestartSec=10
# 78 (EX_CONFIG) is used for malformed/unreadable startup policy.
RestartPreventExitStatus=2 78
User=root
# Or use dedicated user with appropriate permissions
# User=dracon-guard
# Group=dracon-guard

# Resource limits
MemoryMax=250M
CPUQuota=20%

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable dracon-system-guard
sudo systemctl start dracon-system-guard
```

## Usage

### Commands

```bash
# Show system status
dracon-system status

# Run diagnostics
dracon-system doctor

# Analyze storage hotspots
dracon-system storage ~/Dev

# Clean up build artifacts (dry-run)
dracon-system storage ~/Dev --cleanup

# Actually clean up
dracon-system storage ~/Dev --cleanup --apply

# Run single guard check (machine-readable snapshot)
dracon-system guard once
dracon-system guard once --json

# Run as daemon (continuous monitoring)
dracon-system guard daemon

# Prune caches and Docker resources
dracon-system guard prune

# Reclaim all reclaimable space (dry-run unless --apply)
dracon-system guard clean
dracon-system guard clean --apply

# Show recent events
dracon-system events
dracon-system events -t 50
dracon-system events -s guard -s severity

# Manage symlinks
dracon-system link status
dracon-system link doctor
dracon-system link apply

# Zram stats
dracon-system zram --status
dracon-system zram --gen-config
```

Destructive flags (`storage --cleanup --apply`, `guard clean --apply`,
`link apply --force-replace`) only act when the operator opts in.
`storage --cleanup --apply` also retains all `cache` hotspots because no
shared package-manager/guard lock exists. `guard clean` with no target flags selects all six cleanup targets; target
flags select a subset. `--all` also requests Docker's aggressive mode for all
unused images, while a Docker-only selection uses Docker's dangling-resource
mode.

### Systemd Service Management

```bash
# Enable at login
systemctl --user enable dracon-system-guard

# Start now
systemctl --user start dracon-system-guard

# Check status
systemctl --user status dracon-system-guard

# View logs
journalctl --user -u dracon-system-guard -f
```

The guard service uses `Restart=on-failure`: a valid `enabled = false` policy
prints `guard disabled in policy` and exits 0, so systemd leaves it stopped.
A missing, malformed, or unreadable explicit startup policy exits with status
78 (`EX_CONFIG`), which is listed in `RestartPreventExitStatus=2 78` and
therefore does not restart-loop. Crashes, abnormal signal termination, and other nonzero failures
remain restartable; deliberate SIGTERM/SIGINT shutdown is handled cleanly.
After fixing a policy, run `systemctl --user reset-failed dracon-system-guard.service`
if systemd recorded a failed unit, then restart it.

## Configuration

The live config lives at `~/.dracon/utilities/system/dracon-system.toml`
(override with `DRACON_SYSTEM_POLICY`). The annotated template is
`dracon-system.example.toml` in this repo
(`dracon-system/dracon-system.example.toml` from the monorepo root).
Note: the shipped example is intentionally stricter (more report-only)
than the compiled defaults — see the header comment in the template:

```toml
[guard]
# Enable the guard daemon
enabled = true

# Seconds between guard check cycles
interval_secs = 30

# Disk usage thresholds (percent) — shipped example posture
# (compiled defaults are 70/80/90/95)
disk_early_warn_percent = 65
disk_warn_percent = 75
disk_action_percent = 85
disk_critical_percent = 92

# Report disk pressure without pausing dracon-sync.
freeze_sync_at_action = false
unfreeze_below_percent = 70

# Process monitoring
process_cpu_percent = 50.0      # Alert if CPU > 50%
process_rss_mb = 4096           # Alert if RSS > 4 GiB
process_sustain_secs = 30

# Report sustained CPU-heavy processes; do not change priorities by default.
auto_renice = false
renice_value = 5
release_after_secs = 120

# Memory-pressure limiting is reversible and gated (both default true):
# acts only after multi-signal pressure persists
# memory_pressure_sustain_secs (120 s)
auto_renice_on_memory = true
bias_oom_on_pressure = true
cap_offenders_cpu_percent = 0   # hard CPU throttle, off by default

# Automatic Rust target cleanup (report-only unless auto_cleanup_apply)
auto_cleanup_rust = true
auto_cleanup_apply = false
auto_cleanup_interval_secs = 1800
cleanup_min_size_mb = 256
rust_search_roots = "~/Dev"

# Proactive cleanup scans begin at 80% disk usage
proactive_cleanup_percent = 80

# Broad cleanup kinds are report-only unless explicitly requested
# (the compiled code defaults these to true)
docker_prune = false
clean_package_caches = false
clean_trash = false
clean_nix_garbage = false

# Notifications (notify_command must be an absolute path)
notify = true
notify_command = "/usr/bin/notify-send"
notify_cooldown_secs = 300
```

## How It Works

### Threshold Actions

| State | Shipped example | Actions |
|-------|-----------------|---------|
| early-warn | 65% | Notification only |
| warn | 75% | Notification, state change alert |
| action | 85% | Auto-cleanup Rust targets (+ freeze sync only if `freeze_sync_at_action = true`) |
| critical | 92% | All above, more aggressive cleanup |

### Cleanup Logic

When disk hits action level:

1. Scan configured directories for Rust `target/` dirs
2. Detect active `cargo`/`rustc` processes
3. Protect target dirs in active build working directories
4. Delete unprotected target dirs ≥ `cleanup_min_size_mb`
5. Detect active cargo/npm/pip/go operations and skip their corresponding cache estimates in dry-run; apply refuses package-cache deletion without a shared lock
6. Clean aged top-level entries only below the explicitly safe `/tmp` root or its descendants; invalid `tmp_search_paths` entries are refused
7. Also clean safe trash, Nix garbage, stale `node_modules/`, and Docker resources when those policy toggles are enabled
7. Send notification with cleanup summary

### Proactive Cleanup

When disk usage is above `proactive_cleanup_percent` (80% in the shipped
example) but below `disk_action_percent`, stale reclaim candidates are
removed on a bounded cadence. Active builds (running cargo/rustc) are
always protected.

### Process Monitoring

The guard monitors processes using ≥`process_cpu_percent`% CPU or
≥`process_rss_mb` MiB RSS for >`process_sustain_secs` seconds:

1. All heavy processes are logged to persistent JSONL file
2. When `auto_renice = true`, heavy processes are reniced with graduated values
3. Higher CPU/memory usage = higher nice value (lower priority)
4. Process still gets full CPU when nothing else needs it
5. Un-reniced after `release_after_secs` of being non-heavy

### Trend Prediction

The guard tracks disk usage over time and uses linear regression to predict when the disk will fill. If the predicted time is within `trend_warn_hours`, it sends an early warning.

### Safety Boundaries

The guard never directly kills processes. Process mitigation is limited to
reversible `renice`, optional `oom_score_adj` biasing, and optional CPUQuota
throttling; OOM bias can only influence which process the kernel chooses if
its last-resort OOM killer fires. Destructive cleanup paths are canonicalized
first, symlinks are rejected, and configured protected paths are honored.
`clean_tmp` accepts only canonical descendants of `/tmp`, never an arbitrary
home-root such as `~`. Log truncation uses the same safety check
before modifying files, so system-protected or user-protected log paths are
skipped.

## Guard Behavior (Observation-First)

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
  scans start at `proactive_cleanup_percent = 80`.

Everything is configurable in the `[guard]` table of the policy file; see
`dracon-system.example.toml`. `dracon-system guard once --json` prints a full
machine-readable snapshot (disk state, memory `observed` vs stabilized
`pressure`, zombies, offenders).

## What Is in This Repo

- `src/` — utility source code
- `tests/` — integration tests
- `Cargo.toml` — standalone build manifest with registry dependencies
- `README.md` — this utility's user guide
- `dracon-system.example.toml` — example config
- `dracon-system-guard.service` — systemd user-service unit
- `LICENSE`, `SECURITY.md`, `.gitignore`, `.github/` — repo metadata
- Architecture + invariants: [`docs/SOURCE_OF_TRUTH.md`](https://github.com/DraconDev/dracon-utilities/blob/main/dracon-system/docs/SOURCE_OF_TRUTH.md)
- Design notes: [`BLUEPRINT.md`](https://github.com/DraconDev/dracon-utilities/blob/main/dracon-system/BLUEPRINT.md)

## Relationship to the Monorepo

| Boundary | Decision |
|----------|----------|
| Source code | The `dracon-system/` directory of the `dracon-utilities` monorepo (`main` branch) |
| Source of truth | The `dracon-utilities` monorepo; the standalone repos are frozen mirrors |
| Workspace integration | Included by the `dracon-utilities` meta workspace when checked out under `dracon-system/` |
| Shared libraries | Published `dracon-system-lib` crate from crates.io |
| Operational policy | `~/.dracon/utilities/` TOML files |

## Why This Name?

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
  Bare `guard clean` selects all six cleanup targets; target flags select a
  subset, and `--all` additionally enables Docker's all-unused-images mode.
  Destructive flags (`storage --cleanup --apply`, `guard clean --apply`,
  `link apply --force-replace`) only act when the operator opts in.

## Maintenance

Changes are made in the `dracon-utilities` monorepo (`dracon-system/` on `main`).
The standalone repos are frozen mirrors of that tree.

## Binary Size

The release binary is approximately 3MB, making it suitable for:
- Embedded systems
- Containers
- Minimal server installs

## License

AGPL-3.0-only — see [LICENSE](LICENSE).

---

*Part of the [Dracon](https://dracon.uk) developer workspace.*
