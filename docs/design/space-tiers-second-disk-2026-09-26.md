# Space tiers + second disk + quarantine design (2026-09-26)

Status: accepted direction, Phase 0 ready to run, Phase 1+ to implement.
Context: `/` (nvme, 907G) at 97% with ~28G free and still filling; guard already
in apply mode (`auto_cleanup_apply=true`, `clean_tmp`, `clean_trash`,
`docker_prune`) yet pressure persists. Deletion-based cleanup has hit its limit:
the remaining bulk is *kept* data in the wrong place, not trash.

## Evidence (2026-09-26, all on `/`)

- `~/Dev/dracon-platform/web/music/out` 77G, git-TRACKED — generated audio/covers
  committed to the repo. Same content as the next item (identical sample hashes,
  same `audio/<genre>-<id>/` layout, 16.5k vs 16.8k files): near-duplicate.
- `~/Dev/dracon-strategy/ai-auto-music/out` 77G, git-ignored — the working copy.
  Keep as canonical.
- `~/Dev/dracon-platform/.git` 50G — inflated by tracked generated output.
- `~/Dev/dracon-platform/web/games/wip` 19G (TRACKED), `web/*/build`+`build-dev`
  ~17G (already git-ignored — working-tree only), `web/books/static` 3.3G,
  `web/games/released` 2.1G (TRACKED).
- `~/Dev/dracon-strategy/ai-auto-{writer,video}` ~26G (`target/` 7.2G,
  `authors/` 5.5G, example/video outputs).
- `/tmp` 83G / 34.7k entries, but only 11 top-level entries older than 7 days:
  mostly FRESH churn (per-run 1.3G audit clones `d*`/`r*`, playwright profiles,
  agent logs). 27 clone-like dirs with same-day refresh timestamps — producer
  is the agentic audit workflow cloning dracon-platform per check. Age-based
  `clean_tmp` (24h) cannot touch active churn; fix is producer-side (one
  reusable scratch clone + `git fetch`, or TMPDIR on the second disk).
- `~/.cache` 22G, `/var/tmp` 6.9G. Trash and docker ~empty (guard works).
- Second disk `/mnt/extra-drive-check` (440G, 149G free): 135G old `/home`
  (previous install), 41G timeshift, ~38G stale swapfiles, dead Ubuntu tree.
  ~150-200G reclaimable after migration check. Mount name is provisional.

## Tier model

- HOT (nvme, working): sources, active `target/`, daily builds. Never moved.
- WARM / quarantine (second disk, TTL'd): "keeping for now, probably delete".
  Explicit expiry (default 30d), then auto-delete. Neither trash nor archive.
- COLD (second disk, symlinked): big, rarely touched, genuinely kept. Moved
  once, reached via managed symlink recorded in link policy.
- TRASH (delete): aged-out quarantine, stale build outputs, caches.

## Rules

- Symlink cold *directories*, never individual files; never relocate `target/`
  (rebuild instead); never auto-delete without a quarantine stop first.
- `relocate <path> --to cold|quarantine` = verified copy, swap in symlink,
  policy entry, `link doctor` confirms. Reversible by construction.
- Guard watches every configured mount with per-mount state; at action level
  it proposes (then optionally applies) top cold relocations, not just deletes.
- `guard clean` moves ambiguous candidates to quarantine with a manifest
  (origin, date, size); `quarantine list/restore/expunge`; guard expires past TTL.
- Cloud (Google 5T) deferred to Phase 3: export only for data cold 90+ days,
  explicit allowlist, rclone + encryption, tested restore. No tiering discipline
  first means filling 5T the same way.

## Phases

- Phase 0 (manual, no code): reclaim second disk, stable `/mnt/data` mount,
  delete the duplicate `out/`, untrack generated output, move cold dirs.
- Phase 1 (code): per-mount guard, `relocate` on top of link policy, quarantine.
- Phase 2 (code): cold-candidate suggestions at action level, quarantine expiry.
- Phase 3 (later): cloud export for proven-cold.
