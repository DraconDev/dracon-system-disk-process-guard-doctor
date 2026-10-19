# Protection repair — in progress, NOT deployed

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

## Verification observed so far

Latest commands (2026-10-19 continuation; this is bounded development evidence,
not a release or live-runtime qualification):

```
CARGO_BUILD_JOBS=2 cargo test -p dracon-system --locked
# PASS: 502 unit tests; 4 events_cli tests; 12 guard_service tests; 0 failures
CARGO_BUILD_JOBS=2 cargo clippy -p dracon-system --locked -- -D warnings
# PASS
cargo fmt --all
# succeeded; final --check remains part of release gates
```

Raw current logs: `/tmp/dracon-protection-followup-tests.log` and
`/tmp/dracon-protection-followup-clippy.log`. These temporary paths are development
logs only; final evidence must be copied into a durable report before claiming.
The earlier full-workspace test invocation was interrupted; it is NOT claimed
as passed. Full workspace tests, release build, workspace clippy, cargo deny,
final format check and controlled deployment checks remain required.

New regressions exercise corrupt/truncated/unavailable ledger preservation,
second-owner lock exclusion without truncation, gone-root legacy unit survival,
boot mismatch, current-name exemption after same-incarnation exec, durable OOM
retry/recovery and CPU intent reconciliation. Existing compatibility and
capability-release regressions remain passing.

## Open safety work — must resolve before deployment

The independent review at `/tmp/dracon-protection-safety-review.md` found a
fundamental remaining OOM provenance problem: ancestry polling alone cannot
attribute a child that forks and reparents before the next sample, or close the
fork-during-restore race. Keeping already-discovered obligations is necessary
but insufficient. Do NOT claim full inherited-debt recovery or deploy the
repair until a supported, independently verifiable lineage/ownership boundary
is implemented and tested. Do not simply disable the configured OOM opt-in,
freeze/kill workloads, or grant speculative capabilities to hide this gap.

Also finish actual manager failure/lock-contention tests; review the CPU lease
expiry and reconciliation changes; persist and validate probe boot/starttimes
instead of checking only PID existence; assess generated helper sandboxing;
and re-review every prior finding against the final revision.

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
