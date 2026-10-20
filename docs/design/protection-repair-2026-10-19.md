# Protection repair — implemented and deployed (2026-10-19)

## Confirmed causes

The installed guard has `ProtectControlGroups=yes`; its old limiter directly
wrote `cgroup.procs`. A bounded live journal sample recorded 275 failed CPU cap
attempts in 30 minutes with EROFS. The October 17 incident report documents the
same conflict. Direct cgroup writes must not be enabled in the main guard.

The old placeholder service was also unsafe: `sleep 3600` exiting under the
normal service lifecycle could terminate migrated workloads. A gone root PID
never proves that its descendants exited. User-manager AttachProcessesToUnit
requires a delegated destination, so arbitrary migration/back-migration is not
a transparent repair. A disposable local probe succeeded for the delegated
Konsole scope, while actual tmux-spawn job scopes reported Delegate=no.

OOM restoration errors are EACCES (13), not proof that all descendants require
CAP_SYS_RESOURCE. A disposable same-user 200 -> 250 -> 200 round trip succeeded.
The code also discarded failed descendant obligations after retry escalation,
omitted their provenance from persistence, and initialized recovery/persistence
in the one-shot CLI rather than the real daemon.

## Implemented source changes

- CPU quota leases use the user manager on existing recognized, isolated job
  scopes. They do not migrate processes or stop workload units. Unknown/shared,
  delegated, nested, or currently exempt-containing scopes fail closed.
- Original quota, invocation, generation, boot identity, and intent are durable.
  A separate expiry helper resets quota; expiry never stops the workload.
- Expiry helpers now have manager-level Restart=on-failure, RestartSec=5s,
  StartLimitIntervalSec=0. Lock contention and transient manager failures therefore
  do not consume the only independent recovery attempt. Successful live
  service-context verification is recorded below; the subsequent lifecycle
  failure-injection coverage and scoped redeployment are recorded in
  `cpu-lease-failure-coverage-2026-10-20.md`.
- Lease intent, arming, verified application and absolute expiry are distinct.
  A pre-existing file alone no longer counts as a successfully enforced cap.
  Reconciliation distinguishes unmutated intent, unprotected applied quota,
  active backed leases, and external edits.
- Legacy uncap never sends stop/reset-failed, including when the root is gone
  or moved. Only a positively absent manager unit resolves the legacy debt;
  unknown/occupied units remain recorded for explicit ownership recovery.
- Daemon and guard once share a nonblocking lifetime singleton lock, preventing
  a second owner from recovering/replacing the live owner's ledger.
- Unreadable, truncated, corrupt and unsupported-version ledgers block both
  replacement and new process mutations. Monitoring continues, hydration retries,
  and policy reload cannot erase that failure state. Disabling the guard retains
  the protected state rather than treating it as a successful restoration.
- Versioned process mitigation ledgers carry boot identity. Previous-boot bytes
  are archived and never applied to current matching PIDs/starttimes. Nonempty
  legacy files without boot provenance remain inspectable but read-only until
  provenance is established; they are not silently discarded or auto-applied.
- OOM application opens one proc descriptor before revalidating incarnation,
  captures the original through it, writes durable intent, and applies/verifies
  using that same descriptor. Restoration likewise holds one descriptor and
  does not overwrite a score changed externally from the applied value.
- Pending OOM obligations and pre-existing descendant identities survive retries
  and restart. Current command names, not only saved names, enforce exemptions.
- Inherited-debt provenance is now launch-time, not sampled. `dracon-system
  managed-job -- COMMAND` admits a job into a fresh nondelegated user scope,
  records its baseline and identity durably before the first workload
  instruction, and keeps a single-threaded sentinel alive until a
  workload-terminal certificate is durable. Registered jobs carry a job-lifetime
  OOM debt that is restored by an independently supervised recoverer and retired
  only against that certificate. Unregistered jobs keep the existing in-process
  behaviour and remain explicitly outside this guarantee; they cannot be
  retroactively certified. The launcher establishes its own baseline rather
  than inheriting the caller's score. Full design and the supported-job
  contract are in `docs/design/managed-job-admission-2026-10-19.md`.
