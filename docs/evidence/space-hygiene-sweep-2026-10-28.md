# Space-hygiene sweep — beyond the disk audit (2026-10-28)

Report-only. This work cleaned or deleted no user/system data and executed no
garbage-collection command. The workspace test suite used isolated temporary
fixtures. The Nix store was sized and compared by reachability only.

Primary measurements below are from 2026-10-28 (roughly 15:53–16:05 UTC). Three
read-only full anchored passes were run at 20:17–20:29, 22:30–22:42, and
23:24–23:37 UTC; focused sandbox resampling and root-stability checks continued
through 23:44. Live state changed during every window, so byte values are exact
`du --bytes` readings for their stated pass, not an atomic filesystem snapshot. Area,
agent-tool and off-limits tables use the latest full pass (23:24–23:37 UTC);
all candidate-ranking tables are individually pass-labeled. The sandbox
sub-decomposition tables are separate and are not new global rankings.

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
| `~/.local/share` | 27,611,876,629 | needs-operator-call | Unprivileged subtotal; container and Trash descendants denied |
| `~/.local/state` | 29,059,432,587 | needs-operator-call | Unprivileged subtotal; protected descendants denied |
| `~/.local/bin` | 3,087,112,036 | keep | User-installed tools |
| `/nix/store` | 48,292,944,394 | keep | Latest anchored pass; 8,366,644,248 estimated unreachable output bytes from the primary pass; see Nix section |
| Agent-tool state (six named tools) | 31,295,594,876 | keep | Exact sum of the six roots below; overlaps `~/.local/share` |
| `~/chat` | 7,321,716,379 | needs-operator-call | Agent conversation/session data |
| `/var/log` | 613,302,528 | needs-operator-call | Unprivileged subtotal; `/var/log/private` and `/var/log/gdm` denied |
| `/var/log/journal` | 602,406,912 | clean-with-approval | `du` size from the latest anchored pass; a separate `journalctl --disk-usage` reading at approximately 21:05 UTC reported 475.7M |
| `~/.local/share/containers` | 243,857,603 | needs-operator-call | Unprivileged subtotal; `containers/storage` separately measured 243,759,299 bytes |
| `~/Backups` | 23,781,110,001 | keep | Backup tree; off-limits to cleaning |
| `~/dracon/backups` | 19,411,474,935 | keep | Backup bundles; off-limits to cleaning |

### Agent-tool state inventory

| Tool | Measured path(s) | Bytes | Verdict |
|---|---|---:|---|
| Codex | `~/.codex` | 4,258,570,597 | keep |
| Hermes | `~/.hermes` | 2,387,521,841 | keep |
| OpenCode | `~/.local/share/opencode` + `~/.opencode` | 13,332,068,891 | keep |
| Pi | `~/.pi` | 6,074,002,731 | keep |
| Zcode | `~/.zcode` | 2,826,633,061 | keep |
| Kilocode | `~/.kilocode` | 2,416,797,755 | keep |
| **Total** | Six named tools, no shared roots | **31,295,594,876** | **keep** |

The OpenCode total combines 12,429,660,938 bytes at `~/.local/share/opencode`
and 902,407,953 bytes at `~/.opencode`. The separate `mimocode` directory below
is an additional candidate, not a substitute for any of the six named tools.

## Findings, largest actionable candidates first

Every row has one verdict from `{keep, clean-with-approval, needs-operator-call}`.
The first three rows are the top three actionable findings by bytes in the
first sweep report (15:53–16:05 UTC), ordered by their exact `du --bytes`
readings: **10,932,233,354 / 6,983,789,017 / 5,946,756,827 bytes**. Rows 1, 3
and 5 carry the first report's readings; row 12 was re-measured in the
20:17 UTC resample. The remaining primary-pass readings are retained as
historical context; subsequent full-pass rankings follow below.

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

### Latest full anchored pass (23:24–23:37 UTC)

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

The pnpm store is content-addressable and any pruning requires approval. The
OpenCode, Muse, mimocode, Codex, and chat paths contain session or restore data;
no retention period was assumed. `rtk/history.db` was actively written during
the measurements. Container storage and Trash have unreadable descendants, so
they need an operator review rather than a cleanup recommendation.

## Off-limits: sized, never flagged for cleaning

| Path | Size | Verdict |
|---|---:|---|
| `~/Videos` | 30,203,671,682 bytes | keep |
| `~/Pictures` | 44,300,634 bytes | keep |
| Git history in 27 `.git` directories under `~/Dev` | 63,767,372,326 bytes | keep |
| `~/Backups` | 23,781,110,001 bytes | keep |
| `~/dracon/backups` | 19,411,474,935 bytes | keep |
| `~/.config` | 16,785,212,713 bytes | keep |
| `~/.dracon` | 4,355,960,634 bytes | keep |
| NixOS configuration target `~/.dracon/nixos` (`/etc/nixos` symlink) | 297,401 bytes | keep |

These are exact measured byte values from the latest anchored pass, except that
the Git-history row is the sum of `du --bytes` over the 27 discovered `.git`
directories; it excludes working-tree files. Media, Git history, backup
bundles, and live configuration all carry `keep` verdicts only; none of them is
flagged for cleaning.

## Nix store: sized only

