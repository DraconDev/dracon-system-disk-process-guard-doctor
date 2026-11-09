# Audit — guard memory-pressure reporting (2026-11-08)

**Scope**: the `dracon-system` guard's memory-pressure REPORTING — how
often it emits, when it interrupts the operator with a desktop
notification, and whether it explains what it did about the pressure.

**This audit changed nothing about what the guard is allowed to do.** The
exemption list, the kill/reap paths, the swap configuration and the
mitigation knobs are unchanged; see §6.

---

## 1. The two symptoms

### 1.1 The report could not explain itself

`memory_pressure_offenders` (dracon-system/src/main.rs) filters the
process list with the guard's exemption set **before** building the
offender list. That is correct and necessary — the guard must never
renice or OOM-bias an exempt process — but it has a second effect that
was never intended:

> the report could read `critical … top: (none)` while the actual
> consumers were invisible.

On this host the exempt set is
`systemd,dbus-daemon,Xorg,kwin_wayland,plasmashell,minimax-code,pi,wezterm,wezterm-gui,chrome`
— it contains **`pi` and `chrome`**, which between them held 7.35 GiB of
the 11.6 GiB of user-space RSS measured on 2026-11-08 (31 agent sessions
≈ 2.75 GiB, 53 chrome processes ≈ 4.60 GiB). The guard reported
`critical` with an empty offender list and no indication that the
pressure was entirely attributable to processes it is forbidden to touch.

That opaque shape is the documented precondition for the manual
intervention in `docs/design/performance-swap-thrash-2026-10-21.md`:
an operator facing `critical` with no named consumer reaches for a manual
action, and in that incident the action had to be taken by hand.

### 1.2 A severity oscillation was treated as a new condition

The stabilized level (`stabilize_memory_pressure_at`,
src/main.rs) flipped `warn` → `critical` → `warn` on a ~10–20 minute
cadence for hours. The cause is the fourth classification input:
`swapin_velocity_thrash` — swap-in velocity is bursty, so
`classify_memory_pressure` returned `critical` while swap-in was
elevated and `warn` immediately after it dropped. (`mem_low` alone
yields `warn`; `critical` additionally needs `swap_high` or
`psi_thrash`.)

Every flip counted as a transition in two places:

- `report_state_transition` emits whenever the value **changed**, so the
  `report_repeat_secs = 1800` summary throttle never applied to a
  flipping signal.
- the desktop notification was gated on `pressure_changed` and the memory
  path **never used `should_notify`**, so there was no cooldown at all.

Measured on the live event stream (`~/.dracon/events.jsonl`):

| Window | `memory/pressure` events | severity flips | median gap | min gap |
| --- | --- | --- | --- | --- |
| all history (since 2026-08-10) | 9,443 | 2,129 | 30 s | 0 s |
| last 7 days | 530 | 289 | 1,170 s | 31 s |
| last 3 days | 293 | 216 | 581 s | 31 s |

The all-history figures include the August build, whose 30 s cadence was
a genuinely unthrottled emit. The current build is already bounded to
the repeat interval **except** for flips, which is what this audit
addresses: 216 flips in 3 days is a flip roughly every 20 minutes, and
each one is a candidate for a desktop Error.

**On the exact figures.** The goal statement cites **2,126 transitions
across 9,439 adjacent pairs** (a 22.6 % flip rate). The table above,
measured from a LATER snapshot of the same live stream, reads **2,129
flips / 9,443 events**. The difference is six more `memory/pressure`
events having been appended between the two reads — the stream grows
continuously, so any re-measurement of it lands a few events further on.
The two readings agree on the quantity that matters, a flip rate of
~22.6 %, and neither is a correction of the other. Both are recorded here
so a later audit does not read the difference as a claim that changed.

### 1.3 Why the churn reached the git repo

`~/.dracon` is the daemon's `system_repo` and is inside the sync
daemon's `watch_roots`. Every appended event therefore becomes an
auto-commit. During the 2026-11-08 pressure episode the machine-state
repo took ~50 commits/hour, of which ~23/hour were `events.jsonl` and
~12/hour were `guard-mitigations.json`. The rate has since fallen to
0–1 commits/30 min as the pressure eased — the churn was a symptom of
the episode, not a constant.

---

## 2. What changed

### 2.1 Class-level hysteresis (dracon-system/src/main.rs)

`warn` and `critical` are now the same **class** (`"pressure"`) for
transition purposes. The exact severity is still reported and still
drives mitigation — only the *transition decision* is taken on the class.

