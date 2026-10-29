# Space-hygiene sweep — beyond the disk audit (2026-10-28)

Report-only for surveyed data. No surveyed candidate was cleaned or deleted,
and this work invoked no collection command. Earlier anchored-measurement helper
runs removed only their generated `/tmp` measurement scratch file after
recording values; that was not candidate cleanup. The helper's scratch-removal
line was removed before the follow-up stability attempts. The scheduled system
`nix-gc.service` ran at 2026-10-29 00:00 while the work was paused; its journal
reports 0 store paths deleted and 0.00 MiB freed. Workspace tests used isolated
temporary fixtures. The Nix store was sized and compared by reachability only.

Primary measurements below began on 2026-10-28 (roughly 15:53–16:05 UTC). Five
full anchored passes were run through 2026-10-29 18:03–18:06 UTC. That latest
completed pass recorded equal within-pass values (634 roots before and after)
but did not match the reported original pre-sweep baseline of 610; it is not
accepted as meeting the strict baseline check. A new baseline-bound retry is
in progress; its attempts and current outcome are recorded in the Nix section.
A scheduled system `nix-gc.service` run during the earlier work window is also
recorded below. Live state changed during every window, so byte values are
exact `du --bytes` readings for their stated pass, not an atomic filesystem
snapshot. Area, agent-tool and off-limits tables currently use the latest
completed pass (18:03–18:06 UTC), except the Git-history row, which is a
separate measurement described below. All candidate rankings are
individually pass-labeled. The sandbox sub-decomposition tables are separate
and not new global rankings.

This supplements `leftover-audit-2026-10-27.md` and
`cleaner-coverage-gaps-2026-10-27.md`. Area rows intentionally overlap: for
example, `~/.local/share` includes OpenCode and container storage, and the
agent-tool total includes OpenCode. Do not add overlapping rows together.

Unprivileged `du` reported permission errors for some protected descendants in
`~/.local/share`, `~/.local/state`, container storage, Trash, and `/var/log`.
Those totals are the exact reported accessible subtotals; they are not claimed
to include unreadable descendants. Passwordless `sudo` was unavailable, so a
complete privileged measurement remains an operator follow-up.

## Swept areas

| Area | Bytes | Verdict | Notes |
|---|---:|---|---|
| `~/.local/share` | 27,614,583,320 | needs-operator-call | Unprivileged subtotal; container and Trash descendants denied |
| `~/.local/state` | 34,992,060,510 | needs-operator-call | Unprivileged subtotal; protected descendants denied |
| `~/.local/bin` | 3,087,112,036 | keep | User-installed tools |
| `/nix/store` | 49,161,672,136 | keep | Latest completed anchored pass (not baseline-qualified); 8,366,644,248 estimated unreachable output bytes from the primary pass; see Nix section |
| Agent-tool state (six named tools) | 31,465,750,447 | keep | Exact sum of the six roots below; overlaps `~/.local/share` |
| `~/chat` | 7,321,716,379 | needs-operator-call | Agent conversation/session data |
| `/var/log` | 614,934,912 | needs-operator-call | Unprivileged subtotal; `/var/log/private` and `/var/log/gdm` denied |
| `/var/log/journal` | 603,979,776 | clean-with-approval | `du` size from the final anchored pass; a separate `journalctl --disk-usage` reading at approximately 21:05 UTC reported 475.7M |
| `~/.local/share/containers` | 243,857,603 | needs-operator-call | Unprivileged subtotal; `containers/storage` separately measured 243,759,299 bytes |
| `~/Backups` | 26,473,384,957 | keep | Backup tree; off-limits to cleaning |
| `~/dracon/backups` | 19,411,474,935 | keep | Backup bundles; off-limits to cleaning |

### Agent-tool state inventory

