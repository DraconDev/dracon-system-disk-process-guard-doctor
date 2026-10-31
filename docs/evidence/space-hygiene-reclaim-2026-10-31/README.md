# Gate-log disk leak — root-cause fix and reclamation (2026-10-31)

The pre-push gate log root was leaking full source snapshots and had reached
**41.6 GB** (38.28 GB of it orphaned snapshots by exact measurement; ~41.8 GB
counting the twelfth, whose size is an estimate). This fixes the two mechanisms
that caused it and reclaims the space. Free space on `/` went from **86.6 GB to
152.0 GB** at the final reading (**+60.9 GiB**, 91% → 84% used; the intermediate
readings and why they move are in [Free space](#free-space)). The daemon ran
throughout.

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
journal shows the backlog reaching **~3,478 unpushed commits** on 2026-10-28
(`🚨 ALERT: … has 3478 unpushed commits (threshold: 50)`, 02:57:26), so a push
attempt had to validate a backlog orders of magnitude larger than a normal one —
and it retried that backlog on every pass. 458 attempts are on record: 314
passed, 127 failed, 9 `running` with
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
recoverable and is not claimed. The derivation is recorded in
`rn6DEt-estimate.txt`, including the two exact same-revision measurements it is
built from. The total across all twelve therefore lies between
**41,775,833,658** and **41,821,521,484** bytes (41.78–41.82 GB, computed as the
eleven exact bytes plus each same-revision bound), of which **38,279,344,364
bytes are exact** and only the twelfth row is estimated.

Gate logs were **not** deleted — 444 attempts held only logs and were left
untouched. The snapshots were removed by the new sweep itself, not by a manual
`rm`, which is the point: the mechanism that leaked them now cleans them.

Live-owner attempts were never touched. Each sweep preserved whichever attempt
was live at that moment, and that set is a *moving* quantity rather than a fixed
number: an attempt has a live owner exactly while a push is in flight, so the
count rises and falls with push activity and is near zero when nothing is
running. The readings, each with its own timestamp:

| Reading | When | Dead-owner snapshots | Live-owner snapshots | Artefact |
|---|---|---:|---:|---|
| Pre-work inventory | ~09:45Z | 11 | 0 | `dead-owner-snapshot-inventory.txt` (`live_preserved=0`) |
| Explicit sweep | 10:15:27Z | 1 reaped | 1 preserved (`hKHTkv`) | `reap-live-run.txt` |
| Explicit sweep | 11:04:53Z | 0 reaped | 1 preserved (`eeX4YC`) | `reap-live-run-2.txt` |
| Final recheck | 12:40:20Z | **0** | **0** | `final-state-1240.txt` (recovered from commit `b9d4aa3`) |
| Post-verification scan | 14:04:53Z | **0** | 1 (`OTQjdk`) | `final-state-1405.txt` |
| Audit-hour scans | ~13:30Z–15:30Z | **0** | 0–1, rotating (`r90mfQ`, `4Hzjog`, `OTQjdk`, `qIonKX`, `HeYAAq`) | `live-scan-latest.txt` |

The two zero and one readings are all correct at their own timestamps: at
12:40:20 no push was mid-flight, so nothing was live; by 14:04:53 a fresh push
had started and a new snapshot existed under a *different* name (`OTQjdk`).
Every live snapshot observed across the day carried a new name — `hKHTkv`,
`eeX4YC`, `r90mfQ`, `4Hzjog`, `OTQjdk`, `qIonKX` — which is itself the evidence
that these are transient in-flight attempts rather than a fixed set being
carried. **The invariant
that matters is the dead-owner column: it is 0 in every reading after the sweep,
while live-owner snapshots are preserved rather than reaped.** One attempt
(`hKHTkv`) was killed mid-run and stayed in place until it passed the 900s
floor, at which point the next sweep removed it — the floor working as designed,
not a miss.

The 12:40:20Z reading predates a rework that overwrote the single
final-state.txt file that used to hold it (that filename no longer exists), so
it is preserved as `final-state-1240.txt` with the block recovered verbatim from
the commit that first recorded it; the later 14:04:53Z scan is
`final-state-1405.txt`. Each row above cites the file that actually holds its
numbers.

## Snapshot provenance and SHA verification

The full per-row record is `dead-owner-snapshot-inventory.txt` (11 rows of JSON
with `dir`, `sha`, `in_hist`, `snap_bytes`, `started`). Every revision was
confirmed present in this repo's history with `git cat-file -e <sha>^{commit}`
*before* its snapshot was deleted, which is what makes the deletion reversible:
the source is reconstructable from the commit, so only derived bytes were lost.
All twelve SHAs, with the verification result:

| Attempt | Revision SHA | `in_hist` | Snapshot bytes |
|---|---|---|---:|
| `1wnl3J` | `778f49770547a8886939dcadc0ec3b87b4021058` | true | 3,496,489,294 |
| `2oTFcv` | `f406d2e26ae9dc8a75a6883253e4e09f8e15dc6d` | true | 3,790,369,070 |
| `6DbN2k` | `2a19c12dc9ade83d0e46e8cb6759580fcf20c5d4` | true | 2,890,725,999 |
| `EB10kd` | `f3b353f262491b5fd597d8f03136e48073a0dd21` | true | 4,342,883,126 |
| `Km2eJj` | `cf17489b332ac2f2c36cd2c5af0137ef4e4dafd4` | true | 3,853,787,740 |
| `U27Pu7` | `bf3ac20baeb87b6ea37eb6b7dd7ea5f25ce8c9ef` | true | 2,901,649,286 |
| `eyCgyI` | `2ef4137b787230b388a20931d48f1da68bbc17f1` | true | 2,562,504,153 |
| `sBnolJ` | `4097256ad06c49500956a263231ee642484b8dcd` | true | 3,803,490,154 |
| `vPUkJR` | `7c7e6dfdde10b71b8c79698db39d6a3096e653be` | true | 3,601,583,814 |
| `y4jzyJ` | `778f49770547a8886939dcadc0ec3b87b4021058` | true | 3,542,177,120 |
| `yX25xv` | `10671395b5b37308222308f939f20dfed7a20abd` | true | 3,493,684,608 |
| `rn6DEt` | `778f49770547a8886939dcadc0ec3b87b4021058` | true | *estimate 3,519,333,207* |

`rn6DEt` is the only row whose bytes are estimated; its SHA is exact and was
read from its own surviving `manifest.json`. The eleven exact rows sum to
38,279,344,364 bytes.

## Free space

| Reading | Free bytes on `/` | `df -h` | Gate tree | Note |
|---|---:|---|---:|---|
| Before (`pre-work-baseline.txt`, 09:45:06Z) | 86,629,908,480 | 81G free, 91% used | 41,620,229,638 | contract baseline |
| After deletion (`post-work-baseline.txt`, 10:21:13Z) | 145,216,069,632 | 136G free, 85% used | 8,655,046,424 | post-deletion peak |
| Mid (`final-state-1240.txt`, 12:40:20Z) | 139,338,043,392 | 130G free, 85% used | 2,322,211,102 | gate tree at its lowest |
| Quiet-hours dip (`final-state-1405.txt`, 14:04:53Z) | 109,017,239,552 | 102G free, 89% used | 5,957,715,195 | see below |
| **Final (`verification-build-cache.txt`, 14:10:33Z)** | **152,042,856,448** | **142G free, 84% used** | 5,957,715,195 | **contract measurement** |
| **Delta (final vs before)** | **+65,412,947,968 (+60.9 GiB)** | 91% → 84% | **−35.7 GB** | recorded in `verification-build-cache.txt` |

Two movements in this table need explaining, because a bare table would look
inconsistent:

- **The 14:04Z dip to 109.0 GB is real and is not a regression of the fix.**
  Between 12:40Z and 14:04Z this goal ran the full workspace test suite three
  times plus clippy to prove the fix, which grew cargo's incremental-compilation
  cache to **22.9 GB** (`target/debug/incremental`, pure rebuild state that
  cargo regenerates on demand). The daemon meanwhile allocated a fresh live
  snapshot (3.7 GB, `OTQjdk`) because a push was in flight. Neither is leak
  debris: one is this goal's own verification overhead, the other is a
  *live-owner* snapshot doing its job.
- **The final 152.0 GB reading re-claims the build cache, and that is the only
  movement this goal caused in that step.** `target/debug/incremental` is a
  compiler cache — no source, no build output of record, regenerated by the next
  build — so deleting it destroys no evidence and reverts nothing. It is recorded
  separately in `verification-build-cache.txt` with its exact byte count and
  before/after free space, rather than being folded into the deletion table above,
  because it is verification overhead rather than part of the leak-reclamation set.
  Free space *also* rose by 17.8 GiB between the 14:04:53Z reading and the moment
  the cache was deleted, and **that movement is not attributable to this goal**:
  the only commands it ran in that window were read-only measurements. Part of it
  is visible and is the fix working — the gate tree fell from 5,957,715,195 to
  2,325,241,758 bytes (3.63 GB) as the live 3.7 GB snapshot `OTQjdk` finished its
  push and was cleaned up — and the remaining ~15 GiB is concurrent host activity
  (other agents, builds, daemon pushes) that nothing here instrumented. The
  full arithmetic and the unmeasured remainder are recorded in
  `free-space-movements.txt` rather than being presented as one clean cause.

The gate tree is the number this change actually governs, and it fell
**41.6 GB → 8.7 GB → 2.3 GB** after the explicit deletions. It later rose to
5.96 GB because the daemon had a live push in flight holding a 3.7 GB snapshot
plus bun/npm caches — the sweep does not touch either while the push lives. At
the final reading the tree held **zero dead-owner** `snapshot-*` directories;
the only snapshot present belonged to a push that was live at that moment (see
[the live-owner table](#reclaimed)).

Free space keeps moving after the final reading above, because this host is in
continuous use. Four of the five readings in the table clear the 40 GiB bar and
the 14:04:53Z dip does not, so the honest statement is per reading, not a
blanket one:

| Reading | Delta vs the 86,629,908,480 baseline | Clears 40 GiB? |
|---|---:|---|
| 10:21:13Z post-deletion | +58,586,161,152 (+54.6 GiB) | yes |
| 12:40:20Z mid | +52,708,134,912 (+49.1 GiB) | yes |
| 14:04:53Z dip | +22,387,331,072 (+20.85 GiB) | **no** |
| 14:10:33Z final | +65,412,947,968 (+60.9 GiB) | yes |
| 14:35:22Z later recheck (`recheck-late.txt`) | +56,725,884,928 (+52.8 GiB) | yes |

The **14:04:53Z dip does not clear the bar, and it is not a regression of the
fix.** It is caused by this goal's own verification: three full workspace test
runs plus clippy passes grew cargo's incremental cache to 22.9 GB, and the daemon
held a live 3.7 GB snapshot because a push was in flight. Neither is leak debris
— one is the cost of proving the fix, the other is a live attempt the sweep is
correctly forbidden to touch. The leak-specific signal stayed clean throughout:
**zero dead-owner snapshots in every scan after the sweep.** The contract is met
by the final reading, which is measured after that verification overhead is
reclaimed.

## The grace, and what it is sized to

`SIGTERM_GRACE` is now **25s**, sized between two real bounds rather than guessed:

- **Lower bound — the measured cleanup cost.** The 2026-10-31 sweep removed
  eleven multi-gigabyte snapshots over ~235s, i.e. **~21s per removal**. A grace
  at or below 21s cannot be "large enough for a multi-gigabyte tree removal"; the
  old 2s value is precisely why the snapshot survived.
- **Upper bound — the settle contract.** The grace is paid *after* an operation
  has already blown its deadline, so it directly lengthens how long a timed-out
  publication takes to settle.
  `owned_commit_tests::cancelled_or_timed_out_publication_kills_owned_hook_descendants`
  requires a 2s-deadline operation to settle within 30s, which caps the grace at
  ~28s. A first attempt at 60s failed that test — a real contract, not a test to
  loosen — so the value was brought into the window instead of the bound being
  relaxed.
- 25s sits inside it: above the 21s measurement, and 2s + 25s = 27s settles
  inside the 30s bound (measured at 27.70s in the passing run).

Two tests hold it in place, and both are **non-vacuous in the direction that
matters**. Temporarily restoring 15s fails `grace_is_long_enough_for_hook_cleanup`
(24s floor, tied to the ~21s measurement) *and*
`trapping_child_gets_the_documented_grace_before_sigkill`, whose fixture now
spends **21s** — one real snapshot removal — cleaning up, failing with `the trap
handler never ran, so cleanup was pre-empted by SIGKILL`. That is the exact
failure that caused the leak, reproduced by shortening the constant.

Two honest limits remain, and they are why the reap sweep still exists:

- The hook is normally **blocked inside a synchronous `execFileSync`** while a
  gate runs, and no signal handler executes while the thread sits in that call.
  No grace length can rescue a hook killed *mid-gate*; the sweep removes that
  snapshot on the next attempt regardless.
- The grace covers **one** removal's worth of time. An attempt holding several
  `snapshot-<sha>` directories can still exceed it — which is why the sweep, not
  the grace, is what guarantees the disk is reclaimed.

## Epoch spools — inventoried, not deleted

`dracon-staging-*` directories under `~/Dev/**/.git`:

| Reading | Count | Bytes | Backing |
|---|---:|---:|---|
| Pre-work (~09:45Z) | 698 | 12,895,651,392 | in-run measurement, **not** persisted at the time — see `pre-work-spools.txt` || Post-work (10:21Z) | 771 | 15,155,605,713 | `post-work-baseline.txt` |
| Later re-check (11:23Z) | 784 | 15,236,143,720 | `pre-work-spools.txt` |

The pre-work row carries a provenance caveat stated in `pre-work-spools.txt`: the
figure was measured during this goal but not written to a file at the time, so it
has no independent artefact. It is not used by any contract item, and the
growing trend is established by the two persisted readings rather than by it.
Note also that the objective's own parenthetical estimate of "~14 GiB across 698
dirs" is larger than what that reading recorded: 12,895,651,392 bytes = **12.0
GiB** over 698 dirs. The dir count matches; the byte estimate in the objective was
high by ~2 GiB. The measured figure is what this document reports.

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

- **The backlog reached ~3,478 unpushed commits** on 2026-10-28 — the daemon
  logged `🚨 ALERT: … has 3478 unpushed commits (threshold: 50). Something may
  be wrong with push.` at 2026-10-28 02:57:26, and had already gone as far as skipping
  auto-commit (`⏸️ daemon backstop: 1593 unpushed commits pending push >300s`) at
  00:16:24. This is the real scale of the failure that fed the leak: each attempt
  had to validate a backlog orders of magnitude beyond one push.
- As it slowly drained, the daemon logged
  `scaling push timeout 900s → 5400s (N commits ahead)` — e.g. `(3309 commits
  ahead)` at 2026-10-28 00:16:24 and `(3466 commits ahead)` at 01:58:59 while the
  backlog was still growing, then `(91 commits ahead)` at 2026-10-28 12:31:23 and
  again at 2026-10-29 09:59:51 once it had drained. The 91 lines are quoted as
  examples of the late stage, **not** as a peak; the peak was ~3,478.
  (An earlier draft of this file said "124 commits ahead"; no measurement of 124
  exists in the captured evidence and that figure is withdrawn rather than
  restated.) The backlog has since drained: `git rev-list --count
  origin/main..HEAD` is **4** at 2026-10-31 13:35:34Z, and the journal records
  `✅ push recovered for /home/dracon/Dev/browser-extensions-shared` at 11:45:50
  and 13:17:17 — pushes are succeeding now, which is why this is a diagnosis of
  the failure window and not a live outage.
- `publication owned by another worker (this pass did not push)` — **3,139
  occurrences** between 2026-10-28 00:45:25 and 2026-10-31 10:27:42. Each says
  the repo was committed but this pass did not attempt the push.
- `local pre-push hook failed` / `Mirror Degraded: 3 consecutive push failures`
  from 2026-10-28 00:16 onward; stuck-push ledger entries decode to
  `2026-10-27T23:20:44Z` and `2026-10-28T10:13:42Z`.
- `ordinary staging absolute deadline expired; candidate retained`
  (e.g. 2026-10-30 14:34:38) — the exact event that leaks a snapshot.
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
- All gates green: `cargo test --workspace --locked` exit 0 with **44 suites,
  2,926 passed, 0 failed**, and `cargo clippy --workspace --locked -- -D
  warnings` exit 0. Raw output in `workspace-gates.txt`.
- Only this host's `browser-extensions-shared` log root is a pre-push gate
  root; the reaper is generic over `DRACON_GATE_LOG_ROOT` and would cover any
  other repo that adopts the hook.