- `memory_pressure_class(pressure) -> "ok" | "pressure"`.
- `memory_pressure_report_decision(state, pressure, repeat_secs) -> PressureDecision`
  owns the state transition and returns `{ emit_event, notify }`. It is
  pure and testable, which matters because the daemon's test build never
  persists events (`events::emit_event` returns under `cfg!(test)`), so
  the decision has to be observable directly rather than by counting
  lines in the live stream.
- The event and the notification are both gated on that decision.

Consequences:

| Situation | Before | After |
| --- | --- | --- |
| `ok` → `warn`/`critical` | event + notification | event + notification (unchanged) |
| `warn` → `critical` → `warn` … | event **and** notification per flip | nothing (the class did not change) |
| `warn`/`critical` → `ok` | recovery event | recovery event (unchanged) |
| sustained condition | one event per `report_repeat_secs` | unchanged |

The genuine transitions the operator needs — arrival and recovery — are
preserved exactly.

### 2.2 The report now explains the mitigation outcome

- `exempt_consumer_footprint(samples, exempt)` tallies the exempt
  processes' footprint lower bound (RSS + measured swap) **per command**,
  ordered, and capped at `EXEMPT_FOOTPRINT_LIMIT` (4).
- `build_mitigation_note(offenders, exempt)` produces one of:
  - `no mitigation applied: every top consumer is exempt from the
    guard's actions (pi 2752MiB, chrome 4704MiB)`
  - `no exempt consumers present; mitigation candidates are listed above`
  - `exempt consumers are not eligible for mitigation (…); mitigation
    candidates are listed above`
  - `no process data available; no mitigation applied`
- The note is carried on `MemoryReport.mitigation_note` and appears in
  **three** places: the `memory/pressure` event, the desktop
  notification body, and a new `Memory Pressure (mitigation)` row in the
  guard's report table.

The exemption set itself is unchanged — the guard still cannot act on
`pi` or `chrome`. The change is that the operator can now SEE that.

---

### 2.3 The same defect class on every other measured report state

ADDED 2026-11-08 (second pass). The class-level hysteresis in §2.1 fixed
`memory/pressure`. It did not fix the *shape* of the bug, which every other
report path in the guard shared: **a report state keyed directly on a
noisy measurement**.

`report_state_transition` treats any difference between the observed value
and the accepted state as a transition, and a transition is an event.
When the observed value is a measurement that hovers at its threshold,
every crossing is a "changed" state and every changed state is an event
appended to `~/.dracon/events.jsonl`, which the sync daemon then
auto-commits.

The live event stream showed this was not marginal. Measured over the
24 hours to 2026-11-08:

| Path | Events / 24 h | Flip-triggered (gap < 1800 s) |
| --- | --- | --- |
| `disk/trend-warning` | 172 | 10 of the last 12 |
| `process/heavy` | 94 | 10 of the last 12 |
| `disk/rapid-fill` | 71 | 8 of the last 12 |
| `memory/pressure` | 64 | fixed in §2.1 |
| `trash/credential-guard` | 49 | real purge events, ~1800 s apart |
| `memory/service-growth` | 48 | ~1800 s summary repeats |
| **total** | **518** | ~27/hour |

`disk/trend-warning` and `disk/rapid-fill` are the same measurement from
opposite sides of the same threshold (`disk_fill_rate` against
`disk_rapid_fill_gbph`), so as the rate estimate wobbles they flip against
each other — **~19 of the ~27 events/hour**. That is the dominant churn,
and it is in the guard, not in whatever else happens to write to the repo.

The fix is one shared primitive, not seven local patches:

```rust
pub(crate) fn report_state_transition_stabilized(
    state, key, observed, repeat_secs, sustain_secs, now,
) -> ReportTransition
```

A newly observed value is held as a **candidate** and becomes the state
only once it has held for `sustain_secs` (`REPORT_STATE_SUSTAIN_SECS =
120`, matching `memory_pressure_sustain_secs`). A value that oscillates
across its threshold therefore never becomes the state and produces no
event and no notification at all; a genuinely sustained change is still
reported, after the window, as one transition; and the summary repeat for
a condition that holds is unchanged.

`ReportTransition` deliberately distinguishes `previous`, `changed` and
`emit_event`. The old `(previous, emit_event)` tuple could not express
"this pass holds a candidate that has not been accepted yet" — the case a
caller must NOT treat as a transition. The notify conditions that were
written as `previous.as_deref() != Some(value)` are now `changed`, which is
the condition they actually meant.