| Tool | Measured path(s) | Bytes | Verdict |
|---|---|---:|---|
| Codex | `~/.codex` | 4,258,549,199 | keep |
| Hermes | `~/.hermes` | 2,387,521,841 | keep |
| OpenCode | `~/.local/share/opencode` + `~/.opencode` | 13,332,068,891 | keep |
| Pi | `~/.pi` | 6,244,179,700 | keep |
| Zcode | `~/.zcode` | 2,826,633,061 | keep |
| Kilocode | `~/.kilocode` | 2,416,797,755 | keep |
| **Total** | Six named tools, no shared roots | **31,465,750,447** | keep |

The OpenCode total combines 12,429,660,938 bytes at `~/.local/share/opencode`
and 902,407,953 bytes at `~/.opencode`. The separate `mimocode` directory below
is an additional candidate, not a substitute for any of the six named tools.

## Findings, largest actionable candidates first

Every ranked candidate row has one verdict from
`{keep, clean-with-approval, needs-operator-call}`; the auxiliary spot-check
and decomposition tables carry their own verdicts in a verdict column, so no
row in this report asserts a verdict outside that set.
The latest completed full-pass ranking is from 18:03–18:06 UTC; it is not
baseline-qualified. Its top three by exact `du --bytes` readings are
**15,656,917,210 / 12,186,624,107 / 6,983,789,017 bytes**; that table appears
below as "Latest completed full anchored pass". The first table below is the original first sweep
report (15:53–16:05 UTC) with top three **10,932,233,354 / 6,983,789,017 /
5,946,756,827 bytes**; rows 1, 3 and 5 carry the first report's readings and
row 12 was re-measured in the 20:17 UTC resample. The first-sweep readings are
retained as historical context; the later full-pass rankings follow.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.{r1jOgx,quytsq,lwsNM1,1ZPjN7}` (four sandboxes; primary-pass subtotal) | 10,932,233,354 | needs-operator-call |
| 2 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 3 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,946,756,827 | needs-operator-call |
| 4 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 5 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 4,183,823,815 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,817,725 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.codex/packages` | 2,022,254,271 | keep |
| 10 | `~/.local/share/rtk/history.db` | 1,612,066,816 | needs-operator-call |
| 11 | `~/.codex/sessions` | 1,120,760,642 | keep |
| 12 | `/var/log/journal` (20:17 UTC resample) | 560,463,872 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

### Second full anchored pass (22:30–22:42 UTC)

The same candidates re-measured in the second anchored pass, again sorted by
bytes. Top three in this pass: **10,089,320,961 / 9,345,044,328 /
6,983,789,017 bytes**. These tables show different moments on a live host;
neither is a claim that a candidate is stale or safe to remove.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared` (whole-tree `du`; included 182 sandbox directories) | 10,089,320,961 | needs-operator-call |
| 2 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 9,345,044,328 | needs-operator-call |
| 3 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 4 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,942,225,851 | needs-operator-call |
| 5 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,928,542 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.codex/packages` | 2,022,254,271 | keep |
| 10 | `~/.local/share/rtk/history.db` | 1,612,066,832 | needs-operator-call |
| 11 | `~/.codex/sessions` | 1,120,760,642 | keep |
| 12 | `/var/log/journal` | 602,406,912 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

The four original sandbox paths in primary row 1 no longer exist at their
sampled names; this work did not delete them. In the 22:30 pass, the two largest
sandbox directories were `dracon-pre-push.U27Pu7` (2,994,434,549 bytes) and
`dracon-pre-push.6DbN2k` (2,983,485,489 bytes). The whole gate directory measured
10,089,320,961 bytes; one `du` call over all 182 matching sandbox directories
reported 8,704,883,941 bytes. Separate date-filtered calls reported 70
current-day directories at 8,149,133,488 bytes and 112 older directories at
57,585,810 bytes. Those subset sums are lower than the all-directory reading,
so the difference is attributed to live changes between separate `du` calls;
no hardlink de-duplication explanation is assumed. The canonical-writer-fence
candidate grew from 4,183,823,815 to 9,345,044,328 bytes during the sweep
window. No cleanup was attempted.

