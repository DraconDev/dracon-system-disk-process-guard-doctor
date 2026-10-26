# Emergency cleanup — 2026-10-26 ~19:50Z

Trigger: root filesystem 100% full, 88MB available (`972992737280 total, 923403247616 used, 88711168 available`).
User authorized verified-idle regenerable deletions; backups/media list-only.

## Deletions (machine record: emergency-cleanup-2026-10-26.json)

| Path | Allocated before | Provenance | Idle proof | Result |
| --- | ---: | --- | --- | --- |
| /home/dracon/Dev/eve/target | 11237711872 | cargo build output; rebuilds via `cargo build` | no cargo/rustc processes; no live non-deleted exe/cwd refs (18 refs were all `(deleted)` stale test binaries); bounded protected-name scan: 0 hits | deleted, absent after: yes |
| /home/dracon/Dev/db-gateway/.cargo/target | 5878517760 | cargo build output; rebuilds via `cargo build` | same checks; 0 hits | deleted, absent after: yes |

Not deleted by this goal: `dracon-utilities/target` and `empire/target` were already absent when checked (removed outside this goal; not claimed).

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

## Completion-time verification block

(To be filled with fresh `df -B1 /`, `dracon-sync health`, per-repo dirty counts.)
