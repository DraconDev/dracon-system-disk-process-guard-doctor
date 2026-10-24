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
  it was only just detached. §9.2 does: a browser that is still driven
  over the DevTools protocol holds an ESTABLISHED connection to its
  debugging port, so it is `Driven` and is left alone. What remains
  theoretical is a browser driven over `--remote-debugging-pipe`, which has
  no port to check: there the conservative CPU proxy decides, which is the
  safe answer but not the precise one. The operator's other levers are the
  exempt list and a higher footprint floor.

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

## 9. The audit (2026-10-24): a third defect, and the one that mattered

The two proofs above were the proof of a tree the guard had been built to
find. An audit of the whole pass, asked the obvious question — *does this
actually catch what it claims to?* — found that the answer for the worst
case was no.

### 9.1 The pass decided "is it still in use?" with LIFETIME CPU

The root selection carried a gate that skipped any tree whose root had
burned more than `reap_report_max_cpu_seconds` (60) of CPU. A tree that
crossed it was `continue`d: not reported, not weighed, not reaped, and with
no message saying so.

That is the wrong measurement, and it fails in the worst possible
direction. The leak that MATTERED is a leak that did real work — a browser
that rendered pages, ran scripts and was then abandoned. The more work the
leaked tree did, the more certain the guard was to ignore it. Every tree the
guard reported on 2026-10-24 had **two seconds** of CPU, because that was
the only kind of tree it could see.

The audit found the shape it had been hiding: an orphaned, `--headless=new`
browser tree of **20 processes holding 1523 MiB**, with **216 seconds** of
lifetime CPU, still burning half a core, whose DevTools port had no client
attached. The guard had never reported it.

### 9.2 The question that was actually being asked

"Is it still in use?" is not "how much has it done". A browser that a
client drives over the DevTools protocol holds an **ESTABLISHED
connection** to its debugging port; a browser nobody drives holds none.
Chromium keeps LISTENING on that port either way, so the port being open
proves nothing — only a live connection does.

The port comes from `--remote-debugging-port=`, or from the profile's
`DevToolsActivePort` file when the flag is `0` and the OS picked it. The
answer is three-valued:

| verdict | meaning | what the guard does |
|---|---|---|
| `Driven` | a client is attached right now | never ends it; REPORTS it |
| `Undriven` | it exposes a DevTools endpoint and nothing is attached | a candidate |
| `Unknown` | no DevTools endpoint, or the profile could not be read | the conservative CPU proxy still decides |

`Unknown` keeps the old proxy, because a tree with no debugging port cannot
be driven over one — there is nothing to check, and the conservative answer
is the only safe one.

The kill-time gate re-asks the question immediately before the signal,
because a client can attach between the scan and the reap. A tree with a
client attached at that moment is not ended at all.

### 9.3 A tree left alone is reported, never silent

The pass used to `continue` past a tree it had decided not to touch, so the
operator's own Chrome, an in-use browser and a busy tree all looked
identical to "nothing there". The report now carries a `protected` section
— the tree, its weight and WHY it was left alone — and the pass log says
so. Silence is the one thing this pass must never do.

### 9.4 The live proof

The new binary was deployed and the very next pass found the tree the old
one had never reported:

```
🧹 forgotten orphan tree: chrome pid=2714042 4 processes 257MiB (root 251MiB) age=11413s cpu=248s — headless
🛡️ forgotten-reap: tree 2714042 (4 processes, 257MiB) -> 4 ended, 0 left, 0 skipped
```

**248 seconds** of lifetime CPU — four times the old gate — and it was
weighed, reported and ended. Under the old binary it would have been
skipped in silence, forever.

Three minutes later the guard reaped another tree the harness had left
behind in the meantime:

```
🧹 forgotten orphan tree: chromium pid=2773863 9 processes 432MiB (root 133MiB) age=315s cpu=1s — headless
🛡️ forgotten-reap: tree 2773863 (9 processes, 432MiB) -> 9 ended, 0 left, 0 skipped
```

Thirteen processes ended, none survived, none was skipped. The operator's
own Chrome (PID 59843) was alive and untouched throughout — 59 processes,
2.2 GiB RSS, still vetoed as a live desktop session.

**One honest caveat.** The tree was 1523 MiB when the audit measured it
and 257 MiB when the guard caught it: Chromium shed 16 of its 20
processes on its own over three hours, and what remained had been swapped
out. It was caught by **1 MiB** of headroom over the 256 MiB floor. The
floor is right for the tree's FRESH weight and for its steady state
together, but a tree that shrinks further would slip under it — which is
why the age floor, the DevTools answer and the pressure gate all exist
alongside it. The honest summary is that the floor is a necessary condition
and not a sufficient one.