### Sandbox decomposition spot-resample (23:07–23:08 UTC)

This is a later size decomposition of the same overlapping gate area, not a
new global findings ranking. One `du` call over the 183 sandbox directories
produced the exact matching today/older subtotals below; the whole-tree read
was a separate call about 47 seconds earlier.

| Scope | Bytes | Verdict |
|---|---:|---|
| Whole `browser-extensions-shared` directory | 11,243,084,228 | needs-operator-call |
| All 183 `dracon-pre-push.*` directories | 9,066,337,298 | needs-operator-call |
| 71 directories dated 2026-10-28 | 9,008,751,488 | needs-operator-call |
| 112 directories older than 2026-10-28 | 57,585,810 | clean-with-approval |

The 71-directory and 112-directory totals sum exactly to the 183-directory
result in this resample. These nested scopes overlap the whole-directory size
and must not be added to it.

### Third full anchored pass (23:24–23:37 UTC)

Top three in this pass: **9,872,972,432 / 9,521,175,879 /
6,983,789,017 bytes**. The full candidate ranking is sorted by its exact
`du --bytes` readings; the first gate-directory row is a whole-tree size.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 9,872,972,432 | needs-operator-call |
| 2 | `~/.local/state/dracon/sync/gates/browser-extensions-shared` (whole-tree `du`; included 184 sandbox directories) | 9,521,175,879 | needs-operator-call |
| 3 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 4 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,942,225,851 | needs-operator-call |
| 5 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,928,542 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.codex/packages` | 2,022,254,271 | keep |
| 10 | `~/.local/share/rtk/history.db` | 1,612,664,832 | needs-operator-call |
| 11 | `~/.codex/sessions` | 1,120,760,642 | keep |
| 12 | `/var/log/journal` | 602,406,912 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

In this pass the gate-directory tree measured 9,521,175,879 bytes; one `du`
call over all 184 matching sandbox directories reported 8,661,956,035 bytes.
Separate date-filtered calls reported 72 directories from 2026-10-28 at
8,004,103,365 bytes and the same 112 older directories at 57,585,810 bytes.
Their subtotal is lower than the all-directory reading by 600,266,860 bytes;
these live, separate measurements do not reconcile, and no hardlink explanation
is assumed. The writer-fence candidate increased from 9,345,044,328 bytes in
the 22:30 pass to 9,872,972,432 bytes here. No cleanup was attempted.

### Fourth full anchored pass (2026-10-29 00:41–01:07 UTC)

This v2 pass warmed up its Nix closure query before bracketing root counts.
Top three findings by this pass's `du --bytes` readings:
**12,185,297,146 / 11,250,196,770 / 6,983,789,017 bytes**.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 12,185,297,146 | needs-operator-call |
| 2 | `~/.local/state/dracon/sync/gates/browser-extensions-shared` (whole-tree `du`; included 188 sandbox directories) | 11,250,196,770 | needs-operator-call |
| 3 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 4 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,942,225,851 | needs-operator-call |
| 5 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,928,542 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.codex/packages` | 2,022,254,271 | keep |
| 10 | `~/.local/share/rtk/history.db` | 1,612,664,832 | needs-operator-call |
| 11 | `~/.codex/sessions` | 1,120,760,642 | keep |
| 12 | `/var/log/journal` | 610,795,520 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

The gate directory measured 11,250,196,770 bytes; one `du` call over all 188
sandbox directories reported 9,773,475,442 bytes. Separate date-filtered calls
reported 76 directories dated 2026-10-28 at 9,715,886,055 bytes and the same
112 older directories at 57,585,810 bytes; their sum differs from the all-dir
measurement by 3,577 bytes. The writer-fence directory grew to 12,185,297,146
bytes. These live scopes overlap; no cleanup was attempted.

