# Dracon-System Blueprint

## Enhanced Disk Space Monitoring

### New Features

#### 1. Early Warning System (70% threshold)
- **New threshold:** `disk_early_warn_percent` (default: 70%)
- Provides proactive notification before reaching warning state
- Gives you time to clean up before space becomes critical

#### 2. Automatic Rust Target Cleanup
- **Enabled by default:** `auto_cleanup_rust = true`
- Automatically cleans `target/` directories when disk hits action level (90%)
- Smart protection for active builds:
  - Detects running `cargo`, `rustc`, `clippy-driver` processes
  - Protects target dirs in their working directories
  - Protects recently modified target dirs (default: 30 minutes)
- Configurable minimum size threshold (default: 256 MiB)
- Searches configurable directories: `~/Dev`, `~/dracon`

#### 3. Build-Aware Monitoring
- Detects active Rust build processes
- Protects their target directories from cleanup
- Prevents breaking active compilation

#### 4. Disk Space Trend Prediction
- Tracks disk usage history over time
- Predicts when disk will fill based on usage rate
- Warns if disk predicted to fill within configurable hours (default: 24h)
- Uses linear regression on recent samples

### New Configuration Options

```toml
[guard]
# Early warning at 70% (before warning state)
disk_early_warn_percent = 70

# Warning at 80%, Action at 90%, Critical at 95%
disk_warn_percent = 80
disk_action_percent = 90
disk_critical_percent = 95

# Automatic Rust target cleanup
auto_cleanup_rust = true
cleanup_min_size_mb = 256
rust_search_roots = "~/Dev,~/dracon"
protect_recent_minutes = 30

# Trend prediction
track_trends = true
trend_warn_hours = 24
```

### Threshold Summary

| State | Default Threshold | Action |
|-------|------------------|--------|
| early-warn | 70% | Notification only |
| warn | 80% | Notification, state change |
| action | 90% | Freeze sync, auto-cleanup Rust targets |
| critical | 95% | All above actions, more aggressive |

---

## Issues Fixed

### 1. Missing daemon lock for Guard daemon
- **Problem:** No exclusive lock to prevent multiple daemon instances running simultaneously
- **Fix:** Added `acquire_daemon_lock()` using fs2::FileExt at line 788-801
- **Priority:** High
- **Status:** [x]

### 2. Unbounded memory growth in GuardRuntimeState
- **Problem:** `notify_cooldowns` HashMap grows indefinitely without cleanup
- **Fix:** Added cleanup after each guard pass - entries older than 2x cooldown period are removed (lines 569-571)
- **Priority:** Medium
- **Status:** [x]

### 3. Silent failures with `let _ =`
- **Problem:** Notification/renice command failures silently discarded
- **Location:** Lines 450-454 (notification), 476-479 (renice)
- **Priority:** Low
- **Status:** [ ] (intentional - these are best-effort operations)

### 4. Config parsing silently ignores errors
- **Problem:** Invalid TOML silently returns defaults
- **Fix:** Added warning on parse failure at line 775-777
- **Priority:** Medium
- **Status:** [x]

---

## Code Quality Notes

### Guard Policy Normalization
- `normalize_guard_policy()` at line 837 ensures all config values are within safe bounds
- Prevents misconfiguration from causing issues

### Link Management
- `evaluate_link()` properly handles symlinks, missing targets, and non-symlink paths
- `apply_link_policy()` supports force-replace with automatic backup

### Storage Analysis
- Delegates to `dracon-system-lib::analyze_workspace_storage`
- Cleanup respects `--allow-tracked` flag to avoid deleting git-tracked directories

### Module Decomposition
- Policy structs (`SystemPolicy`, `StoragePolicy`, `LinkPolicy`, `GuardPolicy`, `LinkEntry`) and default value functions extracted into `src/policy.rs`
- This reduces the monolithic `main.rs` from ~3900 lines and improves testability
- Main.rs re-exports everything via `mod policy; pub(crate) use policy::*;` for backward compatibility

---

## Future Improvements

These are not release blockers for the current public release; they are candidate follow-ups if operators request them.

- Add graceful shutdown handling for the guard daemon.
- Add signal handling for cleanup on termination.
- Add progress indication for very large storage cleanup operations.
