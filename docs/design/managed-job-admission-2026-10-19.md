# Managed job admission — cooperative repair in progress

The operator explicitly chose managed-launcher coverage: future registered jobs,
no new root privileges, and existing unregistered jobs untouched and explicitly
unsupported. This is not enforced containment against arbitrary same-user actors.

## Initial implementation

`dracon-system managed-job -- COMMAND ARGS...` creates a fresh nondelegated user
scope through systemd-run. Its internal entry point runs before creating Tokio
or any threads. It validates the scope's manager invocation and ControlGroup,
cgroup device/inode, only-self membership and pids.current=1; captures a supported
inherited baseline; and durably registers the job before spawning COMMAND.
A job's HOME override cannot redirect the registry: it uses the account database
home and a private `.dracon/oom-jobs` directory. Generations cannot overwrite an
existing admission record.

The sentinel does not exec or fork again after launching the workload. It waits
for the root and then all remaining tasks. Only the single-threaded sentinel can
write workload-terminal proof, after observing only-self cgroup.procs followed
by exactly one charged task. It never interprets an empty scan, root exit, missing
scope or exhausted retry budget as this proof. Transient terminal-read/lock/write
errors retain the sentinel and retry; full immutable admission identity is checked
before proof persistence. A lost sentinel means unresolved provenance, not a
certificate a recoverer can fabricate later.

This is a workload-terminal certificate; the scope still contains the sentinel
until it exits. It is not a cgroup-empty certificate.

## Explicit supported-job contract

Participating jobs must not migrate/join tasks across job boundaries, create
nested cgroups, independently write OOM policy, exec exempt roles inside a biased
job, perform unsupported shared-mm/OOM-policy transitions, or change credentials/
dumpability beyond ordinary restoration access. No privileged recovery or hidden
minimum bypass is promised. Unsupported or uncertain observations retain debt and
report the limitation rather than guessing ownership or granting privileges.

Existing running jobs cannot be registered retroactively. A baseline already at
250 or protected/outside the admitted range is refused, not silently normalized.
In particular, a new command launched from an old, unregistered biased ancestor
may inherit an unsupported baseline. The caller must not interpret that refusal
as successful execution or recovery of the historical ancestor's unknown debt.

## Foreground semantics, not transparent wrapping

This is an explicit new foreground job API, not a silent replacement for tmux,
Pi, a shell or another launcher. It preserves command argv/env/cwd/stdio and
shell-style numeric exit status, but intentionally waits for the entire job,
not only a daemonizing root. The sentinel keeps inherited descriptors open until
job termination. PID cancellation, root-only completion, EOF timing and true
signal/core-dump semantics are not claimed identical to direct exec. No automatic
kill-on-drop, subreaper, signal-based cleanup or workload restart is introduced.
No existing launcher was rewired without locating its actual producer and checking
its completion/cancellation contract. Explicit registered-job coverage is provided
by the new command; arbitrary future commands outside it are still unsupported.

## Registered-job steering and lifetime debt (implemented)

`src/managed_oom.rs` now owns the registered-job OOM path:

- Steering applies only inside an admitted managed job. Unregistered jobs keep
  the existing in-process behaviour; they are explicitly outside this guarantee.
- The launcher establishes a known baseline (200) instead of inheriting the
  caller's score. A caller that inherited a biased 250 no longer refuses
  admission; it gets a job whose baseline is recorded by construction.
- Biasing writes a durable job-level debt BEFORE any process is mutated, and
  verifies the applied value afterwards. The sentinel is excluded by
  incarnation, never by name.
- The debt is retired only against the same-generation sentinel certificate.
  A running lease, a clean scan, a root exit, retry exhaustion or an
  unidentifiable member all keep the debt alive. A previous boot's debt is
  archived, never restored into current processes.
- Recovery runs in an independently supervised user service
  (`dracon-system-oom-recovery.service`) started with Restart=on-failure,
  RestartSec=2s and no capabilities. Its identity (executable inode and
  command line) is checked before any bias is applied, so a mutation never
  happens without independent recovery armed.
- Recovery restores admitted members to the job baseline by compare-and-restore,
  so an externally changed score is never overwritten.
- An orphan whose boundary vanished without a certificate keeps its debt until
  an explicit `oom-job-reconcile <id> --confirm` retires it. Reconciliation
  refuses while the scope, cgroup or sentinel still exists.
- The guard daemon also drains outstanding debts each pass, so a stale
  independent recoverer cannot hide a stuck debt.

## Observations so far (development only)

- Focused admission/persistence/proof tests passed, followed by all 515 unit,
  4 events CLI and 12 guard-service tests; workspace clippy and format check
  passed.
- A debug-binary `managed-job -- true` completed with status 0 through the actual
  user manager. Its durable record is terminal with proof=true and baseline=200.
  The installed guard and installed binary were NOT changed for this experiment.
- A requested exit-42 check was REFUSED before execution when the verification
  shell inherited oom_score_adj=250, and the observed status was 1, not 42. The
  launcher now establishes its own baseline, so this refusal no longer occurs;
  the numeric exit path is covered by unit tests and still needs a qualifying
  live check.
- The managed probe (`protection-probe <dir> managed`, disposable workload only)
  completed with status 0 against the real user manager: baseline 200, biased
  250, lifetime debt restored to baseline, and the debt retired against the
  terminal certificate. No managed-job unit and no outstanding debt remained
  afterwards. A deliberately stopped disposable job left an orphan debt, which
  `oom-job-reconcile --confirm` retired only after confirming the scope was
  absent.
- These are development-binary observations outside the deployed guard sandbox.
  They do NOT establish deployed-service behaviour, the CPU path, full live
  compatibility or any final release gate.

Next: qualify the CPU lease path under the same service context, finish
failure/interleaving tests, run the full workspace gates (workspace tests,
release build, cargo deny), then perform the authorized deployment and live
verification of both repaired paths with disposable workloads.