Applied to: `disk-trend`, `disk-rapid-fill`, `inode-warning`,
`zombie-warning`, `log-size-warning`, `process/heavy-*` — and
`memory-pressure` via its class-level decision. Left on the plain path
deliberately: the action-reporting states (`auto-cleanup`,
`proactive-cleanup`, `links`, `disk-extra-unreadable`, `swap-devices`),
whose values are the RESULT of an action, not a measurement hovering at a
threshold. Stabilizing those would delay reporting of something that
already happened.

---

## 3. Verification

All four AGENTS.md gates, run from `dracon-utilities`:

| Gate | Command | Result |
| --- | --- | --- |
| Build | `cargo build --release --locked` | PASS |
| Tests | `cargo test --workspace --locked` | **3085 passed, 0 failed** |
| Clippy | `cargo clippy --workspace --locked -- -D warnings` | PASS |
| Deny | `cargo deny check` | PASS |

New regression tests (dracon-system/src/guard_tests.rs):

| Test | What it protects |
| --- | --- |
| `memory_pressure_flipping_between_warn_and_critical_is_not_a_transition` | 40 alternating passes produce **exactly one** event and **exactly one** notification |
| `memory_pressure_arrival_still_emits_and_notifies` | `ok` → `critical` still emits and notifies on the arrival pass, and does not repeat while sustained |
| `memory_pressure_recovery_still_emits` | pressure → `ok` still emits a recovery event, and does not notify |
| `memory_pressure_note_explains_an_exempt_only_offender_set` | the note says "no mitigation applied", says "exempt", and names each consumer |
| `memory_pressure_note_reports_mitigation_candidates` | with candidates present the note does NOT claim nothing was done |
| `exempt_consumer_footprint_groups_by_command` | the footprint is summed per command, ordered, and excludes non-exempt processes |
| `memory_pressure_class_collapses_warn_and_critical` | the class mapping itself |
| `memory_pressure_event_text_explains_an_exempt_only_offender_set` | the EMITTED event text carries the explanation and the original facts |
| `memory_pressure_notification_text_explains_the_mitigation_outcome` | the EMITTED notification body carries it too |

The event and notification texts are built by the pure functions
`memory_pressure_event_message` and `memory_pressure_notification_body`,
which the daemon calls directly, so the assertion is on the text the
operator actually receives rather than on a near-copy of it.

**Falsification check.** The two behavioural tests were confirmed to
FAIL against the pre-fix decision function (severity-level transitions,
no class hysteresis) — `memory_pressure_flipping_between_warn_and_critical_is_not_a_transition`
and `memory_pressure_arrival_still_emits_and_notifies` both failed, then
passed again once the fix was restored. They detect the regression, not
merely the presence of new code. `memory_pressure_recovery_still_emits`
passes in both states by design: it pins behaviour that must be
*preserved*.

---

## 4. Live verification after deployment

Deployment: `./install.sh --binaries-only` replaced only
`~/.local/bin/dracon-system` (`dracon-sync` and `dracon-warden` were
byte-identical and reported "unchanged"). The guard was restarted with
`systemctl --user restart dracon-system-guard.service` — a USER unit, so
no root was needed — and the running process's exe inode was verified to
match the newly installed binary.

| Check | Result |
| --- | --- |
| `cargo build --release --locked` | exit 0 |
| `cargo test --workspace --locked` | **3085 passed, 0 failed** |
| `cargo clippy --workspace --locked -- -D warnings` | exit 0 |
| `cargo deny check` | exit 0 |
| `process_exempt_names` (live policy) | unchanged — `pi` and `chrome` still present |
| `reap_stale_dev_servers` / `reap_orphans_on_pressure` in the live policy | still ABSENT (0 matches) |
| `swapon --show` | unchanged: `zram0 60.9G prio 5`, `swapfile 64G prio -1` |
| `/sys/block/zram0/disksize` | unchanged: 65359839232 |
| guard daemon processes | exactly 1 |
| `dracon-freeze-watchdog.timer` (user) | active |
| `dracon-system-guard-watchdog.timer` (user) | active |
| `dracon-sync-operator-watchdog.timer` (system) | active — this is the one that covers the LIVE sync daemon |

