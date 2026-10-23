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
| `forgotten_orphan_tree_footprint_mb` | 1024 MiB — the tree's AGGREGATE |

A tree qualifies when **either** its aggregate crosses the tree floor
**or** its root crosses the per-process floor. The per-process trigger
keeps its 2026-10-22 meaning exactly; the aggregate trigger is new and is
separately controllable (`forgotten_orphan_tree_footprint_mb = 0` disables
it and leaves the old behaviour).

1 GiB is an order of magnitude below what actually cost that day (8.6 GB
across 792 processes), and a leaked tree holding 1 GiB of RAM+swap is
unambiguously worth reclaiming.

### 3.2 The crashpad handler belongs to the tree

Chromium's crashpad handler calls `setsid`: it leaves the browser's
process group and session, and when the browser dies it is reparented to
init as a lone orphan. Left alone, it is exactly the residue the tree
pass exists to remove — and it is invisible on its own, because it has no
`--user-data-dir` and is therefore vetoed as a "desktop session".

It is tied back to its tree by the `--crashpad-handler-pid=<pid>` its
siblings carry. A handler is only pulled into a tree when it is itself an
orphan: a handler that still has a live parent belongs to a LIVE browser
and is never touched.

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

That is the operator's policy, and it was deliberate — "SIGKILL of it logs
you out of every site". It is not widened here. The desktop veto is the
precise instrument for "do not touch my live session", and it protects the
operator's Chrome whether or not the name exemption is present; removing
`chrome` from the list is what would let the forgotten pass see the
google-chrome test trees, and that is the operator's call.

Measured on this host while verifying the change, five qualifying trees:

| root | binary | processes | aggregate | caught? |
|---|---|---|---|---|
| 3335362 | google-chrome | 25 | 1083 MiB | no — `chrome` is exempt |
| 3350461 | google-chrome | 21 | 1020 MiB | no — `chrome` is exempt |
| 3897063 | chromium | 9 | 386 MiB | **yes** |
| 3816878 | chromium | 9 | 365 MiB | **yes** |
| 554746 | chromium | 9 | 299 MiB | **yes** |

So this change makes roughly 1 GiB of already-leaked browser memory
reclaimable where the old guard could see none of it, and leaves the
operator's google-chrome policy exactly as it was.

## 5. The knobs

| knob | default | meaning |
|---|---|---|
| `forgotten_orphan_footprint_mb` | 2048 MiB | the ROOT's own resident-plus-swapped weight floor |
| `forgotten_orphan_tree_footprint_mb` | 512 MiB | the tree's AGGREGATE resident-plus-swapped weight floor; 0 disables the aggregate trigger |

Both are carried by the crate's defaults, the NixOS operator module, the
example policy and the live policy, and `scripts/check-guard-knob-parity.sh`
asserts that they agree.

The aggregate floor was measured, not guessed. A single orphaned headless
Chromium tree on this host (root + 8 renderers) held **807 MiB** when it
was fresh, and fell to **329 MiB** once the kernel had swapped most of it
out and discarded the dirty pages. A floor of 1 GiB therefore MISSES a
single leaked browser tree — the natural unit of leakage — and only
catches it once a second one has piled up.

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
- **The crashpad residue.** The crashpad handler is pulled back into its
  tree by `--crashpad-handler-pid`, but ONLY while the browser that owns it
  is still alive. A handler whose browser is already gone has nothing left
  to tie it to: it is a lone orphan of about 2 MiB, it classifies as a
  desktop session (it carries no `--user-data-dir`), and it is too small
  to ever cross a floor. This host has eight of them, about 16 MiB in
  total. It is residue, not a leak, and it is not worth a mechanism.
- **A browser farm's detached browsers.** A browser that was launched days
  ago, detached on purpose, and is still driven over CDP by a client in
  another process tree passes every gate: orphaned, headless, idle,
  heavy, old. The age floor does not catch this one — the browser is old,
  it was only just detached. This is a theoretical false positive and it
  is inherent to the shape, not to this implementation; the operator's
  lever is the exempt list or a higher footprint floor.
