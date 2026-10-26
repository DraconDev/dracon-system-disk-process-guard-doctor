# System disk analysis — 2026-10-26 (post-emergency-cleanup)

Analysis only: **nothing was deleted in this pass.** Measurements below are allocated sizes (`du -sb`/`du -xs`) unless the row says otherwise; observed `df` deltas during concurrent activity are not attribution proof.

## Current state

```
/dev/nvme0n1p2 ext4 907G 673G 192G 79% /        (root disk — the one that filled)
/dev/nvme0n1p1 vfat 1022M 26M 997M 3% /boot
/dev/sda2     ext4 440G 301G 117G 73% /mnt/data (second disk, not part of the problem)
```
Inodes: `60407808 total, 5852491 used (10%)` — inode exhaustion is **not** a risk.
Health: `dracon-sync` healthy, freeze off, policy valid.

**Measurements drift while you read this.** Free space moved 1.3G → 167G → 195G → 192G during the analysis, and `/home/dracon/Dev` itself shrank 392G → 319G mid-scan, driven by the sync daemon's own `auto_gc` reclaiming git garbage (see "Live reclaim"). Rows below were taken at slightly different moments; they are consistent with each other but are not a single atomic snapshot. Treat exact percentages as approximate and re-measure before acting on any specific-path decision.

## Where the space is

| Area | Size | Notes |
| --- | ---: | --- |
| /home/dracon/Dev | 319–392G | the dominant consumer; shrinking live via daemon gc |
| /nix/store | 45G | NixOS packages; `nix-store --gc --print-dead` reports **0 dead paths** — nothing reclaimable without removing generations |
| /var/lib | 34G | dominated by a 32G swapfile (see below) |
| /var/lib/swapfile | 32G | **inactive** orphan |
| /home/dracon/dracon | 26.9G | backups 19G + conv-work 7G |
| /home/dracon/Backups | 24G | operator backup bundles |
| /home/dracon/Videos | 15.8G | `youtube/` 15G of recorded video |
| /home/dracon/chat | 7.4G | |
| hegemon-pre-filterrepo-backup…git | 5.2G | standalone bare repo backup |
| ci-runner + ci-runner-doomtap | 6.6G | |

### Dev breakdown (last full scan; values drift downward as gc runs)

| Repo | Size | Largest known components |
| --- | ---: | --- |
| dracon-platform | 239–240G | `web/music/out` **144G** (26,140 mp3 + covers); `.git` 32.8G; `web/games` 43G; `web/books` 12G |
| dracon-strategy | 38–78G | `ai-auto-writer/.git` was 45G at peak, then 12G after gc |
| dracon-utilities | 15–16G | rebuilt `target/` trees |
| browser-extensions-shared | 15–16G | `.git` 4.9G; `node_modules` 3.2G |
| eve | 1.1–10.6G | target rebuilt after the cleanup (regrows on build) |
| db-gateway | 3.6–4.9G | rebuilt cargo target |
| pi-plugins | 3.0–3.3G | |

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

At the peak of this analysis, `ai-auto-writer/.git` contained **11 `tmp_pack_*` files totalling 33.0G** — orphaned packs from interrupted/parallel `git gc`/`repack` runs (largest single files on the box: 4.37G, 4.13G, 4.05G, 3.93G, 3.81G, 3.65G, 3.59G, 3.30G, 3.23G, 3.08G, 3.05G, 3.01G). They were reclaimed **by the daemon's own auto-gc** during the session: the repo's `.git` went 45G → 12G, `dracon-strategy` went 78G → 38G, `dracon/conv-work` 7G → 2.4G, and free space jumped 167G → 195G. Within minutes only 2 near-zero `tmp_pack_*` files remained.

This matches the documented failure class in `AGENTS.md` (`auto_gc_garbage_threshold_bytes`, default 2 GiB, added v0.113.0 to self-heal exactly this `tmp_pack_*` bloat). The mechanism worked — but only after the disk had already hit 100%, because auto-gc needs headroom to run. That ordering is the actual defect: **gc cannot rescue a full disk because a full disk prevents gc.** The useful follow-up is a lower gc threshold or a periodic gc, not more emergency deletion.

### 4. `/nix/store` 45G with zero dead paths

Nothing reclaimable by `nix-store --gc`. Space here only returns by removing old generations (`nix-collect-garbage -d`), which is a different, more destructive operation than deleting dead store paths.

## Session-level safety lesson (from the preceding cleanup)

