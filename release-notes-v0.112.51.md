# dracon-system v0.112.51 (2026-10-25)

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

### The rapid-fill alarm no longer invents a fill rate out of a single burst

The rate behind "Disk growing at X GiB/h … estimated N hours until full" was a
least-squares slope over the last 30 byte-precise `df` samples. That window is
routinely only **60–90 seconds long**, because the guard's own cleanup
truncates it — a reclamation starts a new growth window. A least-squares slope
over such a window is decided by its steepest segment, so one burst became the
disk's published rate.

Measured live on the reference host:

| | |
|---|---|
| Alarm published | **184.5 GiB/h · 0.5 hours until full** |
| Independent 10 s sampling, same window | peak drawdown 1.91 GiB in 180 s (38 GiB/h), net **+0.03 GiB** |

Within one hour the same disk was reported as 25.1, 36.3, 57.7, 42.9, 124.6,
107.2, 36.9 and 184.5 GiB/h — and the trend warning quoted a *different* figure
for the same quantity a minute later (11.2 GiB/h against 42.9 GiB/h). Two
numbers, one disk, one minute apart: a false deadline on every pass.

The slope is now the **median of the pairwise slopes** (Theil–Sen). A single
burst, or a single reclamation step, touches only a minority of the pairs, so
neither can move the published rate. The reclamation rule is unchanged, and
what the alarm publishes is now unambiguously the disk's **sustained** fill —
which is what "hours until full" is a statement about.

## Install

```bash
# --root "$HOME/.local" puts the binary where the shipped unit's ExecStart
# (%h/.local/bin/dracon-system) expects it. A bare `cargo install` lands it in
# ~/.cargo/bin instead, and the service then dies with 203/EXEC.
cargo install dracon-system --version 0.112.51 --root "$HOME/.local"
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor/main/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.112.50...dracon-system-v0.112.51
