# Space-hygiene sweep — beyond the disk audit (2026-10-28)

Report-only. No user/system data was cleaned or deleted, and no host garbage
collection was run. The workspace test suite used isolated temporary fixtures.
The Nix store was sized and compared by reachability only. Primary measurements below are from 2026-10-28 (roughly 15:53–16:05 UTC), with
a separate read-only anchored follow-up at 20:17–20:29 UTC. Live state changed
during both windows, so byte values are exact `du --bytes` readings for their
stated pass, not an atomic filesystem snapshot. Follow-up readings are labeled
where they differ from the primary pass.

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
| `~/.local/share` | 27,611,094,805 | needs-operator-call | Follow-up unprivileged subtotal; container and Trash descendants denied |
| `~/.local/state` | 21,092,958,816 | needs-operator-call | Follow-up unprivileged subtotal; protected descendants denied |
| `~/.local/bin` | 3,087,112,036 | keep | Follow-up size; user-installed tools |
| `/nix/store` | 48,292,600,416 | keep | Primary-pass size; 8,366,644,248 estimated unreachable output bytes. Follow-up size: 49,080,070,864; see Nix section |
| Agent-tool state (six named tools) | 31,233,361,770 | keep | Follow-up sum of the six roots below; overlaps `~/.local/share` |
| `~/chat` | 7,321,716,379 | needs-operator-call | Agent conversation/session data |
| `/var/log` | 571,318,784 | needs-operator-call | Follow-up unprivileged subtotal; `/var/log/private` and `/var/log/gdm` denied |
| `/var/log/journal` | 560,463,872 | clean-with-approval | Follow-up `du` size; separate later `journalctl --disk-usage` reading was 475.7M |
| `~/.local/share/containers` | 243,857,603 | needs-operator-call | Follow-up unprivileged subtotal; `containers/storage` separately measured 243,759,299 bytes |
| `~/Backups` | 23,781,110,001 | keep | Backup tree; off-limits to cleaning |
| `~/dracon/backups` | 19,411,474,935 | keep | Backup bundles; off-limits to cleaning |

### Agent-tool state inventory

| Tool | Measured path(s) | Bytes | Verdict |
|---|---|---:|---|
| Codex | `~/.codex` | 4,259,339,796 | keep |
| Hermes | `~/.hermes` | 2,387,521,841 | keep |
| OpenCode | `~/.local/share/opencode` + `~/.opencode` | 13,332,068,891 | keep |
| Pi | `~/.pi` | 6,014,958,664 | keep |
| Zcode | `~/.zcode` | 2,826,633,061 | keep |
| Kilocode | `~/.kilocode` | 2,412,839,517 | keep |
| **Total** | Six named tools, no shared roots | **31,233,361,770** | **keep** |

The OpenCode total combines 12,429,660,938 bytes at `~/.local/share/opencode`
and 902,407,953 bytes at `~/.opencode`. The separate `mimocode` directory below
is an additional candidate, not a substitute for any of the six named tools.

## Findings, largest actionable candidates first

Every row has one verdict from `{keep, clean-with-approval, needs-operator-call}`.
The first three rows are the top three actionable findings by bytes in the
primary pass, ordered by their exact `du --bytes` readings. Later live resamples
are identified separately and do not rewrite that ranking.

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
| 12 | `/var/log/journal` | 560,463,872 | clean-with-approval |
| 13 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 14 | `~/.local/share/containers/storage` | 243,759,299 | needs-operator-call |
| 15 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

The primary top-three byte values are the sums/readings from `du -sx --bytes`;
the four original sandbox paths were no longer present at their sampled paths
in the later follow-up, and this work did not delete them. In the 20:17–20:29
anchored follow-up, a new census found 65 current-day sandbox directories
(total 8,991,355,495 bytes) and 112 older directories (57,585,810 bytes). These
changing sandbox measurements are evidence to review, not a claim that live
copies are stale or safe to remove. No cleanup was attempted.

The pnpm store is content-addressable and any pruning requires approval. The
OpenCode, Muse, mimocode, Codex, and chat paths contain session or restore data;
no retention period was assumed. `rtk/history.db` was actively written during
the measurements. Container storage and Trash have unreadable descendants, so
they need an operator review rather than a cleanup recommendation.

## Off-limits: sized, never flagged for cleaning

| Path | Size | Verdict |
|---|---:|---|
| `~/Videos` | 30,164,030,821 bytes | keep |
| `~/Pictures` | 43,059,919 bytes | keep |
| Git history in 27 `.git` directories under `~/Dev` | 63,458,042,388 bytes | keep |
| `~/Backups` | 23,781,110,001 bytes | keep |
| `~/dracon/backups` | 19,411,474,935 bytes | keep |
| `~/.config` | 16,816,247,909 bytes | keep |
| `~/.dracon` | 4,293,937,793 bytes | keep |
| NixOS configuration target `~/.dracon/nixos` (`/etc/nixos` symlink) | 297,401 bytes | keep |

These are exact measured byte values except that the Git-history row is the
sum of `du --bytes` over the 27 discovered `.git` directories; it excludes
working-tree files. Media, Git history, backup bundles, and live configuration
all have `keep` verdicts and no `clean-with-approval` verdict.

## Nix store: sized only

- Primary-pass measured store size: **48,292,600,416 bytes**. The anchored
  follow-up measured **49,080,070,864 bytes** at 20:17 UTC.
- The primary-pass reachability calculation found 4,604 reachable paths and
  32,931 paths outside those closures. Of the latter, 31,515 were derivations;
  1,416 were outputs totaling **8,366,644,248 bytes** (7.79 GiB). This is a
  read-only unreachable-output estimate from the primary pass, not a prediction
  that every path will necessarily be reclaimed.
- The anchored follow-up at 20:17–20:29 UTC measured 4,606 reachable paths
  before and after, while the store entry count moved 37,540 → 37,979. The
  reachable path set was identical, but the GC-root count moved **637 → 633**
  and the root set changed. Earlier observations recorded the pre-sweep count
  as 610, followed by 612 and 614/615; an intermediate anchored read was
  636 → 637. These changing live-host readings do not satisfy the required
  unchanged-root check.
- This work executed no garbage-collection command. The objective's unchanged
  root-count acceptance item is **not met** and is recorded as a contract
  deviation requiring operator sign-off. The unchanged reachable set is
  supplementary evidence only; it is not substituted for that acceptance item.
  Nothing here establishes that host activity caused by another process did or
  did not run a collection.

## Read-only commands used

```text
du -sx --bytes <path>                         # byte measurements
du -x --max-depth=1 --bytes <dir>             # breakdowns
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'  # file sizes and dates
find <dir> -mindepth 1 -maxdepth 1 -type d    # sandbox enumeration
ls -la --time-style=+%F <dir>                 # dates and names
readlink -f /etc/nixos                        # resolve config symlink
nix-store --gc --print-roots | wc -l          # read-only root listing/count
nix-store -qR <root store paths>              # read-only reachability closure query
nix path-info --json --all                    # store path metadata
journalctl --disk-usage                       # journal accounting
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
