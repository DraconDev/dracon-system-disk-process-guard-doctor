# dracon-system v0.112.41 (2026-09-14)

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

- Bump version to 0.112.41
- (See CHANGELOG.md for the full list of changes in this release)

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

**Full Changelog**: https://github.com/DraconDev/dracon-utilities/compare/dracon-system-v0.112.40...v0.112.41
