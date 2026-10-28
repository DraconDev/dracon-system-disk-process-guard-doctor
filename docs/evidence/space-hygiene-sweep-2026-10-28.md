# Space-hygiene sweep — everything the disk audit cannot see (2026-10-28)

Report-only. Nothing was deleted, no garbage collection was run, and the nix
store was sized but not touched.

Complements `leftover-audit-2026-10-27.md` (the disk-audit sweep) and
`cleaner-coverage-gaps-2026-10-27.md` (the cleaner coverage work). This pass
covers the areas the `disk` classes do not reach: agent-tool state, chat and
session stores, the nix store, system journals, container images and backup
trees.

All sizes are `du -sx --bytes` (exact bytes, same filesystem).

## Swept areas

| Area | Bytes | GiB | Note |
|---|---|---|---|
| `~/.local/share` | 27,603,401,427 | 25.71 | agent tools, pnpm store, containers, Trash |
| `~/.local/state` | 30,484,096,236 | 28.39 | daemon state, session state |
| `/nix/store` | 48,292,600,416 | 44.98 | 37,535 paths, 610 GC roots |
| `~/Backups` | 23,781,110,001 | 22.15 | off-limits: backup bundles |
| `~/dracon/backups` | 19,411,474,935 | 18.08 | off-limits: backup bundles |
| `~/chat` | 7,321,716,379 | 6.82 | agent session data |
| `/var/log` | 586,361,472 | 0.55 | journals |
| `~/.local/share/containers` | 243,857,603 | 0.23 | container image storage |

## Findings, largest first

| # | Path | Bytes | GiB | Verdict |
|---|---|---|---|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (4 stale sandboxes) | 10,932,233,354 | 10.18 | clean-with-approval |
| 2 | `~/.local/share/opencode/snapshot` (53 git-style session snapshots, oldest 2026-04-28) | 5,898,491,422 | 5.49 | needs-operator-call |
| 3 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,946,756,827 | 5.54 | needs-operator-call |
| 4 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 4,183,823,815 | 3.90 | needs-operator-call |
| 5 | `~/.local/share/muse/sessions` (per-year session store) | 3,701,817,725 | 3.45 | needs-operator-call |
| 6 | `~/.local/share/pnpm/store` (content-addressable package store) | 2,833,801,709 | 2.64 | clean-with-approval |
| 7 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | 2.46 | needs-operator-call |
| 8 | `~/.codex/packages` (installed daemon/tool packages) | 2,022,254,271 | 1.88 | keep |
| 9 | `~/.local/share/rtk/history.db` (shell command-history DB, written today) | 1,610,940,416 | 1.50 | needs-operator-call |
| 10 | `~/.codex/sessions` (conversation history) | 1,120,760,642 | 1.04 | keep |
| 11 | `~/chat/pi/kimi` | 6,983,789,017 | 6.50 | needs-operator-call |
| 12 | `/var/log/journal` (systemd journal) | 575,668,224 | 0.54 | clean-with-approval |
| 13 | `~/.local/share/containers/storage` | 243,857,603 | 0.23 | clean-with-approval |
| 14 | `~/.local/share/Trash` | 553,895,960 | 0.52 | needs-operator-call |

Top three findings: **10.18 GiB** (stale pre-push sandboxes), **5.54 GiB**
(repair snapshot), **5.49 GiB** (opencode snapshots).

## Verdict notes

**1 — stale pre-push gate sandboxes (10.18 GiB), clean-with-approval.**
The sync daemon copies the whole repo into
`~/.local/state/dracon/sync/gates/<repo>/dracon-pre-push.<rand>` for each
pre-push hook run. Four sandboxes are older than the current day and hold
3.39 / 3.02 / 2.72 / 1.06 GiB (dated 2026-10-26, 10-27, 10-26, 10-25). The
daemon is live and its own sandboxes from today (`CckNvr`, `U27Pu7`,
`bun-cache`, `npm-cache`) must be left alone. These four are leftovers, not
live work. Safe to remove once the daemon is paused; the durable fix is a
retention policy inside the daemon, not a manual `rm`.

