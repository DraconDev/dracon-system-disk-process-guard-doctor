# Gate-log disk leak — root-cause fix and reclamation (2026-10-31)

The pre-push gate log root was leaking full source snapshots and had reached
**41.6 GB** (38.8 GB of it orphaned snapshots). This fixes the two mechanisms
that caused it and reclaims the space. Free space on `/` went from **86.6 GB to
145.2 GB** (+58.6 GB) at the post-deletion reading, and stands at **139.3 GB**
(+52.7 GB vs the pre-work baseline) at the final reading — the higher number is
the moment right after the explicit deletions, the lower one is after several
hours of other work on the host; the contract is measured against the final
reading. The daemon ran throughout.

Read-only measurements only, except for the deletions listed in
[Reclaimed](#reclaimed). No daemon stop or pause. The epoch-spool reaper was not
touched.

## Root cause

Two independent defects had to line up, and both are now fixed.

**1. The gauntlet never finished its cleanup.** `run-push-gates.mjs` removes its
multi-gigabyte source snapshot in a `finally` block and again in its SIGTERM
handler (`run-push-gates.mjs:54,370` before this change). But when a push
exceeds the daemon's staging deadline, dracon-sync sends SIGTERM, waits **2
seconds**, then SIGKILLs the process group (`dracon-sync/src/git/ops.rs:34-69`),
and `GroupKillGuard::drop` sends SIGKILL again. The hook spends that window
blocked inside `execFileSync` running a gate, so its signal handler never runs.
The snapshot survived, and `manifest.json` still said `"status": "running"` with
a dead `ownerPid`. Nothing ever reaped those directories.

**2. A hook that cannot finish in the deadline it is given.** The gate runs a
full copy, install and 4-gate pass per *distinct pushed revision*. The daemon's
journal records the backlog at **91 commits ahead** on 2026-10-28 12:31:23 and
again on 2026-10-29 09:59:51, each time scaling its push timeout 900s → 5400s in
response; a push attempt therefore had to validate a backlog far larger than a
normal one. 458 attempts are on record: 314 passed, 127 failed, 9 `running` with
dead owners, 8 cancelled. The nine `running` ones are exactly the leak
population — a pass that was killed, not a pass that failed.

The push failure is *why* the leak kept refilling: every retry allocated a fresh
snapshot, failed the deadline, and left it behind. Diagnosing it is in scope;
fixing it is not.

## What changed

| Change | File | Effect |
|---|---|---|
| Dead-owner reap sweep | `scripts/git-hooks/attempt-reaper.mjs` (new) | Deletes `snapshot-*`, `toolchains` and `source.tar` from attempts whose owner is provably gone and which are older than a 900s floor; keeps the gate logs; drops the whole attempt only past a 7-day TTL |
| Reap on attempt start | `scripts/git-hooks/run-push-gates.mjs` | Runs the sweep before allocating a new attempt, so the root self-heals instead of needing a cron job |
| SIGTERM grace 2s → 15s | `dracon-sync/src/git/ops.rs` | Best-effort: lets a hook that is *between* gates run its handler and shortens how long a timed-out group outlives its deadline. It cannot rescue a hook blocked mid-gate (see below) — the reap sweep is what actually removes the snapshot |

**Liveness is decided by `/proc/<pid>/cmdline`, not a bare PID probe.** A
recycled PID must not make a dead attempt look permanent — and the first version
of this check was wrong in exactly that way: it matched the substring
`run-push-gates`, so the *test* process (whose argv mentions
`run-push-gates.test.mjs`) counted as a live owner. The regression test for the
recycled-PID case caught it. The check now matches a whole argv element ending
in `run-push-gates.mjs`.

## Reclaimed

Every row was verified before deletion; the byte count is what `du -sx --bytes`
reported immediately before removal.

| Item | Bytes | Reversibility proof |
|---|---:|---|
| 11 orphaned gate-attempt snapshots, exact sizes (owner dead) | 38,279,344,364 | Every snapshot's revision SHA verified with `git cat-file -e <sha>^{commit}` in this repo *before* deletion — all 11 present in history (per-row proof in `dead-owner-snapshot-inventory.txt`, `in_hist: true`; the SHAs are reproduced in [Snapshot provenance and SHA verification](#snapshot-provenance-and-sha-verification) below). The trees are rebuildable from the recorded revision. **Provenance:** all 11 were removed by live hook invocations at 10:03:30–10:07:25, i.e. by the new sweep running inside real pushes, not by a manual `rm`. Proof in `sweep-timing-proof.txt`: each directory's manifest `started` is days old while its mtime is the removal second. |
| 12th snapshot, `rn6DEt` — **estimated**, see note | ~3,519,333,207 | SHA `778f49770547a8886939dcadc0ec3b87b4021058` read from this attempt's surviving `manifest.json` and verified present with `git cat-file -e`; the snapshot itself was already gone by the time it could be measured. **Provenance:** removed by the explicit sweep invocation at 10:15:27 (`reap-live-run.txt`: `reaped: dracon-pre-push.rn6DEt age=1059s`), not by a live hook. |
| `qualification/canonical-writer-fence-2026-10-27/target` | 12,186,607,915 | Cargo target dir (`CACHEDIR.TAG` present); rebuilt on demand by `cargo build` |
| `protection-repair-2026-10-20/verification-target` | 5,942,225,851 | Cargo target dir (`CACHEDIR.TAG` present); rebuilt on demand |
| `~/.local/share/pnpm/store/v10` | 1,884,541,110 | Installed pnpm is 11.5.1 and `pnpm store path` resolves to `store/v11`; no file under `~/.config` or any `package.json` references `store/v10` |

**Twelve snapshots were removed, not eleven.** The exact measured total is
38,279,344,364 bytes across 11 snapshots whose size was captured before
removal. `rn6DEt` is the twelfth: it was reaped by the 10:15:27 sweep, which
reports its age but not its byte count, and its directory had already been
removed, so **its size is an estimate and is labelled as one everywhere it
appears**. The estimate is the mean of the two exact measurements of the *same
revision* `778f4977…`: `1wnl3J` at 3,496,489,294 and `y4jzyJ` at 3,542,177,120
bytes. A snapshot is a plain `git archive` extraction of one revision plus
whatever build output its gates produced before the kill, so the same revision
lands within a few percent of these two — but the exact figure is not
recoverable and is not claimed. The total across all twelve therefore lies
between **41,775,833,658** and **41,821,521,484** bytes (41.78–41.82 GB), of
which **38,279,344,364 bytes are exact** and only the twelfth row is estimated.

Gate logs were **not** deleted — 444 attempts held only logs and were left
untouched. The snapshots were removed by the new sweep itself, not by a manual
`rm`, which is the point: the mechanism that leaked them now cleans them.

Live-owner attempts were never touched. Each sweep preserved whichever attempt
was live at that moment — the preserved names across the recorded sweeps are
`dracon-pre-push.hKHTkv` (`reap-live-run.txt`), then `dracon-pre-push.eeX4YC`
(`reap-live-run-2.txt`); the pre-work inventory separately records
`live_preserved=0` because at that moment no attempt was mid-flight. One attempt
(`hKHTkv`) was killed mid-run and stayed in place until it passed the 900s
floor, at which point the next sweep removed it — the floor working as designed,
not a miss. The final recheck reports **0** dead-owner snapshots and 1
live-owner snapshot.

## Free space

| Reading | Free bytes on `/` | `df -h` | Gate tree |
|---|---:|---|---:|
| Before (`pre-work-baseline.txt`, 09:45:06Z) | 86,629,908,480 | 81G free, 91% used | 41,620,229,638 |
| After deletion (`post-work-baseline.txt`, 10:21:13Z) | 145,216,069,632 | 136G free, 85% used | 8,655,046,424 |
| Final (`final-state.txt`, 12:40:20Z) | 139,338,043,392 | 130G free, 85% used | 2,322,211,102 |
| **Delta (final vs before)** | **+52,708,134,912 (+49.1 GiB)** | 91% → 85% | **−39.3 GB** |

The free-space figure moves between readings because other work on this host is
allocating and freeing concurrently; the gate tree, which is what this change
controls, fell monotonically: **41.6 GB → 8.7 GB → 2.3 GB**. That continued fall
after the manual deletions is the fix working — live hook invocations keep
reaping as they run, rather than the tree only shrinking when someone
intervenes. At the final reading it holds no `snapshot-*` directories at all.

## Epoch spools — inventoried, not deleted

`dracon-staging-*` directories under `~/Dev/**/.git`:

| Reading | Count | Bytes | Backing |
|---|---:|---:|---|
| Pre-work (~09:45Z) | 698 | 12,895,651,392 | in-run measurement, **not** persisted at the time — see `pre-work-spools.txt` |
| Post-work (10:21Z) | 771 | 15,155,605,713 | `post-work-baseline.txt` |
| Later re-check (11:23Z) | 784 | 15,236,143,720 | `pre-work-spools.txt` |

The pre-work row carries a provenance caveat stated in `pre-work-spools.txt`: the
figure was measured during this goal but not written to a file at the time, so it
has no independent artefact. It is not used by any contract item, and the
growing trend is established by the two persisted readings rather than by it.

**They are growing and this goal deliberately did not stop that.**
`reap_abandoned_spools()` returns `0` and `sweep_epoch_spools(apply = true)`
refuses with `"recovery spools preserved; age-only deletion is unsafe"`
(`dracon-sync/src/git/epoch_spool.rs:18-35`). That is a deliberate 2026-10-26
correction after an over-deletion incident
(`docs/design/epoch-spool-disk-leak-2026-10-26.md`): the same doc's
content-addressed retention design remains open work. Widening deletion
authority there needs its own goal, not a side effect of this one.

## Why the push keeps failing

Evidence with timestamps in `push-failure-evidence.txt`; journal excerpts, not
commands this goal ran.

- **124 commits ahead** of upstream, so each push must validate a large backlog.
- `publication owned by another worker (this pass did not push)` — **3,139
  occurrences** between 2026-10-28 00:45:25 and 2026-10-31 10:27:42. Each says
  the repo was committed but this pass did not attempt the push.
- `local pre-push hook failed` / `Mirror Degraded: 3 consecutive push failures`
  from 2026-10-28 00:16 onward; stuck-push ledger entries decode to
  `2026-10-27T23:20:44Z` and `2026-10-28T10:13:42Z`.
- `ordinary staging absolute deadline expired; candidate retained`
  (e.g. 2026-10-30 14:30:38) — the exact event that leaks a snapshot.
- Of the **127** failed attempts (of 458 total: 314 passed, 127 failed, 9
  `running`, 8 cancelled), the recorded errors are dominated by `dependency
  workspace packages/extension-forms failed to build (exit 1)` and `snapshot
  dependency installation exited 1`, i.e. the build, not the network.

Interpretation: the backing-up commit stream outruns the gate's per-revision
cost, so passes are killed by the staging deadline rather than finishing. The
descriptor `publication owned by another worker` is consistent with a lock held
by a still-running previous attempt.

## Scope and limits

- Not fixed: the push failure itself, which needs its own goal.
- Not fixed: epoch-spool accumulation (deliberately fail-closed).
- All gates green: `cargo test --workspace --locked` exit 0 with **45 suites,
  2,926 passed, 0 failed**, and `cargo clippy --workspace --locked -- -D
  warnings` exit 0. Raw output in `workspace-gates.txt`.
- Only this host's `browser-extensions-shared` log root is a pre-push gate
  root; the reaper is generic over `DRACON_GATE_LOG_ROOT` and would cover any
  other repo that adopts the hook.
