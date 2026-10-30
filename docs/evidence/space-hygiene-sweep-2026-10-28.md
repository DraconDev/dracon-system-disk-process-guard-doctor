# Space-hygiene sweep — beyond the disk audit (2026-10-28)

Report-only for surveyed data. This survey invoked no cleanup or garbage-
collection command. However, the scheduled system `nix-gc.service` ran twice
while the work was active: the 2026-10-29 run deleted 0 paths, and the
2026-10-30 run deleted 368 store paths and freed 2510.01 MiB. This external
collection means the literal host-wide no-GC/no-deletion condition was not
met; no non-Nix candidate was intentionally cleaned by this survey. Earlier
anchored-measurement helpers removed only generated `/tmp` scratch files, not
surveyed candidates. Workspace tests used isolated temporary fixtures.

Primary measurements began on 2026-10-28. Five historical full anchored passes ran
through 2026-10-29 18:03–18:06 UTC. On 2026-10-30, the 15:11 complete pass
failed its root bracket (656→636), and the 15:28 attempt timed out before an
after-count. A later complete pass at 19:20 also failed (640→636). The fresh
post-GC pass at 19:40:57–19:42:18 UTC preserved a 609-root pre-sweep list and
matched it before and after, with byte-identical sorted lists; its bounded
journal/service check found no GC. This is a new post-GC baseline, not a
reconstruction of the earlier unpreserved 610-root observation. The area,
agent-tool and off-limits tables now use the 19:40 pass, except the container
storage row, which uses the later `podman unshare du` reading. `~/.local/share`,
`~/.local/state`, and `/var/log` remain unprivileged subtotals because exact
host-root output is not retained here. Candidate top-three findings use the
19:40 full pass. Live state changed during every window, so sizes are exact
readings for their stated pass, not an atomic filesystem snapshot. Historical
full-pass tables are retained separately.

This supplements `leftover-audit-2026-10-27.md` and
`cleaner-coverage-gaps-2026-10-27.md`. Area rows intentionally overlap: for
example, `~/.local/share` includes OpenCode and container storage, and the
agent-tool total includes OpenCode. Do not add overlapping rows together.

Unprivileged `du` reported denied descendants in `~/.local/share`,
`~/.local/state`, Trash, and `/var/log`. Read-only `podman unshare du` could
read the UID-mapped container tree (3,461,884,035 bytes, no permission errors),
but earlier runs could not read the root-owned Trash backup, five state
handover backups, `/var/log/private`, or `/var/log/gdm`. The operator later
reported running a host-root `du`, but its exact terminal output is not present
in the retained evidence; no privileged totals are inferred or substituted.
`sudo -n true` still reports that a password is required in this session, so
complete privileged totals remain an operator follow-up. Table subtotals are
labeled rather than presented as full totals.

## Swept areas

| Area | Bytes | Verdict | Notes |
|---|---:|---|---|
| `~/.local/share` | 27,622,929,585 | needs-operator-call | Unprivileged subtotal from the complete 19:40 pass; earlier `podman unshare du` subtotal was 30,833,970,932 bytes and still denied one root-owned Trash subtree |
| `~/.local/state` | 50,083,125,306 | needs-operator-call | Unprivileged subtotal from the complete 19:40 pass; five root-owned handover backups remain unreadable in this session |
| `~/.local/bin` | 3,090,902,012 | keep | User-installed tools; `du` in the complete 19:40 pass |
| `/nix/store` | 48,312,514,794 | keep | `du` in the complete 19:40 post-GC pass; preserved root bracket 609→609 and no GC in its window. 8,366,644,248 bytes remains a historical pre-GC unreachable-output estimate; see Nix section |
| Agent-tool state (six named tools) | 32,175,371,407 | keep | Exact sum of the six roots measured in the complete 19:40 pass; overlaps `~/.local/share` |
| `~/chat` | 7,321,716,379 | needs-operator-call | Agent conversation/session data; `du` in the complete 19:40 pass |
| `/var/log` | 606,780,928 | needs-operator-call | Unprivileged subtotal from the complete 19:40 pass; `podman unshare du` previously read 589,901,568 but denied `/var/log/private` and `/var/log/gdm` |
| `/var/log/journal` | 595,591,168 | clean-with-approval | `du` in the complete 19:40 pass; separate `journalctl --disk-usage` reading was 475.7M at approximately 21:05 UTC |
| `~/.local/share/containers` | 3,461,884,035 | needs-operator-call | `podman unshare du` at 09:11; all descendants readable in that user namespace (0 permission errors) |
| `~/Backups` | 26,473,384,957 | keep | Backup tree; off-limits to cleaning |
| `~/dracon/backups` | 19,411,474,935 | keep | Backup bundles; off-limits to cleaning |