A `(deleted)` suffix on a `/proc/*/exe` link does **not** mean the process is dead, and it can already be present *before* your own `rm` (inherited from an earlier unlink of the same path). Verify `stat -L /proc/<pid>/exe` ctime against the deletion time instead of inferring from the suffix. Four checks before any build-tree deletion: builders running, `/proc/*/exe` refs, `/proc/*/cwd` refs, `/proc/*/fd/*` refs — with raw output captured before the `rm`.

## Recommended order, if more space is needed

1. **32G orphaned swapfile** — safest large win, but needs root; see blocking note below.
2. **`Videos/youtube` 15G** — user content; only the owner can say.
3. **`Backups` 24G + `dracon/backups` 19G + hegemon backup 5.2G = 48G of bundles** — per-path approval needed; several are pre-slim safety copies of history that is now published.
4. **`music/out` 144G** — the decisive item; needs a product decision about whether generated tracks are reproducible.
5. Rebuilt `target/` trees — free but temporary; they regrow on the next build.

## What was actually done in this pass

### Auto-gc threshold lowered to 512 MiB (applied)

`~/.dracon/utilities/sync/dracon-sync.toml` now sets `auto_gc_garbage_threshold_bytes = 536870912` (was the 2 GiB code default), with a comment recording the incident. Backup of the prior config: `dracon-sync.toml.bak-<timestamp>` beside it.

Verified: `dracon-sync config validate` → `✅ Policy is valid`; `dracon-sync health` → healthy, policy valid.

**Mechanism verified by measurement, not assumption:** `git count-objects -v` in ai-auto-writer reported `size-garbage: 824288` (KiB ≈ 805 MiB across 5 `tmp_pack_*` files) at a moment when `objects/pack` held 7.9G. So the garbage ledger does include tmp_pack bytes and the knob sees this failure class. The old 2 GiB threshold required the garbage to pass 2 GiB before any gc ran; with real temp packs at 3–4.4 GiB each that was routinely exceeded *at the same time* the disk was already full, so the self-heal could never get the headroom it needed. 512 MiB triggers the existing one-hour-cooldown gc well before that point.

### Swapfile deletion attempted — BLOCKED on sudo password

The user approved deleting the 32G orphan. All four safety checks passed (not in `/proc/swaps`, not in `/etc/fstab`, not in NixOS config, zero exe/cwd/fd references, file dated 2026-01-11). The command `sudo rm -f /var/lib/swapfile` **failed**: `sudo: a terminal is required to read the password`. `sudo -n true` also fails, so there is no passwordless path. The file is unchanged at 34,359,738,368 bytes.

To complete it, run:

```
sudo rm -f /var/lib/swapfile
```

Or configure an askpass helper / NOPASSWD entry for the specific command. Nothing else in this analysis needs root.

## Second finding: two repos have pushing permanently broken, and that is what grows `.git`

While verifying the gc behaviour, the live journal showed reproductions distinct from disk size. Both are **warden-gate refusals**, not network faults:

**`/home/dracon/.dracon`** — `main` is **ahead 485 commits** and growing by ~1 commit/second; every push is refused with:

```
⚠️  Possible plaintext secrets detected in push.
   The warden filter may have been bypassed.
   Run: dracon-warden once /home/dracon/.dracon
```

Journal confirmation: `⏸️ /home/dracon/.dracon committed; push paused (commit-only mode)` on every pulse. Logged as `stuck repos (expire after 24h): … /home/dracon/.dracon (2h ago, 6 consecutive failures)`.

**`/home/dracon/Dev/browser-extensions-shared`** — **ahead 287 commits**, `.git` 9.2G with `size-garbage: 3.34 GiB`; push is blocked by repeated `dracon-warden: malformed or undecryptable secret tag … left as-is — the clean filter may have been bypassed` warnings.

Why this matters for disk: a repo that commits but never pushes accumulates history forever, and every failed push attempt leaves new `tmp_pack_*` garbage behind. That is the same bloat class that filled the disk. Fixing the push gate is a **safer, higher-leverage** action than deleting space, and it is also the only way to stop the growth from recurring.

Not actioned here because it is a credentials/secret-handling decision, not a disk decision: the suggested remedy is the one the daemon already prints (`dracon-warden once <repo>`), and for browser-extensions-shared the malformed tags need an owner decision about re-encrypting or rewriting those blobs. Neither should be attempted by an agent without approval.

Current git footprint for context: `dracon-platform/.git` 34.3G, `browser-extensions-shared/.git` 9.2G, `dracon-strategy/.git` 2.1G — **46G total**, versus 144G for `music/out` alone.
