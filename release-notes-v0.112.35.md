# dracon-system v0.112.35 (2026-08-10)

Disk/process guard, storage analyzer, and system diagnostics for local development machines.

## What's Changed

The guard now covers the failure modes from the 2026-08-09/10 incidents
(disk filled to 98%, Chrome crashed with `No space left on device`,
RAM exhausted with kswapd thrashing at 86% CPU, 19 GiB swap in use,
stuck svelte-check processes holding 6 GiB at ~285% CPU).

### Added

- **Memory/swap pressure guard** (`monitor_memory`, default on): reads
  `/proc/meminfo` + PSI (`/proc/pressure/memory`) every guard pass.
  Warns when free memory is low (`mem_available_warn_percent`, default
  10), swap usage is high (`swap_used_warn_percent`, default 50), or
  the system is swap-thrashing (PSI `full avg10` >=
  `mem_psi_full_warn`, default 10). Notifications include the top-5
  RSS offenders so the operator knows what to kill. Never kills
  anything itself. Falls back to a pswpin-rate check when PSI is
  unavailable. The 2026-08-09 scenario previously had NO guard at all.
- **Sustained-heavy "stuck candidate" escalation**
  (`process_stuck_after_secs`, default 600): a process still heavy
  past the sustain window is reported as "POSSIBLY STUCK" (e.g. the
  svelte-check ×4 case). Notification only; no auto-kill.
- **Zombie process detail**: zombies are enumerated per-pid with comm,
  ppid, parent command, parent-alive status, and age since first seen
  in Z state; the alert names the oldest offenders instead of a bare
  count. Zombies remain diagnostic — they cannot be killed.
- **Rapid disk-fill alert** (`disk_rapid_fill_gbph`, default 20):
  byte-precise df history (percent deltas are too coarse on large
  disks) alerts "disk filling at X GiB/h" long before the percent
  thresholds bite.
- **Trash credential guard** (`trash_credential_guard`, default on):
  before emptying the trash, a recursive scan checks for
  credential-signal filenames (chrome/credential/password/secret/
  token/*.env/*.pem/*.key/*.age/etc.). Any match aborts the deletion —
  the 2026-08-10 scan found 665 credential-pattern matches in a
  56 GiB trash.
- `guard once` report now shows Memory Pressure, Zombies, and Disk
  Fill Rate rows (same fields added to `--json`).

### Notes

- New policy knobs are all optional; defaults activate the new checks
  without config changes.
- Install the new binary and (re)start the guard service:
  `systemctl --user enable --now dracon-system-guard.service` — the
  service was found disabled/inactive before this release, which is
  why the incidents went unmonitored.

## Install

```bash
cargo install dracon-system --version 0.112.35
```

## Docker / systemd

```bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor/main/dracon-system-guard.service \
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
```

**Full Changelog**: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/v0.112.34...v0.112.35