**2/7 — opencode and mimocode snapshots (7.95 GiB total), needs-operator-call.**
Each holds one git-style directory per session (`config`, `description`,
`HEAD`, object store). opencode has 53 snapshot dirs, the oldest from
2026-04-28; mimocode has its own copy. These are the tools' session-restore
data — deleting them removes the ability to restore those sessions. No
opencode or mimocode process is running, so nothing is currently writing them.
Operator decision: keep N months of history, or drop snapshots older than a
chosen date.

**3 — protection-repair snapshot (5.54 GiB), needs-operator-call.**
A dated, one-off repair artifact from 2026-10-20: a full `verification-target`
copy. It is the evidence of that repair, not a cache. Keep until the incident
is closed, then archive or drop.

**4 — canonical-writer-fence qualification fixture (3.90 GiB), needs-operator-call.**
A dated test fixture from 2026-10-27. Test scaffolding, not production state,
but it is what the daemon's qualification run produced — confirm the run is
finished before removing.

**6 — pnpm store (2.64 GiB), clean-with-approval.** Content-addressable
package store; `pnpm store prune` reclaims what no project references. No
pnpm process is running.

**9 — rtk history.db (1.50 GiB), needs-operator-call.** The shell wrapper's
command-history database, actively written (last modified today). It is the
same store that produced the multi-megabyte `~/.local/share/rtk/tee/` logs
seen during this sweep. It grows without bound — the durable fix is a
retention/rotation policy for it, not a one-off delete.

**11 — `~/chat/pi/kimi` (6.50 GiB), needs-operator-call.** Another agent's
session store, not ours to prune unilaterally.

**12/13/14 — journals, container storage, Trash, clean-with-approval /
needs-operator-call.** The journal is 492 MB as reported by `journalctl
--disk-usage` and can be vacuumed to a smaller cap. Container storage holds
one image set. The Trash is 579 MB, partly root-owned (the `dg-clean-host-*`
and `doomtap.*` entries with an unreadable `owner.key`) — those need `sudo`
and an operator look first.

## Off-limits (sized only, never flagged for cleaning)

- **Media** — `~/Videos` 29 GiB, `~/Music`, `~/Pictures`.
- **Git history** — every `.git` directory in `~/Dev` (365 GiB of repos).
- **Backup bundles** — `~/Backups` 22.15 GiB and `~/dracon/backups` 18.08 GiB,
  including `doomtap-rewrite-20260922` (2.98 GiB),
  `main1-build-artifacts-20260917` (4.47 GiB),
  `main1-rollbacks-20260916` (3.65 GiB),
  `main1-compiler-output-20260917` (1.62 GiB), `auto-prune` (0.71 GiB) and
  the `history-cleanup` rewrite mirror (0.31 GiB). These are the only copies
  of rewritten history.
- **Live configs** — `~/.config` 16 GiB, `~/.dracon`, NixOS configuration.

## Nix store (sized, not touched)

- On-disk: 48,292,600,416 bytes (44.98 GiB apparent), 37,535 store paths,
  610 GC roots.
- Reachable from the GC roots: 4,604 paths. The other **32,931 paths
  (87.7%)** are not referenced by any root: 31,515 of those are `.drv`
  build recipes (0.11 GiB) and **1,416 are real outputs totalling 7.79 GiB**.
- Unreachable registrations run 2025-12 → 2026-10.
- **No GC was run.** `nix-store --gc --print-dead` needs root to read the
  store DB and returned nothing as this user, so the 7.79 GiB figure is the
  unreachable-by-roots calculation, not a GC dry-run. The authoritative number
  is `sudo nix-collect-garbage --dry-run`.

## Read-only commands used

```
du -sx --bytes <path>                       # every size in this report
du -x --max-depth=1 --bytes <dir>           # breakdowns
ls -la --time-style=+%F <dir>               # ages
nix-store --gc --print-roots                # 610 roots
nix-store -qR <roots>                       # 4,604 reachable paths
nix path-info --json --all                  # 37,535 paths, sizes
journalctl --disk-usage
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'
```

No `rm`, `clean`, `--apply`, `nix-collect-garbage`, `podman system prune` or
any other mutating command was executed during this sweep.
