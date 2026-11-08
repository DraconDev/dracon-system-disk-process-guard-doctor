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
restart produced **1** event. Same episode, same inputs, ~3x fewer
events, and the one that remains is the periodic summary repeat rather
than a flip. The behavioural proof is in §3's falsification check; this
is the same effect observed on the live stream.

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
- The three user-scope watchdog timers (`dracon-sync-watchdog.timer`,
  `dracon-freeze-watchdog.timer`, `dracon-system-guard-watchdog.timer`)
  were already installed, enabled and active; they are verified, not
  modified.
