# dracon-system v0.112.41 (2026-09-14)

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

- Action-level Rust target cleanup is age-gated (new
  `rust_target_action_min_age_days`, default 7 days): targets touched
  within the window are never candidates at any pressure level.
- Audit batch F36–F48, F58–F69: bounded event storage, machine-readable
  empty event output, `status` reports the effective policy path,
  consistent `~` expansion for log paths, `/tmp`-rooted tmp cleanup,
  race-safe package-cache handling, process-cwd protection in tmp
  cleanup. (See CHANGELOG.md for the full list of changes in this release)

## Install

```bash
cargo install dracon-system --version 0.112.41
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-utilities/main/dracon-system/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.112.40...dracon-system-v0.112.41