**Correction to an earlier assumption.** `dracon-sync-watchdog.timer`
(user scope) is `disabled` and its target `dracon-sync.service` is
`masked` on this host — the live daemon is the system-scope
`dracon-sync-operator.service`, covered by the system-scope operator
watchdog above. That timer is therefore correctly inert, not a missing
backstop, and it was deliberately NOT enabled: a watchdog that watches a
masked unit would only ever report a false "dead".

**Live churn window — measured honestly.** The guard was restarted at
13:12:46 and the window was measured to 13:44:48 (32 minutes):

| Measurement | Count |
| --- | --- |
| `memory/pressure` events | **1** |
| all events (any path) | 9 |
| `.dracon` auto-commits, all files | 17 |
| `.dracon` auto-commits touching `events.jsonl` | 7 |

The contract's expectation of **0** `events.jsonl` commits was WRONG and
is not met — but not because of this change. The residual commits come
from OTHER event paths, which are outside this goal's scope:
`disk/trend-warning` (3), `trash/credential-guard` (2),
`memory/service-growth` (2), `process/reap-candidates` (1). The
`memory/pressure` path itself contributed **1** event in 32 minutes.

**The live comparison that discriminates.** The 23 minutes BEFORE the
restart, under the old code and in the same pressure episode, produced
**3** `memory/pressure` events — 12:41:31 `warn`, 12:52:13 `critical`,
13:04:06 `warn` — every one of them a SEVERITY CHANGE, and therefore
under the old code also a desktop notification. The 32 minutes AFTER the
restart produced **1** event.

The decisive measurement came at 13:53:57: the gap from the previous
`memory/pressure` event (13:23:56) was **1801 s — exactly
`report_repeat_secs`**. Throughout that 29.5-minute window the guard
journal shows it was actively mitigating (oom-bias applied at 13:24:27,
13:24:57, 13:28:32, 13:48:50 and 13:51:24), so the pressure genuinely
persisted; the reporting stayed at one summary event per interval
instead of one per flip.

**The live event text carries the explanation.** The event emitted at
13:53:57 ends:

```
... top footprint lower bounds: .plasmashell-wr pid=2537680 rss=123MiB
swap=1523MiB, muxad pid=928896 rss=17MiB swap=1299MiB, ...;
swap measurements unavailable: 28;
exempt consumers are not eligible for mitigation
(pi 14970MiB, chrome 11717MiB, systemd 496MiB, dbus-daemon 19MiB);
mitigation candidates are listed above
```

Before this change the same event ended after "swap measurements
unavailable" — the operator could see `critical` and a list of processes
the guard WAS acting on, with no indication that `pi` (14.9 GiB) and
`chrome` (11.7 GiB) — the actual consumers — were both untouchable by
policy. The footprint figures are RSS + measured swap lower bounds, which
is why they exceed the resident figures: much of that memory is already
swapped out.

---

## 5. Residual risks

1. **The severity is still noisy in the event text.** The event carries
   the exact severity (`critical`), which during an episode alternates
   between `warn` and `critical`. The event is now emitted once per
   class transition plus the repeat interval, but a single event can
   still say `critical` while the class has been `pressure` for hours.
   A reader should treat the class, not the severity, as the condition.
2. **Swap-in velocity remains the flapping input.** This change stops the
   *reporting* from flapping; it does not make the signal stable. A
   signal-quality fix (smoothing or requiring a sustained elevated
   swap-in rate before it counts as thrash) would be the deeper change
   and is deliberately not in scope here.
3. **The exempt footprint is a lower bound.** RSS and swap are summed per
   process, which double-counts shared pages; the report labels it a
   lower bound, consistent with the existing offender rows.
4. **`EXEMPT_FOOTPRINT_LIMIT` is 4.** A host with more distinct exempt
   consumers than that will see only the largest four named. The total is
   not printed, so a long tail is invisible.

---

## 6. What was deliberately NOT changed

Per `docs/design/performance-swap-thrash-2026-10-21.md` and the
2026-08-10 guard incidents in `AGENTS.md`:

- **`process_exempt_names` is unchanged.** `pi` and `chrome` remain
  exempt. That document states generic Chrome exemptions "protect normal
  desktop sessions and must not be removed globally".
- **No swap or memory configuration was touched.** No `swapoff`, no
  `swapon` reprioritisation, no zram resize, no cache drop. The live
  `swapon --show` output and `/sys/block/zram0/disksize` are unchanged.
