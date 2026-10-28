# Space-hygiene sweep — beyond the disk audit (2026-10-28)

Report-only. This work removed nothing and ran no host garbage collection. The
Nix store was sized and compared by reachability only. Measurements below are
readings from 2026-10-28 (roughly 15:53–16:05 UTC); live state changed during
that window, so each byte value is the exact `du --bytes` output for its
measurement, not an atomic filesystem snapshot.

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
| `~/.local/share` | 27,604,341,781 | needs-operator-call | Unprivileged subtotal; container and Trash descendants denied |
| `~/.local/state` | 22,657,671,121 | needs-operator-call | Unprivileged subtotal; five service-migration backup directories denied |
| `~/.local/bin` | 3,087,112,066 | keep | User-installed tools |
| `/nix/store` | 48,292,600,416 | keep | 8,366,644,248 estimated unreachable output bytes; see Nix section |
| Agent-tool state (six named tools) | 31,135,381,678 | keep | Exact sum of the six roots below; overlaps `~/.local/share` |
| `~/chat` | 7,321,716,379 | needs-operator-call | Agent conversation/session data |
| `/var/log` | 562,811,904 | needs-operator-call | Unprivileged subtotal; `/var/log/private` and `/var/log/gdm` denied |
| `/var/log/journal` | 552,075,264 | clean-with-approval | `journalctl --disk-usage` separately reported 459.7M |
| `~/.local/share/containers` | 243,857,603 | needs-operator-call | Same measured subtotal as `containers/storage`; protected descendants denied |
| `~/Backups` | 23,781,110,001 | keep | Backup tree; off-limits to cleaning |
| `~/dracon/backups` | 19,411,474,935 | keep | Backup bundles; off-limits to cleaning |

### Agent-tool state inventory

| Tool | Measured path(s) | Bytes | Verdict |
|---|---|---:|---|
| Codex | `~/.codex` | 4,259,082,138 | keep |
| Hermes | `~/.hermes` | 2,387,521,841 | keep |
| OpenCode | `~/.local/share/opencode` + `~/.opencode` | 13,332,068,891 | keep |
| Pi | `~/.pi` | 5,913,277,992 | keep |
| Zcode | `~/.zcode` | 2,826,633,061 | keep |
| Kilocode | `~/.kilocode` | 2,416,797,755 | keep |
| **Total** | Six named tools, no shared roots | **31,135,381,678** | **keep** |

The OpenCode total combines 12,429,660,938 bytes at `~/.local/share/opencode`
and 902,407,953 bytes at `~/.opencode`. The separate `mimocode` directory below
is not a substitute for any of the six named tools.

## Findings, largest actionable candidates first

Every row has one verdict from `{keep, clean-with-approval, needs-operator-call}`.
The first three rows are the top three actionable findings by measured bytes.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (48 current-day sandboxes; transient subtotal) | 8,986,622,727 | needs-operator-call |
| 2 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |
| 3 | `~/.local/state/dracon/protection-repair-2026-10-20/verification-target` | 5,942,225,851 | needs-operator-call |
| 4 | `~/.local/share/opencode/snapshot` | 5,898,491,422 | needs-operator-call |
| 5 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 4,167,353,169 | needs-operator-call |
| 6 | `~/.local/share/muse/sessions` | 3,701,817,725 | needs-operator-call |
| 7 | `~/.local/share/pnpm/store` | 2,833,801,709 | clean-with-approval |
| 8 | `~/.local/share/mimocode/snapshot` | 2,645,718,475 | needs-operator-call |
| 9 | `~/.local/share/rtk/history.db` | 1,612,066,816 | needs-operator-call |
| 10 | `~/.local/share/Trash` | 553,895,960 | needs-operator-call |
| 11 | `/var/log/journal` | 552,075,264 | clean-with-approval |
| 12 | `~/.local/share/containers/storage` | 243,857,603 | needs-operator-call |
| 13 | `~/.local/state/dracon/sync/gates/browser-extensions-shared/dracon-pre-push.*` (112 directories older than 2026-10-28) | 57,585,810 | clean-with-approval |

