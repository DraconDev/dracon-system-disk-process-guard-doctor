# Performance incident and source qualification — 2026-11-01

## Outcome and scope

The incident evidence shows severe paging/reclaim and I/O stalls, with guard
pressure detection and safety refusals already operating. It does **not** prove
that the portal owned the released swap, that the guard was absent, or that
nominal zram capacity was allocated RAM. The subsequent source repairs address
swap-aware candidate coverage and add separately bounded, report-only service
growth and tmux persistence diagnostics.

**The targeted regressions and all four required parent-workspace gates passed
on the frozen source state identified below.** This is source/test
qualification, **not deployment or proof that the installed guard executes the
repairs**. No binary was installed, live policy/zram changed, observed workload
or service signalled, or epoch-spool reclamation resumed. Test fixtures own
their temporary files/processes/endpoints; they are not authority over live
services or tmux producers.

## Incident findings, not retrospective attribution

The [incident record](incident.md) maps timestamped claims to
[original tool observations](raw/export-observations.json),
[historical journal extracts](raw/journal-extract.json) and capture provenance.
It establishes high memory/IO stall observations, interval swap traffic and
explicit guard warning/mitigation/refusal evidence. Historical executable and
policy identity, portal VmSwap/PSS, workload ownership of the swap decline and
completed useful autosaves were not captured. Coincident improvement after
historical actions does not establish exclusive causation or sustained recovery.

The [zram assessment](zram-assessment.md) separately distinguishes logical
capacity, stored original data, compressed payload, driver-reported RAM and
lower-priority disk-swap allocation. It retains units/timestamps and directional
current paging/stall intervals. Occupancy alone is not active thrashing; a
populated-device shrink can require evacuation and increase reclaim, latency
or OOM risk rather than guarantee RAM relief. It recommends evidence and safe
future validation, **not a resize or a promised saving**. Its documented existing
CLI rounding/unknown-field limitations were not repaired or deployed here.

## Actual repaired source behavior

| Area | Source behavior and protected boundary | Targeted evidence on the qualified source |
|---|---|---|
| Swap-aware offender coverage | Every identity-verified process row gets a bounded swap-read attempt **before** memory shortlisting. RSS-plus-known-swap lower bounds can include a mostly swapped low-RSS process. Unknown swap remains null, and missing coverage outside the shortlist is disclosed. Process incarnation/exemption checks and existing mitigation authority remain intact. This selection change can affect existing offender ranking; it is not itself an automatic-recovery feature. | [8 passing attribution tests](workspace-qualification/attribution.log), including the low-RSS/high-swap fixture and old-algorithm control, unknown versus measured zero, churn, exemptions, overflow and input-order invariance. [Implementation and earlier discrimination evidence](swap-selection/README.md). |
| Service memory growth | The existing guard pass emits separate `service_memory_growth` observations/rate-limited events for explicit system/user targets. Cgroup-v2 charged RAM and swap are **not RSS/PSS or a heap-leak diagnosis**. Generation-bound monotonic windows distinguish sustained growth from burst/plateau and RAM-to-swap migration; missing/racing/reset/gap observations reset history. **Report-only:** no mitigation eligibility, restart, memory cap, quota or admission action is added. | [15 passing growth tests](workspace-qualification/growth.log), including sustained counters/window/rate, noisy plateau/migration controls, irregular timing, restart/PID/cgroup changes, unavailable provenance, bounded query-only collection, policy/reload handling and target reordering. [Implementation/limits](service-growth/README.md). |
| Tmux persistence health | `doctor` human/JSON reports bind each socket/server to separate producer, snapshot and launch-marker observations. Existing/sleeping/stopped state is not useful-save success or a stop-cause diagnosis. Fresh/stale bounded snapshot evidence does not prove periodic execution, producer attribution, restorability or pane-content backup. Shared evidence cannot be borrowed by another selected server. **Report-only:** no recovery, SIGCONT, new daemon or changed strict-mode requirement. | [9 passing persistence tests](workspace-qualification/persistence.log), covering fresh/stale/stopped facts, multi-server cross-binding and shared files, unknown provenance, lifecycle races, query-only/file preservation, delayed-server clock handling, parser/budget and special-file rejection. [Implementation and earlier one-shot observation](tmux-health/README.md). |

