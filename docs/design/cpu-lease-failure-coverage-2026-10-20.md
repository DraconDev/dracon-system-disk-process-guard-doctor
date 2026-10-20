# CPU lease lifecycle failure coverage and reproducible build (2026-10-20)

## Why this follow-up exists

The auditor correctly found that the original CPU tests primarily exercised
parsers, intent decisions and serialization. A successful live expiry did not
prove recovery after a rejected manager call. No requirements were removed.

`src/cpu_limit.rs` now exposes a private `LeaseManager` I/O boundary. Production
still uses the same busctl/systemctl/systemd-run operations. The shared
`cap_prepared`, `release_with` and `recover_with` functions own the actual
write-ahead files, locking, ownership checks, reconciliation and retirement.
The public cap path retains its real /proc/cgroup membership, incarnation and
exemption checks. The expiry CLI and guard recovery use the shared lifecycle.

`src/cpu_lease_tests.rs` injects failures at that I/O boundary, never replacements
for lifecycle decisions. Each test uses a private real filesystem directory;
there is no HOME/PATH mutation or real user-manager operation. Coverage:

- Expiry timer creation denied: persisted unarmed intent, no quota write,
  manager outage retaining the intent, successful recovery retiring it.
- Quota application rejected: armed intent retained, next pass reconciles the
  unapplied state, subsequent application and release restore the original.
- Ambiguous application (write happened, response lost): retained unverified
  obligation, failed restoration retaining quota and debt, subsequent expiry
  invocation restoring the original and only then retiring the debt.
- Manager unavailable after verified application: expiry and startup recovery
  retain the file; a later startup recovery pass restores the quota.
- Unit invocation replacement during preparation and before release: no write
  to the replacement, which keeps its own quota.
- Unknown unit state retains debt; positively absent unit retires without writes.
- Workload revalidation failure never applies a quota; startup recovery retires
  only the unapplied intent. Obsolete helper generations cannot release a new
  lease, and external quota edits are preserved.

All recorded stop targets are expiry timers, never workload units. Generated
helper commands must invoke `cpu-lease-release`, carry restart-on-failure with
unlimited restart attempts, and never contain workload-stop/timeout/migration
operations. Tests do not pin incidental command order or test totals.

A deliberate mutation in a disposable source snapshot removed the actual
release restoration call. Three lifecycle tests failed (remaining quota
500000 instead of the original infinity). The mutation was reverted only in
that disposable copy; the working repository and deployed artifact never
contained it. Raw output: `revision2-mutation.log`.

## Reproducing the build environment

Workspace root: `/home/dracon/Dev/dracon-utilities`. Rust and C dependencies are
required; OpenSSL runtime libraries alone are insufficient. The parent
`flake.nix` development shell supplies pkg-config, cmake, OpenSSL and libssh2:

```sh
cd /home/dracon/Dev/dracon-utilities
nix develop --command bash
# Then run the unchanged acceptance commands below.
```

The auditor's bounded Nix-shell attempt timed out; this is not a Rust test
failure. For independent local reproduction without Nix evaluation/network
work, a sourceable snapshot of the already-installed development inputs is at:

```sh
source dracon-system/docs/evidence/protection-repair-2026-10-19/build-environment.sh
```

That snapshot names the split OpenSSL 3.6.2 header and library store paths,
sets pkg-config's search path explicitly, checks the files exist, and sets two
Cargo build jobs. It does not read credentials or require a desktop shell.
On another machine use the locked development shell rather than assuming
these local store paths exist. The verification run explicitly removed all
inherited OPENSSL_* and PKG_CONFIG_PATH values before sourcing the snapshot.
`revision2-environment.log` records paths, versions and lock/config hashes.
The missing OpenSSL discovery condition is therefore reproducible and resolved
without changing dependencies, skipping code, or weakening checks.

```sh
cargo test --workspace --locked --manifest-path /home/dracon/Dev/dracon-utilities/Cargo.toml --config /home/dracon/Dev/dracon-utilities/.cargo/config.toml
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo build --release --locked
cargo deny check
```

All commands are bounded by `timeout` in the raw logs; all gates must pass.
Always load the repository cargo config for tests from outside the repository:
its existing RUST_TEST_THREADS=1 setting applies to the full workspace and
avoids interference among existing process-global/fork-based tests.

Observed failures are retained, not overwritten:
- `revision2-workspace.log`: unrelated dracon-sync activity-label fixture failed;
  that exact test passed in isolation (`revision2-sync-isolated.log`).