### 9.5 The parser that made the answer impossible

The first implementation of the DevTools check read `/proc/net/tcp` wrong,
and a test caught it. A row begins with the **slot** column (`42:`), which
is not an address; the implementation took the first whitespace-separated
fields as the local and remote addresses, so the addresses were never
compared and **no live client could ever have been detected**. The answer
would have been a permanent "no".

The port column is also **hexadecimal**, and getting that wrong turns a
live client into an invisible one: the leaked browser's DevTools port 41549
appears in `/proc/net/tcp` as `A24D`. An earlier fixture in the test used
`A2A5` (41637) while asserting the port was 41549 — an assertion that
could only ever have been satisfied by a bug. The columns are now located by
shape rather than by position, and a test pins the conversion.

## 10. The other half of the leak: the profile directory

Reaping the processes is only half of what a leaked browser costs. The
other half is the profile directory the browser owned: on this host, about
**850 MiB** per tree.

The guard's own measurement on 2026-10-24, after the tree path had been
proven:

| profile | size | age | owner |
|---|---|---|---|
| `cw-real-profile-standard-1792777061299` | 833 MiB | 295 min | none |
| `cw-real-profile-standard-1792777103439` | 839 MiB | 294 min | none |
| `cw-real-profile-standard-1792777711199` | 828 MiB | 294 min | none |
| `cw-real-profile-standard-1792778314328` | 860 MiB | 294 min | none |
| `cw-real-profile-standard-1792779219790` | 838 MiB | 294 min | none |
| `cw-real-profile-standard-1792782158510` | 825 MiB | 294 min | none |
| `cw-real-profile-standard-1792800354167` | 842 MiB | 5 min | none (the tree the guard had just reaped) |

**5.9 GB**, every one of them the residue of a browser that was already
dead. The last row is the proof that the residue is not incidental: it is
the profile of the very tree the guard had just reaped three minutes
earlier.

The tmp sweep would have got them — eventually. The general age floor on
this host is 6 hours, and these are 4.9 hours old, so they were minutes
from being swept. But the sweep is a general mechanism and the floor is
conservative by design; the CI leaks a browser tree every few minutes, so
the residue was growing at roughly **1.2 GB/hour** while `/` sat at 87%.

### 10.1 Why the guard may reclaim it now

The age floor exists because the tmp sweep has no way to know whether
something is still in use — it can only guess from a timestamp. The guard,
at reap time, has something much better: it has just **proved** that the
browser is dead and abandoned, by ending its whole tree and confirming
that no member survived.

That proof is a stronger statement than "this directory is six hours old",
and it is exactly the statement the age floor was approximating. So the
guard reclaims the profile at reap time, and the six-hour wait stops
applying to a directory whose owner is known to be dead.

### 10.2 What the reclaim will not touch

The proof is re-checked at the moment of removal, because it was earned a
moment ago and the world moves:

- the tree must have been **fully ended** — a member that survived means
  the browser may still be running, and one surviving member is enough;
- the profile must live under one of the guard's **validated tmp roots**.
  This is the check that keeps the operator's real
  `~/.config/google-chrome` out of reach, however the tree that named it
  ended. A headless browser pointed at a real profile is still a bad
  neighbour, but its profile is not the guard's to delete;
- no live process may **name it in its command line** — a browser that
  started on the same path after the old one died is exactly the moment
  when deleting it would break that browser;
- nothing may hold a file **open** under it, and no live unix socket may
  live in it. The socket probe is separate for the same reason as in the
  tmp sweep (the codex app-server incident, 2026-10-21): a bound socket is
  an anonymous inode, invisible to the open-file scan;
- it must pass the guard's normal **protected-path and symlink** checks.

Every check that can fail closed does. If the open-file scan cannot be
run, nothing is removed — an empty set would silently disable the
protection, which is the failure mode that cost the codex daemon its
control socket.

The knob `reclaim_reaped_browser_profiles` (default true) turns the whole
thing off, and the profile goes back to waiting out the tmp age floor.

### 10.3 The live proof

A tree was created deliberately for the purpose — a headless Chromium
launched with `--user-data-dir=/tmp/proof-profile-reclaim-1792813146`,
orphaned (reparented to systemd), and left alone.

The first build of this proof FAILED, and the failure is §9.6: the guard
did not report the tree at all. It weighed 379 MiB to anything that
counted its members and **121 MiB** to the guard, which weighed the root
alone and stopped there. After the fix, the same tree, unremarkably:

```
🧹 forgotten orphan tree: chrome pid=3888221 10 processes 369MiB (root 120MiB) age=1259s cpu=4s — headless
🛡️ forgotten-reap: tree 3888221 (10 processes, 369MiB) -> 10 ended, 0 left, 0 skipped
🧹 reclaimed browser profile: /tmp/proof-profile-reclaim-1792813146 (201.7 MiB)
```

