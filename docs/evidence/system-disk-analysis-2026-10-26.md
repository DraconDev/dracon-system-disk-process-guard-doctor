# System disk analysis — 2026-10-26 (post-emergency-cleanup)

Analysis only: **nothing was deleted in this pass.** Measurements below are allocated sizes (`du -sb`/`du -xs`) unless the row says otherwise; observed `df` deltas during concurrent activity are not attribution proof.

## Current state

```
/dev/nvme0n1p2 ext4 907G 666G 195G 78% /        (root disk — the one that filled)
/dev/nvme0n1p1 vfat 1022M 26M 997M 3% /boot
/dev/sda2     ext4 440G 301G 117G 73% /mnt/data (second disk, not part of the problem)
```
Inodes: `60407808 total, 5852491 used (10%)` — inode exhaustion is **not** a risk.
Health: `dracon-sync` healthy, freeze off, policy valid.

Free space moved 1.3G → 167G → 195G during this analysis, driven mostly by the sync daemon's own `auto_gc` reclaiming git garbage (see "Live reclaim" below), not by anything in this goal.

## Where the 666G actually is

| Area | Size | Notes |
| --- | ---: | --- |
| /home/dracon/Dev | 392G | 59% of used space |
| /nix/store | 45G | NixOS packages; `nix-store --gc --print-dead` reports **0 dead paths** — nothing reclaimable without removing generations |
| /var/lib | 34G | dominated by a 32G swapfile (see below) |
| /var/lib/swapfile | 32G | **inactive** orphan |
| /home/dracon/dracon | 26.9G | backups 19G + conv-work 7G |
| /home/dracon/Backups | 24G | operator backup bundles |
| /home/dracon/Videos | 15.8G | `youtube/` 15G of recorded video |
| /home/dracon/chat | 7.4G | |
| hegemon-pre-filterrepo-backup…git | 5.2G | standalone bare repo backup |
| ci-runner + ci-runner-doomtap | 6.6G | |

### Dev breakdown (392G)

| Repo | Size | Largest known components |
| --- | ---: | --- |
| dracon-platform | 240G | `web/music/out` **144G** (26,140 mp3 + covers); `.git` 32.8G; `web/games` 43G; `web/books` 12G |
| dracon-strategy | 78G | `ai-auto-writer/.git` 12G; `authors` ~6.4G; `market-research` 6G; `ai-auto-video` 3.9G |
| dracon-utilities | 15.4G | rebuilt `target/` trees |
| browser-extensions-shared | 14.7G | `.git` 4.9G; `node_modules` 3.2G |
| db-gateway | 3.6G | rebuilt cargo target |
| pi-plugins | 3.3G | |
| eve | 1.1G | rebuilt target (was 11G before cleanup + rebuild) |

## Live findings worth acting on

### 1. 32G inactive swapfile on the root disk (largest single reclaimable item)

`/var/lib/swapfile` is 34,359,738,368 bytes, dated 2026-01-11, and is referenced by **nothing**:

- not in `/proc/swaps` (only `/dev/zram0` 60.9G and `/mnt/data/swapfile` 64G are active),
- not in `/etc/fstab`,
- not in the NixOS configuration,
- not held open by any process.

It is dead weight occupying 32G on the filesystem that just filled up. Swap is already covered twice over (60.9G zram + 64G on the second disk). Removing it is a one-line root action; it is **not** regenerable data, but it is also not data — it is an orphaned swap file. **Requires explicit approval** because it needs root and touches a system path.

### 2. `web/music/out` at 144G — the single dominant consumer

26,140 mp3 files plus ~5,400 covers. This is generated music output. It is 21% of the entire used disk and 60% of `dracon-platform`. No regeneration provenance was ever established, so it stays untouched without approval. Any real headroom plan has to answer this directory first.

### 3. Git garbage churn was the real cause of the 100% incident

At the peak of this analysis, `ai-auto-writer/.git` contained **11 `tmp_pack_*` files totalling 33.0G** — orphaned packs from interrupted/parallel `git gc`/`repack` runs. They were reclaimed **by the daemon's own auto-gc** during the session (the repo's `.git` went 45G → 12G, and free space jumped 167G → 195G). Two remain, near zero bytes.

This matches the documented failure class in `AGENTS.md` (`auto_gc_garbage_threshold_bytes`, default 2 GiB, added v0.113.0 to self-heal exactly this `tmp_pack_*` bloat). The mechanism worked — but only after the disk had already hit 100%, because auto-gc needs headroom to run. That ordering is the actual defect: **gc cannot rescue a full disk because a full disk prevents gc.**

### 4. `/nix/store` 45G with zero dead paths

Nothing reclaimable by `nix-store --gc`. Space here only returns by removing old generations (`nix-collect-garbage -d`), which is a different, more destructive operation than deleting dead store paths.

## Session-level safety lesson (from the preceding cleanup)

A `(deleted)` suffix on a `/proc/*/exe` link does **not** mean the process is dead, and it can already be present *before* your own `rm` (inherited from an earlier unlink of the same path). Verify `stat -L /proc/<pid>/exe` ctime against the deletion time instead of inferring from the suffix. Four checks before any build-tree deletion: builders running, `/proc/*/exe` refs, `/proc/*/cwd` refs, `/proc/*/fd/*` refs — with raw output captured before the `rm`.

## Recommended order, if more space is needed

1. **32G orphaned swapfile** — safest large win, root action, no data loss.
2. **`Videos/youtube` 15G** — user content; only the owner can say.
3. **`Backups` 24G + `dracon/backups` 19G + hegemon backup 5.2G = 48G of bundles** — per-path approval needed; several are pre-slim safety copies of history that is now published.
4. **`music/out` 144G** — the decisive item; needs a product decision about whether generated tracks are reproducible.
5. Rebuilt `target/` trees — free but temporary; they regrow on the next build.
