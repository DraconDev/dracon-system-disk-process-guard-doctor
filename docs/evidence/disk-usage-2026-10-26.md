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

### Root-wide top 20 directories (genuinely descending, bytes via `du -xs -B1`, one command per path, run in parallel)

| Rank | Path | Bytes |
| ---: | --- | ---: |
| 1 | /home/dracon/Dev/dracon-platform | 289648291840 |
| 2 | /home/dracon/Dev/browser-extensions-shared | 83504553984 |
| 3 | /home/dracon/Dev/dracon-strategy | 38631157760 |
| 4 | /home/dracon/dracon (incl. backups 19G) | 26923106304 |
| 5 | /home/dracon/Backups | 24018268160 |
| 6 | /home/dracon/Dev/dracon-utilities | 13753507840 |
| 7 | /home/dracon/.cache | 12564787200 |
| 8 | /home/dracon/.local/share/opencode (incl. opencode.db 5.6G) | 12454801408 |
| 9 | /home/dracon/Dev/eve | 12029120512 |
| 10 | /home/dracon/.config/google-chrome | 8159600640 |
| 11 | /home/dracon/.cargo | 8114130944 |
| 12 | /home/dracon/Dev/db-gateway | 7585517568 |
| 13 | /home/dracon/chat | 7370440704 |
| 14 | /tmp | 5796102144 |
| 15 | /home/dracon/hegemon-pre-filterrepo-backup-20260706.git | 5169688576 |
| 16 | /home/dracon/Dev/pi-plugins | 3593179136 |
| 17 | /home/dracon/ci-runner | 3454435328 |
| 18 | /home/dracon/ci-runner-doomtap | 3167006720 |
| 19 | /home/dracon/.local/share/mimocode | 2963591168 |
| 20 | /var/tmp | 2766381056 |

Next largest (outside top 20): /home/dracon/.npm 2714451968, /home/dracon/.local/share/rtk 1584984064 (incl. history.db 1.5G), /home/dracon/Dev/empire 1131884544, /home/dracon/.local/share/Trash 612823040, /var/log 500985856, /home/dracon/Dev/terhub 139096064. Per-top-level notes: `du -xs -B1 /home`, `/var`, `/nix` exceed the 50s per-path bound (trees too large); /nix is a separate mount (excluded by design, not on the root filesystem); `du -xs -B1` on /tmp 6117040128, /etc 360448, /usr 12288, /opt 4096, /root 4096, /srv 4096, /mnt 8192.

### Whole-root files over 1G (`find / -xdev -type f -size +1G -printf '%s %p'`, single filesystem, exit 0)

| Bytes | Path | Disposition |
| ---: | --- | --- |
| 7593661501 | dracon-strategy/ai-auto-writer/.git pack | PROTECTED (git history) |
| 5634293760 | .local/share/opencode/opencode.db | app database; SKIP (no provenance) |
| 4485338236 | dracon-platform/.git pack | PROTECTED |
| 4461548704 | dracon/backups/dracon-platform-pre-slim-20260930.bundle | operator backup; SKIP (needs per-path approval) |
| 4459279524 | dracon/backups/dracon-platform-pre-slim-20260929.bundle | same |
| 3407704136 | dracon-platform/.git second pack | PROTECTED |
| 3251764125 | .git/modules/web-games-doomtap pack | PROTECTED |
| 3196837608 | dracon/backups/doomtap-rewrite-20260922/pre-rewrite.bundle | operator backup; SKIP |
| 2783991002 | .cargo/git db pack (capture-anime-girls) | cargo git cache; regenerable via fetch but shared build input - SKIP while builders active |
| 2783685631 | .git/modules/web-games-capture-anime-girls pack | PROTECTED |
| 2714580088 | .git/modules/web-games-hellhunter pack | PROTECTED |
| 2711561519 | dracon/backups/hellhunter-pre-slim-20260929.bundle | operator backup; SKIP |
| 2557138862 | .local/share/mimocode snapshot pack | app snapshot; SKIP |
| 2445154379 | hegemon-pre-filterrepo-backup pack | operator backup; SKIP |
| 1887746449 | capture-anime-girls second pack | PROTECTED |
| 1862679789 | conv-work/cag/.git pack | PROTECTED (worktree history?) - verify before touching |
| 1838512428 | dracon/backups/cag-pre-slim-20260925.bundle | operator backup; SKIP |
| 1833712106 | opencode snapshot pack | app snapshot; SKIP |
| 1583013888 | .local/share/rtk/history.db | app database; SKIP |
| 1564811347 | .git/modules/web-games-junk-runner pack | PROTECTED |

The earlier `find /home/dracon /var/tmp /tmp` (empty) is superseded by this whole-root scan; the bulk claim is corrected: the largest single items are git packs, operator backup bundles, and app databases - all protected or approval-gated, none deleted.

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

Filesystem observations (concurrent activity; NOT exact attribution): before `834549735424 used / 88942223360 available (91%)`, after first deletions `831620542464 used / 91871416320 available (91%)`, then `831143182336 used / 92348776448 available (90%)`. At 2026-10-26T18:56:05Z the disk read `858714288128 used / 64777670656 available (93%)` - concurrent workloads refilled it after cleanup. No sustained-improvement claim is made.

### Completion-time verification block (2026-10-26T18:56:05Z)

Fresh `df -B1 /`: `/dev/nvme0n1p2 972992737280 858714288128 64777670656 93% /`.

Fresh `dracon-sync health`: healthy, daemon running, freeze off, policy valid, 36 repos across 3 roots.

Fresh per-repo dirty-entry counts (`git status --porcelain | wc -l`): dracon-utilities/dracon-system 0, dracon-platform 35, dracon-strategy 0, eve 6, terhub 0, empire 0, db-gateway 0. The system repo reads 0 because the daemon auto-committed this report; platform/eve counts drift with concurrent work (35/36 and 6/7 across observations) and were preserved, never reset or staged by this goal. Reconciliation with earlier snapshots: prior claim (system 1, platform 35, strategy 5, eve 6, empire 3) differs because the daemon committed report files and other agents committed/advanced their work between observations; the current block supersedes it.
