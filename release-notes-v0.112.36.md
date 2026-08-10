# dracon-system v0.112.36 — memory-pressure limiting: renice + OOM bias + optional CPU caps

Released 2026-08-10. Companion to v0.112.35 (the memory/swap pressure
*detector*); this release adds the *limiting* layer the 2026-08-09
swap-thrash incident called for.

## The problem

When the system starts choking (CPU pegged, RAM exhausted, swap
thrashing), Linux has no built-in safety: the OOM killer only fires at
the last moment and picks its victim by roulette. Disk was already
covered by the guard's proactive cleanup; CPU/memory had detection
(v0.112.35) but no mitigation.

Design constraint from the operator: **no killing by default**, and
"capping the system crashes it" — whole-session cgroup caps
(`MemoryMax` on a slice) kill apps (Chrome dies inside a memory cgroup
the same way it died from ENOSPC) and freeze the desktop. Memory caps
also free nothing: existing RSS stays; `MemoryMax` only frees by
killing the process, `MemoryHigh` just stalls it into uselessness.

## The levers (all per-process, all reversible, all whitelist-aware)

### 1. Renice on memory pressure (`auto_renice_on_memory`, default ON)

When pressure is warn/critical, the top-5 RSS offenders get graduated
nice (4 GiB → nice 5, 8 GiB → nice 10; verified against the existing
`graduated_nice_value` tiers). The unresponsiveness symptom is CPU
starvation — kswapd + offenders saturating cores while Xorg/Chrome UI
starve. Renice hands CPU back to interactive apps **while the
offenders keep running**. Restored to nice 0 after `release_after_secs`
of recovered pressure. Never a cap: a single runnable process at nice
19 still gets all the CPU when nothing competes — a video editor
exporting alone is unaffected.

### 2. OOM-killer bias (`bias_oom_on_pressure`, default ON)

During CRITICAL pressure, top offenders get `oom_score_adj` raised to
250 (from neutral 0). This **never triggers a kill** — it only tilts
the victim choice IF the kernel's OOM killer fires anyway, steering it
to the stuck 6 GiB svelte-check instead of your editor with unsaved
work. Restored on recovery. Never touches:
- `oom_score_adj <= -500` (deliberately protected / unkillable
  processes) — checked via the `oom_bias_target()` gate
- kernel threads and `process_exempt_names` entries

Same-uid `oom_score_adj` writes verified working on this machine
(2026-08-10).

### 3. Optional CPUQuota caps (`cap_offenders_cpu_percent`, default OFF)

During CRITICAL pressure, top offenders are moved into a transient
user systemd unit with `CPUQuota=N%`. CPU throttling is the *safe*
kind of cap — it never kills and never frees-then-crashes. Tames a
stuck busy-loop that nice 19 still lets burn a full core. On recovery
the process is moved back to its original cgroup and the unit stopped.

Verified live: a `yes > /dev/null` busy loop went **100% → ~51% CPU**
under `CPUQuota=50%`, then back to full after removal.

Why a transient **service**, not `--scope`: `systemd-run --scope`
blocks (foreground), and `--scope --no-block` tears the scope down the
moment systemd-run exits (both discovered live during testing).
`--no-block` without `--scope` creates a manager-owned unit that
persists; the cgroup is polled (≤5 s) before the pid move.

Why OFF by default: it moves processes between cgroups (a stronger
operation than nice), and requires a user systemd manager with cpu
controller delegation (verified present here:
`cpu io memory pids`). Operators enable it in
`~/.dracon/utilities/system/dracon-system.toml`:

```toml
# hard-throttle the top offenders to 50% CPU during critical pressure
cap_offenders_cpu_percent = 50
```

Crash-safety: the placeholder `sleep` bounding each cap unit dies
after 3600 s, so a guard crash leaves any capped process uncapped
within an hour at most (no stale limits survive).

## Config knobs (live config updated 2026-08-10)

| knob | default | meaning |
|---|---|---|
| `auto_renice_on_memory` | `true` | renice top RSS offenders during warn/critical pressure |
| `bias_oom_on_pressure` | `true` | raise `oom_score_adj` to 250 on offenders during critical pressure |
| `cap_offenders_cpu_percent` | `0` (off) | transient CPUQuota unit at N% during critical pressure |

All three respect `process_exempt_names` (the whitelist) and kernel
prefixes. To whitelist e.g. a video editor: add its comm name to
`process_exempt_names` and it is never reniced, biased, or capped.

## Guard resilience (watchdog, same day)

`dracon-system-guard-watchdog.service` + `.timer` (every 2 min) now
restart the guard if it is ever stopped or disabled — `Restart=always`
only covers crashes, and the Aug 9–10 incidents ran with the guard
INACTIVE AND DISABLED. Escape hatch for maintenance:
`touch ~/.dracon/dracon-system.maintenance-hold` (remove afterwards).
Files: `~/.config/systemd/user/dracon-system-guard-watchdog.{service,timer}`,
script `~/.dracon/system-notify/dracon-system-guard-watchdog.sh`.

## Testing

- 5 new tests: `oom_bias_target` (neutral raise / protected skip /
  boundary), memory-limiter policy defaults, TOML round-trip.
- Workspace: **1285 passed**, 9 ignored; clippy `-D warnings` clean;
  `cargo deny check` clean.
- Live-verified: oom_score_adj write/restore as same uid; CPUQuota
  cap/uncap round-trip on a real busy process (100% → 51.5% → 100%).