### Latest completed full anchored pass (not baseline-qualified; 2026-10-29 18:03–18:06 UTC)

This pass is the authoritative one. It was started only after the root list had
been observed byte-identical across five consecutive 30-second readings
(samples 15–19 at 18:01:01–18:03:07 UTC, all 634 roots; the readings are
31–32 seconds apart because each sample costs the command's own runtime on
top of the 30-second sleep), so the bracket was taken in a settled window
rather than across live churn. Top three findings by this pass's `du --bytes`
readings: **15,656,917,210 / 12,186,624,107 /
6,983,789,017 bytes**.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared` (whole-tree `du`) | 15,656,917,210 | needs-operator-call |
| 2 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 12,186,624,107 | needs-operator-call |
| 3 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 4 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,942,225,851 | needs-operator-call |
| 5 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,928,542 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.codex/packages` | 2,022,254,271 | keep |
| 10 | `~/.local/share/rtk/history.db` | 1,613,217,792 | needs-operator-call |
| 11 | `~/.codex/sessions` | 1,120,760,642 | keep |
| 12 | `/var/log/journal` | 603,979,776 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,998 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

Row 1 is a whole-tree `du` of the gate directory. The sandbox decomposition
below is a separate scope and must not be added to it.

#### Sandbox decomposition of the latest completed pass

One `du` call per scope. The two date-filtered scopes partition the
all-sandboxes scope exactly, and the all-sandboxes scope is smaller than the
whole-tree figure because the whole tree also holds non-sandbox entries and
because the calls ran at different moments on a live directory.

| Scope | Directories | Bytes | Verdict |
|---|---:|---:|---|
| Whole `browser-extensions-shared` directory | — | 15,656,917,210 | needs-operator-call |
| All `dracon-pre-push.*` sandboxes | 338 | 13,461,371,994 | needs-operator-call |
| …dated 2026-10-28 | 226 | 13,403,786,184 | needs-operator-call |
| …older than 2026-10-28 | 112 | 57,585,810 | clean-with-approval |

13,403,786,184 + 57,585,810 = 13,461,371,994, which equals the all-sandboxes
figure exactly. The whole-tree figure exceeds it by 2,195,545,216 bytes; the
calls are separate and the tree was live, so no hardlink or de-duplication
explanation is asserted. These nested scopes overlap the whole-directory size
and must not be added to it.

#### Post-pass live spot check (2026-10-29 20:52–20:56 UTC)

A read-only re-measurement of the same paths after the pass confirms that most
figures are still current, and names the three that drifted after the pass
ended:

| Path | Latest-pass bytes | Live bytes at spot check | Verdict | Note |
|---|---:|---:|---|---|
| `~/chat` | 7,321,716,379 | 7,321,716,379 | needs-operator-call | unchanged |
| `~/.local/share/containers` | 243,857,603 | 243,857,603 | needs-operator-call | unchanged |
| `~/.local/share/containers/storage` | 243,759,299 | 243,759,299 | needs-operator-call | unchanged |
| `~/dracon/backups` | 19,411,474,935 | 19,411,474,935 | keep | unchanged |
| `~/.hermes` | 2,387,521,841 | 2,387,521,841 | keep | unchanged |
| `~/.kilocode` | 2,416,797,755 | 2,416,797,755 | keep | unchanged |
| `~/.zcode` | 2,826,633,061 | 2,826,633,061 | keep | unchanged |
| `~/.opencode` | 902,407,953 | 902,407,953 | keep | unchanged |
| `~/chat/pi/kimi` | 6,983,789,017 | 6,983,789,017 | needs-operator-call | unchanged |
| `~/.codex/sessions` | 1,120,760,642 | 1,120,760,642 | keep | unchanged |
| `~/.local/share/muse/sessions` | 3,701,928,542 | 3,701,928,542 | needs-operator-call | unchanged |
| `~/.local/share/pnpm/store` | 2,833,801,709 | 2,833,801,709 | clean-with-approval | unchanged |
| `~/.local/share/mimocode/snapshot` | 2,645,718,475 | 2,645,718,475 | needs-operator-call | unchanged |
| `~/.local/share/opencode/snapshot` | 5,898,491,422 | 5,898,491,422 | needs-operator-call | unchanged |
| `~/.local/bin` | 3,087,112,036 | 3,086,664,460 | keep | shrank by 447,576 bytes; a tool directory changed |
| `~/.local/share/opencode` | 12,429,660,938 | 12,429,809,171 | keep | grew by 148,233 bytes |
| `~/.codex/packages` | 2,022,254,271 | 2,118,979,232 | keep | grew by 96,724,961 bytes; the Codex app-server was running |

