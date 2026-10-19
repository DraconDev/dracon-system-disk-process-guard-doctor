# Code Context

## Verdict

**The sentinel design closes premature-retirement/fork-enumeration races under the explicitly cooperative contract, not against arbitrary same-UID behavior.** At a successful `pids.current == 1` observation, the single-threaded sentinel is the only remaining charged task; with no migration/independent joins, no child cgroups, and no future sentinel forks, no workload task remains that can create another descendant. A preceding incomplete `cgroup.procs` enumeration cannot invalidate that conclusion. Keep the reads ordered members then task count as currently implemented. The proof is workload-terminal, not literally cgroup-empty (the sentinel remains).

This does **not** make restoration atomic, guarantee bounded completion under endless forks, or repair sentinel loss. Missing proof must remain unresolved debt, even after an empty scan or vanished scope. The contract must also prohibit creating child cgroups, changing credentials/dumpability beyond ordinary restore access, and unsupported shared-mm/OOM-policy transitions. `Delegate=no` is not a same-user enforcement boundary.

## Files Retrieved

1. `/home/dracon/Dev/dracon-utilities/dracon-system/docs/design/oom-provenance-decision-2026-10-19.md` (lines 1–119) — cooperative versus enforced provenance, durable lifetime obligation and permission constraints.
2. `/home/dracon/Dev/dracon-utilities/AGENTS.md` (lines 1–70, 400–619, 730–809; remaining instructions supplied in task context) — nested repo ownership, no service/workload actions, destructive behavior opt-in, test discipline.
3. `/home/dracon/Dev/dracon-utilities/dracon-system/src/oom_job.rs` (lines 1–174, concurrent-work snapshot) — new, untracked initial admission/sentinel implementation.
4. `/home/dracon/Dev/dracon-utilities/dracon-system/src/main.rs` (lines 9410–9449; targeted symbol searches) — existing Tokio entry point and existing ancestry-based OOM integration. No `oom_job`/managed-job wiring found at search time.
5. `/home/dracon/Dev/dracon-utilities/dracon-system/src/oom_restore.rs` (lines 1–75) — descriptor-bound compare/restore, not provenance.
6. `/home/dracon/Dev/dracon-utilities/dracon-system/src/cpu_limit.rs` (lines 180–304) — existing scope allowlist/member snapshot, not launch admission.
7. `/home/dracon/.local/bin/pi` (lines 1–45) — actual Pi shim, references installed Pi and outside-WezTerm launcher.
8. `/home/dracon/.npm-global/lib/node_modules/@earendil-works/pi-coding-agent/docs/tmux.md` (lines 1–57) — terminal configuration only, **not** tmux-spawn integration.

## Key Code

Current `oom_job.rs:94–95`:

```rust
own_threads == 1 && tasks == 1 && members == [sentinel]
```

Admission is persisted before spawn (`119–126`); terminal writes use lock/load/save (`135–141`). The new module explicitly says steering/recovery integration is separate (`1–4`). It must not be represented as a complete deployed repair.

## Actionable findings

### 1. Preserve exit, signal, and cancellation semantics explicitly

A non-exec sentinel necessarily changes workload PPID, PID-based cancellation, and completion timing. `wait()` follows only the direct child, then the launcher remains until **all** descendant tasks disappear (`126–146`). A daemonizing root or intentionally long-lived child therefore holds the managed launch open indefinitely. That is a visible semantic change, not merely bookkeeping; do not silently substitute this wrapper everywhere.

The sentinel/systemd-run/outer wrapper also keep inherited stdio open. A descendant that closes output but remains alive can now prevent consumers from seeing EOF because the sentinel still owns the pipe writer. Release unnecessary descriptors after spawn if possible; distinguish this from returning root completion. If existing launcher requires root-result completion, communicate that result separately while keeping an independent sentinel alive; do not silently change its completion contract.

Initial snapshot collapsed nonzero status to an anyhow error; concurrent revision now adds `JobExit` (`77–85`). Verify **both** `managed-job-enter` and outer launcher dispatch map that typed error to the exact exit code. `128+signal` is shell-compatible numeric encoding, not true signal termination/core-dump semantics. Specify which is promised. Preserve 126/127 launch failures where expected and avoid misreporting a workload failure as infrastructure failure.

No signal forwarding is present in this snapshot. Cancelling only the wrapper PID can strand a live workload; terminal/process-group signals may also kill the sentinel before proof. Do not add automatic kill-on-drop or subreaper behavior as a hidden fix. Define compatibility with existing tmux pane/group cancellation, and test it in a separately authorized disposable run. Sentinel SIGKILL/OOM/pane loss means retained unresolved debt, not a false proof.

### 2. Failures are conservative but currently unnecessarily abandon the proof writer

`spawn`, `wait`, enumeration, locking and persistence all propagate errors via `?` (`126–141`). A momentary registry-lock conflict at terminal time causes the sentinel to exit without proof. Keep retryable errors alive with bounded backoff/visible diagnostics; do not turn them into terminal success. A permanently failed spawn can still be certified terminal after confirming only-self: admission was durable but no workload began. Retain debt if that proof cannot be made durable.

Never allow a recovery worker to mint the sentinel's proof from a clean scan after sentinel death. This intentionally permits lifetime unresolved records following sentinel failure; document that operational cost and the explicitly authorized reconciliation path.

### 3. Strengthen admission and generation validation