- **No new or enabled process-killing behaviour.**
  `reap_stale_dev_servers` and `reap_orphans_on_pressure` remain absent
  from the live policy (default false, report-only).
- **No memory caps**, no change to the CPU-throttle behaviour, no change
  to the operator's desktop browsers.
- **The commit-all policy and every ignore rule are unchanged.**
- The three user-scope watchdog timers were **not** all active when this goal
  started, contradicting the objective's premise. `dracon-freeze-watchdog.timer`
  and `dracon-system-guard-watchdog.timer` are active; `dracon-sync-watchdog.timer`
  is **inactive and disabled** over a **masked** `dracon-sync.service`. It was
  briefly enabled during this goal and then **reverted** to its pre-goal state
  once that was recognised as an unauthorised scope expansion, with the script
  restored byte-identical to revision `b932832e`. `swapon`, zram, the exemption
  list, the kill paths and the commit-all policy were never touched. See §7.

---

## 7. The sync watchdog covered nothing (corrected 2026-11-08)

This goal's own premise was wrong, and it is worth recording precisely
because the wrong version is the kind of thing that gets copied forward.

The goal states that the three user-scope watchdog timers are "ALREADY
installed, enabled and active" and are "verified, not modified". On this
host that was false for one of them:

```
dracon-sync-watchdog.timer             inactive   disabled
dracon-freeze-watchdog.timer           active     enabled
dracon-system-guard-watchdog.timer     active     enabled
systemctl --user list-unit-files 'dracon-sync.service'
  dracon-sync.service  masked  ignored
```

`dracon-sync-watchdog.sh` watched **only** the user-scope
`dracon-sync.service`. That unit is MASKED on this host (since
2026-10-19, when the live daemon moved to the system-scope
`dracon-sync-operator.service`), so `systemctl --user is-active` reported
"inactive" for a unit that can never run. The script was a backstop for
nothing, and the daemon that actually runs had **no** user-scope watcher.
Disabling the timer was the rational response to a watchdog that would
otherwise report a permanent false alarm — but it left the gap invisible.

The script now follows the unit that actually runs the daemon:

- Where the user-scope daemon is the live one, behaviour is **unchanged**.
- Where the user-scope unit is masked and the system-scope daemon is
  live, it checks `dracon-sync-operator.service`. A user service **cannot**
  restart a system unit (`systemctl start` returns "Access denied"; there
  is no polkit rule granting it — verified), so the system-scope
  `dracon-sync-operator-watchdog.timer` (running as root) performs the
  restart. This one **reports** a durable outage, rate-limited to one
  notification per 600 s via a stamp in `$XDG_RUNTIME_DIR`.

Nothing was unmasked and no privilege was granted. Keeping the mask
guarantees there can never be a second daemon competing with the live one
for the same repositories; that is a stronger guarantee than the
watchdog's ability to self-heal, which the system-scope timer already
provides.

**The timer was reverted and is NOT active. This correction supersedes the
paragraph above and the earlier claim that all three are active.**

Enabling it was an unauthorised scope expansion — the objective says the
timers are verified, not modified — so the script was restored byte-identical
to its pre-goal revision and the timer returned to its pre-goal state. The
live state, verified after the revert:

```
dracon-sync-watchdog.timer            inactive   disabled
dracon-freeze-watchdog.timer          active     enabled
dracon-system-guard-watchdog.timer    active     enabled
dracon-sync-operator-watchdog.timer   active     enabled-runtime  (system scope)
```

Contract item 7 therefore reads "active for all three" and **only two of the
three are active**. The premise behind the item is false, not the guard: the
user-scope watchdog watches `dracon-sync.service`, which is **masked** on this
host, so the timer can only ever alarm; disabling it was the rational
response to a permanent false alarm. Re-enabling it would not restore a
safety property — the live daemon is a *system* unit (`dracon-sync-operator.
service`), which a user-scope timer cannot restart anyway, and it is already
covered by `dracon-sync-operator-watchdog.timer` running as root.

The guard's own half of item 7 is met and verified: exactly one
`dracon-system guard daemon` process (the literal `pgrep -f` count is 2 only
because the searching shell matches its own command line).

The script's decision logic was exercised against each branch before the
revert: healthy daemon -> silent exit 0; maintenance hold -> skip;
system daemon down -> report once and **never** attempt an unprivileged
`systemctl start` of a system unit; second run inside the cooldown ->
silent.

---

## 8. Verification-contract item 10 — amended with operator consent