The three drifting paths are live, actively written directories, so their
figures are point-in-time readings rather than stable values. The report keeps
the pass-labelled numbers above; the live column shows what a reader would
measure if they re-ran `du` themselves. A second spot check at 21:21–21:24 UTC
re-measured the three largest candidates: the gate directory had grown to
19,629,958,273 bytes and then to 19,631,453,716 bytes, while
`canonical-writer-fence-2026-10-27` measured 12,186,624,107 bytes (unchanged)
and `~/chat/pi/kimi` measured 6,983,789,017 bytes (unchanged).

The pnpm store is content-addressable and any pruning requires approval. The
OpenCode, Muse, mimocode, Codex, and chat paths contain session or restore data;
no retention period was assumed. `rtk/history.db` was actively written during
the measurements. Container storage and Trash have unreadable descendants, so
they need an operator review rather than a cleanup recommendation.

## Off-limits: sized, never flagged for cleaning

| Path | Size | Verdict |
|---|---:|---|
| `~/Videos` | 30,401,010,987 bytes | keep |
| `~/Pictures` | 51,124,689 bytes | keep |
| Git history in 27 `.git` directories under `~/Dev` | 63,432,860,650 bytes | keep |
| `~/Backups` | 26,473,384,957 bytes | keep |
| `~/dracon/backups` | 19,411,474,935 bytes | keep |
| `~/.config` | 16,860,648,231 bytes | keep |
| `~/.dracon` | 4,245,086,077 bytes | keep |
| NixOS configuration target `~/.dracon/nixos` (`/etc/nixos` symlink) | 297,401 bytes | keep |

Every row except the Git-history row is an exact `du --bytes` reading from the
latest completed anchored pass (18:03–18:06 UTC), which is not baseline-qualified. The Git-history row is a separate
measurement taken at 20:31 UTC: the sum of `du --bytes` over the 27 discovered
`.git` directories, which excludes working-tree files. Media, Git history,
backup bundles, and live configuration all carry `keep` verdicts only; none of
them is flagged for cleaning.

## Nix store: sized only

- Primary-pass measured store size: **48,292,600,416 bytes**. Later full-pass
  readings were **48,739,098,826 bytes** (2026-10-28 22:30),
  **48,292,944,394 bytes** (23:24), **50,942,398,774 bytes**
  (2026-10-29 00:41), and **49,161,672,136 bytes** (2026-10-29 18:03).
- The primary-pass reachability calculation found 4,604 reachable paths and
  32,931 paths outside those closures. Of the latter, 31,515 were derivations;
  1,416 were outputs totaling **8,366,644,248 bytes** (7.79 GiB). This is a
  read-only unreachable-output estimate from the primary pass, not a prediction
  that every path will necessarily be reclaimed.
- The second full pass (22:30:31–22:41:57 UTC) measured **4,604 reachable
  paths before and after, with an identical reachable path set**, while the
  store path count stayed at **37,535 before and after**. The GC-root count
  moved **651 → 657** (621 → 627 indirect roots; 30 direct roots in both
  readings): eight root entries were added and two removed (net +6); the added
  entries pointed at existing store paths, while removed entries were
  `ffmpegthumbs-25.08.3` and `taglib-2.1.1`.
