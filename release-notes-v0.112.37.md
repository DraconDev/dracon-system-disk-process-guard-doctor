# dracon-system v0.112.37 — quieter, stateful guard reporting + bounded cleanup scans

Released 2026-08-14. Follow-up to v0.112.35/36 (memory/swap pressure
detection + reversible limiting). This release makes the guard **quieter by
design**: it stops repeating itself, stops treating telemetry as incidents,
and stops re-scanning the filesystem under sustained pressure.

## The problem

The v0.112.35/36 guard added many detectors (disk trends and fill-rate,
zombies, memory/swap pressure, heavy processes, cleanup candidates). On a
busy machine those detectors could each fire repeatedly:

- A persistent heavy process was re-notified every cooldown window.
- Swap occupancy alone was classified as memory pressure, even with plenty
  of free RAM and zero PSI stall.
- A disk in the ordinary warning band re-notified on every service restart.
- While disk stayed above action level, the report-only cleanup scan walked
  the whole `~/Dev` tree (du + find) every guard cycle (30 s), producing
  nothing actionable and constant I/O.

This release replaces "loud, instant, repeating" with a state machine that
matches how operators actually want to be told about a chronic condition.

## What changed

### 1. State-aware reporting with rate limits

Every reportable condition now flows through `report_state_transition()`:

- **Transitions** (entering a bad state, escalating, recovering) notify
  immediately — once.
- **Unchanged non-`ok` conditions** repeat at most every
  `report_repeat_secs` (default 1800 = 30 min) — a heartbeat, not a nag.
- Structured events (`events.jsonl`) and desktop notifications share the
  same backoff.

Conditions covered: memory pressure, zombie threshold, heavy processes,
rapid disk fill, early disk warning, disk state changes, auto/proactive
cleanup outcomes.

### 2. Memory pressure requires sustained multi-signal evidence

`classify_memory_pressure` no longer treats swap usage as pressure by
itself. Pressure requires **low available memory and/or active PSI/swap-in
thrash**; the stabilized state must persist for
`memory_pressure_sustain_secs` (default 120 s) before the guard notifies,
renices, or biases OOM. This prevents a transient sample — or a warm swap
with plenty of free RAM — from disturbing process priorities.

`guard once --json` now distinguishes `observed_pressure` (instantaneous
classification) from `pressure` (the stabilized state used for actions).

### 3. Heavy-process alerts are per-incarnation

Alert state keys on `pid + /proc starttime`:

- One notification on entry, one on `stuck` escalation, none in between.
- A recycled PID gets a fresh window and a fresh notification — it cannot
  inherit silence from the previous process that used the pid.
- Alert state is dropped when the incarnation is no longer heavy, so a
  later recurrence notifies again.

### 4. Disk notifications only on real transitions

`check_disk_state_change` notifies only when the state **changes during the
guard's lifetime**. Restarting the service while disk sits at 82% (warn)
no longer fires "state changed to warn". An initial **critical** reading
remains actionable and still notifies. Early-warning notifications are
similarly transition-gated.

### 5. Bounded cleanup scans

- **Action-level scans** (`run_auto_cleanup` at action/critical disk
  states) now run at most every `auto_cleanup_interval_secs` (default
  1800 s) — in *both* apply and report-only modes. The timestamp is set
  before the scan so a persistent filesystem error cannot turn into a
  30-second retry loop.
- **Proactive cleanup** now starts at **80%** disk usage (was 50%) on the
  example/live policy and retains its 120-cycle cadence; healthy disks are
  not scanned on a schedule.

## Defaults (unchanged safety posture)

The observation-first posture from v0.112.35/36 is preserved:

```toml
auto_renice = false            # CPU-heavy processes are reported, not slowed
auto_renice_on_memory = true   # reversible renice ONLY under sustained pressure
bias_oom_on_pressure = true    # steers the OOM victim; never triggers a kill
cap_offenders_cpu_percent = 0  # hard caps remain opt-in
freeze_sync_at_action = false  # disk pressure never pauses dracon-sync
auto_cleanup_apply = false     # cleanup is dry-run/report-first
auto_cleanup_interval_secs = 1800
proactive_cleanup_percent = 80
report_repeat_secs = 1800
```

No process is killed, reniced, or moved, and no file is deleted, unless the
operator opts in.

## Install

```bash
cargo install dracon-system --version 0.112.37
```

## systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor/main/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```
