# dracon-system v0.112.52 (2026-11-12)

Disk, process, storage, and service diagnostics for local development machines.

## What's Changed

All five fixes are in `relocate`, the cold-data mover that copies a directory
to the second disk and leaves a symlink behind. On hosts with
`auto_relocate_apply = true` the guard runs it unattended.

### An unverifiable swap is rolled back instead of left half-done

After the swap, relocate asks git whether the new symlink exposes the tree.
That check is documented as "cannot tell is exposure", but if git could not
answer (not runnable, `status` failed) the error propagated through `?` with
**no rollback**: the symlink stayed live, a full duplicate of the tree sat in
the staging dir, the move was not recorded, and the next run misreported the
state as "only staging cleanup failed". It now rolls back exactly like a
detected exposure: source restored, copy removed, no staging left.

### Writes that race the copy are no longer deleted

The copy was verified only against the plan's file/byte totals, which are
minutes old in daemon mode. Anything written to the source after its file was
copied (an append, a new file, a same-size rewrite) survived only in the
staging dir, and staging is then deleted. Once the source has been renamed
aside, so no path-based writer can reach it, staging is now compared against
the copy (paths, sizes, mtimes). A mismatch restores the source, removes the
copy and aborts with "the source changed while it was being copied".

### Relocated files keep their mtimes

`copy_tree` stamped every copied file with the current time, so cold data
looked freshly written to anything age-based. File mtimes are now preserved
(directory mtimes are not yet).

### Auto-relocate no longer stalls the guard

The candidate scan, the plan and the copy (up to hundreds of GiB) ran inline on
the async runtime, so a single move froze the guard pass, memory-pressure
response included, for the whole copy. They now run on the blocking pool, as
the reap paths already did.

### Sibling duplicate trees are no longer hidden

The already-cold report collapsed nested trees by string prefix, so
`/x/media2` was folded into `/x/media` and never reported. The check now
compares path components.

### Tests

Five regressions pinned: rollback on an unverifiable swap, an append racing
the copy, a same-size rewrite caught by mtime, mtime preservation, and sibling
already-cold trees.

## Install

```bash
# --root "$HOME/.local" puts the binary where the shipped unit's ExecStart
# (%h/.local/bin/dracon-system) expects it. A bare `cargo install` lands it in
# ~/.cargo/bin instead, and the service then dies with 203/EXEC.
cargo install dracon-system --version 0.112.52 --root "$HOME/.local"
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor/main/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.112.51...dracon-system-v0.112.52