- The third full pass (2026-10-28 23:24:56–23:37:03 UTC) measured **4,603
  reachable paths before and after, with an identical reachable path set**.
  Root count changed **635 → 632** (605 → 602 indirect roots; 30 direct roots
  in both readings). The store-entry count changed **37,535 → 37,532**; the
  cause was not established.
- The earlier anchored pass (20:17–20:29 UTC) read 637 → 633 roots with the
  same 4,606-path reachable set. On 2026-10-28, spot samples ranged 615–739
  roots from 21:13–22:29 UTC; a transient burst ranged 2,616–3,069 at
  22:08–22:12 UTC. A five-list stability check at 23:21–23:24 observed five
  identical lists at 635 roots, but the ensuing full pass ended at 632.
- The 2026-10-29 v2 full pass ran 00:41:01–01:07:03 UTC after a 19-second
  reachability warm-up. It measured **620 → 615 roots** (590 → 584 indirect;
  30 → 31 direct) and **37,537 → 37,908 store entries**. Two profile
  generations appeared during this interval: `profile-66-link` at 00:52:35 and
  `profile-67-link` at 01:06:54. Their manifests list the same 25 packages, but
  the `empire` output changed each time and points to successive revisions of
  local `~/Dev/empire` `main`. The pass's final root list added `profile-66-link`
  plus `mpdecimal-4.0.1`, while removing seven indirect entries. This is
  consistent with external profile upgrades during the pass; the invoking
  process was not identified.
- Stability checks after the v2 pass still failed: five samples at 00:24–00:26
  varied 614, 619, 650, 647, 645; a 150-second interval without Nix queries
  changed 620 → 622 and added `fontconfig-conf` and
  `tmuxplugin-resurrect-unstable-2022-05-01`. Later spot readings ranged
  614–650; the 01:20:54 sample was 621. Separate probes also showed Nix
  closure/path-info queries coinciding with indirect-root changes, so the
  instrument may contribute transient roots; this was not isolated from other
  activity. The original pre-sweep reading was 610, followed by 612 and 614/615.
  A 20-sample check at 09:28–09:38 ranged 638–645 with a maximum identical
  full-list streak of four, so the compressed v4 pass was not started. Five-
  second diffs showed `tmuxplugin-resurrect` repeatedly appearing/disappearing;
  one sample also gained a Chromium sandbox root. A `/proc` file-descriptor
  check observed tmux-resurrect `scripts/save.sh` holding the store path while
  its root was present; the short-lived process exited before its parent could
  be identified. At that earlier sample the active tmux server reported
  `@continuum-save-interval 5`, so its periodic save hook was enabled. These
  live-process roots are not represented by `/nix/var/nix/gcroots/auto` symlink
  changes. No tmux process or setting was stopped during that earlier check.
- Additional root-list-only stability checks on 2026-10-29 16:29–16:49 UTC made
  six five-sample attempts; all failed to obtain five byte-identical sorted
  lists 30 seconds apart, so no anchored pass was started. The 16:44 attempt
  counted 647, 647, 3,068, 3,068, 3,068 roots; the 16:47 attempt counted 3,068,
  3,096, 3,094, 643, 697. A separate 16:43 pair changed 643 → 647 and showed
  four added root entries. These checks invoked only `nix-store --gc
  --print-roots`, not `nix-store -qR`; the bursts are observed but not
  attributed. Three immediate root-list readings at 16:50:32–16:50:46 were
  724, 700, and 693, and a nix-daemon worker was present; its effect on the
  changing counts was not established.
- During that follow-up observation, both tmux servers had
  `@continuum-save-interval 0`, and three `tmux-persist-daemon` processes plus
  the legacy `/tmp/tpd` driver were stopped. The root lists still changed.
  At 16:51 UTC both server intervals were restored to 5 and all four stopped
  processes were resumed; their running states and settings were verified.