- The independent recoverer runs with no capabilities and its executable
  identity is verified before any bias is applied, so a mutation never happens
  without independent recovery armed. An orphan debt whose boundary vanished
  without a certificate is retired only by an explicit confirmed
  `oom-job-reconcile`, which refuses while the scope, cgroup or sentinel lives.

## Managed-probe isolation follow-up (2026-10-20)

The live managed probe previously discovered a target among all registered jobs
and forced a global drain, so verification could touch an unrelated registered
workload. The probe is now bound to its own pre-chosen disposable generation:
target selection uses the worker's own recorded incarnation, bias revalidates
the admission, restoration is scoped to that generation, and retirement needs a
matching terminal certificate. A regression proves an unrelated registered job
and its debt stay byte-identical, and a mutation rehearsal shows that check
fails when the scoped path is reverted to a global sweep. Details, live evidence
and deployment scope are in `managed-probe-isolation-2026-10-20.md`.

## Follow-up qualification (2026-10-20)

The latest lifecycle tests, explicit OpenSSL build prerequisites, raw gate
results, mutation check, scoped artifact provenance and fresh sandbox probes
are recorded in `cpu-lease-failure-coverage-2026-10-20.md`. That follow-up
supersedes the deployment identity below while preserving the historical
observations and limitations. It excludes concurrent unrelated disk-cleanup
changes from the deployed artifact; the full current workspace gates remain
required as well.

## Verification observed so far

Latest commands (2026-10-19 continuation; this is bounded development evidence,
not a release or live-runtime qualification):

```
CARGO_BUILD_JOBS=2 cargo test -p dracon-system --locked
# PASS: 516 unit tests; 4 events_cli tests; 12 guard_service tests; 0 failures
CARGO_BUILD_JOBS=2 cargo test --workspace --locked
# PASS: 35 test binaries ok. An earlier run failed one unrelated dracon-sync
# timing test (bucket_qualification_daemon_new_enrollment_retry_and_fresh_restore);
# it passes in isolation and on every later run, and no dracon-sync source was changed.
CARGO_BUILD_JOBS=2 cargo clippy --workspace --locked -- -D warnings
# PASS
cargo fmt --all -- --check
# PASS
CARGO_BUILD_JOBS=2 cargo build --release --locked
# PASS
cargo deny check
# PASS: advisories ok, bans ok, licenses ok, sources ok
```

Raw current logs are preserved in `docs/evidence/protection-repair-2026-10-19/`:
`dracon-final2-crate-tests.log`, `dracon-final2-clippy.log`,
`dracon-final3-workspace-tests.log`, `dracon-final3-release.log`,
`deployed-binary-hashes.log`, `deployed-guard-service-properties.log` and
`deployed-guard-journal.log`.

One earlier full-workspace invocation FAILED on the pre-existing live storage
activity test: it expected an error containing `active` but received a
conservative refusal for an unrelated python3 process with an empty command
line. It was not weakened or rewritten; the later workspace runs above pass.

Live observations with the development binary, before deployment:

- The managed probe completed with status 0 against the real user manager:
  baseline 200, bias 250, lifetime debt restored, and the debt retired against
  the sentinel terminal certificate. No managed-job unit and no outstanding
  debt remained afterwards.
- The CPU protection probe completed with status 0: quota 500ms applied, normal
  release, stricter and operator edits preserved, independent expiry executed.
- The independent expiry timer restored the scope quota to `infinity` on its
  own within ~50s, with the workload still running.

The deployed-service verification is recorded in its own section below.

New regressions exercise corrupt/truncated/unavailable ledger preservation,
second-owner lock exclusion without truncation, gone-root legacy unit survival,
boot mismatch, current-name exemption after same-incarnation exec, durable OOM
retry/recovery and CPU intent reconciliation. Existing compatibility and
capability-release regressions remain passing.

## Resolved safety findings

