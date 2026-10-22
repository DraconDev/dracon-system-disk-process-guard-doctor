# Guard disk + memory hardening — 2026-10-22

## Why

The 2026-10-21 disk incident (the parent repository's
`docs/design/disk-full-2026-10-21.md`) filled the root filesystem: 144 GiB
of agent scratch accumulated in `/tmp` while the guard's sweep reclaimed
54.5 MiB per cycle. The guard was running and did warn ("Disk growing at
32.6 GiB/h (85% used) … estimated 4.1 hours until full") — the gap was
between the warning and the reclaim, and between the guard's own
diagnostics and the truth.

Three things were wrong with the guard, and one thing was wrong with the
guard's report of itself:

1. the `/tmp` sweep had one age floor and no way to reclaim the bulk that
   the floor was holding;
2. the memory pass ranked by resident size alone, the slow cleanup
   commands ran unbounded inside the guard's loop, and the browser cleanup
   was not scoped to jobs the guard could prove disposable;
3. there was no way to see a heavy, idle, orphaned TEST job — the shape
   the operator described as "not long running and working but truly heavy
   and forgotten, like some chrome instances left behind during tests";
4. `dracon-system doctor` and `dracon-system status` reported the daemon
   under its pre-migration unit names, so two REQUIRED checks could never
   pass and their remediation named a unit that is deliberately masked.

## 1. The `/tmp` reclaim

### What changed

- **A size-aware floor.** An entry of at least `tmp_large_entry_bytes`
  is judged against `tmp_large_min_age_hours` instead of the general
  `tmp_min_age_hours`. A bulk scratch clone does not have to wait the
  general floor out. It is consulted only when the disk is filling (the
  proactive/action tier), because deciding it costs a size measurement
  per entry and that walk is only worth paying when the disk needs the
  space.
- **The reclaimable scratch is reported.** The sweep now says how much it
  is NOT reclaiming and why: the bytes held back only by the age floor,
  the number of entries, and — from the same sustained fill rate the
  rapid-fill alarm uses — how many hours of disk that is.
- **A scratch-volume warning.** `tmp_scratch_warn_gib` (20 GiB) turns
  that figure into a warning. The disk alarms are percent-used and rapid
  fill; neither of them ever saw the deletable litter waiting for its
  floor.
- **One line per cycle for entries the guard cannot remove.** Root-owned
  entries failed individually, once a cycle each, which buried the lines
  that matter. They are now aggregated and counted.

### What did NOT change (and must not)

The liveness side of the sweep was already sound and is unchanged: the
pass refuses to run at all when the open-file scan fails; every open file,
every process working directory and every running executable under the tmp
roots is collected; entries holding a live unix socket are kept; a
directory is judged by its newest content rather than its own mtime;
protected paths and symlinks are refused. The size-aware floor only ever
makes the floor SHORTER for a LARGE entry, and it still passes through
every one of those gates.

### The new knobs

| knob | default | meaning |
|---|---|---|
| `tmp_large_entry_bytes` | 1073741824 (1 GiB) | the size at which the short floor applies |
| `tmp_large_min_age_hours` | 2 | the short floor; 0 disables the size-aware floor |
| `tmp_scratch_warn_gib` | 20 | warn when the age floor holds back this much scratch; 0 never warns |
| `cleanup_command_timeout_secs` | 300 | the hard bound on an external cleanup command; 0 means the default |
| `forgotten_orphan_footprint_mb` | 2048 | the resident-plus-swapped weight floor for a forgotten orphan (section 3) |

The live policy on this host also lowers the general floor from 24 h to
6 h, which is what bounds the LONG tail: 20,530 entries of under 20 MiB
held 62.7 GiB, and no size threshold reaches that. The example policy
documents the code default (24 h) — the live value is the operator's
choice, and the two are not supposed to match.

`scripts/check-guard-knob-parity.sh` asserts that every knob this work
added carries the same value in the crate's defaults, the NixOS operator
module, the crate's example policy and the live policy, and that the
daemon's task budget still matches between the shipped unit and the NixOS
module. It fails on a drifted source (verified by drifting one and
watching it fail — in the module as well as in the crate).

### The NixOS module's role

The goal names the NixOS operator module (the parent's `flake.nix`) as one
of the places this work ships through, and the parity check reads the
module. The module does two things, and the distinction between them
matters:

- it ships `dracon-system-guard.service` (and its watchdog), and
  `scripts/check-flake.sh` asserts that unit against the shipped unit
  property-for-property, so a NixOS deployment runs exactly the unit this
  repository ships;
- it **declares the guard's policy knobs** in
  `services.dracon.system.policy`, with the crate's own defaults:

  ```nix
  services.dracon.system.policy = {
    tmp_large_entry_bytes = 1073741824;
    tmp_large_min_age_hours = 2;
    tmp_scratch_warn_gib = 20;
    cleanup_command_timeout_secs = 300;
    forgotten_orphan_footprint_mb = 2048;
  };
  ```

  The key names are the TOML knob names verbatim, so a grep-based parity
  check compares this source with the crate's defaults and the live policy
  without a translation table. A NixOS install with no policy file runs the
  crate's DEFAULTS, so the declared values are what such an install
  actually gets — the declaration is not a claim about a file that nobody
  reads.

The module does **not** write the operator's policy file by default. That
file is the OPERATOR's: the live policy is deliberately not a copy of the
example, because the general tmp floor is 6 h on this host against the
example's 24 h. `services.dracon.system.managePolicy` (default `false`)
turns the module into the file's owner — the pinned example policy plus
the declared knobs, rendered into `~/.dracon/utilities/system/
dracon-system.toml` with the knobs placed inside the `[guard]` table — and
the parity script asserts that the switch is OFF by default, so a module
can never quietly overwrite the operator's tuning.

`scripts/check-flake.sh` renders that policy during the check and asserts
that every declared knob appears exactly once, with the declared value,
inside the `[guard]` table — a duplicate key is invalid TOML, and the
guard refuses to start on a malformed policy (EX_CONFIG).

## 2. The memory pass

- **External cleanup commands are bounded.** `nix-collect-garbage` and
  `docker prune` ran INLINE in the guard's loop; a GC that runs for
  minutes blocked the whole loop, so the memory monitoring that would
  notice a swap spike never ran in the meantime. They now have a hard
  bound (`cleanup_command_timeout_secs`) and the child is killed when it
  expires.
- **Swapped footprint is recorded and ranked on.** RSS alone misses the
  largest processes once the kernel has evicted their pages: a process can
  be down to a few MiB resident while holding tens of GiB in swap. The
  report carries resident AND swapped per offender, and the renice /
  OOM-bias / CPU-cap ranking uses the combined footprint.
- **Browser cleanup stays scoped to disposable jobs.** The browser
  cleanup only acts on processes the guard can prove are disposable test
  jobs — the same proof the forgotten-orphan pass uses (below). Generic
  browser exemptions for the operator's own session are untouched.

## 3. Forgotten heavy orphans

The operator's shape: "not long running and working but truly heavy and
forgotten, like some chrome instances left behind during tests".

### The desktop-session veto

**A process that is not headless and does not carry a test profile is
vetoed, whatever its weight.** The classification is
`forgotten::classify_desktop_session`:

- the command line must look like a browser at all (it contains
  "chrom"); anything else is not a candidate;
- `--headless` (or a `headless_shell` binary) ⇒ a test job;
- a `--user-data-dir` under a temporary or test-harness path
  (playwright, puppeteer, chromium-, chrome-, headless, .cache, tmp) ⇒ a
  test job;
- **no `--user-data-dir` at all, or one that is not a test profile ⇒ a
  live desktop session ⇒ VETOED.**

The rule is deliberately one-directional. A browser the rules do not
recognise is therefore vetoed rather than risked, and the operator's own
Chrome — a normal, long-running, working session — is never a candidate
however heavy it is. Every veto is REPORTED, so the operator can see that
a heavy Chrome was weighed and left alone.

### The kill preconditions

A candidate must satisfy ALL of:

1. the owner is provably dead (reparented to init/systemd);
2. no controlling terminal (no shell, tmux pane or agent session owns it);
3. sleeping, not working;
4. idle by measurement: near-zero lifetime CPU;
5. heavy: resident PLUS swapped footprint at or above
   `forgotten_orphan_footprint_mb` (2048 MiB);
6. classified as a TEST job by the rule above.

**Reporting is unconditional; ending is pressure-gated.** The scan runs on
every pass and reports every qualifying process with its evidence (pid,
age, RSS, swap, why it qualifies) plus every veto. A process is ENDED
only under warn/critical memory pressure, through the existing
pressure-orphan path, and only after every gate is RE-VERIFIED live at
kill time — including the starttime nonce, so a recycled PID is never
signalled, and including the desktop veto, so a process that no longer
looks like a test job is not ours to end.

The pressure gate itself is `forgotten::forgotten_reap_allowed_under_pressure`
— a named function, not an inline string comparison in the middle of the
pass — and it fails CLOSED: an unrecognised pressure level is not
permission to end a process. `the_forgotten_reap_is_skipped_without_memory_pressure`
pins the closed side of that gate by name, so it cannot be edited into an
unconditional kill without a test failing.

The new knob:

| knob | default | meaning |
|---|---|---|
| `forgotten_orphan_footprint_mb` | 2048 | the resident-plus-swapped weight floor for a forgotten orphan |

## 4. The guard's own diagnostics

`doctor.rs` and `status` asked about `dracon-sync.service` and
`dracon-sync-watchdog.timer`. The fleet moved the daemon to
`dracon-sync-operator.service` and masked the old unit, so both checks
could only ever report "inactive" and their remediation told the operator
to enable a masked unit. They now ask the SYSTEM manager about the real
operator units.

The third REQUIRED failure had the same shape: the "legacy config absent"
check failed whenever `~/.config/dracon` EXISTED, but that directory is
shared with other tools — on this host it holds another tool's app state
and two status tokens. Failing a REQUIRED check because those exist, and
telling the operator to move credentials, is worse than the stale-config
risk the check was written for. It now asks whether a legacy DAEMON
CONFIG FILE is present (the names the daemon and guard would have read),
and unrelated content in that directory is not the daemon's to move.

Both diagnostics now report every REQUIRED check as ok on this host.

## 5. The privileged step

The guard runs in the user session and can never prune the root-owned
`/nix/var/nix/profiles/default`: nix-env answers "Read-only file system
(needs root)". Attempting it every cycle produced a failure line on every
cycle of the guard's life. The guard now attempts the prune only when the
generation count is above the retention — so the expected permission
failure is rare and meaningful — and when it IS needed it names the
apply-ready privileged path:

```
sudo /home/dracon/Dev/dracon-utilities/dracon-system/scripts/prune-nix-system-profile.sh
```

The script refuses to run unprivileged, refuses a retention below 1, and
keeps the newest N generations (`+N`, the same reading the guard uses).

## 6. The next goal: the per-job ownership registry

Deliberately OUT of this work, and recorded here as the next one: an
admission/ownership registry for browser, build and test jobs, with
bounded concurrency. It is the fourth of the tracked memory items in the
parent repository's `docs/design/performance-swap-thrash-2026-10-21.md`,
and it is a new subsystem with its own state, configuration and tests —
too large to fold into this change, and not needed for any of the four
defects above. The forgotten-orphan pass is a partial answer to the same
need: it identifies disposable browser jobs after the fact, where the
registry would admit them up front.

## Left open (observed, not fixed)

- **A mitigation the guard cannot reverse.** The guard can LOWER a
  process's priority (raising its nice value) without any privilege, but
  raising it back needs CAP_SYS_NICE, and the restore failed with
  "Permission denied" for the operator's agent sessions (they were left
  at nice 5). The guard's own capability check reads its own
  `/proc/self/status` and reports the capability as present, so the
  mismatch is not understood yet — the root cause was not established, and
  it is outside this work's scope. The live policy now exempts `pi` (the
  operator's agent sessions) from the mitigation entirely, which is the
  crate's own documented intent for agent sessions, so the guard stops
  degrading them; the one-way restore itself remains unfixed.
- **The long tail.** Lowering the general floor to 6 h bounds it on this
  host, but a host with a faster scratch churn would still accumulate
  small entries between sweeps.