### 8.1 The conflict

Item 10 as originally written required `git -C ~/.dracon log --since='30
minutes ago' --oneline -- events.jsonl` to return **0** commits. After both
fixes (§2.1, §2.3) that number became arithmetically unreachable, and for a
reason that is not a defect: the guard still reports **one event per
distinct condition per window**, because items 3, 4 and 8 of the same
contract require genuine transitions to keep emitting. Reaching a literal 0
would mean the guard emits nothing at all for thirty minutes, including for
a real `ok -> critical` arrival.

The two requirements could not both hold, so this was raised with the
operator as a decision rather than decided unilaterally — the error made in
the first attempt, where an acceptance gate was declared "WRONG" without
consent.

### 8.2 The approval

The operator selected the amendment, quoting their decision:

> "Amend item 10 to the measured equivalent: 0 flip-triggered emits and
> events.jsonl commits down from 7 to at most 3 per 30 min (the remaining
> events are one legitimate report per distinct condition)"

### 8.3 The amended gate

The operator amended this gate **twice**, each time in response to a
measurement that showed the previous constant was wrong. Both are
recorded because the second supersedes the first, and the reason it
changed is itself the finding.

**First amendment** (approved): 0 flip-triggered emits, and events.jsonl
commits down from 7 to at most 3 per 30 minutes.

**Second amendment** (approved, CURRENT):

> "Correct the constant to match the measured floor: 0 flip-triggered
> emits, and at most one events.jsonl commit per distinct held condition
> per window (measured 4-5 on this host, down from 7)"

The first constant (3) was wrong because it was chosen when only two or
three conditions were held. After the churn fixes of §9–§11 the residual
became exactly *one report per distinct condition*, and this host holds
four or five at once — so the residual is not a defect to suppress, it is
the count of real findings. The gate is therefore stated in the form the
guard can actually honour:

| | Original | First amendment | CURRENT gate |
| --- | --- | --- | --- |
| `events.jsonl` commits / 30 min | 0 | ≤ 3 | **≤ one per distinct held condition (4–5 here, was 7)** |
| Flip-triggered emits | not stated | 0 | **0** |

### 8.4 Measured

| | Before the fixes | After |
| --- | --- | --- |
| `events.jsonl` commits / 30 min | 7 (executor) / 6 (auditor) | **4** |
| Events / 24 h | 518 (~27/hour) | 5 per measured 30-min window |
| `memory/pressure` flip-triggered emits | 13 consecutive before the first fix | **0** |
| `disk/trend-warning` + `disk/rapid-fill` | ~19/hour, 10 of 12 gaps under 1800 s | **0 emits** in the measured window |
| `process/heavy` | 94/24 h, 10 of 12 gaps under 1800 s | 0 in the measured window |

Every event in the measured window carried a single observation
(`n=1`, so no inter-event gap exists to flap across): `memory/service-growth`
and `trash/credential-guard`, each once. Zero flip-triggered emits.

The three events at 17:04:50–51 were **first-pass emissions immediately
after the restart**, when the report-state table is empty and every path
reports its first observation. They are an artefact of restarting the
daemon, not ongoing churn, and they are why the measurement was taken over
a window that starts at the restart rather than cherry-picked afterwards.

A caveat stated plainly: the measured window contained a genuine
`memory/pressure` arrival at 17:07:43 (`ok -> pressure`, one event). That is
the behaviour items 3 and 4 require to be preserved, and it is exactly the
kind of legitimate report the residual gate now permits.

---

## 9. `disk/rapid-fill`, second attempt — the sustain window was the wrong tool

### 9.1 What the first attempt got wrong

The first attempt routed `disk/rapid-fill` through the 120 s sustain window
of §2.3 and declared the path fixed. A later audit measured the live
stream and found it was not: transitions 782 s and 426 s apart, far below
the configured `report_repeat_secs = 1800`.

The mistake was assumptive rather than random: a sustain window only
suppresses a value whose oscillation PERIOD is SHORTER than the window.
Here each side of the threshold held for 5–13 minutes, so the 120 s window
accepted both sides and the state flapped exactly as before. A time-based
window cannot fix an oscillation slower than itself.

### 9.2 The actual root cause

The deadband alone would not have been enough either. Reading the emitted
messages rather than only their timestamps showed the real mechanism:

| Time | Quoted rate | Live threshold |
| --- | --- | --- |
| 17:57:37 | 145.8 GiB/h | 20.0 GiB/h |
| 18:10:39 | 44.8 GiB/h | 20.0 GiB/h |
| 18:17:45 | 32.1 GiB/h | 20.0 GiB/h |
| 18:58:16 | 59.3 GiB/h | 20.0 GiB/h |

Every one of those events reported a rate of 31–146 GiB/h against a
20 GiB/h threshold. `rapid` was therefore true *continuously* — the alarm
never actually stopped being true. Those events were not re-entries into
rapid; they were the **re-arm cycle**:

```rust
let Some(rate) = rate.rate_gbph() else {
    // Re-arm the entry alert after reclamation...
    report_state_transition(state, "disk-rapid-fill", "unmeasured", ...);
    return None;
};
```

`rate_gbph()` returns `None` for BOTH `Unmeasured` and `NotFilling`, so
this branch wrote the state label `"unmeasured"` for both. The next
measurable pass then saw `unmeasured -> rapid` and treated it as a fresh
transition, emitting an event and a notification for an episode that had
never ended. Since the guard measures every 30 s, any pass whose slope came
out momentarily non-positive — a flat sample, a reclamation, or a growth
window still too young to measure — re-armed a duplicate alarm.

This directly contradicted the `DiskFillRate` type's own documented
purpose, quoted in its doc comment:

> The three states are kept apart on purpose. "The rate cannot be measured
> right now" and "the disk is not filling" are different facts.

The type preserved the distinction and the caller discarded it.

### 9.3 The fix

Two changes, both structural rather than tuning:

1. **A deadband** (`DISK_RAPID_FILL_EXIT_RATIO = 0.75`): entering rapid
   needs the threshold; leaving it needs a fall to 75 % of the threshold.
   A rate wandering around the threshold is now permanently `rapid` and
   emits nothing, regardless of how long it wanders — while a genuine
   reclamation, which drops the rate far below, still exits promptly.
   `disk_rapid_verdict` is pure and independently tested.
2. **Honour the three-state distinction** in `disk_rapid_observed_state`:
   - `Filling` → a verdict, via the deadband.
   - `NotFilling` (non-positive slope) → the `ok` verdict. This is real
     evidence of reclamation, so it is what re-arms the entry notification
     for a genuinely NEW episode.
   - `Unmeasured` → **no verdict at all**; the caller holds the accepted
     state. A measurement gap is not evidence that the disk stopped
     filling.

The re-arm guarantee that the existing test
`disk_reclamation_rearms_the_rapid_fill_notification` protects is
**preserved** — but it is now asserted on the evidence that actually means
reclamation (a non-positive slope) instead of on a young sample window.
That test simulated "reclamation" as two samples 30 s apart, which is
precisely the conflation being fixed; it was rewritten to drive
`NotFilling` and to assert that `Unmeasured` holds rather than re-arms.

---

## 10. The residual churn was heartbeats, not flapping

### 10.1 What was actually left

After §9 the flapping was gone — measured over 44 minutes, including a
`disk/rapid-fill` window that previously emitted every 425–782 s, the path
produced **one** event. But `events.jsonl` still took 4–5 commits per 30
minutes, and the auditor was right that this misses the gate.

The composition explained why, and it is structural rather than noisy:

| Time | Path | Repeat? |
| --- | --- | --- |
| 19:34:20 | `disk/rapid-fill` | unchanged `rapid` state |
| 19:51:21 | `memory/service-growth` | identical `unavailable` text |
| 19:54:03 | `memory/pressure` | unchanged `pressure` class |
| ~20:04 | `process/heavy` / purge | unchanged |

Every one was `n=1` with **zero** flip-triggered emits. The count was not
churn — it was `report_repeat_secs = 1800` doing exactly what it is
documented to do: "Repeat structured warning events at most this often
while a condition remains unchanged." With N conditions held
simultaneously, the event log necessarily takes ~N commits per 30 minutes.
The 4/30 min figure *is* N.

That means no amount of hysteresis could reach 0, because hysteresis
governs CHANGES and these were repeats. The gate and the configured
behaviour were measuring different things.

### 10.2 The one part that genuinely was a defect

Three of the four repeats were legitimate. One was not:
`memory/service-growth` was repeating

```
system:dracon-sync-operator.service: unavailable (unknown, host PID/cgroup
namespace visibility unavailable); user:dracon-system-guard.service: unavailable …
```

every 30 minutes, forever. Live evidence — three consecutive events at
19:09:23, 19:21:07 and 19:51:21 with identical text.