### Agent-tool state inventory

| Tool | Measured path(s) | Bytes | Verdict |
|---|---|---:|---|
| Codex | `~/.codex` | 4,359,100,962 | keep |
| Hermes | `~/.hermes` | 2,387,521,841 | keep |
| OpenCode | `~/.local/share/opencode` + `~/.opencode` | 13,332,394,851 | keep |
| Pi | `~/.pi` | 6,852,922,937 | keep |
| Zcode | `~/.zcode` | 2,826,633,061 | keep |
| Kilocode | `~/.kilocode` | 2,416,797,755 | keep |
| **Total** | Six named tools, no shared roots | **32,175,371,407** | keep |

The OpenCode total combines 12,429,986,898 bytes at `~/.local/share/opencode`
and 902,407,953 bytes at `~/.opencode`. The separate `mimocode` directory below
is an additional candidate, not a substitute for any of the six named tools.

## Container image inventory (read-only, 2026-10-30 09:16 UTC)

| Backend / item | Bytes | Verdict | Notes |
|---|---:|---|---|
| Docker image-layer store | 165,670,938 | keep | Docker `/system/df` API `LayersSize`; 3 images, 0 containers |
| `dg-cleanhost:ubuntu-24.04` | 149,441,316 | keep | Docker image inspect `Size` |
| `alpine:latest` | 8,422,040 | keep | Docker image inspect `Size` |
| `alpine:3.20` | 7,807,582 | keep | Docker image inspect `Size` |
| Docker build cache | 71,261,146 | keep | Docker `/system/df` API; 2 entries |
| Podman image storage | 250,748,103 | keep | `podman system df --format json` `RawSize`; 2 images, 0 containers |
| `~/.local/share/containers/storage` | 3,461,785,731 | needs-operator-call | `podman unshare du`; 0 permission errors, nested within the area row |
| Untagged Podman image `6232b387…` | 80,694,099 | needs-operator-call | Exact image-inspect size; no repo tag |
| `docker.io/library/ubuntu:24.04` (Podman) | 80,702,836 | keep | Exact image-inspect size |

Backend totals are storage accounting and are not added to their per-image
rows. Docker reports two local volumes at 0 bytes; Podman reports one local
volume at 0 bytes. The container-store directory measurement is listed in the
area table, not added to image sizes.

## Findings, largest actionable candidates first

Every ranked candidate row has one verdict from
`{keep, clean-with-approval, needs-operator-call}`; the auxiliary spot-check
and decomposition tables carry their own verdicts in a verdict column, so no
row in this report asserts a verdict outside that set.
The latest per-path top three are from the complete 19:40 post-GC pass:
**30,673,976,941 / 12,186,624,107 / 6,983,789,017 bytes** by exact
`du --bytes` readings. This pass satisfied its preserved 609-root baseline
bracket; the figures remain point-in-time readings, not an atomic filesystem
snapshot. Historical full-pass rankings follow.

| # | Path | Bytes | Verdict |
|---|---|---:|---|
| 1 | `~/.local/state/dracon/sync/gates/browser-extensions-shared` | 30,673,976,941 | needs-operator-call |
| 2 | `~/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27` | 12,186,624,107 | needs-operator-call |
| 3 | `~/chat/pi/kimi` | 6,983,789,017 | needs-operator-call |