- `revision2-workspace-retry.log`: a concurrent disk-cleanup test failed while
  its owner was editing it. This work did not weaken or change that test.
- `revision2-workspace-final.log`: subsequent full unchanged workspace passed.
- `revision2-scoped-tests.log`: running from outside the workspace without its
  cargo config exposed parallel fork/lock interference; the config was added.
- `revision2-scoped-tests-serial.log`: the disposable workspace initially lacked
  parent install.sh required by a pre-existing integration test. Supplying that
  fixture produced the passing `revision2-scoped-tests-final.log`.
- The initial format check observed concurrent unformatted disk-cleanup edits;
  a later formatted checkout is used for final gate verification.
- The mutation rehearsal and real workspace shared Cargo's target directory.
  Cargo reused the mutated test executable on a later focused invocation even
  though both source copies had been restored. The resulting failures are kept
  in `revision2-cpu-tests-cache-collision.log`. Updating the real source mtime
  forced recompilation from the canonical checkout; all 13 CPU tests then
  passed (`revision2-cpu-tests-final.log`). The scoped release binary had been
  built and saved before the mutation and was never affected. Future mutation
  rehearsals should use a separate CARGO_TARGET_DIR, not shared cache output.

## Scoped deployment, avoiding concurrent unrelated storage work

While this follow-up ran, another writer changed disk-cleanup source/tests in
the shared checkout. Those changes are outside this goal. To preserve existing
storage behavior, the deployed artifact is built from git baseline `1ce63d3`
(the source of the previously deployed guard), overlaid with exactly:

- `src/cpu_limit.rs`
- `src/cpu_lease_tests.rs` (test-only)

A disposable directory, not a branch/reset, holds this snapshot. Its workspace
manifest/lock/config and parent install.sh are copied from the real workspace;
other workspace crates are referenced by path. No canonical checkout is moved
or reverted. Both the scoped crate tests and its release build pass. Baseline,
overlay hashes, directory and artifact hash are recorded in
`revision2-release-provenance.log`; build output is `revision2-scoped-release.log`.
The current full workspace gates remain required in addition to this scoped
artifact qualification. The scoped release does not claim to match the latest
unrelated disk-cleanup source in main.

The scoped tested binary was atomically installed; only the real guard was
restarted. The installed unit's before/after hashes match. Loaded and installed
binary SHA-256:
`9804ee6f7b8c6f2e4a61b11c4542fe66ad9e33ace6108b93a7fab64c425cc696`.
The predecessor is durably preserved at
`~/.local/bin/dracon-system.backup-2026-10-20-pre-cpu-lifecycle`, in addition to
the original pre-repair backup. The scoped release artifact is also retained at
`~/.local/state/dracon/protection-repair-2026-10-20/dracon-system`; it is distinct
from the current workspace's release artifact containing unrelated edits.

The temporary probe unit copies the installed guard unit, changing its command
and disabling probe restarts; it includes the guard's effective 2 GiB memory
limit. All security restrictions remain enabled. Fresh live receipts show:

- CPU quota 500ms, measured CPU 49.315%, normal release, stricter/operator edits
  preserved, independent expiry successful.
- OOM permission failure retained and rehydrated, then root and child restored.
- Real guard restart: scope quota 500ms -> infinity, worker+child alive,
  unchanged cgroup and restored OOM values.
- Managed-job probe: baseline 200, bias 250, restored=true, retired=true.
  An initial managed probe reused the exercise directory and correctly refused;
  rerunning in a fresh directory passed. Both observations remain in the log.

Receipts and probe context: `revision2-live-probe.log`. Restart observations:
`revision2-live-restart.log`. Deployed executable, sandbox, unchanged unit and
journal: `revision2-deployed-*`, `revision2-unit-*.sha256`. Workers exited via
their own stop marker, not a unit stop or unverified PID signal. Cleanup log
confirms worker PIDs absent, scope gone, no managed debt remaining; only the
owned temporary probe unit file was removed. Existing user workloads, Chrome,
concurrency, storage policies and destructive-cleanup opt-ins were untouched.

Rollback still requires reconciling outstanding CPU/OOM obligations using the
new binary before restoring the saved predecessor and restarting only the
guard. See the original repair report. The pre-existing namespace/renice
limitation and cooperative managed-job coverage boundary remain unchanged.

## Review boundary

The fresh-context reviewer tool is not exposed in this session. An inline
review checked production wrappers against the previous I/O calls and reviewed
failure tests for durable-state retention and no workload termination. The
independent completion audit must still inspect the source, reproduce the gates
with the documented environment, and assess the new failure coverage.
