# dracon-system v0.112.42 (2026-09-29)

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

### Space tiers — use a second disk instead of deleting

The headline of this release. Disk pressure no longer has to be answered by
deleting: large, idle, regenerable data can be moved to a second disk first,
held in a restorable quarantine, or only reported.

- **`relocate`** — move a cold directory to another root, verify the copy by
  file count and bytes, remove the source, and leave a symlink. Dry-run by
  default; `--apply` performs the move. Refuses git-tracked directories
  unless `--allow-tracked` is passed, since replacing a repo with a symlink
  destroys the working tree.
- **`quarantine`** (`move`/`list`/`restore`/`expire`) — hold-then-delete
  staging under `quarantine_dir` (default
  `~/.local/share/dracon/quarantine`) with a TTL (`quarantine_ttl_days`,
  default 30; 0 disables expiry). Every entry carries a manifest recording
  its origin, so `restore` puts it back. `expire` deletes entries past the
  TTL, dry-run unless `--apply`.
- **Automatic cold relocation** (Phase 2) — at action/critical the guard
  scans `relocate_candidate_roots` for big, idle, untracked directories and
  moves them to `relocate_cold_root` *before* deleting anything rebuildable.
  Report-only unless `auto_relocate_apply`. Tracked content is never a
  candidate and there is no override in auto mode. Moves are re-verified,
  recorded in a daemon-owned state file, and disk pressure is re-read between
  moves (max `relocate_max_moves_per_pass` per pass).
- **Quarantine-first cleanup** (`clean_quarantine_first`, default off) —
  daemon rust-target and `node_modules` cleanup moves candidates to
  quarantine instead of deleting them. Explicit `guard clean` always
  deletes. Reclaimed bytes are reported honestly: 0 for a same-filesystem
  move, because nothing was actually freed.
- **Extra mount reporting** (`disk_extra_mounts`) — report additional mounts
  alongside the primary. Visibility only: cleanup and freeze decisions still
  key off the primary mount.
- **Link drift monitoring** — the daemon checks managed and auto-relocated
  links every pass and notifies once on drift. `link status` merges both
  sources.
- **`setup`** — reports space-tier readiness (policy, mounts, candidate
  roots, cold root, quarantine); `setup --apply` creates the missing
  cold/quarantine directories. No machine path is assumed.
- Design record: `docs/design/space-tiers-second-disk-2026-09-26.md`.

### Configuration is now enforced, not just documented

- **Numeric knob ranges are enforced at load.** Out-of-range values are
  clamped to their documented range and each clamp is reported once on
  stderr, naming the fields. A policy that omits `log_dirs` now scans a
  sensible default; a policy that sets an impossible value no longer runs
  silently on it.
- **`notify_command` accepts arguments** — `notify-send -u critical` works
  with no wrapper script. The value is split into a program and an argv
  array and executed directly, so **no shell is ever invoked** and
  metacharacters in the config or the notification text are inert.
  `guard notify-test` sends a test notification so you can confirm the
  setting works.
- **Log-size monitoring works with no configuration.** Omitting `log_dirs`
  scans the guard's own state directory; setting `log_dirs = ""` explicitly
  disables the check. A configured directory that does not exist is now
  reported instead of silently skipped.
- **Clamp reports no longer flood the journal** — the report is emitted once
  per distinct set rather than once per policy load, and a fix is confirmed
  explicitly.

### Removed

- The per-utility `deny.toml` is deleted (audit decision D5). It was a
  shadowing, weaker duplicate of the workspace root's config, and while it
  existed `cargo deny check` run from this repo silently enforced the
  *weaker* policy while CI enforced the real one. `scripts/release.sh` now
  treats cargo-deny's "falling back to default config" warning as a fatal
  error rather than letting a third, unintended policy run.

### Fixes

- Quarantine: restore no longer leaves a stray manifest in the restored
  directory, and expiry no longer aborts a whole batch — losing the
  removals it had already performed — because one entry was missing or had
  escaped the quarantine root.
- Privilege-adjacent execs (`renice`, `systemctl`) resolve an absolute path
  and refuse to fall back to a bare PATH-relative name, which PATH
  poisoning could otherwise redirect.
- `is_git_tracked` now recognises repository roots, so relocating
  `~/Dev/<repo>` can no longer replace a repo with a symlink.
- `link apply --force-replace` no longer bypasses `protected_paths`.
- `setup --apply` refuses to write through a symlinked root.
- Relocation no longer deletes the source before the symlink is in place, and
  copy verification no longer silently skips entries it could not read.
- Capped-PID bookkeeping prunes recycled PIDs instead of leaking entries.

See CHANGELOG.md for the full list of changes in this release.

## Install

```bash
cargo install dracon-system --version 0.112.42
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-utilities/main/dracon-system/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.112.41...dracon-system-v0.112.42