The original first-sweep ranking (15:53–16:05 UTC) follows as historical
context, with top three **10,932,233,354 / 6,983,789,017 / 5,946,756,827
bytes**. Rows 1, 3 and 5 carry the original readings; row 12 was re-measured
in the 20:17 UTC resample. Later full-pass rankings follow.

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

### Historical full anchored pass (not baseline-qualified; 2026-10-29 18:03–18:06 UTC)

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

#### Historical sandbox decomposition of the 2026-10-29 full pass

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
| `~/Videos` | 30,743,674,509 bytes | keep |
| `~/Pictures` | 67,565,294 bytes | keep |
| Git history in 27 `.git` directories under `~/Dev` | 61,919,612,221 bytes | keep |
| `~/Backups` | 26,473,384,957 bytes | keep |
| `~/dracon/backups` | 19,411,474,935 bytes | keep |
| `~/.config` | 16,910,142,887 bytes | keep |
| `~/.dracon` | 4,117,202,897 bytes | keep |
| NixOS configuration target `~/.dracon/nixos` (`/etc/nixos` symlink) | 297,401 bytes | keep |

Every row is an exact `du --bytes` reading from the complete 2026-10-30
19:40–19:42 UTC pass; the Git-history row sums the 27 discovered `.git`
directories and excludes working-tree files. Media, Git history, backup
bundles, and live configuration all carry `keep` verdicts only; none is
flagged for cleaning.

## Nix store: sized only

- Primary-pass measured store size: **48,292,600,416 bytes**. Later full-pass
  readings were **48,739,098,826 bytes** (2026-10-28 22:30),
  **48,292,944,394 bytes** (23:24), **50,942,398,774 bytes**
  (2026-10-29 00:41), and **49,161,672,136 bytes** (2026-10-29 18:03). The
  2026-10-30 08:53 attempt measured **48,303,274,209 bytes**; the 15:11 pass
  measured **50,983,998,714 bytes** and failed its bracket (656→636). A 19:20
  full pass measured **50,993,583,694 bytes** but also failed (640→636). The
  latest complete pass at 19:40 measured **48,312,514,794 bytes** and passed
  the fresh post-GC bracket at 609→609.
- The primary-pass reachability calculation found 4,604 reachable paths and
  32,931 paths outside those closures. Of the latter, 31,515 were derivations;
  1,416 were outputs totaling **8,366,644,248 bytes** (7.79 GiB). This is a
  read-only unreachable-output estimate from the primary pass, not a prediction
  that every path will necessarily be reclaimed. It predates the 2026-10-30
  scheduled GC and was not recalculated afterward, so it is not a current
  post-GC estimate.
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
  processes were resumed; their running states and settings were verified. A
  fresh read-only check at 09:24 UTC on 2026-10-30 found the default tmux
  server active with `@continuum-save-interval 5`, `/tmp/tpd` running, and 16
  `tmux-persist-daemon` processes. The exact original three daemon PIDs cannot
  be matched amid later process churn, but no paused autosave state is
  observable and no further restoration action was needed.
- A wider stability gate was then applied: instead of five readings, the retry
  loop was run with twenty samples 30 seconds apart, and it would start the
  anchored pass only once the sorted root list had been byte-identical five
  times in a row. The 17:53 attempt reached an identical streak of five at
  18:03:07 UTC (samples 15–19, all 634 roots) and started the pass
  immediately. The attempt log for every earlier attempt is preserved; the
  failed attempts recorded their readings rather than being discarded.
