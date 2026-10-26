# Disk usage investigation — 2026-10-26

Date: 2026-10-26T18:29:02Z (follow-up observations during report drafting)
Scope: whole root filesystem (operator selected). Depth: top dirs + big files.

## Filesystem totals

```
Filesystem        1B-blocks         Used   Available Use% Mounted on
/dev/nvme0n1p2 972992737280 832927186944 90564771840  91% /
Filesystem      Size  Used Avail Use% Mounted on
/dev/nvme0n1p2  907G  776G   85G  91% /
```

Commands: `df -B1 /`, `df -h /`. Later observation during drafting:
`972992737280 total, 832603947008 used, 90888011776 available, 91%`.
Available space fluctuates with concurrent workloads; deltas are observations, not attribution proof.

## Where the space is (allocated sizes via `du`, NOT freed-byte claims)

| Area | Size | Command |
| --- | --- | --- |
| /home/dracon/Dev/dracon-platform | 267G | `du -sh /home/dracon/Dev/*/` |
| /home/dracon/Dev/browser-extensions-shared | 71G | same |
| /home/dracon/Dev/dracon-strategy | 34G | same |
| /home/dracon/Dev/dracon-utilities | 13G | same |
| /home/dracon/Dev/eve | 11G | same |
| /home/dracon/Dev/db-gateway | 6.8G | same |
| /home/dracon/Dev/pi-plugins | 3.4G | same |
| /home/dracon/Dev/empire | 1.1G | same |
| /home/dracon/Dev/terhub | 133M | same |
| /home/dracon/Backups | 23G | `du -sh /home/dracon/Backups/*` |
| /home/dracon/.cache | 13G | `du -sh` per path |
| /home/dracon/.cargo | 7.6G | same |
| /home/dracon/.config/google-chrome | 7.6G | same |
| /home/dracon/chat | 6.9G | same |
| /home/dracon/.npm | 5.3G | same |
| /home/dracon/ci-runner + ci-runner-doomtap | 3.3G + 3.0G | same |
| /tmp | 4.8G | `du -sh /tmp` |
| /var/tmp | 2.6G | `du -sh /var/tmp` |
| /var/log | 478M, journals 468M | `du -sh /var/log`, `journalctl --disk-usage` |
| Docker images 165.7MB, build cache 71MB | negligible | `docker system df` |

### Platform split (267G)

- `.git` 59G — PROTECTED (git objects/history). Command: `du -sh .../dracon-platform/.git`
- `web` 207G, of which:
  - `web/music` 151G → `out` 145G = 26140 mp3 + 5394 jpg + 200 png + covers (generated music/media output). NOT proven regenerable — SKIP without explicit operator approval.
  - `web/games` 43G → wip 41G (per-game 1.9–7.8G), released 2.3G. Per-game visible sources are small (e.g. outpost-eden node_modules 215M + build 33M); remainder is hidden/VCS content — treat as PROTECTED unless proven otherwise.
  - `web/books` 12G — content; SKIP (no provenance).
  - `web/node_modules` 937M + 1.4G at web root — regenerable via install (verify no live server before removal).
- No single file over 1G found: `find /home/dracon /var/tmp /tmp -xdev -type f -size +1G` returned empty. Bulk is many small media files, not one huge file.

### Other Dev splits

- browser-extensions-shared 71G: extensions 7.1G, node_modules 3.2G (regenerable).
- dracon-strategy 34G: ai-auto-writer 20G = .git 7.2G (protected) + authors 6.4G (protected) + target 3.2G (regenerable) + data 2.4G (protected); market-research 6.0G, ai-auto-video 3.9G (check media before touching).
- eve 11G: target 9.8–10G (regenerable), vendor 454M, .git 104M.
- dracon-utilities 13G: target 11G (regenerable).
- db-gateway 6.8G: .cargo/target 5.4G (regenerable).
- empire 1.1G: target 1.1G (regenerable).

### Caches (regenerable with provenance)

- ms-playwright 3.7G, puppeteer 910M, npm _cacache 2.8G, pip 337M. Rebuild via `npx playwright install`, `npm install`, `pip install` on demand.

### Backups 23G — SKIP

`main1-build-artifacts`, `main1-rollbacks`, `*-before-purge-*.bundle` (2.6G, 1.8G, 1.4G…), `dracon-platform-orphans` 896M. These are operator backups, NOT regenerable-only. No deletion without explicit per-path approval.

### /tmp 4.8G — mixed

`gs/det1/det2/det3` 453M each, `node-compile-cache` 441M, e2e/smoke tmp dirs 50–200M each. E2E/smoke dirs look like test output (likely regenerable); `det*`/`gs` need provenance before deletion — SKIP unless identified.

## Regenerable candidate inventory (pre-cleanup allocated, NOT freed claims)

| Path | Allocated |
| --- | --- |
| dracon-utilities/target | 11G |
| eve/target | 10G |
| db-gateway/.cargo/target | 5.4G |
| ai-auto-writer/target | 3.2G |
| empire/target | 1.1G |
| browser-extensions-shared/node_modules | 3.2G |
| platform web/node_modules | 1.4G |
| ms-playwright + puppeteer + npm _cacache + pip | ~7.7G |
| /tmp e2e/smoke/node-compile-cache (subset) | ~2G identifiable |

Total safe-ish pool ≈ 40G if all verified idle. Media (.git, music/out, authors/data, books, Backups, chat, ci-runner, chrome) explicitly excluded.

## Cleanup record (this goal, machine record: docs/evidence/disk-usage-2026-10-26-cleanup.json)

Deleted (regenerable caches, idle at deletion time):

| Path | Allocated before | Provenance | Precheck |
| --- | ---: | --- | --- |
| /home/dracon/.npm/_cacache | 2933747712 | npm package cache; regenerates on next fetch | no npm install/ci running (one `npm start` only); 0 protected-name hits; absent after: yes |
| /home/dracon/.cache/pip | 352985088 | pip cache; regenerates on next pip install | no pip processes; 0 hits; absent after: yes |

Skipped with reasons (NOT deleted):

- All `target/` trees (utilities 11G, eve 10G, db-gateway 5.4G, ai-auto-writer 3.2G, empire 1.1G): cargo + rustc builders active AND live executables running from eve/ai-auto-writer/db-gateway targets. Deleting now would disrupt running processes.
- All `node_modules` (b-ext 3.2G, platform web 1.4G): live bun esbuild binaries executing from inside them; `npm start` active.
- Playwright 3.7G / puppeteer 910M browser caches: chromium processes active; deleting binaries under running browsers is unsafe.
- `/tmp/node-compile-cache` (463M): deletion attempted, failed `ENOTEMPTY` (concurrent writers) - left intact.
- `/tmp/det1`, `/tmp/det2`, `/tmp/det3`, `/tmp/gs` (453M each): contain `a.m4a`/`w.wav` media with unknown provenance - same class as the prior Agnes loss; SKIP.
- `/tmp` e2e/smoke dirs: live processes hold cwd inside /tmp test surfaces - SKIP.
- Music `out` 145G (mp3/jpg), games content, books, Backups 23G, chat, ci-runner, chrome config, all `.git`/authors/data: no regeneration provenance - SKIP. Backups need explicit per-path operator approval.

Filesystem observations (concurrent activity; NOT exact attribution): before `834549735424 used / 88942223360 available (91%)`, after first deletions `831620542464 used / 91871416320 available (91%)`, latest `831143182336 used / 92348776448 available (90%)`. Net available movement approx +3.4G, consistent with ~3.28G cache removal minus concurrent writes.
