# Tmux persistence health — source/test evidence

## Delivered behavior and limits

`doctor` human/JSON output now includes a typed, per-socket `tmux_persistence`
report. Producer state, save evidence and the optional continuum launch marker
are separate observations, not a global autosave-success verdict. Existing
strict-mode requirements are unchanged. No new daemon, recovery or guard
mitigation path was added.

[Design](../../../design/tmux-persistence-health-2026-11-01.md), root README and
`dracon-system.example.toml` document two defaulted `[guard]` settings:
`tmux_persistence_sockets` and `tmux_persistence_max_save_age_secs`.
Empty sockets select current TMUX or the standard default; explicit arrays
allow multi-server inspection. Arrays are TOML/custom-policy settings, not
supported by the existing Nix scalar override map; age is a scalar override.

The adapter queries only fixed explicit-socket `display-message -p` output.
It binds socket metadata, reported server PID/starttime and proc listening-fd
ownership, with revalidation after collection. The known producer adapter
reads the per-socket tmux-persist-daemon POSIX-cksum registration and validates
UID, argv, server/socket environment binding, process generation/state and
registration stability. It does not scan all processes or infer that every
possible producer is absent. Unknown provenance remains unavailable. T/t is
observed stopped state, not a stop-cause diagnosis or a recommendation to
SIGCONT; sleeping/existing does not establish useful autosave.

Snapshot evidence requires explicit server `@resurrect-dir`, a safe last
reference and bounded complete pane-marker evidence in a regular file.
Linux O_PATH pins an inode for type inspection before a read-only reopen:
FIFOs/devices/symlinks are not opened for data I/O. Identity/age are retained
independently of server-age validity. Physical directory/file sharing cannot
borrow another selected server's evidence. Snapshot age is **not** periodic
execution: resurrect intentionally retains unchanged snapshots; continuum's
marker is set on launch, not completion. Neither proves producer attribution,
restorability or pane-content backup.

Inspection caps targets, byte reads and fd enumeration, with asynchronous
per-query/inspection deadlines. Kernel filesystem calls are bounded in count
and input, not guaranteed preemptible during a kernel stall. Unsupported
paths/permissions/producers remain unavailable. Query cancellation owns only
the adapter's spawned client, never an observed server or producer PID.

## Verification contract

| Contract | Observed evidence |
|---|---|
| (1) Fresh, stale despite process existence, stopped, multi-server and unknown provenance | [Focused tests](focused.log): fresh/stale/stopped facts; current launch marker with stale snapshot; missing registration/PID/argv/environment/listening fd/directory/snapshot; partial/invalid/future/old-generation evidence; producer/server reuse; server/socket/producer query races. Multi-server fixture rejects cross-bound producers and shared directories/hardlinked files, including different server generations. |
| Meaningful failure versus harmless input change | The multi-server fixture tolerates reversed target order and reordered/irrelevant environment entries, but changing the server binding yields unavailable producer provenance. The delayed-first-server fixture protects later-server clock/freshness; the indeterminate-PID fixture distinguishes lookup failure from absence. No assertion pins explanatory wording or incidental inventory sizes. |
| (2) No recovery or mutation | `inspection_is_query_only_and_preserves_owned_files_and_socket` checks actual command-builder authority, explicit socket requests, preserved fixture file contents and socket identity. All fixture listeners/proc/files belong to TempDir; no real server/producer is launched or signalled. Production has only query/proc/file-reading paths; no observed PID is sent to a signal/control API. Special-file selection fixture rejects owned FIFO/symlink while reading a regular file. |
| Configuration/reporting integration | Old policies default successfully; explicit fields deserialize, clamp and normalize idempotently. Root package example/known-key and doctor strict/JSON tests pass. Doctor uses the existing policy load and JSON envelope; its strict check table is unchanged. |
| (3) Required gates | [Package tests](package-tests.log): 744 unit + 4 event CLI + 12 service tests pass, zero failures. [Strict package Clippy](package-clippy.log) exits 0; additional [all-targets strict Clippy](all-targets-clippy.log) exits 0. These are observed totals, not permanent acceptance pins. [Gate commands/statuses/source hashes](gates.json). |

[Additional check metadata](additional-checks.json) records focused-test and
read-only host command exits. [Source patch](source.patch) against the retained
[pre-goal baseline](base-revision.txt) makes committed changes inspectable despite
the auto-committer's clean worktree. [Review and corrective fixtures](review.md)
retain rehearsal findings/limits. The final integrity check verifies current
code hashes against gate inputs and local report citations.

## Read-only host observation

The locally built one-shot `doctor --json` exited 0; [raw report](host-doctor.json)
and [stderr](host-doctor.stderr) are retained. It bound the current server/socket,
reported fresh configured-path snapshot evidence, and separately reported
**unavailable producer provenance**. It did not classify that as successful
autosave or assume the reason for missing registration. No live snapshots,
configuration, installed binaries, services or observed processes were modified
by this inspection. This is one observation, not continuous monitoring or a
loaded-runtime receipt for the installed guard.

## Reproduction

From the nested checkout:

```sh
timeout 600 env CARGO_BUILD_JOBS=2 cargo test --locked -p dracon-system
timeout 600 env CARGO_BUILD_JOBS=2 cargo clippy --locked -p dracon-system -- -D warnings
timeout 600 env CARGO_BUILD_JOBS=2 cargo clippy --locked -p dracon-system --all-targets -- -D warnings
timeout 120 env CARGO_BUILD_JOBS=2 cargo test --locked -p dracon-system tmux_health
git diff --check
```

Tests do not invoke the package runner or external tmux servers. Teardown
removes only owned disposable endpoints/files. The installed guard is unchanged;
automatic recovery, generic stopped-process repair and browser cleanup were
not performed. Full-workspace qualification remains the separate queued gate.