The independent review at `/tmp/dracon-protection-safety-review.md` found a
fundamental OOM provenance problem: ancestry polling alone cannot attribute a
child that forks and reparents before the next sample, or close the
fork-during-restore race. Keeping already-discovered obligations was necessary
but insufficient. The operator selected cooperative managed-launcher coverage,
and the repair now supplies a supported, independently verifiable ownership
boundary: launch-time admission with a durable job-lifetime debt, a
single-threaded sentinel that certifies workload-terminal generations, and an
independently supervised recoverer. The configured OOM opt-in is preserved and
no speculative capability was granted.

The read-only architecture review is saved in
`docs/design/oom-provenance-decision-2026-10-19.md`; the launcher and
recoverer design with its limitations is in
`docs/design/managed-job-admission-2026-10-19.md`; the implementation review
that required durable directory-chain initialisation and a non-panicking retry
diagnostic is in `docs/design/managed-job-bootstrap-review-2026-10-19.md`.

Remaining limitations, stated plainly:

- Coverage is cooperative, not enforced containment. A participating job that
  migrates out of its boundary, writes OOM policy itself, creates nested
  cgroups or execs an exempt role inside a biased job is outside the
  guarantee. Unregistered jobs keep the pre-existing in-process behaviour and
  their historical unknown debts are not retroactively certified.
- Restoration is not atomic and not bounded-time under an adversarial endless
  fork chain. The debt stays visible until the terminal certificate, rather
  than being declared recovered.
- A lost sentinel means unresolved provenance, not a certificate; only explicit
  operator reconciliation retires such an orphan.

No installed binary, guard policy or live guard service has been changed by
this implementation batch. No existing user workload was terminated/restarted.

## Deployment (executed 2026-10-19)

- The installed binary was preserved at
  `~/.local/bin/dracon-system.backup-2026-10-19-pre-repair` before the swap.
- The tested release binary was installed atomically (write to a temporary path,
  then rename) and `dracon-system-guard.service` was restarted. No other unit
  was restarted, no workload was terminated, and no policy, exemption or
  opt-in was changed.
- The loaded executable was verified by hash against the built release binary
  (`sha256sum /proc/<guard-pid>/exe` matched `target/release/dracon-system`).
- The service sandbox was re-inspected after the restart and is unchanged:
  `ProtectControlGroups=yes`, `ProtectHome=read-only`, `NoNewPrivileges=yes`,
  `AmbientCapabilities=cap_sys_nice`, `CapabilityBoundingSet=cap_sys_nice`.

## Live verification in the deployed service context

The verification unit is a copy of the deployed guard unit with only the
`ExecStart` line changed, so it runs the probe under the guard's REAL sandbox
(`ProtectControlGroups`, `ProtectSystem=strict`, `ProtectHome=read-only`,
`CapabilityBoundingSet=cap_sys_nice`, `SystemCallFilter=@system-service`,
`NoNewPrivileges`). It is disposable and was removed afterwards.

- CPU path: quota 500ms applied; sampled CPU 49.99%; normal release; a stricter
  pre-existing quota and a later operator edit both preserved; the independent
  expiry timer fired; the OOM permission-failure obligation was retained and
  rehydrated across a restart; the root and its child were restored.
- Guard restart recovery: after restarting the real guard, the quota of the
  disposable scope went from 500ms back to `infinity`, while the disposable
  worker and its child stayed alive and kept their cgroup and their restored
  OOM values. The probe's restart-check confirmed `quota=infinity`,
  `worker_alive=true`, `child_alive=true`, `oom_restored=true`.
- Managed-job path: the admitted job started at baseline 200, was biased to
  250, its lifetime debt restored it to the baseline, and the debt was retired
  against the sentinel's terminal certificate, with no managed-job unit and no
  outstanding debt left afterwards.
- The probe's receipts record the sandbox itself: a read-only cgroup mount,
  `CapEff=cap_sys_nice` only, and `NoNewPrivs=1`.

## Pre-existing limitation found during deployment (not caused by this repair)

