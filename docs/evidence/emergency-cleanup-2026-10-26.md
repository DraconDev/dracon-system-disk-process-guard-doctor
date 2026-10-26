# Emergency cleanup — 2026-10-26 ~19:50Z

Trigger: root filesystem 100% full. The objective text was written when the earlier reading was 1.9G available at 19:49Z; the actual measured trigger immediately before deletion was **88,711,168 bytes (~88MB) available** (`972992737280 total, 923403247616 used`), i.e. the disk degraded further between the objective being filed and the cleanup running. Both figures are recorded so the before/after chain is unambiguous: 88MB (measured trigger) → 16,981,155,840 bytes (~17G) after deletion.
User authorized verified-idle regenerable deletions; backups/media list-only.

## Deletions (machine record: emergency-cleanup-2026-10-26.json)

| Path | Allocated before | Provenance | Idle proof | Result |
| --- | ---: | --- | --- | --- |
| /home/dracon/Dev/eve/target | 11237711872 | cargo build output; rebuilds via `cargo build` | no cargo/rustc builders at delete time; bounded protected-name scan: 0 hits; **all 18 `/proc/*/exe` refs already carried `(deleted)` in the pre-delete scan at 19:55:13.924Z**, 43s before the `rm`, while `du` at 19:55:34 showed the tree intact at 11237711872 bytes. Inode ctimes confirm every held inode was unlinked on an earlier date (Oct 6/12/20/24, Oct 26 13:37, and three at Oct 26 16:34 from the prior cleanup of this same path); **zero were unlinked at this goal's 19:55:56 rm**. Guard rule `strict` (see below) therefore classified them as stale, which is correct. | deleted, absent after: yes; tree recreated 20:15 by a later rebuild |
| /home/dracon/Dev/db-gateway/.cargo/target | 5878517760 | cargo build output; rebuilds via `cargo build` | no cargo/rustc builders; zero exe refs and zero cwd refs; bounded protected-name scan: 0 hits. **Open-file (fd) check was NOT run before deletion** — the post-deletion scan in `emergency-cleanup-fd-scan-2026-10-26.txt` reports `fd_refs_total=0` and `db_gateway_exe_refs=0`, which cannot retroactively prove pre-delete state. | deleted, absent after: yes |

Not deleted by this goal: `dracon-utilities/target` and `empire/target` were already absent when checked (removed outside this goal; not claimed).

## Safety finding: what was actually verified, and what was not

An earlier revision of this document claimed the 18 eve references were live at deletion time and that `(deleted)` "flips the moment `rm` succeeds". **That claim was wrong and is retracted here.** The mechanism it described does not match the evidence:

- The pre-delete scan timestamp is **19:55:13.924Z** and it already printed `(deleted)` on all 18 refs. The `rm` ran at **19:55:56.170097Z** — 43 seconds later. A suffix that was already present before the deletion cannot have been caused by it.
- `du` at 19:55:34 (after the scan, before the rm) measured the tree at its full 11237711872 bytes, so the tree itself was intact while those refs already read `(deleted)`.
- `stat -L /proc/<pid>/exe` ctimes give the unlink moment of each held inode: Oct 6 21:06 / 18:05, Oct 12 23:23, Oct 20 22:47, Oct 24 02:17–02:18, Oct 26 13:37, and Oct 26 **16:34:08** for three of them (pids 3613065/3613071/3613077) — which is the *prior* cleanup's removal of this same path, not this goal's. **Zero inodes were unlinked at 19:55:5x.**

So the references were genuinely stale: they point at older build generations whose inodes had already been unlinked, and the `(deleted)` filter correctly skipped them. The real gap is narrower and different:

1. the pre-delete scan's raw `readlink` output was not preserved (only my derived labels were), and
2. **check 4 — open file descriptors — was never executed** for either tree before deletion.

