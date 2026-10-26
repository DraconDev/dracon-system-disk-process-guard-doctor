# Emergency cleanup — 2026-10-26 ~19:50Z

Trigger: root filesystem 100% full. The objective text was written when the earlier reading was 1.9G available at 19:49Z; the actual measured trigger immediately before deletion was **88,711,168 bytes (~88MB) available** (`972992737280 total, 923403247616 used`), i.e. the disk degraded further between the objective being filed and the cleanup running. Both figures are recorded so the before/after chain is unambiguous: 88MB (measured trigger) → 16,981,155,840 bytes (~17G) after deletion.
User authorized verified-idle regenerable deletions; backups/media list-only.

## Deletions (machine record: emergency-cleanup-2026-10-26.json)

| Path | Allocated before | Provenance | Idle proof | Result |
| --- | ---: | --- | --- | --- |
| /home/dracon/Dev/eve/target | 11237711872 | cargo build output; rebuilds via `cargo build` | no cargo/rustc builders at delete time; bounded protected-name scan: 0 hits. **CORRECTION: the only live-process filter used was "exe path does not contain `(deleted)`", which is unsound when applied before deletion — 18 processes were running binaries out of this tree before `rm` and simply inherited `(deleted)` afterwards.** See "Corrected safety finding" below. | deleted, absent after: yes |
| /home/dracon/Dev/db-gateway/.cargo/target | 5878517760 | cargo build output; rebuilds via `cargo build` | no cargo/rustc builders; no exe or cwd references at any point; 0 protected-name hits | deleted, absent after: yes |

Not deleted by this goal: `dracon-utilities/target` and `empire/target` were already absent when checked (removed outside this goal; not claimed).

## Corrected safety finding: eve/target was NOT verified idle

The pre-delete filter treated `(deleted)` in an exe/cwd path as proof the reference was stale, and skipped only non-`(deleted)` references. That test is unsound BEFORE deletion: a live process whose binary lives in a to-be-deleted tree reads as non-`(deleted)` only while the tree still exists, and flips to `(deleted)` the moment `rm` succeeds. So the 18 pre-existing references recorded in `emergency-cleanup-proc-exe-2026-10-26.txt` were live at deletion time, not stale.

Process evidence (raw `/proc/*/exe` dump + `ps -o lstart,stat`, captured after deletion; start times predate the 19:55:56 deletion): fixture-mcp pids 826069/840346 (Oct 6 21:03/21:04), fixture-mcp 1984368/2013263 (Oct 6 18:02/18:03), fixture-plugin 2818455/2818499 (Oct 10 16:38), eve/eve-fs/eve-bash/eve-git 3613024..3613077 (Oct 26 13:32), and 3780837..3822626 (Oct 20 22:36/22:38). All report state `S (sleeping)` — still running, not zombies.

This contradicts the `ai-auto-writer/target` skip, where a single live non-`(deleted)` process (pid 1385303) was correctly treated as disqualifying. The two decisions applied different standards: the skip was right, and the eve deletion did not satisfy `verified-idle`.

Impact assessment (observed, not assumed): the deleted tree contained build outputs only; both deleted trees' sources are present (`/home/dracon/Dev/eve/Cargo.toml` and `/home/dracon/Dev/eve/crates/` intact), so `cargo build` restores the artifacts. The 18 processes continue running from their already-mapped pages — Linux keeps the inode alive until they exit — so they were not killed by the deletion. What is genuinely lost is any un-rebuilt artifact snapshot (e.g. a binary whose source has since moved ahead); recovery is a rebuild, not a restore. No source, config, or `.git` content was in those trees.

## Skips (NOT deleted)

- `ai-auto-writer/target` (4.1G): live running binary `target/debug/ai-auto-writer` (pid 1385303, exe not deleted). Deleting under a running process is unsafe — SKIP.
- All `node_modules`: running servers may resolve from them at any time; cwd-based checks cannot rule out dependents whose cwd lies elsewhere — SKIP under verified-idle-only authority.
- `/tmp/det*`, `/tmp/gs` media, `/tmp` e2e dirs, node-compile-cache: unchanged from prior assessment — SKIP.
- Backups, music output, git packs, app databases: list-only (below).

## Filesystem observations (concurrent activity; NOT exact attribution)

- Before: `923403247616 used / 88711168 available (100%)`.
- After deletions: `906294333440 used / 17197625344 available (99%)`.
- Net available movement ≈ +17.1G, consistent with 11.2G + 5.9G removals minus concurrent writes.

## Follow-up candidates (LISTED, NOT deleted — need per-path operator approval)

| Candidate | Size | Why gated |
| --- | --- | --- |
| /home/dracon/dracon/backups (bundles: 4.4G, 4.4G, 3.2G, 2.7G, 1.8G…) | ~26G | operator backups |
| /home/dracon/Backups | ~24G | operator backups |
| platform web/music/out (26140 mp3 + covers) | 145G | generated media, no regeneration provenance |
| platform web/games content, web/books | 43G + 12G | content/assets |
| opencode.db / mimocode snapshots / rtk history.db | 5.6G / 3G / 1.5G | app databases |
| hegemon-pre-filterrepo-backup | 5.2G | operator backup |
| Rebuilt target/ trees when builders idle | varies | re-verify idle at that time |

## Completion-time verification block (2026-10-26T19:56:11Z)

Fresh `df -B1 /`: `/dev/nvme0n1p2 972992737280 906510802944 16981155840 99% /` — ~17G available, up from 88M at trigger.

Fresh `dracon-sync health`: healthy, daemon running, freeze off, policy valid.

Fresh per-repo dirty-entry counts: system 0, platform 35, strategy 0, eve 7, terhub 0, empire 0, db-gateway 0. Unrelated edits preserved; nothing reset, staged, or discarded by this goal. Eve count includes files whose target/ outputs were deleted (source tree untouched — only the regenerable `target/` build dir was removed, which `cargo build` recreates).
