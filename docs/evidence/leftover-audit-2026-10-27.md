# Leftover audit — whole-system sweep (2026-10-27)

Report-only. Nothing was deleted. Full `disk audit` transcript alongside:
`leftover-audit-2026-10-27.log` (58 findings, 24.9 GiB classified).

## Method

1. `dracon-system disk audit` (all four classes, report mode) — exit 0.
2. `du` sweep of `/home`, `/tmp`, Trash, caches, backup trees — everything
   outside the audit's class roots, sized but (per contract) never flagged
   when off-limits.

Disk: `/` 907G, 664G used (78%), 197G free. `/mnt/data` 440G, 301G used.

## What the audit classified (actionable)

| Finding | Size | Verdict |
|---|---|---|
| Git tmp_packs in `ai-auto-writer/.git` (9 files) | ~7.5 GiB | remove-with-approval — orphaned scratch, no live git process; next `disk clean` takes them |
| Git tmp_packs in `browser-extensions-shared/.git` (7 files) | ~4.9 GiB | remove-with-approval — same as above |
| `/tmp/k2`, mcode-install-*, eve leftovers | ~13 MiB | remove-with-approval — aged past 24h floor, unowned |
| `/tmp/mvs-audit` (50.6 MiB, in use by endless-td shells) | 50.6 MiB | keep — live processes hold it |
| `/tmp/codex-daemon-1000` (live codex fds) | 0 B | keep — live |
| Go module download cache | 277 MiB | remove-with-approval — `go clean -modcache`, no owner |
| Cargo registry cache | 187 MiB | remove-with-approval — `cargo cache`, no owner |
| ms-playwright 3.7G, .npm 3.5G, bun 2.3G, chrome 1.5G, puppeteer 0.9G | ~11.9 GiB | keep while owners live — purge from within each manager after exit |
| `~/dracon-lfs-proof` 122M, `~/tmp-shield` 7M, `~/.dracon-git-recovery-*` 26K, `~/eve-audit-new` 667B | 129 MiB | needs-operator-call — scratch markers, per-path approval required |
| Swap check (`/dev/zram0` + `/mnt/data/swapfile`, both in NixOS `swapDevices`) | — | keep — correctly placed and referenced |

SAFE total if approved: ~12.5 GiB git scratch + ~13 MiB tmp + ~464 MiB caches.

## Odd folders found by the sweep (verdicts)

- `/tmp` browser-test trees (`iso-route` 851M, `ai-tts-*` ~630M,
  `cw-browser-test-*`, `base-copy`, `synth4`, `junk-runner-e2e` 197M):
  ~9 GiB of Chrome-profile/TTS-eval artifacts, mostly Oct 5–27, some
  being written by live Chrome/playwright right now.
  Verdict: remove-with-approval, only when browsers and test harnesses
  are idle (guard live-process rule). The class-2 scan only flagged 63 MiB
  here because most of it is fresh or live-owned.
- `/tmp/root_nm` → symlink to
  `~/Dev/browser-extensions-shared/node_modules` (99k files, Oct 3).
  Verdict: needs-operator-call — a stray symlink into a live repo; harmless
  to du accounting but confusing; remove the link, not the target.
- `/tmp/node-compile-cache` 164M. Verdict: remove-with-approval when node idle.
- `~/chat` 6.9G (agent session data: audit/, compact/, extension/).
  Verdict: needs-operator-call — likely stale session logs, confirm before purge.
- `~/Backups` 23G vs `~/dracon/backups` 19G — two backup trees
  (dracon-platform/, orphans, dated incident dirs).
  Verdict: needs-operator-call — consolidate or confirm both are wanted.
- `~/ci-runner` 3.3G + `~/ci-runner-doomtap` 3.0G.
  Verdict: needs-operator-call — confirm runners still in use.
- Trash remainder 579M, root-owned (`dg-clean-host-*`, `doomtap.*` with an
  `offhost/owner.key` that cannot be read).
  Verdict: needs-operator-call — `sudo rm` only after inspecting `owner.key`.
- `~/.hermes` 2.5G, `~/.kilocode` 2.4G, `~/.zcode` 2.7G, `~/.codex` 4.0G,
  `~/.opencode` 865M. Verdict: needs-operator-call — agent-tool data,
  confirm tools before purging caches.
- `~/.rustup` 12G, `~/.cargo` 7.6G, `~/.npm-global` 6.3G, `~/.pi` 6.0G,
  `~/.dracon` 4.4G, `~/go` 1.7G. Verdict: keep — toolchain and system roots.

## Off-limits (sized only, never flagged)

`~/Dev` 365G (incl. dracon-platform with games/music sources), `~/Videos`
29G, `~/dracon` 26G, `/nix` 49G, `.config` 16G (live app/browser state).
No verdicts issued against media, git history, bundles, or live configs.