Ten processes ended and the profile went with them — in the same pass,
instead of six hours later. None survived, none was skipped.

Two things about the shape of that proof are worth recording, because both
would have looked like "the guard is not working":

- the tree's root had **120 MiB** and its ten members **369 MiB**. A guard
  that weighs the root sees 120; the floor is 256. The leak sits exactly
  in the gap between the two measurements, which is why the defect could
  hide a tree this plain;
- the reap fired only once the pressure state had been sustained for 120
  seconds. The daemon had been restarted for this deployment, so its
  pressure state began at "ok" and had to earn its way to "warn" — four
  passes of "reported but not ended" before the gate opened. That is the
  gate working, not the gate failing: the tree was reported on every one
  of those passes, so the operator could see exactly what was waiting.

The operator's own Chrome (PID 59843) was alive and untouched throughout —
56 processes, 2.2 GiB RSS, still vetoed as a live desktop session.

### 9.6 A third defect, found by the proof of the second fix

Building the live proof for §10 (reclaiming the profile directory) needed
a deliberate leak of its own. It was created, orphaned, and left alone —
and the guard **did not report it**.

The tree was 9 processes holding 379 MiB (RSS 203 + swap 215), orphaned,
headless, 500 seconds old, on a host in critical memory pressure. Its
DevTools port had only a LISTEN socket and no ESTABLISHED connection. It
passed every gate the documentation described.

It was invisible because of a defect the same audit had already found
elsewhere and fixed in one place but not in another.

**The exempt list names APPLICATIONS, and every process of a `chrome` tree
is named `chrome`.** The operator's exempt list contains `chrome`. The ROOT
of a browser tree is admitted despite that exemption — for a browser, the
desktop veto decides instead of the name, which was fixed earlier in this
work. But the tree BUILDER still applied the name exemption to the tree's
DESCENDANTS while it was assembling them.

Two consequences, both wrong:

- the tree's **aggregate weight was measured as the root's alone**. The
  proof tree weighed **122 MiB** to the guard and **379 MiB** to anything
  that actually counted its members. The 256 MiB floor is between those
  two numbers, so the leak was under the floor and the guard could not see
  it — in the very pass that exists to see it;
- when such a tree **was** ended, its helpers were not in the tree, so
  they were never signalled and survived it.

The kill-time gate had this right all along — a browser helper inside a
dead test's tree is that job's own machinery, not one of the operator's
applications — so the tree that was **weighed** and the tree that would
have been **ended** disagreed with each other. The builder now agrees with
the gate, and the two are the same tree.

A **non-browser** tree keeps the exemption exactly as before: an exempt
daemon inside a dev-server tree is still the operator's, and is neither
ended nor counted as reclaimable.

### 9.7 What the audit turned up, in one place

Three defects, all the same shape — a gate that was right in one place
and wrong in another, or right in principle and wrong in the measurement:

| # | the gate | what it did |
|---|---|---|
| 1 | lifetime CPU as "is it in use?" | hid every leak that did real work |
| 2 | the exempt list applied to a tree's descendants | weighed a browser tree as its root, and left its helpers running |
| 3 | silent `continue` on a tree left alone | made "weighed and left alone" look identical to "not there" |
| 4 | lifetime CPU as "is this worth reporting?" | hid the operator's own 10.2 GiB browser, forever |

Each of them was found by asking the guard a question the documentation
claimed it could answer, and watching it not answer it.

### 9.8 The fourth defect: the biggest tree on the host was invisible

Defect 1 was the lifetime-CPU proxy in the REAP path. The same proxy
stood in the REPORT path, and nobody had noticed because the second one
is quieter: the veto path skipped any desktop tree whose root had burned
more than `reap_report_max_cpu_seconds` of lifetime CPU, so a busy
desktop tree was skipped **before it was ever weighed**.

On this host that meant the operator's own Chrome: 65 processes, 10.2 GiB
of resident-plus-swapped footprint, 8.3 hours of lifetime CPU. The guard
had never reported it — not once in the whole journal — and it had never
measured it either. The busier and heavier a tree was, the more certain
the guard was to hide it, so the single largest tree on the machine was
the one the operator could never see.

The fix is to gate the veto report on WEIGHT — the same per-process floor
the "heavy trees left alone" report already uses — instead of on how much
work the tree did. A desktop session is still never a candidate and is
never ended; it is now weighed and named, which is exactly what the
veto report was written to do.

This is the whole shape of the audit, once more: the same measurement
error, in a third place, and each occurrence hid a different thing.