- Explicitly request `--property=Delegate=no` at launch (`73`), not only inspect the default (`109`). Maintain the no-child-cgroup contractual requirement through lifetime; a one-time snapshot cannot enforce it.
- Reuse the same tasks==1/only-self check at admission (`103–114` currently checks own threads and members separately but not `pids.current`). The contractual freshly-created boundary makes this largely defensive; it avoids treating a weak procs snapshot as the certificate.
- Verify manager `ControlGroup` equals the discovered group and bind the full generation. Before terminal persistence compare immutable record fields (UUID/version/boot/unit/InvocationID/group/device/inode/sentinel PID/full process identity/baseline), not only boot/invocation/starttime (`138`). Do not overwrite a newer/corrupt record with an old sentinel's proof.
- New runtime dispatch must parse and run enter **before** Tokio construction. Current inspected main still uses `#[tokio::main]`; placing enter inside existing async dispatch violates the single-thread premise even if `/proc/self/task` sometimes happens to show one thread.
- Ensure baseline remains unchanged until workload spawn; never bias the sentinel, including indirectly via shared-mm propagation. Exclude its identity from offender selection and restore sweeps, not merely its `comm`. Avoid synchronous helper forks after the terminal check.
- Handle controller/read errors as unsupported/unresolved, never infer emptiness from missing `pids.current`. Keep UUID admission collision fail-closed and prohibit reopening admission after proof.

### 4. Registry ownership and recovery are still the essential missing half

`Admitted/Active/Draining/Terminal` exist, but the snapshot only stores Admitted and Terminal. Recovery must not race the sentinel's updates or reactivate a draining record. Use a shared ownership protocol, write-ahead job debt before any process bias, and immutable admission identity. A directory under inherited `$HOME` (`35`) needs a fixed agreed location or verified identity: arbitrary job environment overrides must not split launcher and recoverer registries. Create private state directories/records and validate corruption/symlinks according to the cooperative trust model.

Recovery lifetime must not determine workload lifetime; no per-pass clean result/root exit/retry exhaustion can retire debt. Keep registered-job-only steering; existing unregistered processes and old uncertain ancestry debts remain untouched and explicitly unsupported. CPU module scope-name checks alone do not admit OOM jobs, and its allowlist does not currently include `dracon-managed-job-*` (`cpu_limit.rs:193–209`); decide that integration separately, without extending CPU scope ownership by accident.

## Architecture

Launcher creates a fresh user scope → pre-runtime enter establishes homogeneous baseline and durable admission → enter spawns requested command at inherited baseline → guard biases eligible **non-sentinel** processes with write-ahead job debt → independent worker drains all admitted membership across fork/reparent/restart → sentinel alone certifies workload-terminal generation → debt owner retires only upon validating that certificate. Sentinel stays outside guard-owned 250, and scope lifetime preserves the identity until proof durability.

## Required tests (not run)

1. Model interleavings: omitted newborn in procs; fork between procs and pids reads; root exit/double-fork; thread-only survivor; fork reservations; dying/zombie survivors; child cgroups; sentinel accidental thread/fork. Never certify while a live fork-capable task remains.
2. Admission-before-first-instruction and before-first-fork; initially unexpected member/task; wrong ControlGroup/InvocationID; missing pids controller; generation/path/inode/boot/PID reuse; stale/corrupt record; unsupported baseline; sentinel never selected for bias.
3. Crash/ENOSPC/fsync failure at admission, spawn, bias intent/apply, restore and terminal proof; lock contention at proof; restart with orphan; clean scan/root exit never retires; delayed recovery must not overwrite proof. No workload starts after failed durable admission.
4. Descendant forks during draining; bounded per-pass work/endless fork chain retains lifetime debt. Same-score unrelated/unregistered jobs never touched. Permission/credential transition failures remain visible without adding privileges.
5. Exit 0/1/42/126/127, signal death, spawn error, wait interruption, exact argv including non-UTF8, env/cwd/stdin/stdout/stderr preservation; direct-child exit with persistent child; output EOF while descendant closes streams; cancellation by PID, process group and tmux pane. These are semantic acceptance tests, not only predicate unit tests.
6. Cooperative violations: attempted migration/independent OOM writer/exempt exec/subgroup creation must never be advertised as covered; detect what is observable, retain uncertainty where not observable. No hard-containment claims.

## Launcher discovery (bounded, read-only)

No actual `tmux-spawn-*` creation source was located. Searches covered likely paths/names in `/home/dracon/Dev/{pi-plugins,mcode-plugins,terhub,dracon-platform,eve,wonderful}`, Pi extension/git roots and installed Pi dist, plus `/home/dracon/.local/bin`. `terhub/src/proc.rs:140` recognizes systemd-run but is not a launcher match. Installed Pi tmux documentation is unrelated; do not edit it.

The actual executable shim is `/home/dracon/.local/bin/pi` → `/home/dracon/.npm-global/bin/pi` → `/home/dracon/.npm-global/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js`. Exact `tmux-spawn-` search of that installed dist found no match. Scoped search of local bin found `/home/dracon/.local/bin/run-outside-wez:16` and `run-pi-outside-wez:16` use user scopes; these are session relocation, **not established per-job integration points**. Existing `cpu_limit.rs` comments establish the expected prefix, not its producer. Locate the configured runner/tool implementation before claiming launcher coverage; no broad home-content search was performed.

## Start Here

Open `src/oom_job.rs:101–146` alongside the synchronous main-entry wiring. Resolve launcher completion/cancellation/stdio compatibility and retryable terminal persistence before enabling any guard bias through this registry.

## Residual risks / validation

Inspection only: no builds, tests, services, workload launches or settings changed. Parent was editing concurrently: `JobExit` appeared during review; findings distinguish snapshots rather than assuming final integration. Last git inspection showed untracked `src/oom_job.rs` and no staged paths; this review changed no repository files. Only this requested `/tmp` artifact was written. Runtime kernel/task-count semantics, permission behavior, launcher signal forwarding, and actual tmux-spawn producer remain unqualified.