Tests are behavioral assertions in
[`memory_sampling_tests.rs`](../../../src/memory_sampling_tests.rs),
[`service_growth_tests.rs`](../../../src/service_growth_tests.rs) and
[`tmux_health_tests.rs`](../../../src/tmux_health_tests.rs). Logs list the actual
executed cases, not just a hand-maintained check list. Counts describe these
runs, not permanent test-count requirements. The full workspace run also
executes these regressions. Earlier feature reports retain isolated mutation
controls; those controls are supporting evidence, not substitutes for Cargo
gates or claims of a new production workload experiment.

## Coherent source state and isolation

[Source-state manifest](workspace-qualification/source-state.json) records the
2026-11-01 **23:17 UTC** capture, exact repository revisions/trees and source
hashes. The parent workspace and all nested repositories were frozen together:

| Repository | Qualified revision |
|---|---|
| Parent workspace | `c6b7b89a067149c557948646da323eceb53e0d64` |
| dracon-system | `f6ad34189e6b988eaae01e8125a50059abe3a8ae` |
| dracon-sync | `6582ecbef357bb07ad987cd8075ead68d8f30c9d` |
| dracon-warden, including security member | `e68ec3615210c57cf27f0e0ac0ab500fa9330db4` |

The gates ran **from the frozen parent workspace**, with its unchanged lock,
deny configuration, workspace members and serial test settings. A separate
`CARGO_TARGET_DIR` prevents overwriting the canonical debug epoch helper. Its
revision-qualified helper points into the same frozen target. Two build jobs
and disabled incremental compilation bound build concurrency/storage; no test,
warning, policy or dependency check was removed.

The required ignored bucket-runner Node SDK was copied and
[fingerprinted](workspace-qualification/sdk-assets.json); its links stay inside
the frozen snapshot. No credentials/configuration were copied to make tests
pass. A checkout filter transformed one unrelated Warden stress-test fixture
while cloning. **Before any gate**, comparison detected it; only the owned
snapshot copy was aligned byte-for-byte with the clean canonical source.
Both hashes and the reason are retained in the manifest. The canonical fixture
was not edited, and every recorded source input matched the canonical source
before qualification. [Toolchain versions](workspace-qualification/toolchain.json)
complete the build context.

Concurrent work subsequently changed four canonical dracon-sync source files;
[post-gate comparison](workspace-qualification/post-gate-source-comparison.json)
records that drift. The qualified frozen source stayed unchanged, and the
preceding dracon-system repairs still match it. **These passes do not qualify
newer sibling edits or claim that an ever-moving live HEAD is green.** Those
edits were neither reverted, overwritten nor silently included after testing.
Any newer state needs its own qualification; no external conflict is called a
pass by borrowing this snapshot's result.

## Required gates and raw outcomes

[Runner](workspace-qualification/run_gates.py),
[targeted commands/statuses](workspace-qualification/targeted-results.json),
[first workspace statuses](workspace-qualification/workspace-results.json) and
[remaining gate statuses](workspace-qualification/remaining-results.json)
retain argv, parent cwd, explicit environment, deadlines, UTC intervals, exits
and raw-log hashes. Required commands were not narrowed or weakened:

| Gate, from frozen parent | Outcome | Raw evidence |
|---|---|---|
| `cargo test --workspace --locked` | Exit 0; 3,016 aggregate passes, zero failures across 44 summaries, including nested helper-process summaries (not a unique-test count). 27 existing ignored tests remain explicitly reported by the standard command; no ignore attributes or filters were added for this run. | [Workspace tests](workspace-qualification/workspace-tests.log) |
| `cargo clippy --workspace --locked -- -D warnings` | Exit 0; strict warnings remain enabled. | [Workspace Clippy](workspace-qualification/workspace-clippy.log) |
| `cargo build --release --locked` | Exit 0; release artifacts stayed inside the isolated target, not installed. | [Release receipt](workspace-qualification/workspace-release.log) |
| `cargo deny check` | Exit 0; advisories, bans, licenses and sources all report `ok`. | [Dependency/policy gate](workspace-qualification/workspace-deny.log) |

