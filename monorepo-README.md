# dracon-system

**Proactive disk space monitoring and automatic cleanup.** Prevents "disk full" emergencies on development machines and servers.

## Install

```bash
cargo install dracon-system
```

The binary will be at `~/.cargo/bin/dracon-system`. Or install from the long-name façade repo:

```bash
git clone https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git
cd dracon-system-disk-process-guard-doctor
cargo build --release
```

## Features

### Disk Space Monitoring
- **Early Warning (70%)** - Proactive notification before space becomes critical
- **Warning (80%)** - State change notification
- **Action (90%)** - Automatic cleanup triggers
- **Critical (95%)** - Aggressive mitigation

### Automatic Rust Target Cleanup
- Automatically cleans `target/` directories when disk hits action level
- Smart protection for active builds:
  - Detects running `cargo`, `rustc`, `clippy-driver` processes
  - Protects target dirs in their working directories
  - Protects recently modified target dirs (configurable)
- Configurable minimum size threshold

### Process Monitoring & Graduated Renice
- Monitors processes using excessive CPU
- Graduated renice based on severity:
  - ≥180% CPU → nice 5 (gentle deprio)
  - ≥300% CPU → nice 10 (moderate deprio)
  - ≥500% CPU → nice 15 (strong deprio)
  - RSS ≥4GB → nice 5 (memory hog deprio)
  - RSS ≥8GB → nice 10 (heavy memory deprio)
- **Never kills processes** — only renices
- Auto-releases renice after process is no longer heavy

### Build-Aware Monitoring
- Detects active Rust build processes
- Protects their target directories from cleanup
- Prevents breaking active compilation

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
cargo build --release

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
cargo build --release

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
Restart=always
RestartSec=10
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

# Run single guard check
dracon-system guard once

# Run as daemon (continuous monitoring)
dracon-system guard daemon

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

## Configuration

Create `~/.dracon/utilities/system/dracon-system.toml`:

```toml
[guard]
# Enable the guard daemon
enabled = true

# Check interval in seconds
interval_secs = 30

# Disk thresholds (percent)
disk_early_warn_percent = 70
disk_warn_percent = 80
disk_action_percent = 90
disk_critical_percent = 95

# Automatic Rust target cleanup
auto_cleanup_rust = true
cleanup_min_size_mb = 256
rust_search_roots = "~/Dev"  # Default; add more paths as needed
protect_recent_minutes = 30

# Proactive cleanup (before disk reaches action level)
proactive_cleanup_percent = 50
rust_target_max_age_days = 14
proactive_cleanup_interval_cycles = 120

# Process monitoring
process_cpu_percent = 180
process_sustain_secs = 30
auto_renice = true
renice_value = 5
release_after_secs = 120

# Trend prediction
track_trends = true
trend_warn_hours = 24

# Inode monitoring
monitor_inodes = true
inode_warn_percent = 85

# Zombie process detection
monitor_zombies = true
zombie_threshold = 20

# Large log file detection
monitor_logs = true
log_size_mb = 100
log_dirs = "/var/log,~/logs"

# Guard log rotation
guard_log_file = "~/.local/state/dracon/dracon-system-guard.log"
guard_log_max_mb = 1

# Notifications
notify = true
notify_command = "notify-send"
notify_cooldown_secs = 300

# Sync freeze (for use with dracon-sync)
freeze_sync_at_action = true
unfreeze_below_percent = 88

# Protected paths (in addition to system defaults)
# protected_paths = ["/mnt/data", "/opt/important"]
```

## How It Works

### Threshold Actions

| State | Threshold | Actions |
|-------|-----------|---------|
| early-warn | 70% | Notification only |
| warn | 80% | Notification, state change alert |
| action | 90% | Freeze sync, auto-cleanup Rust targets |
| critical | 95% | All above, more aggressive cleanup |

### Cleanup Logic

When disk hits action level (90%):

1. Scan configured directories for Rust `target/` dirs
2. Detect active `cargo`/`rustc` processes
3. Protect target dirs in active build working directories
4. Delete unprotected target dirs ≥ `cleanup_min_size_mb`
5. Also clean safe trash, package caches, Nix garbage, stale `node_modules/`, and Docker resources when those policy toggles are enabled
6. Send notification with cleanup summary

### Proactive Cleanup

When disk usage is above `proactive_cleanup_percent` (default 50%) but below `disk_action_percent`:

1. Only target dirs older than `rust_target_max_age_days` (default 14) and ≥ `cleanup_min_size_mb` are removed
2. Active builds (running cargo/rustc) are always protected
3. Runs every `proactive_cleanup_interval_cycles` guard cycles

### Process Monitoring

The guard monitors processes using ≥`process_cpu_percent`% CPU or ≥`process_rss_mb` MiB RSS for >`process_sustain_secs` seconds:

1. All heavy processes are logged to persistent JSONL file
2. When `auto_renice = true`, heavy processes are reniced with graduated values
3. Higher CPU/memory usage = higher nice value (lower priority)
4. Process still gets full CPU when nothing else needs it
5. Un-reniced back to nice 0 after `release_after_secs` of being non-heavy

### Trend Prediction

The guard tracks disk usage over time and uses linear regression to predict when the disk will fill. If the predicted time is within `trend_warn_hours`, it sends an early warning.

### Safety Boundaries

The guard never kills processes; process mitigation is limited to `renice`.
Destructive cleanup paths are canonicalized first, symlinks are rejected, and configured protected paths are honored. Log truncation uses the same safety check before modifying files, so system-protected or user-protected log paths are skipped.

## Binary Size

The release binary is approximately 2.9MB, making it suitable for:
- Embedded systems
- Containers
- Minimal server installs

## License

AGPL-3.0-only — see the root [LICENSE](../LICENSE) for details.