- A wider stability gate was then applied: instead of five readings, the retry
  loop was run with twenty samples 30 seconds apart, and it would start the
  anchored pass only once the sorted root list had been byte-identical five
  times in a row. The 17:53 attempt reached an identical streak of five at
  18:03:07 UTC (samples 15–19, all 634 roots) and started the pass
  immediately. The attempt log for every earlier attempt is preserved; the
  failed attempts recorded their readings rather than being discarded.
- **The latest completed full anchored pass (2026-10-29 18:03:07–18:06:08
  UTC) recorded equal within-pass readings, not the original pre-sweep
  baseline.** It measured **634 roots before and 634 after**, with a
  byte-identical sorted root list (`roots_unchanged=YES`,
  `root_set_identical=YES`); 30 direct and 604 indirect roots in both
  readings. The store path count was **37,534 before and after**, and the
  reachability closure was **4,604 paths before and after**. The continuous
  5-second series taken across the pass recorded 30 samples ranging 633–637
  with 15 transitions, and its first and last samples were 635 and 634. This
  does not satisfy the reported 610-root pre-sweep baseline.
- The scheduled `nix-gc.service` run at 2026-10-29 00:00 GMT reports **0
  store paths deleted, 0.00 MiB freed**. The baseline-bound retry observations
  from 23:08–23:53 UTC had no `nix-gc.service` journal entries; at 23:53 the
  service was inactive and its last start remained 00:00 GMT. The next timer
  was scheduled for 2026-10-30 00:00 GMT. This work invoked no collection
  command, but the earlier scheduled host GC did run, so no host-wide
  no-GC claim is made.
- **Acceptance-item status: not yet met.** The reported original pre-sweep
  count of 610 and the latest completed pass's 634→634 bracket are not equal;
  the 610 reading was not retained as a separate root-list artifact. The
  store path count was 37,535 in the 2026-10-28 22:30 pass, peaked at 37,978
  in a 16:50 UTC spot reading, measured 37,534→37,534 across the 18:03 pass,
  and measured 37,534 again at 21:23–21:24 UTC. New profile generations
  (`profile-68-link` through `profile-75-link`) were registered between 11:23
  and 17:31 UTC on 2026-10-29. The 444-path fall from the 16:50 peak was not
  associated with a collection event in the checked journal window; its
  mechanism was not established.
- A revised bounded retry gate preserves every sorted root-list sample,
  requires five consecutive byte-identical lists, saves that settled list as
  the explicit pre-sweep baseline, and accepts the anchored pass only when
  both its before/after lists and counts match that baseline and no GC service
  start appears during the bounded observation window. The 23:08 UTC attempt
  saved a 646-root baseline but did not obtain a stable list over 20 readings.
  The subsequent 23:19, 23:29, 23:39, and 23:50 checks also failed to reach
  five identical full lists; the 23:50 counts were 649, 649, 651, 650, and
  652. No new anchored pass was started and no replacement baseline-qualified
  measurement is claimed. These observations include transient 2,000–3,000
  root-list bursts; their cause is not established.
- The latest completed pass measured the reachable closure at **4,604 paths
  before and after** and the *GC-root set* byte-identical before and after.
  Its artifact records closure counts rather than a set-level comparison, so
  no final reachable-set identity is claimed.
- Privileged measurements remain incomplete: `sudo -n true` returned
  `sudo: a password is required`. The unreadable descendants under
  `~/.local/share`, `~/.local/state`, container/Trash paths, and `/var/log`
  therefore remain unprivileged subtotals pending an operator-authorized
  `du --bytes` measurement. The scheduled GC at 2026-10-29 00:00 GMT reports
  zero paths deleted, but it did run; the literal host-wide no-GC requirement
  is not claimed.