The parent command wrapper was interrupted during the first release attempt;
its child compilation was observed still running. That attempt's exit is
**unknown**, not a pass. [Interruption record](workspace-qualification/interruption.json)
and [original log](workspace-qualification/workspace-release-interrupted.log)
are preserved. Without signalling that build, the exact build was reissued,
waited for its lock, and returned a recorded exit 0; deny then returned 0.
Earlier test/Clippy results were retained, not inferred or overwritten.

Reproduce in the **identified** full workspace with the required ignored SDK,
an isolated target and matching epoch helper; see runner/manifest. Do not run
the runner from inside a collected test. Reproduction on newer dependencies,
advisory databases or source revisions is a new result, not retroactive proof
that those inputs passed this run.

## Installed runtime and operational limitations

The [read-only runtime observation](workspace-qualification/runtime-observation.json)
and [unit properties](workspace-qualification/runtime-properties.stdout) still
show the same user guard PID/start timestamp as the
[incident-era capture](raw/guard-user-properties.stdout), launched from the
installed local path. This is unit-continuity evidence, **not** a historical
executable digest, loaded-policy snapshot or receipt that the new code is
loaded. The source snapshot binaries were never installed or used to replace
that service. Source tests must not be advertised as deployed monitoring or
incident mitigation. No live policy, Nix pin, zram or service state was changed.

Remaining limits are operationally significant:

- VmSwap is not comprehensive PSS/job ownership. Unknown readings may exceed
  their lower bounds; sampling can race and does more bounded status reads
  than the old incorrect shortlist-only path.
- Cgroup charge changes can include cache/kernel memory and do not prove a
  heap leak. Unsupported accounting/namespaces remain unavailable; history
  resets and short/noisy growth can be missed. One-shot guard runs do not
  inherit daemon history. Collection has count/byte/depth/time budgets, not a
  guarantee that a stalled kernel filesystem syscall is preemptible.
- Known-producer tmux registration and explicit safe save paths are required
  for that provenance. Other producers may exist; missing provenance is not
  absence or healthy saving. Unchanged snapshots may be intentionally retained.
  No useful backup/restore or resumed autosave was demonstrated for the incident.
- Current short zram observations are not sustained recovery evidence and do
  not identify an optimal algorithm/capacity/swappiness or guaranteed savings.
- New configuration fields are defaulted and tested, not applied live. Existing
  Nix scalar overrides do not support target/socket arrays; those use the
  documented TOML/custom-policy path. Report-only features add no repair rights.
- Deployment, service control, workload admission, browser cleanup and paused
  epoch-spool reclamation are outside this qualification. Their absence is
  deliberate, not a failed gate recast as operational success.

## Evidence verification and independent review

The [stored-evidence checker](workspace-qualification/verify.py) verifies exact
required argv/exit receipts, raw-log hashes, nonempty passing targeted tests,
frozen source/SDK integrity, isolated target/helper paths and local citations.
It keeps the interrupted exit unknown and rejects drift in the performance
source rather than silently lending old results to new code. Its
[raw outcome](workspace-qualification/verification.log) and
[derived summary](workspace-qualification/verification.json) retain the report
hash and distinguish qualified frozen inputs from unqualified sibling edits.

A [fresh-context review](workspace-qualification/review.md) independently
inspected the source assertions, production wiring, receipts and scope. No
substantive issue was found. The reviewer had no execution tool: this is not
an independent rerun or digest recomputation. The parent retained execution
outputs; final acceptance remains separate from that rehearsal.

The next useful operational step is a separately approved deployment and
observational validation plan, not an automatic restart or tuning action from
this report.
