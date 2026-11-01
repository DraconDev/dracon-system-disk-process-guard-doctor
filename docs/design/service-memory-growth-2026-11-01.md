# Report-only sustained service memory growth

## Contract

Add an observation inside the existing guard pass, independent of host memory
pressure and mitigation opt-ins. No new daemon, process scan, persistence
ledger, recovery actions, installation or live configuration changes.

Use explicit `system:` / `user:` canonical service targets. Defaults are the
system dracon-sync operator service and user guard / xdg-desktop-portal
services. Configuration remains optional and backwards compatible. At most
16 targets and 32 retained observations per target; configuration windows
are bounded. Batch manager property reads by manager, with capped bytes and
timeouts, rather than spawning a process per metric or recursively scanning
cgroups. Only `systemctl show` is permitted.

## Measurement and identity

Read cgroup-v2 `memory.current` and `memory.swap.current` through a directory
opened component-by-component without following symlinks. These measure
**charged subtree resident RAM (including cache/kernel) and swapped bytes**,
not process RSS/PSS, private heap, or proof of a leak. Counters are sequential
point observations, not an atomic transaction. Preserve unavailable values.

Bracket sampling with manager properties: canonical Id, loaded/active state,
nonempty InvocationID, ControlGroup and MainPID. Bind the cgroup path to the
main process's unified-cgroup membership; check namespace visibility, cgroup
filesystem type, directory device/inode and process starttime. Compare
properties/identities after sampling. Root/unsafe paths, namespace mismatch,
manager/accounting errors or changed identities are unavailable, never zero.

A series generation includes boot ID, explicit manager/UID, service invocation,
cgroup path/device/inode and main PID/starttime. A restart, PID reuse, cgroup
replacement, missing observation, missing counter, backward/duplicate clock,
large sampling gap or footprint decrease resets the comparison. Decreases
are described as reset/release, not proven cumulative-counter resets.

## Sustained observation

Default window: 600 seconds; minimum growth: 256 MiB. Keep time-spaced samples
and a predecessor at the window boundary, not an unbounded per-cycle list.
Require sufficient elapsed time and meaningful growth in both halves of the
window, not merely a high endpoint. Each half must contribute at least one
quarter of the observed delta and one quarter of the configured minimum;
a dominant burst plus negligible drift does not satisfy this balance. A single burst followed by a plateau must
not become sustained growth. Flat RSS/swap migration must not become growth.
This conservative detector may miss noisy/short-lived growth; it must never
claim that sustained growth proves a leak or compare across missing evidence.

Report current/baseline counters, elapsed window, byte delta, MiB/min rate,
identity/provenance and status/reason. A one-shot guard can only warm up, not
establish sustained growth. Use existing rate-limited structured events;
history is not consulted by kill, renice, resume, restart, quota or admission
logic. Successful policy reload/restart clears observation-only history.

## Configuration surfaces

Flat scalar knobs (monitor flag/window/minimum MiB) fit the existing Nix bool /
unsigned-integer policy override renderer. Target lists are TOML settings;
Nix operators needing custom lists use the existing unmanaged/custom policy
path rather than pretending the scalar override map supports arrays. Update
shipped example and README; do not change live policies, Nix pins or units.

## Verification

Use injected manager properties, proc/cgroup fixture roots and monotonic
clocks, never live interventions: sustained growth vs burst/plateau/drop,
RSS-to-swap movement, irregular intervals, reset/restart/PID reuse, missing
samples/counters, identity races, namespace/path safety, retained-history and
command/output budgets, policy compatibility/normalization, JSON/table
supporting evidence and report-only isolation. Run package tests and strict
Clippy and obtain fresh-context review before submitting the item.