## Read-only commands used

This list contains only read-only measurement and inspection commands. The
reversible tmux controls used during root-stability observation and their
restoration are described in the Nix section above, not presented as read-only.

```text
du -sx --bytes <path>                         # byte measurements
du -x --max-depth=1 --bytes <dir>             # breakdowns
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'  # file sizes and dates
find <dir> -mindepth 1 -maxdepth 1 -type d    # sandbox enumeration
find <dir> -type d -name .git -prune -exec du -sx --bytes {} +  # git history (find's -prune only stops descent; it removes nothing)
ls -la --time-style=+%F <dir>                 # dates and names
readlink -f /etc/nixos                        # resolve config symlink
nix-store --gc --print-roots | wc -l          # read-only root listing/count
nix-store --gc --print-roots | sort           # full root-list stability comparison
nix-store -qR <root store paths>              # read-only reachability closure query
nix path-info --json --all                    # store path metadata
journalctl --disk-usage                       # journal accounting
journalctl -u nix-daemon.service --since ...  # daemon log inspection
journalctl -u nix-gc.service --since ...      # scheduled GC result (0 paths)
systemctl show nix-gc.service -p ExecMainStartTimestamp  # timer metadata only
systemctl list-timers --all                   # scheduled timer metadata
ps -eo pid,ppid,stat,etime,args               # process inventory
ps -p <observed pid> -o pid,ppid,etimes,stat,comm  # process state only
tmux show-option -g                           # live option read, no changes
# Read /proc/<pid>/maps,comm,status,exe for mapped store-path attribution
ls -1 /nix/var/nix/gcroots/auto               # registered root symlinks
find /nix/var/nix/gcroots/auto -maxdepth 1 -type l -printf '%TY-%Tm-%Td %p -> %l\n'
find /nix/store -mindepth 1 -maxdepth 1        # store path enumeration
```

`nix-store --gc --print-roots` only prints roots; despite the option name, it
does not run collection. No cleanup, apply, or collection command was executed
against surveyed data. The earlier helper's generated-scratch removal is
qualified above. Workspace-test fixtures remained confined to their temporary
test directories.

## Workspace test transcript

The workspace test used the Nix OpenSSL headers and libraries required in this
shell. The following is the complete 44-line suite-result transcript from the
bounded `cargo test --workspace --locked` invocation that closed this sweep;
aggregate: 2,917 passed, 0 failed, 25 ignored.

```text
$ cd /home/dracon/Dev/dracon-utilities
$ OPENSSL_INCLUDE_DIR=/nix/store/cqkz127l4l98xgp7p6y1j2cqvyx0w3jl-openssl-3.6.2-dev/include \
  OPENSSL_LIB_DIR=/nix/store/by19gg2pqp2b1gqphdf6bwk2g3hgak79-openssl-3.6.2/lib \
  timeout 2400 cargo test --workspace --locked > /home/dracon/.cache/space-hygiene/workspace-test-final.log 2>&1
$ grep '^test result:' /home/dracon/.cache/space-hygiene/workspace-test-final.log
 test result: ok. 87 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 16.65s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.58s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.36s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s
 test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.29s
 test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.37s
 test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.27s
 test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.32s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 27.15s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.88s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.37s
 test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.06s
 test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.55s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s
 test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.78s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.59s
 test result: ok. 38 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 1.35s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
 test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.66s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s
 test result: ok. 144 passed; 0 failed; 13 ignored; 0 measured; 0 filtered out; finished in 108.39s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.01s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.01s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.01s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1532 filtered out; finished in 0.00s
 test result: ok. 1530 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 624.97s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.74s
 test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.49s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
 test result: ok. 14 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 10.14s
 test result: ok. 712 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 20.82s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.50s
 test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s
 test result: ok. 248 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 33.47s
 test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 28.05s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 48.00s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.12s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
WORKSPACE_TEST_EXIT=0
```
