# Protection repair — source complete, NOT deployed

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

## Implemented source changes (not a completion judgment)

- CPU quota leases use the user manager on existing recognized, isolated job
  scopes. They do not migrate processes or stop workload units. Unknown/shared,
  delegated, nested, or currently exempt-containing scopes fail closed.
- Original quota, invocation, generation, boot identity, and intent are durable.
  A separate expiry helper resets quota; expiry never stops the workload.
- Expiry helpers now have manager-level Restart=on-failure, RestartSec=5s,
  StartLimitIntervalSec=0. Lock contention and transient manager failures therefore
  do not consume the only independent recovery attempt. This still needs live
  service-context verification and failure-injection coverage before deployment.
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

## Verification observed so far

Latest commands (2026-10-19 continuation; this is bounded development evidence,
not a release or live-runtime qualification):

```
CARGO_BUILD_JOBS=2 cargo test -p dracon-system --locked
# PASS: 515 unit tests; 4 events_cli tests; 12 guard_service tests; 0 failures
CARGO_BUILD_JOBS=2 cargo test --workspace --locked
# PASS on re-run: 35 test binaries ok. The first run failed one unrelated
# dracon-sync timing test (bucket_qualification_daemon_new_enrollment_retry_and_fresh_restore);
# it passes in isolation and on the clean re-run, and no dracon-sync source was changed.
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
`dracon-managed-oom-tests5.log`, `dracon-managed-oom-clippy5.log`,
`dracon-workspace-final-tests2.log`, `dracon-release-build.log`,
`dracon-managed-probe-run.log` and `dracon-protection-probe-run.log`.

One earlier full-workspace invocation FAILED on the pre-existing live storage
activity test: it expected an error containing `active` but received a
conservative refusal for an unrelated python3 process with an empty command
line. It was not weakened or rewritten; the later workspace run above passes.

Live observations with the development binary (NOT the deployed service):

- The managed probe completed with status 0 against the real user manager:
  baseline 200, bias 250, lifetime debt restored, and the debt retired against
  the sentinel terminal certificate. No managed-job unit and no outstanding
  debt remained afterwards.
- The CPU protection probe completed with status 0: quota 500ms applied, normal
  release, stricter and operator edits preserved, independent expiry executed.
- The independent expiry timer restored the scope quota to `infinity` on its
  own within ~50s, with the workload still running.
- The probe restart-check phase requires a restarted deployed guard, so it is
  part of the deployment verification, not of the pre-deployment evidence.

These observations are development-binary evidence outside the deployed guard
sandbox. They do NOT establish deployed-service behaviour or the final release
gates; the deployment checks below remain outstanding.

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

## Deployment and rollback (planned; not executed)

After all gates and safety findings are resolved, preserve the installed binary
and unit configuration, atomically install the tested release binary, and restart
ONLY dracon-system-guard.service. Preserve hardening/capabilities and configured
opt-ins. Use bounded disposable scopes for application, release, expiry,
permission failure, descendants and real guard-restart verification. Record the
loaded executable identity, raw quota/CPU observations, process incarnations,
service context and journals. Stop disposable workers by their controlled stop
marker, never a possibly reused unverified PID.

Rollback must first release/reconcile all new CPU leases and process obligations
using the new binary. Do not restore an old binary incapable of reading current
recovery state until all obligations are resolved or a reviewed migration keeps
them recoverable. Restore the saved binary/unit and restart only the guard;
verify its loaded identity and record any residual risk explicitly.
