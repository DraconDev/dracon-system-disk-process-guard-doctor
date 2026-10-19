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
until it exits. It is not a cgroup-empty certificate. The state model includes
admitted/active/draining/terminal; runtime steering and independent debt recovery
are the next implementation step, not yet supplied by the bootstrap itself.
Do not deploy or describe this initial launcher as a finished OOM repair.

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

## Observations so far (development only)

- Focused admission/persistence/proof tests passed, followed by all 508 unit,
  4 events CLI and 12 guard-service tests; workspace clippy passed.
- A debug-binary `managed-job -- true` completed with status 0 through the actual
  user manager. Its durable record is terminal with proof=true and baseline=200.
  The installed guard and installed binary were NOT changed for this experiment.
- A subsequent requested exit-42 check was REFUSED before execution because the
  verification shell inherited oom_score_adj=250. The observed status was 1,
  not 42. This is recorded as unsupported admission, NOT a passing exit-42 live
  test. No original score was guessed or reset to bypass the refusal. Numeric
  exit propagation is covered by unit tests but still needs qualifying live checks.
- These are launcher-bootstrap observations outside the guard sandbox. They do
  NOT establish deployed-service OOM recovery, independent recovery, full live
  compatibility or any final release gate.

Next: implement registered-job-only steering, write-ahead active/draining lifetime
debts, an independently supervised recoverer, restart reconciliation, and explicit
unsupported coverage reports. Keep the sentinel exempt by incarnation, not merely
by name. Retire debt only using its validated terminal certificate. Then finish
failure/interleaving tests, final workspace gates, and authorized deployment/live
checks of both repaired paths with disposable workloads.
