# Guard: orphan-tree reaping (2026-10-23)

## 1. What happened

On 2026-10-23 the doomtap CI (`bun test src/lib`) leaked a whole browser on
every run. By the time it was found, the host had:

- **949 browser processes** holding **13.6 GB RSS** (one orphan tree alone
  was 9.5 GB across 835 processes),
- RAM exhausted (196 MB free of 30 GB) and **55 GB of swap** in use,
- `kswapd0` pegged at 100% CPU for eight days,
- sustained swap thrashing at ~500K pages/s, load average **85** on 16 cores.

The leak itself was fixed in `f1e337c1f` (a missing `afterAll` that closed
a memoised Puppeteer browser), and the orphaned processes were reaped by
hand. The guard, which exists precisely to catch this shape, did nothing.
This document is about the guard.

## 2. Why the guard saw nothing

Three separate defects, each sufficient on its own.

### 2.1 The weight floor was per process

`scan_forgotten_heavy_orphans` measured **one process's** resident-plus-
swapped footprint and compared it to `forgotten_orphan_footprint_mb`
(2048 MiB).

A browser is not one process. It is a root, a zygote, renderers, GPU and
network helpers, and a crashpad handler — and when the test runner dies,
only the ROOT is reparented to init. The other members stay attached to
their parents inside the tree, so the guard's orphan scan saw the root and
nothing else.

The median single-process weight of the leaked tree was **3 MiB**. Not one
member of 792 ever crossed a 2 GiB floor. The guard logged `critical`
memory pressure on every pass and had no path to act on
death-by-a-thousand-cuts.

The weight that matters is the tree's **aggregate**.

### 2.2 The exempt list swallowed the whole browser family

`process_exempt_names` contains `chrome` — the operator's way of saying
"never touch my browser". The reaper matched exempt entries as a
**substring of the entire command line**:

```rust
policy.exempt_names.iter().any(|exempt| args.contains(exempt.as_str()))
```

A Puppeteer test browser's command line carries
`--user-data-dir=/tmp/puppeteer_dev_chrome_profile-*`, which contains
`chrome`. So every Puppeteer test browser matched the exemption and was
skipped **before any other gate ran** — not reported, not weighed, not
reaped. The exemption is checked first precisely because it is the
strongest statement, and that made it the strongest blind spot.

Playwright's `playwright_chromiumdev_profile-*` does not contain the
substring `chrome`, which is why the two browser families behaved
differently under the same policy.

Audit F119 (2026-10-08) had already recorded the reaper's whole-command-
line substring as the odd one out: the renice path has always matched the
executable **name**. The reaper never changed.

### 2.3 Chromium rewrites its own argv, so the command line is not the executable

Found while verifying this change against the live host.

Chromium reuses the memory after its argv for its own purposes, so
`/proc/<pid>/cmdline` comes back for a Chrome process as **one NUL-free
blob** rather than as NUL-separated fields:

```
/nix/store/…/share/google/chrome/chrome --ozone-platform=x11 --headless=new
  --user-data-dir=/tmp/cw-real-profile-standard-1792777061299 … about:blank
```

A naive "split on NUL, take the first field" therefore yields the WHOLE
blob, and the basename of that is whatever followed the last slash in the
blob — not the executable's name. On this host that made the operator's
own Chrome stop being exempt, which is the exact protection the exempt
list exists to provide. The same mangling is why the OLD substring rule
happened to work for Chrome at all: the blob still contains
`/share/google/chrome/chrome`.

Two fixes, both in `reap.rs`:

- the executable name is read from the **`/proc/<pid>/exe` symlink**,
  which is authoritative and unaffected by the argv rewrite (with the
  command line as the fallback for processes whose exe link cannot be
  read, such as another user's);
- the command-line fallback takes the first **whitespace-delimited**
  token of the blob, which is the executable path.

### 2.4 The documented pressure gate did not exist

`docs/design/guard-disk-memory-hardening-2026-10-22.md` states:

> The pressure gate itself is `forgotten::forgotten_reap_allowed_under_pressure`
> — a named function, not an inline string comparison in the middle of the
> pass — and it fails CLOSED [...] `the_forgotten_reap_is_skipped_without_memory_pressure`
> pins the closed side of that gate by name, so it cannot be edited into an
> unconditional kill without a test failing.

Neither the function nor the test existed. The pass compared the pressure
level inline. The property the documentation promised was enforced
nowhere.

## 3. The design

### 3.1 A tree is weighed as a whole

The forgotten pass now walks the process tree, not the process list.

A **root** is a process whose owner is provably dead (reparented to
init/systemd), which is detached from any terminal, sleeping, idle by
measurement, and not on the exempt list.

The **tree** is the root, its descendants, and the crashpad handler its
members name.

| property | value |
|---|---|
| `forgotten_orphan_footprint_mb` | 2048 MiB — the ROOT's own footprint |
| `forgotten_orphan_tree_footprint_mb` | 256 MiB — the tree's AGGREGATE |

A tree qualifies when **either** its aggregate crosses the tree floor
**or** its root crosses the per-process floor. The per-process trigger
keeps its 2026-10-22 meaning exactly; the aggregate trigger is new and is
separately controllable (`forgotten_orphan_tree_footprint_mb = 0` disables
it and leaves the old behaviour).

The floor was 1 GiB when this was first written and is now 256 MiB: a
single leaked tree settles at 293–304 MiB once the kernel has swapped it
out, so a 1 GiB floor misses exactly the thing this pass exists to catch.
The measurement is in §5.

### 3.2 The crashpad handler belongs to the tree

Chromium's crashpad handler calls `setsid`: it leaves the browser's
process group and session, so while its browser is still alive it is
reparented to init and looks like a lone orphan. On its own it is invisible
to the rest of the guard, because it has no `--user-data-dir` and is
therefore vetoed as a "desktop session".

It is tied back to its tree by the `--crashpad-handler-pid=<pid>` its
siblings carry, so the tree is ended as a unit and the handler goes with
the browser that owns it. A handler that still has a live parent belongs to
a LIVE browser and is never touched.

**It is not a leak of its own.** This section originally claimed that a
handler whose browser had died is "exactly the residue the tree pass exists
to remove". A controlled experiment on 2026-10-24 — launch a headless
Chromium, `kill -9` the browser root only — showed the handler **exiting
with its browser** rather than lingering. All twelve detached handlers on
this host (29 MiB in total) are still referenced by a live browser. Tying
the handler into the tree is about ending the tree as a unit, not about
cleaning up leftovers.

### 3.3 The desktop veto protects the tree, not just the root

The veto is evaluated on the tree's ROOT, and a vetoed root vetoes its
whole tree. The operator's own Chrome is protected twice over: its
executable name is `chrome` (on the exempt list), and it carries no
`--user-data-dir`, so the classification vetoes it as a live desktop
session whatever its weight.

A veto is only reported when the tree is actually heavy — the operator
wants to see that a heavy Chrome was weighed and left alone, not a list of
every helper process.

### 3.4 A tree is ended as a unit, deepest first

`terminate_tree` signals every member in ONE round and then waits once.
The single-process path waits up to ~7s for each kill before moving on,
so a 792-process tree would have held the guard's blocking thread for over
an hour. One round costs about as much as one kill.

Members are ended **deepest first**. A root that dies first leaves its
children reparented to init — the exact shape this pass exists to clean up
— and a parent that outlives its children can respawn them.

### 3.5 The kill-time re-verification, per member

Before any signal, every gate is checked again against the live tree:

- the root's starttime must match (PID reuse), its owner must still be
  dead, it must still have no terminal, still be sleeping, still be idle,
  and still classify as a TEST job;
- every member must still be the process the scan saw (starttime nonce),
  must not have become exempt, and must not be a zombie;
- the tree's live aggregate must still clear a floor.

A member that has exited, whose PID was recycled, or that has become
exempt is **left out of the kill set** rather than ending the whole tree:
the rest of the tree is still reclaimable. The root's gates are
all-or-nothing.

### 3.6 A tree must be old enough

Measured on this host on 2026-10-23, while verifying the change: an
orphaned headless Chromium root had burned only **four seconds** of
lifetime CPU.

So the lifetime-CPU gate does **not** separate "forgotten" from "just
detached and still in use". A browser that a harness deliberately
detaches (`setsid`, double-fork) and still drives over CDP is orphaned by
design, headless by design, and idle-looking by design. The only thing
that tells the two apart is TIME.

`FORGOTTEN_MIN_TREE_AGE_SECS = 300` therefore gates the pass: a tree must
have been running for five minutes before the guard will touch it. A leak
lives for hours or days and is reaped on the first pass after it ages
past the floor; a browser detached a moment ago is left alone.

This is a constant, not a knob: it is a safety floor, and a safety floor
an operator can casually tune is not a floor. The escape hatch for "do
not touch these trees at all" is the footprint floor and the exempt list,
both of which are knobs.

### 3.7 The pressure gate is a named function

```rust
pub(crate) fn forgotten_reap_allowed_under_pressure(pressure: &str) -> bool {
    matches!(pressure, "warn" | "critical")
}
```

It fails CLOSED: an unrecognised pressure level is not permission to end a
process. `the_forgotten_reap_is_skipped_without_memory_pressure` pins the
closed side by name, so the gate cannot be edited into an unconditional
kill without a test failing.

## 4. The exempt list now matches process names

```rust
pub(crate) fn matches_exempt(exe: &str, exempt_names: &[String]) -> bool {
    exempt_names.iter().any(|exempt| {
        if exempt.is_empty() { return false; }
        exe == exempt.as_str()
            || (exe.len() > exempt.len()
                && exe.starts_with(exempt.as_str())
                && exe.as_bytes()[exempt.len()] == b'-')
    })
}
```

The match is on the executable name — the basename of the binary the
process is running — which is what the renice path has always done, and
what the example policy tells operators to write ("bare process names
here").

A token also covers a name that extends it with a `-`, so `wezterm` still
covers `wezterm-gui`. It deliberately does **not** extend with a `_`, so
`chrome` protects the operator's own browser while a
`chrome_crashpad_handler` stays visible to the forgotten pass.

### 4.1 What this costs, and what it does not

The operator's `chrome` entry now does exactly what it says: every
google-chrome process is exempt, **including** the headless google-chrome
trees that some harnesses leak. The name exemption is checked before
every other gate, so those trees are invisible to the forgotten pass.

That was the state on 2026-10-23, and the operator's call was to change
it: **for a browser, the desktop-session veto decides and the exempt
list does not apply.** The veto tells the two shapes apart exactly — a
live desktop session carries no `--user-data-dir`, a test job is headless
or runs from a temporary profile — where the name exemption is blunt and
hides the leak.

So the forgotten pass now asks the veto for browsers and the exempt list
for everything else. The operator's own Chrome stays protected twice
over: the veto classifies it as a live desktop session, and every other
guard path (renice, OOM bias) still consults the exempt list by name.

Measured on this host while verifying the change, five qualifying trees:

| root | binary | processes | aggregate | before | after |
|---|---|---|---|---|---|
| 3335362 | google-chrome | 25 | 1083 MiB | exempt | **reaped** |
| 3350461 | google-chrome | 21 | 1020 MiB | exempt | **reaped** |
| 3897063 | chromium | 9 | 386 MiB | reaped | reaped |
| 3816878 | chromium | 9 | 365 MiB | reaped | reaped |
| 554746 | chromium | 9 | 299 MiB | reaped | reaped |

Deployed on 2026-10-23, the first two went from holding 2.1 GB to gone
within one daemon cycle, and the operator's own Chrome — the same binary,
the same executable name — was left untouched.

## 5. The knobs

| knob | default | meaning |
|---|---|---|
| `forgotten_orphan_footprint_mb` | 2048 MiB | the ROOT's own resident-plus-swapped weight floor |
| `forgotten_orphan_tree_footprint_mb` | 256 MiB | the tree's AGGREGATE resident-plus-swapped weight floor; 0 disables the aggregate trigger |

Both are carried by the crate's defaults, the NixOS operator module, the
example policy and the live policy, and `scripts/check-guard-knob-parity.sh`
asserts that they agree.

The aggregate floor was measured, not guessed, and then measured AGAIN by
the live proof. A single orphaned headless Chromium tree on this host (root
+ 8 renderers) held **807 MiB** when it was fresh, and fell to **329 MiB**
once the kernel had swapped most of it out and discarded the dirty pages.
A floor of 1 GiB therefore MISSES a single leaked browser tree — the natural
unit of leakage — and only catches it once a second one has piled up.

The live proof sharpened that. The tree it reaped (nine processes: root,
two zygotes, the GPU process, the network and storage services, two
renderers and the detached crashpad handler) was **293 MiB** at the moment
it was reaped — not fresh, but in the state a real leak spends most of its
life in, because the kernel has swapped it out. So a 512 MiB floor catches
one only while it is young, and MISSES the very thing this pass exists to
catch: a tree that has been leaking for hours. **256 MiB** catches one tree
in its steady state, with headroom.

Note that the weight is measured as resident PLUS swapped, so a tree that
the kernel has swapped out is not mistaken for a small one: the 329 MiB
above is the sum of both, not the RSS alone.

## 6. What is still open

- **Cancelled runs.** A CI job cancelled mid-test never runs its `afterAll`,
  so it leaks a tree by construction. The guard is now able to catch that
  tree; the cancelled job itself is still not prevented from leaking.
- **The broad pressure-orphan path** (`reap_orphans_on_pressure`, default
  OFF) still measures the weight of one process, and still catches only
  allowlisted dev-server signatures. It is a separate, opt-in tool with a
  wider blast radius, and this change does not touch it.
- **The leak's other 17 files.** The audit found 17 browser-launching files
  across the fleet with a launch/close imbalance. The doomtap one is
  fixed; the rest are still leaking, more slowly.
- **The crashpad residue — NOT a leak, and this document used to say
  otherwise.** A controlled experiment on 2026-10-24 (launch a headless
  Chromium, `kill -9` the browser root only) showed the crashpad handler
  **exiting with its browser**: it does not linger as an orphan. All
  twelve detached handlers on this host (29 MiB in total) are still
  referenced by a live browser through `--crashpad-handler-pid`. The
  handler is still tied into the tree so that a tree is ended as a unit,
  but there is no accumulating residue to clean up.
- **A browser farm's detached browsers.** A browser that was launched days
  ago, detached on purpose, and is still driven over CDP by a client in
  another process tree passes every gate: orphaned, headless, idle,
  heavy, old. The age floor does not catch this one — the browser is old,
  it was only just detached. This is a theoretical false positive and it
  is inherent to the shape, not to this implementation; the operator's
  lever is the exempt list or a higher footprint floor.

## 7. The live proof (2026-10-24)

The tree path was verified end to end against the running daemon, on a tree
that was created deliberately for the purpose and then left alone:

```
🧹 forgotten orphan tree: chromium pid=4148111 9 processes 293MiB (root 107MiB) age=1107s cpu=2s — headless
🛡️ forgotten-reap: tree 4148111 (9 processes, 293MiB) -> 9 ended, 0 left, 0 skipped
```

Nine processes: the root, two zygotes, the GPU process, the network service,
the storage service, two renderers, and the detached crashpad handler that
`setsid`ed out of the tree and was tied back by `--crashpad-handler-pid`.
All nine ended; none survived; none was skipped.

The operator's own Chrome (PID 59843, 49 descendants, 6.4 GB) was alive and
untouched throughout. Its command line carries no `--headless` and no
`--user-data-dir`, so the desktop veto classifies it as a live session and
the tree is weighed and left alone, whatever its weight. It is in fact
protected three times over: it has burned 4.96 HOURS of lifetime CPU, so the
idle gate excludes it before the veto is even reached; the veto classifies
it; and every other guard path still consults the exempt list by name.

That first proof ran at a 64 MiB floor, on a tree created for the purpose.
The SECOND proof ran at the production floor of 256 MiB, against the real
configuration, and reaped TWO trees — the deliberate one and another that
the Playwright harness had left behind in the meantime:

```
🧹 forgotten orphan tree: chromium pid=2121473 9 processes 304MiB (root 108MiB) age=748s cpu=2s — headless
🧹 forgotten orphan tree: chromium pid=2330587 8 processes 364MiB (root 119MiB) age=492s cpu=2s — headless
🛡️ forgotten-reap: tree 2330587 (8 processes, 364MiB) -> 8 ended, 0 left, 0 skipped
🛡️ forgotten-reap: tree 2121473 (9 processes, 304MiB) -> 9 ended, 0 left, 0 skipped
```

Seventeen processes ended, none survived, none was skipped. The guard's
pass cadence held at 32-33 seconds throughout — including while the disk
cleanup was running.

The second proof also proved the RELOAD path, by using it: the floor was
set to 4096 MiB and the daemon reloaded (the aged 363 MiB tree was then NOT
reported, proving the new value was in force), then set back to 256 MiB and
the tree was reported and reaped. Before the fix in 8.1 that second SIGHUP
would have been deferred and the floor would still have been 4096: the tree
would have been reported at the first SIGHUP and never reaped.

## 8. Two defects the proof exposed

Both were found by the proof itself, and both are fixed here.

### 8.1 The policy could never be reloaded again

The daemon refused the SIGHUP that was supposed to pick up the new floor:

```
SIGHUP reload deferred: keeping previous policy so pending process adjustments are retried
```

The guard applies a nice value to a heavy process to slow it down. Restoring
it needs `CAP_SYS_NICE`, and the guard's unit has it — but **`CAP_SYS_NICE`
is namespaced**. A process holding it in a nested user namespace has it in
that namespace and in its DESCENDANTS, never in an ancestor. The guard's
unit runs in a nested user namespace (`user:[4026532749]`) while the
processes it wants to renice live in the initial one (`user:[4026531837]`).

Lowering a process's priority needs no capability at all when you own the
process, so the guard could always make things worse and never better. The
probe that decides "can I restore this?" looked only at the guard's own
`/proc/self/status`, so it said yes for processes the guard provably could
not touch. Every restore failed with `Permission denied`, every restore was
retried, the restore never succeeded — and because the reload is deferred
while a restore is outstanding, **every later SIGHUP was deferred too**. One
un-restorable renice made the whole guard impossible to reconfigure.

Two changes:

- the probe is namespace-aware now (`nice_restore_namespace_ok`): the
  capability must apply in the TARGET's user namespace, and it fails closed
  when either namespace cannot be read;
- a restore that fails permanently — the guard is not permitted to set that
  process's priority at all — is DROPPED instead of retried forever, so it
  stops holding the reload hostage. The process still keeps its degraded
  priority until it exits; what changes is that the guard stops pretending
  it can undo it, and stops blocking the policy reload while it cannot.

The same probe now also refuses to APPLY a priority the guard could not take
back. That follows the guard's own stated rule — "do not apply a reversible
limiter unless the service can complete both halves of that lifecycle" —
which the old namespace-blind probe let it break.

### 8.2 The housekeeping starved the protection loop

The first proof attempt failed for a different reason: the tree was detected
on the first pass and was not reaped for another eight minutes.

The disk cleanup ran INLINE, ahead of the memory checks, in the same pass.
On this host a single cleanup pass takes minutes: a `/tmp` with thousands of
entries, a Trash the CI's `safe-delete` keeps refilling. The daemon
completed exactly ONE pass in thirteen minutes, and the forgotten-orphan
tree — eligible from the first minute — was reaped only when the cleanup
finished and the next pass ran.

The guard was doing housekeeping while the memory it exists to protect
drained away. The cleanup now runs in its own task: the pass never waits
for it, so the protection loop keeps its `interval_secs` cadence whatever
the filesystem is doing, and the cleanup's summary comes back through a
shared slot for the report.

### 8.3 What this costs

The renice mitigation no longer applies to processes outside the guard's own
user namespace — on this host, that is all of the heavy ones. The guard
keeps its other two levers (OOM biasing and CPU quota capping), both of
which are reversible without a capability. The processes that were already
left at a degraded nice by the old one-way ratchet are not restored by this
change: undoing them needs `CAP_SYS_NICE` in the INITIAL user namespace,
which is root's to give.
