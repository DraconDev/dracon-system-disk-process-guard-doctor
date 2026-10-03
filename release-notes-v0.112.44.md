# dracon-system v0.112.44 (2026-10-03)

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

- Bump version to 0.112.44
- (See CHANGELOG.md for the full list of changes in this release)

## Install

```bash
# --root "$HOME/.local" puts the binary where the shipped unit's ExecStart
# (%h/.local/bin/dracon-system) expects it. A bare `cargo install` lands it in
# ~/.cargo/bin instead, and the service then dies with 203/EXEC.
cargo install dracon-system --version 0.112.44 --root "$HOME/.local"
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor.git/main/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git/compare/dracon-system-v0.112.43...dracon-system-v0.112.44
