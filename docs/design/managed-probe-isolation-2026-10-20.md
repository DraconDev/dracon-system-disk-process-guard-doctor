# Managed-probe isolation: bind verification to its own job generation (2026-10-20)

## The defect the auditor found

`src/protection_probe.rs` verified the managed OOM path by discovering a target
in the shared registry: it scanned every process, took the first sample that
belonged to *any* registered managed job, and biased it with an empty exemption
set. It then called the global `managed_oom::drain(true, &exempt)`, which walks
every `*.debt.json` in the registry and force-drains each one. Retirement was
checked by the mere absence of a debt pathname.

With any unrelated registered workload present, live verification could bias and
restore that workload and could retire its debt. That exceeds the disposable-only
authorization for this repair and could modify a real user's job.

## The correction

Admission, target selection, bias, restoration and retirement are now bound to
one verified disposable generation.

- **Generation chosen before launch.** The probe allocates its own job id
  (`oom_job::launch_command`) instead of discovering a job after the fact. The
  public `managed-job` launcher keeps its existing behavior and now shares that
  constructor.
- **Target identified by the worker's own record.** The disposable worker writes
  its pid, start time and cgroup to `managed-worker.json` in its own probe
  directory. The probe matches that exact incarnation and generation and refuses
  any other process. The sentinel is never a bias target.
- **Bias bound to the admission.** `managed_oom::bias_job` revalidates the
  target's generation and the full admission record under the generation lock
  before any write, and refuses a target outside the expected job.
- **Restoration scoped to the generation.** `managed_oom::drain_job` restores
  only the captured generation. It never enumerates the registry. The global
  `drain` remains for the production recoverer, which legitimately owns every
  registered job.
- **Retirement requires a matching certificate.** `managed_oom::job_retired`
  requires the job to be terminal with a terminal proof AND a retired debt whose
  admission matches the captured one. An absent debt pathname is no longer
  treated as a certificate, and a foreign retired record is an error.
- **Real exemptions applied.** The probe uses the configured
  `process_exempt_names` instead of an empty set, and its disposable worker
  carries a distinct `dracon-probe` process name so it is never confused with
  the guard's own binary.
- **Fresh directory required.** A probe directory that already contains
  managed state, a stop marker or a previous worker record is refused.

## Regression coverage

`src/managed_probe_tests.rs` covers the isolation property in a temporary
registry with no user units, no global HOME and no live processes:

- `scoped_probe_restoration_and_retirement_leave_other_jobs_and_debts_byte_identical`
  publishes the probe's own terminal job, an unrelated active job, an unrelated
  terminal job and an unrelated corrupt debt, then runs the scoped restoration
  and retirement. The unrelated job records and debts must be byte-identical
  afterwards, the corrupt debt must be unread by the scoped path, the
  unrelated job must not gain a retired record, and the caller's own OOM score
  must be unchanged. A repeated call must be idempotent and equally scoped.
- `replaced_probe_admission_or_debt_is_rejected_before_mutation` replaces the
  admission and then the debt of the captured generation and requires the
  scoped path to refuse without changing any record.
- `absent_debt_is_not_a_probe_retirement_certificate` requires a missing or
  foreign retired debt to fail the retirement check.

`protection_probe::tests::managed_target_never_selects_another_registered_job_or_reused_pid`
covers target selection directly: an unrelated registered sample placed FIRST
must not be chosen, a reused pid must not match, a worker in another job's
cgroup must not match, and the sentinel must never be a target.

**Mutation check.** In a disposable copy of the source, the scoped restoration
was replaced with the old global sweep over every debt. The isolation regression
then failed with "probe changed unrelated
22222222-2222-2222-2222-222222222222.json" and the replacement-admission
regression failed as well, because a global sweep no longer rejects a replaced
admission. The mutation was applied only in the disposable copy; raw output is
`revision3-mutation.log`. An earlier attempt mutated a different call path
than the one under test and therefore passed; the mutation was moved onto the
exact entry point the probe calls, and the check is now meaningful.

## Live verification in the deployed service context

The corrected probe was deployed and exercised against the real user manager
with an unrelated registered job deliberately present.

- An unrelated disposable managed job was admitted and left running, with its
  own outstanding lifetime debt.
- The deployed probe's managed phase selected only its own generation:
  baseline 200, biased 250, restored to 200, and the debt retired against the
  terminal certificate. The receipt names the probe's own `job_id` and
  `invocation`.
- The unrelated job's record and debt files were byte-identical before and
  after the probe, its scope stayed loaded and active, its sentinel's
  `oom_score_adj` was unchanged, and it gained no retired record.
- The exercise phase and a real guard restart still pass with the renamed
  worker: quota 500ms measured 43.6% CPU, normal release, stricter and operator
  edits preserved, independent expiry, OOM permission failure retained and
  rehydrated, root and child restored. After a real guard restart the scope
  quota returned to `infinity` with the worker and child alive, an unchanged
  cgroup and restored OOM values.
- The unrelated job stayed byte-identical after BOTH probe phases and after the
  guard restart.

Raw evidence: `revision3-live-probe.log`, `revision3-live-exercise.log`,
`revision3-live-restart-check.log`, `revision3-unrelated-before/after*.log`,
`revision3-cleanup.log`.

## Deployment scope and rollback

The deployed artifact is built from the previously deployed source baseline
plus only the CPU lease lifecycle overlay and this probe-isolation change
(`revision3-release-provenance.log` records the baseline, the overlay files and
their hashes). This deliberately excludes concurrent unrelated disk-cleanup
edits in the shared checkout, so no storage policy changed with this deployment.

Installed and loaded executable SHA-256:
`66152f7f924c50b98a144e88a534de8558467fb304db4a900fbebcf91251a6cb`.
The predecessor is preserved at
`~/.local/bin/dracon-system.backup-2026-10-20-pre-probe-isolation`, next to
the earlier pre-repair and pre-CPU-lifecycle backups, and a durable copy of the
scoped artifact is kept under
`~/.local/state/dracon/protection-repair-2026-10-20/dracon-system`. Only the
guard was restarted; the installed unit file is unchanged.

Rollback still requires reconciling outstanding CPU leases and OOM obligations
with the new binary before restoring a predecessor and restarting only the
guard. The pre-existing namespace/renice limitation and the cooperative
managed-job coverage boundary are unchanged.