The current-day sandbox subtotal varied across repeated reads while hook
sandboxes were changing. It is therefore a measurement to review, not a claim
that the copies are stale or safe to remove. The earlier 10,932,233,354-byte
four-sandbox set from the first report was no longer present at its sampled
paths during this follow-up; this work did not delete it. A fresh scan found
112 older sandbox directories totaling 57,585,810 bytes and 48 current-day
directories. No cleanup was attempted.

The pnpm store is content-addressable and any pruning requires approval. The
OpenCode, Muse, mimocode, Codex, and chat paths contain session or restore data;
no retention period was assumed. `rtk/history.db` was actively written during
the measurements. Container storage and Trash have unreadable descendants, so
they need an operator review rather than a cleanup recommendation.

## Off-limits: sized, never flagged for cleaning

| Path | Size | Verdict |
|---|---:|---|
| `~/Videos` | 30,159,410,550 bytes | keep |
| `~/Pictures` | 39,687,466 bytes | keep |
| Repository data under `~/Dev` | 365 GiB (prior sweep estimate) | keep |
| `~/Backups` | 23,781,110,001 bytes | keep |
| `~/dracon/backups` | 19,411,474,935 bytes | keep |
| `~/.config` | 16,667,539,386 bytes | keep |
| `~/.dracon` | 4,201,071,920 bytes | keep |
| NixOS configuration target `~/.dracon/nixos` (`/etc/nixos` symlink) | 297,401 bytes | keep |

`~/Music` does not exist and is not listed as a sized path. Backup bundles,
media, repository history, and live configuration have no
`clean-with-approval` verdict.

## Nix store: sized only

- Measured store size: **48,292,600,416 bytes**.
- The sweep's reachability calculation found 4,604 reachable paths and 32,931
  paths outside those closures. Of the latter, 31,515 were derivations; 1,416
  were outputs totaling **8,366,644,248 bytes** (7.79 GiB). This is a
  read-only unreachable-set estimate, not a prediction that every path will
  necessarily be reclaimed.
- No host GC was run by this work. The recorded pre-sweep root count was 610;
  later observations were 612, 614/615, and follow-up samples of 644, 614, and
  639 (the last at 16:05 UTC). The required unchanged-root check is therefore
  **not satisfied**. This is a documented contract deviation requiring
  operator sign-off; the repeated unreachable-set evidence is supplementary
  and is not represented as a substitute for that check.
- The root-count drift occurred while this live host's Nix roots were changing.
  Nothing here establishes that GC caused the drift, and this report does not
  claim that the root-count acceptance item passed.

## Read-only commands used

```text
du -sx --bytes <path>                         # byte measurements
 du -x --max-depth=1 --bytes <dir>            # breakdowns
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'  # file sizes and dates
find <dir> -mindepth 1 -maxdepth 1 -type d    # sandbox enumeration
ls -la --time-style=+%F <dir>                 # dates and names
readlink -f /etc/nixos                       # resolve config symlink
nix-store --gc --print-roots | wc -l          # read-only root listing/count
nix-store -qR <root paths>                    # read-only reachability closures
nix path-info --json --all                    # store path metadata
journalctl --disk-usage                       # journal accounting
```

`nix-store --gc --print-roots` only prints roots; it does not run collection.
No clean/apply/delete or host GC command was executed for this sweep.

## Workspace test transcript

The plain environment lacked OpenSSL include/library locations. This exact
workspace invocation supplied the Nix OpenSSL paths and exited successfully:

```text
$ cd /home/dracon/Dev/dracon-utilities
$ OPENSSL_INCLUDE_DIR=/nix/store/cqkz127l4l98xgp7p6y1j2cqvyx0w3jl-openssl-3.6.2-dev/include \
  OPENSSL_LIB_DIR=/nix/store/by19gg2pqp2b1gqphdf6bwk2g3hgak79-openssl-3.6.2/lib \
  timeout 1800 cargo test --workspace --locked
...
test result: ok. 712 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
...
test result: ok. 245 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
...
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
...
44 test-result records; aggregate 2,897 passed; 0 failed
WORKSPACE_TEST_EXIT=0
```