- **The historical full anchored pass (2026-10-29 18:03:07–18:06:08
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
  store paths deleted, 0.00 MiB freed**. The next scheduled run, at
  2026-10-30 00:00 GMT, deleted **368 store paths and freed 2510.01 MiB**.
  Both were system-timer actions, not commands invoked by this survey. No
  host-wide no-GC/no-deletion condition can be claimed. The 19:20 pass had no
  GC event but failed its 640→636 root bracket. The fresh 19:40 pass had no GC
  journal event, the service start remained at 00:00 GMT, and the preserved
  609-root baseline matched both endpoints. At 09:11 UTC the next timer was
  scheduled for 2026-10-31 00:00 GMT; no timer change was made.
- **Fresh bounded-pass gate: met; original sweep baseline remains
  unreconstructed.** The reported original pre-sweep reading of 610 has no
  preserved root-list artifact. The fresh 19:40 post-GC pass instead saved a
  609-root baseline, measured 609 before and after with byte-identical lists,
  held store paths at 37,539, and had no GC service/journal event in its
  bounded window. This satisfies the new bounded-pass check without rewriting
  the historical 610→634 record. The 08:53 completed anchored attempt saved
  an attempt-specific 639-root baseline, then measured 637 roots before and
  640 after. Its full baseline, before, and after lists are preserved in
  `docs/evidence/space-hygiene-root-lists-2026-10-30T085316Z/` (line counts
  baseline/before/after: 639/637/640; SHA-256: `f31e682191deead0431f939a6a586678b20ca6e3a305929c3941539590e1e9fc`,
  `039c698886a60e30f5bf45a4b0f0c7b4d669fc3e315b972b3aebd01ca1dee707`,
  `42a0c38ee82186ab1991dbb9a929b81fbc246a57e04fbf4935e77ea0b1d835d8`).
  The anchored counts do not match the saved baseline, so the attempt did not
  qualify. The store path count was 37,535 in the 2026-10-28 22:30 pass,
  peaked at 37,978 in a 16:50 UTC spot reading, measured 37,534→37,534 across
  the 18:03 pass, and measured 37,534 again at 21:23–21:24 UTC. New profile
  generations (`profile-68-link` through `profile-75-link`) were registered
  between 11:23 and 17:31 UTC on 2026-10-29. The 444-path fall from the
  16:50 peak was not associated with a collection event in the checked
  journal window; its mechanism was not established.
- The 2026-10-29 23:08–23:53 retries used a stricter full-list stability
  check. Their saved baseline and subsequent readings are retained in the
  local attempt log; the 23:08 attempt reached 646 roots but did not establish
  a qualifying stable window, and later counts also varied. Those pre-GC
  attempts produced no replacement baseline-qualified measurement and
  included transient 2,000–3,000-root bursts whose cause is not established.
- The retry gate now preserves an exact sorted root list after its reachability
  warm-up and passes that immutable file directly into the anchored sweep; it
  does not re-capture a later baseline. Acceptance requires the preserved
  baseline count, `roots_before`, and `roots_after` to match, plus
  `roots_unchanged=YES` within the pass and no new `nix-gc.service` start or
  journal event during the bounded window. Root-set identity is diagnostic,
  not an acceptance requirement. Disposable fixtures accepted a matching
  count/no-GC case (including same-count root-set churn) and rejected a count
  mismatch, a GC event, and unavailable journal evidence; outputs and helper
  scripts are preserved under
  `docs/evidence/space-hygiene-baseline-gate-2026-10-30/gate-fixtures/`.
- After the 2026-10-30 scheduled GC, earlier full attempts also failed the
  count gate: the 00:35 UTC run saved 613 then measured 613→610 roots; the
  01:01 UTC run saved 641 then measured 641→616. The 08:53:16 attempt saved
  639 then measured 637→640 roots; its 20-sample series ranged 637–640 with 13
  transitions. It kept the store-path count at 37,536 before and after and
  the reachability closure at 4,605, while recording five root entries added
  and two removed between bracket lists. The GC start remained unchanged, but
  the count mismatch means it was not accepted. Its full lists are retained in
  `docs/evidence/space-hygiene-root-lists-2026-10-30T085316Z/`. The next retry
  after 08:55 did not reach five equal counts and started no pass.
- A subsequent count-only retry ran 2026-10-30 09:27:16–09:43:07 UTC. Its 30
  sorted-root samples ranged from 631 to 3,102, with a maximum equal-count
  streak of two; bursts reached 3,099–3,102 and later 2,674–2,697. It did not
  reach the five-count baseline gate, so it started no anchored pass and ran
  no reachability query. The GC journal had no entries in this interval and
  the service's last start remained 00:00 GMT. Since these bursts occurred in
  a count-only retry, the earlier association between some bursts and
  `-qR` diagnostics is not a universal explanation; the cause of these
  readings remains unknown. The 30-sample count log and retry result are
  preserved in `docs/evidence/space-hygiene-root-retry-2026-10-30T092716Z/`
  (SHA-256 `samples.txt`:
  `1ad532c54249a81604cf9bc5293b69db2d2b0488fe3ce6dd0f8901847a8cb852`;
  `result.txt`:
  `d370a77480e3ed14ffd827fcb0f4c838d7a9da53609bcaa6ecbe6443d5af9c99`).
- The historical 2026-10-29 pass measured the reachable closure at **4,604
  paths before and after** and had a byte-identical GC-root list. The 08:53
  2026-10-30 attempt measured **4,605 reachable paths before and after**; the
  15:11 pass measured **4,608**, and the fresh 19:40 pass measured **4,606
  before and after**. Those newer artifacts preserve counts rather than
  closure path sets, so no reachable-set identity is claimed. The 15:28
  attempt ended before its final closure count.
- Privileged totals remain incomplete in the retained evidence: `sudo -n true`
  returned `sudo: a password is required`. The operator mentioned later
  host-root `du` output, but the exact rows and timestamp are not present in
  the saved artifacts, so those figures are not incorporated. The root-owned
  Trash backup, five state handover backups, `/var/log/private`, and
  `/var/log/gdm` therefore still lack independently reviewable privileged
  sizes. The Oct-30 scheduled GC removed 368 store paths; no host-wide
  no-GC/no-deletion claim is made.

### Earlier baseline-bound rechecks (2026-10-30 15:11–15:43 UTC)

The complete attempt beginning at 15:11:27 captured and reused the exact
15:11:52 pre-sweep root list (656 entries). Its anchored measurement ran
15:11:52–15:28:23 and recorded `roots_before=656`, `roots_after=636`,
`store_paths_before=37983`, `store_paths_after=37987`, and 4,608 warmed reachable
paths. The root-count series contained 110 samples, ranged from 623 to 2,719,
and had 79 transitions. The before/after root lists differ, so this measured
pass is not accepted despite `gc_unchanged=YES`. Its Git-history aggregation
also emitted an AWK parse error, so the 27-repository history size remains the
separate 20:31 measurement rather than a row from this pass. A `printf`-based
aggregator correction passed `bash -n` and a small sum fixture, but has not
been run over the host tree. The evidence bundle is
`docs/evidence/space-hygiene-baseline-gate-2026-10-30/attempt-20261030T151127Z-count-mismatch/`.

A second attempt began at 15:28:24 with a new preserved baseline of 639 and
started its anchored pass at 15:28:49. The 30-minute outer timeout ended before
an after-count or completed measurement record was captured. Its partial
`du` file includes spot readings of the gate tree (29,454,836,584 bytes),
`~/.local/state` (48,863,863,900 bytes), `~/.local/share`
(27,619,892,394 bytes), and `~/.config` (16,876,548,041 bytes). These do not
replace the full-pass table; the attempt is not a full or baseline-qualified
pass. The later read-only
check found no `nix-gc.service` journal events since 15:28:24 and the service
start timestamp still read 00:00 GMT. Partial artifacts and that after-check
are under
`docs/evidence/space-hygiene-baseline-gate-2026-10-30/attempt-20261030T152824Z-incomplete/`.
The 15:11–15:43 journal window also had no GC entries. Those attempts are
non-qualifying historical evidence; the new endpoint-only pass below provides
a later post-GC bracket.

The retry gate's disposable tests are preserved under
`docs/evidence/space-hygiene-baseline-gate-2026-10-30/gate-fixtures/` with the
helper scripts and mock dependencies. They exercise count match/mismatch,
GC-event rejection, journal-unavailable fail-closed behavior, and the
contract's count-only acceptance when identities churn at a constant count.
The added endpoint-only fixture verifies that the anchor still rejects a
changed after-count when concurrent root polling is disabled.

### Fresh endpoint-only baseline-bound pass (2026-10-30 19:40:41–19:42:18 UTC)

The retry gate preserved the exact sorted root list after reachability warm-up
and before the complete measurement. The anchored pass then used the saved
list as `roots_before`, disabled continuous root polling for the accepted
bracket, and measured `roots_after` once at the end:

- Baseline, `roots_before`, and `roots_after`: **609**; root lists byte-identical.
- Store paths: **37,539 before and after**; reachable closure counts: **4,606
  before and after**.
- Root-list provenance: 30 direct and 579 indirect roots in both endpoints.
- `gc_unchanged=YES`; no `nix-gc.service` journal event, and its last-start
  timestamp stayed at 2026-10-30 00:00 GMT.
- The exact sizes and preserved baseline/before/after lists are in
  `docs/evidence/space-hygiene-baseline-gate-2026-10-30/attempt-20261030T194041Z-endpoint-only/`.

The preceding 19:20 full pass kept store paths at 37,987 and the reachable
count at 4,610, but its sampled series ranged 636–641 and its endpoints were
640→636; no GC event was found. The earlier pass is retained at
`attempt-20261030T192012Z-root-mismatch/`. This comparison motivated disabling
the extra sampler only for the accepted bracket; it does not attribute the
root changes to the sampler or any specific process. The fresh pass is
post-GC and does not erase the historical scheduled collection described
above.

## Read-only commands used

This list contains only read-only measurement and inspection commands. The
reversible tmux controls used during root-stability observation and their
restoration are described in the Nix section above, not presented as read-only.

```text
du -sx --bytes <path>                         # byte measurements
du -x --max-depth=1 --bytes <dir>             # breakdowns
podman unshare du -sx --bytes <path>          # UID-mapped read-only subtotal
podman image inspect --format '{{.Id}}|{{join .RepoTags ","}}|{{.Size}}' <id>
podman system df --format json                # image/backend accounting
podman container ls -a --size                  # container inventory
docker image inspect --format '{{join .RepoTags ","}}|{{.Id}}|{{.Size}}' <id>
docker container ls -a --size                 # container inventory
docker system df                              # human-readable accounting
curl --silent --show-error --unix-socket /var/run/docker.sock http://localhost/system/df
find <dir> -type f -printf '%s %TY-%Tm-%Td %p\n'  # file sizes and dates
find <dir> -mindepth 1 -maxdepth 1 -type d    # sandbox enumeration
find <dir> -type d -name .git -prune -exec du -sx --bytes {} +  # git history (find's -prune only stops descent; it removes nothing)
ls -la --time-style=+%F <dir>                 # dates and names
readlink -f /etc/nixos                        # resolve config symlink
sudo -n true                                  # privilege availability probe (password required)
sudo du -sx --bytes "$HOME/.local/share" "$HOME/.local/share/Trash" "$HOME/.local/state" /var/log /var/log/private /var/log/gdm  # operator-run; exact output not retained
date -Is                                      # observation timestamps
systemctl is-active nix-gc.service            # current service state
nix-store --gc --print-roots | wc -l          # read-only root listing/count
nix-store --gc --print-roots | sort           # full root-list stability comparison
nix-store -qR <root store paths>              # read-only reachability closure query
nix path-info --json --all                    # store path metadata
journalctl --disk-usage                       # journal accounting
journalctl -u nix-daemon.service --since ...  # daemon log inspection
journalctl -u nix-gc.service --since ...      # scheduled GC journal results
systemctl show nix-gc.service -p ExecMainStartTimestamp  # service start timestamp
systemctl list-timers --all  # scheduled timer metadata
timeout 1800s env MAX_ATTEMPTS=3 SPACE_HYGIENE_LOGDIR=/home/dracon/.cache/space-hygiene/attempts/endpoint-only-20261030T1944Z bash /home/dracon/.cache/space-hygiene/retry-anchored-pass.sh  # bounded, read-only; accepted bracket disables continuous root polling
ps -eo pid,ppid,stat,etime,args               # process inventory
ps -p <observed pid> -o pid,ppid,etimes,stat,comm  # process state only
tmux show-option -g                           # live option read, no changes
# Read /proc/<pid>/maps,comm,status,exe for mapped store-path attribution
ls -1 /nix/var/nix/gcroots/auto               # registered root symlinks
find /nix/var/nix/gcroots/auto -maxdepth 1 -type l -printf '%TY-%Tm-%Td %p -> %l\n'
find /nix/store -mindepth 1 -maxdepth 1        # store path enumeration
wc -l docs/evidence/space-hygiene-root-lists-2026-10-30T085316Z/*.txt
sha256sum docs/evidence/space-hygiene-root-lists-2026-10-30T085316Z/*.txt
wc -l docs/evidence/space-hygiene-root-retry-2026-10-30T092716Z/*.txt
sha256sum docs/evidence/space-hygiene-root-retry-2026-10-30T092716Z/*.txt
```

`nix-store --gc --print-roots` only prints roots; despite the option name, it
does not run collection. This survey invoked no cleanup, apply, or collection
command against surveyed data; the scheduled host GC is separately recorded
above. The earlier helper's generated-scratch removal is qualified above.
Workspace-test fixtures remained confined to their temporary test directories.

## Operator follow-up: privileged path sizes

The fresh Nix baseline-bound pass now qualifies, but the protected-path size
objection remains unresolved in the saved evidence. `sudo` requires an
interactive password in this session, and the exact output of the operator-run
privileged measurement is not retained, so the report cannot cite those
values or the individual sizes of the five state handover backups. Please
repeat the read-only commands below and include their exact output and the
measurement timestamp:

```sh
date -Is
sudo du -sx --bytes \
  "$HOME/.local/share" "$HOME/.local/share/Trash" \
  "$HOME/.local/state" /var/log /var/log/private /var/log/gdm
sudo find "$HOME/.local/state" -type d -iname '*handover*' -print0 \
  | sudo xargs -0 -r du -sx --bytes
```

No cleanup, deletion, service pause, or garbage-collection action is requested.

## Workspace test transcript

The latest bounded `cargo test --workspace --locked` run exited 0 with 44
suite-result lines: **2,923 passed, 0 failed, 25 ignored**. Its full output is
preserved at
`docs/evidence/space-hygiene-baseline-gate-2026-10-30/workspace-test-20261030T1944Z.log`.
The command used the Nix OpenSSL headers and libraries shown below. No Rust
source changed in this report-only work. The 2,917-pass transcript below is an earlier historical run, not the latest
result. The latest command was:

```text
$ cd /home/dracon/Dev/dracon-utilities
$ OPENSSL_INCLUDE_DIR=/nix/store/cqkz127l4l98xgp7p6y1j2cqvyx0w3jl-openssl-3.6.2-dev/include \
  OPENSSL_LIB_DIR=/nix/store/by19gg2pqp2b1gqphdf6bwk2g3hgak79-openssl-3.6.2/lib \
  timeout 2400 cargo test --workspace --locked > /home/dracon/.cache/space-hygiene/workspace-test-20261030T1944Z.log 2>&1
$ echo WORKSPACE_TEST_EXIT=0
$ awk 'test result lines => 44; passed=2923; failed=0; ignored=25'
```

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