- Primary-pass measured store size: **48,292,600,416 bytes**. The second full
  pass measured **48,739,098,826 bytes** at 22:30 UTC; the latest full pass
  measured **48,292,944,394 bytes** at 23:24 UTC.
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
- The latest full pass (23:24:56–23:37:03 UTC) measured **4,603 reachable
  paths before and after, with an identical reachable path set**. Root count
  changed **635 → 632** (605 → 602 indirect roots; 30 direct roots in both
  readings), with three entries added (`wl-clipboard-2.3.0`, `xdg-utils-1.2.1`,
  `xgcc-14.3.0-libgcc`) and six removed (`kcolorpicker-0.3.1`,
  `libkdcraw-25.08.3`, `gwenview-25.08.3`, `kimageannotator-0.7.2`,
  `cfitsio-4.6.3`, `exiv2-0.28.8`). The store-entry count also changed
  **37,535 → 37,532** during this pass; the cause of that inventory change was
  not established.
- The earlier anchored pass (20:17–20:29 UTC) read 637 → 633 roots with the
  same 4,606-path reachable set. Read-only spot samples on 2026-10-28 ranged
  615–739 roots from 21:13–22:29 UTC; a transient burst ranged 2,616–3,069 at
  22:08–22:12 UTC. Samples at 23:08–23:10 were 617, 616, 617, 616, 617, with
  no identical consecutive full lists. During that interval the
  `xgcc-14.3.0-libgcc` indirect root was mapped by a live `cargo`/`rustc`
  process; this work did not start or stop it. It had exited by 23:11, but
  subsequent lists remained unstable. A five-list stability check at
  23:21–23:24 finally observed five identical lists at 635 roots, so the full
  pass was run; the count nevertheless changed to 632 by the end. The pre-sweep
  reading was 610 roots, followed by 612 and 614/615. Thus even the specified
  precheck did not guarantee an unchanged count across the full measurement.
  Ten additional list samples at 23:39–23:44 varied 630–647
  (633, 630, 631, 637, 639, 631, 632, 646, 647, 645); no consecutive full
  lists were identical, so another anchored pass was not started.
- This work executed no garbage-collection command. At 23:37 UTC the scheduled
  unit `nix-gc.service` still showed its last start at 2026-10-28 00:00:00 GMT
  and next run at 2026-10-29 00:00 GMT; the inspected nix-daemon journal window
  contained no GC/collection text. However, because the store-entry count
  changed by three during the latest pass and its cause was not established,
  this evidence does not prove that no external store mutation or collection
  occurred.
- The objective's unchanged root-count acceptance item is therefore **not met**
  and is recorded as a contract deviation requiring operator sign-off. The
  identical reachable path sets do not substitute for that acceptance item;
  the latest pass also lacks an unchanged store-entry count.

## Read-only commands used

```text
du -sx --bytes <path>                         # byte measurements
du -x --max-depth=1 --bytes <dir>             # breakdowns
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'  # file sizes and dates
find <dir> -mindepth 1 -maxdepth 1 -type d    # sandbox enumeration
ls -la --time-style=+%F <dir>                 # dates and names
readlink -f /etc/nixos                        # resolve config symlink
nix-store --gc --print-roots | wc -l          # read-only root listing/count
nix-store --gc --print-roots | sort           # full root-list stability comparison
nix-store -qR <root store paths>              # read-only reachability closure query
nix path-info --json --all                    # store path metadata
journalctl --disk-usage                       # journal accounting
journalctl -u nix-daemon.service --since ...  # daemon log inspection
systemctl show nix-gc.service -p ExecMainStartTimestamp  # timer metadata only
systemctl list-timers --all                   # scheduled timer metadata
ps -p <observed pid> -o pid,ppid,etimes,stat,comm  # process state only
# Read /proc/<pid>/maps,comm,status,exe for mapped store-path attribution
ls -1 /nix/var/nix/gcroots/auto               # registered root symlinks
find /nix/store -mindepth 1 -maxdepth 1        # store path enumeration
```

`nix-store --gc --print-roots` only prints roots; despite the option name, it
does not run collection. No cleanup, apply, delete, or collection command was
executed by this sweep. Workspace-test fixtures remained confined to their
temporary test directories.

## Workspace test transcript

The workspace test used the Nix OpenSSL headers and libraries required in this
shell. The following is the complete 44-line suite-result transcript from the
bounded `cargo test` invocation; aggregate: 2,897 passed, 0 failed, 25 ignored.

```text
$ cd /home/dracon/Dev/dracon-utilities
$ OPENSSL_INCLUDE_DIR=/nix/store/cqkz127l4l98xgp7p6y1j2cqvyx0w3jl-openssl-3.6.2-dev/include \
  OPENSSL_LIB_DIR=/nix/store/by19gg2pqp2b1gqphdf6bwk2g3hgak79-openssl-3.6.2/lib \
  timeout 1800 cargo test --workspace --locked > /tmp/space-hygiene-workspace-test-recheck.log 2>&1
$ grep '^test result:' /tmp/space-hygiene-workspace-test-recheck.log
 test result: ok. 83 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 19.88s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
 test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 15.62s
 test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.05s
 test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
 test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.73s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 52.50s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 13.38s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.55s
 test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.78s
 test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.42s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
 test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.65s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.10s
 test result: ok. 38 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 8.45s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
 test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.03s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s
 test result: ok. 144 passed; 0 failed; 13 ignored; 0 measured; 0 filtered out; finished in 129.37s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.01s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.00s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1521 filtered out; finished in 0.00s
 test result: ok. 1519 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 707.37s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.67s
 test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.13s
 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.41s
 test result: ok. 14 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 15.10s
 test result: ok. 712 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 36.16s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
 test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s
 test result: ok. 245 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 125.19s
 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 40.32s
 test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 80.67s
 test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.61s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
WORKSPACE_TEST_EXIT=0
```