The guard's memory-renice mitigation cannot be undone in this deployment. The
guard runs in a nested user namespace (`uid_map` maps only uid 1000), while the
session's processes live in the parent user namespace, so the guard's
`CAP_SYS_NICE` does not reach them: lowering a nice value back fails with
EACCES. The pre-repair binary logged exactly the same renice restore failures
at 22:31:58 before any of this work was deployed, and two `pi` processes
(684159, 866088) were already stranded at nice 5 with a `original_nice: 0`
ledger record before the swap. The new ledger keeps those obligations visible
and retried rather than dropping them, which is the intended behaviour.

This is outside the repaired CPU/OOM scope and changing it would alter an
existing opt-in's effective behaviour, so it was NOT changed in this batch. The
OOM path is unaffected and was verified end to end: `oom_score_adj` writes
succeeded across the namespace boundary (200 -> 250 -> 200 observed), and the
only EACCES observed was the deliberately non-dumpable probe child, whose
obligation was retained and later restored.

Recommended follow-up (operator decision): make the renice capability probe
verify that the capability actually applies to the target's user namespace, so
the guard's existing "cannot restore, therefore do not apply" rule engages
instead of stranding processes at a raised nice value.

## Audit-gate finding: the contract commands were not self-locating

The 2026-10-19T22:42:56Z independent audit did not judge this repair. It
fast-failed earlier, in the deterministic mechanical pre-audit gate, with:

```
Deterministic Pre-Audit Fast-Fail: Mechanical contract check failed:
`cargo test --workspace --locked` (exit code 101)
error: could not find `Cargo.toml` in `/home/dracon/.dracon/convos`
or any parent directory
```

The gate extracts a command from the verification contract and runs it with
the session's working directory. The contract scoped that command to the
repository in prose ("passes from /home/dracon/Dev/dracon-utilities"), but a
mechanical runner cannot act on a prose "from <dir>" qualifier, so the
extracted command was `cargo test --workspace --locked` and it ran in the
session directory, which holds no `Cargo.toml`. The exit code 101 therefore
reported a missing manifest, not a failing test.

The durable fix is to make every contract command self-locating, so the same
bytes are checked regardless of the caller's working directory:

```
cargo test  --workspace --locked --manifest-path <repo>/Cargo.toml --config <repo>/.cargo/config.toml
cargo fmt   --all --manifest-path <repo>/Cargo.toml -- --check
cargo clippy --workspace --locked --manifest-path <repo>/Cargo.toml -- -D warnings
cargo build --release --locked --manifest-path <repo>/Cargo.toml
```

`--config <repo>/.cargo/config.toml` is required, not optional: that file
pins `RUST_TEST_THREADS=1` because the dracon-sync storage tests mutate
process-global `HOME`/`PATH` and hold flock leases. Without it a run from
any directory executes the suite in parallel and the storage tests fail
nondeterministically. Observed at `--test-threads=16`: three failures, all
`journal lease is busy or unavailable`; the same suite with the repository
config passes 144/144.

`cargo deny check` has no `--manifest-path` flag and resolves the workspace
from the current directory, so it must keep running from the repository root.
The contract states that explicitly rather than pretending otherwise.

All four self-locating commands were re-run from the session directory
`/home/dracon/.dracon/convos` and each exited 0: the workspace suite passed
twice with 35 test binaries ok, and fmt, clippy and the release build were
clean.

## Rollback (retained; not needed so far)

The installed binary is preserved at
`~/.local/bin/dracon-system.backup-2026-10-19-pre-repair`. To roll back:
release/reconcile all new CPU leases and process obligations using the NEW
binary first, then restore the saved binary and restart ONLY the guard. Do not
restore an old binary incapable of reading current recovery state until all
obligations are resolved or a reviewed migration keeps them recoverable. Verify
the loaded executable identity afterwards and record any residual risk.

The installed unit file was NOT modified, so no unit rollback is required; the
deployed unit still matches the shipped repository unit byte for byte.