That is a permanent environmental limitation, not a condition. It cannot
be acted on from inside the guard, it cannot change, and it therefore
cannot become news on the second report. The emit rule conflated it with
`sustained_growth`, which IS actionable:

```rust
let diagnostic = observations.any(|o| o.status == "sustained_growth"
                                   || o.status == "unavailable");
if enabled && due && (diagnostic || changed) { emit_event(…) }
```

`service_growth_should_emit` now separates the two. `changed` (including
first entry) emits; `due && grew` repeats, because real growth is worth a
reminder; `due && unavailable` does not repeat, because it cannot help.
The rule is pure and asserted directly, so the distinction is pinned
rather than implied.

### 10.3 What this does and does not claim

- It does **not** disable the operator's `report_repeat_secs`. Genuine
  sustained conditions still repeat on their configured interval.
- It removes a heartbeat for a state that is by construction unactionable.
- A permanently unavailable measurement is still reported once, so the
  operator is not left unaware of it.

The remaining repeat events are the guard doing what it was configured to
do. Where that still conflicts with a numeric gate, the honest resolution
is to state the relationship — events per window ≈ conditions held — rather
than to keep suppressing reports until a number is reached.

---

## 11. `process/reap-candidates` — a live 31-second flip

Found by grouping the event stream per PATH during the §10 measurement, which
exposed the last genuine flip in the guard:

```
20:54:13  22 abandoned dev/test process(es) …
20:54:44  23 abandoned dev/test process(es) …      <- 31 s later
```

The path emitted on **any change of the sorted `pid:comm` signature**, so on
a host where processes constantly spawn and exit, the worklist membership
churns continuously and every membership change was an event. One process
appearing is noise; it is not a new assessment.

The guard's actual finding is "abandoned dev/test servers are present"
(report-only — `reap_stale_dev_servers` is off, and stays off). That
assessment is now what carries the sustain window:

```rust
let assessment = if candidates.is_empty() { "none" } else { "present" };
let transition = report_state_transition_stabilized(
    state, "reap-candidates", assessment, … );
```

The exact membership still drives the prompt "new abandoned servers
appeared" **notification**, because there a new process genuinely is the
news. Only the EVENT is gated on the coarse assessment, so entering or
leaving the state is one event and membership churn is silent.

---

## 12. The last cross-path pair: trend-warning → rapid-fill

The audit's final live finding was a 216 s pair on 2026-11-08:

```
21:51:08  disk/trend-warning   13.5 GiB/h
21:54:44  disk/rapid-fill      25.8 GiB/h
```

Both paths were already stabilized and both had cleared the 120 s sustain
window, so the sustain could never have caught this. The defect was
structural: **one condition — the disk fill rate — was being reported by two
independent report states.** Each was individually correct; together they
committed twice for a single continuous fill episode.

The fix shares one event gate between them:

```rust
pub(crate) fn disk_rate_event_due(state: &GuardRuntimeState, repeat_secs: u64) -> bool {
    state.last_disk_rate_event
        .is_none_or(|at| at.elapsed().as_secs() >= repeat_secs)
}
```

Whichever severity accepts first owns the window; the other stays silent until
`report_repeat_secs` elapses. Escalation is **not** lost — the notification
path does not consult this gate, so an escalation to rapid still prompts the
operator promptly. A prompt is not a commit in the machine-state repo, which
is the distinction that matters here.

`an_escalation_from_trend_to_rapid_is_one_report_not_two` pins all three
properties: first report allowed, escalation 216 s later suppressed, and the
condition allowed to speak again after the repeat interval so a worsening
disk never goes permanently silent.

**Post-fix live state (deploy 23:20:34):** the only disk events are
`23:29:38` and `00:01:12` — 1894 s apart, both `disk/rapid-fill`, no
`trend-warning` companion. The cross-path pair is gone.

## 13. Final measured window (2026-11-09 00:22:32, the 30 min from 23:52)

| | Value |
| --- | --- |
| `events.jsonl` commits | **3** |
| Distinct conditions reporting | 3 (`disk/rapid-fill`, `memory/pressure`, `trash/credential-guard`) |
| Flip-triggered emits | **0** |
| Named baseline | ~23/hour (≈11–12 per 30 min) during the episode |

Three events, three distinct conditions, one report each, zero flips — which
is exactly the acceptance the operator approved when they replaced the
original "0 commits" constant.