Consequence of the error: an incorrect self-accusation was published, and a guard rule was written on a false premise. Both are corrected below. The substantive conclusion — that all four idle checks must run, with raw output retained, before any `rm` — is unchanged and is now stated soundly.

Recovery status (observed): sources intact (`/home/dracon/Dev/eve/Cargo.toml`, `/home/dracon/Dev/eve/crates/`), the eve tree was rebuilt at 20:15 and `cargo metadata --offline` resolves all 36 workspace members, so no artifact was permanently lost. Stale processes kept running from already-mapped inodes, untouched by the deletion.

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

Note: `ai-auto-writer/target` (4.1G) is an in-use tree, not an approval candidate — pid 1385303 has a live, non-deleted exe under it. It is skipped by the four-check rule above.

| Candidate | Size | Why gated |
| --- | --- | --- |
| /home/dracon/dracon/backups | 19411474935 (~19G) | operator backups |
| /home/dracon/Backups | 23781110001 (~24G) | operator backups |
| platform web/music/out (26140 mp3 + covers) | 155046375897 (~145G) | generated media, no regeneration provenance |
| platform web/games content, web/books | 43G + 12G | content/assets |
| opencode.db / mimocode snapshots / rtk history.db | 5.6G / 3G / 1.5G | app databases |
| hegemon-pre-filterrepo-backup | 5.2G | operator backup |
| Rebuilt target/ trees when builders idle | varies | re-verify idle at that time |

(`/home/dracon/dracon` as a whole is 26845323450 bytes; the ~19G figure is the `backups` subdirectory only.)

## Permanent guard against this class of error

Run these FOUR checks before any `rm -rf` of a build tree, and retain the raw output. Disqualify the tree if any check finds a live user:

1. **Builders** — any `cargo`/`rustc`/`npm`/`bun`/`pip`/`playwright` process is running (`ps -eo comm=`).
2. **Executables** — any `/proc/*/exe` whose target path starts with the tree's path **and does not end in ` (deleted)`**. Note explicitly: `readlink`/`readlink -f` DO preserve the `(deleted)` suffix, and the suffix CAN already be present before your own removal (it can be inherited from an earlier unlink of the same path). A `(deleted)`-suffixed exe is by definition not a live user of the tree you are about to remove — but confirm that with check 3 of the inode timeline below rather than assuming.
3. **Working directories** — any `/proc/*/cwd` resolving to a path under the tree (same `(deleted)` caveat).
4. **Open files** — any `/proc/*/fd/*` link resolving under the tree (`(deleted)` caveat again). This is the check that was skipped here and must never be skipped: an mmap'd or open data file under the tree is an in-use signal that checks 2–3 do not catch.

Additional rule learned here: when check 2 or 4 reports a `(deleted)` reference, resolve **when** the inode died before drawing any conclusion — `stat -L /proc/<pid>/exe` (ctime equals the unlink moment) compared against your own `rm` timestamp. Do not infer from the suffix alone; earlier revisions of this document did exactly that and produced a false accusation in both directions.

Pre-delete state can never be reconstructed after the fact, so the raw output must be captured before the `rm` — the post-deletion scans shipped with this record (`emergency-cleanup-proc-exe-2026-10-26.txt`, `emergency-cleanup-fd-scan-2026-10-26.txt`) document current state only, not the pre-delete condition.

## Completion-time verification block

Fresh `df -B1 /`: `/dev/nvme0n1p2 972992737280 906510802944 16981155840 99% /` — ~17G available, up from 88M at trigger.

Fresh `dracon-sync health`: healthy, daemon running, freeze off, policy valid.

Fresh per-repo dirty-entry counts: system 0, platform 35, strategy 0, eve 7, terhub 0, empire 0, db-gateway 0. Unrelated edits preserved; nothing reset, staged, or discarded by this goal. Eve count includes files whose target/ outputs were deleted (source tree untouched — only the regenerable `target/` build dir was removed, which `cargo build` recreates).
