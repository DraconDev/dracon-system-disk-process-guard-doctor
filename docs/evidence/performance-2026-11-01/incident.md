# Performance incident: 2026-11-01, approximately 13:50–14:04 UTC

## Finding

The export establishes severe memory/paging and I/O stalls, not simply CPU saturation. **The guard was not absent or oblivious:** its historical journal records critical-pressure detection, the portal in its offender report, OOM steering, and mitigation refusals that protected reversibility and exempt/shared workloads. The relative contribution of each application and the exact cause of the large post-termination swap reduction remain unproven.

This item is an investigation only. No binary, configuration, process, service, browser, zram device or paused reclamation goal was changed. Source observations below describe inspected source, not an assertion that its exact revision was running during the incident.

## Evidence and provenance

- [Selected original tool observations](raw/export-observations.json): extracted without executing the HTML from `/home/dracon/.dracon/convos/pi-session-2026-11-01T13-49-49-892Z_01a1975f-a3c4-761d-8c9c-be8dac106191.html`. The artifact records its source SHA-256, timestamps and entry indices. Assistant interpretations are not treated as raw observations.
- [Capture commands, times and statuses](raw/commands.json).
- [User-service journal](raw/guard-user-incident-journal.stdout), [stderr](raw/guard-user-incident-journal.stderr), and [derived exact excerpts](raw/journal-extract.json). The query requested 13:45–14:10 UTC with a 600-line limit; the returned tail covers 13:48:40–14:09:48 UTC. Absence from this bounded capture does not establish absence from all logs.
- [Current user-unit properties](raw/guard-user-properties.stdout) identify an active user guard, PID 2798562, with a recorded start on October 25. [Current system-unit properties](raw/guard-system-properties.stdout) report that the system-level unit is not found. These are current observations, not a historical unit/configuration snapshot.
- [Selected current policy](raw/current-policy-selection.json) contains only relevant non-secret fields. Its values must not be silently backdated to the incident.

## What the incident measurements show

All entries below refer to the original-tool observation artifact, not later retellings:

| Time / entry | Observation | Interpretation and limits |
|---|---|---|
| 13:51:19 / 12 | Memory PSI `full avg10=78.09`; I/O PSI `full avg10=81.52` | Severe contemporaneous stalls. These are short-window samples, not proof of uninterrupted pressure for weeks. |
| 13:53:51 / 27 | Zram original data 64,697,733,120 bytes; compressed data 12,972,688,106; allocated RAM 13,342,056,448 | About 60.3 GiB of pages stored using 12.4 GiB of physical RAM. Device capacity and actual RAM allocation are different quantities. |
| 13:53:51 / 28 | `top` reports 19.1% I/O wait; `kswapd0` 79.8% CPU | Consistent with heavy reclaim/paging. Does not isolate all I/O wait to disk swap or exclude compilation/filesystem I/O. |
| 13:56:14 / 38 | Five-second samples: 10,872 major faults/s; 27,885 combined swap-in/out pages/s | Active faulting and swap traffic. Combined in/out is not a swap-in rate; major faults may also involve file-backed pages. |
| 14:00:32 / 43 | Portal PID 1642075: RSS 685 MiB | Resident footprint only. Its `VmSwap` and proportional footprint were not measured in the exported investigation. |
| 14:02:22 / 50 | Portal received SIGTERM and was reported exited; twelve stopped persistence processes received SIGCONT | Actions belong to that historical session, not this audit. They are not authorization for new actions. |
| 14:02:41 / 52–53 | Stopped count zero; three zombies; available RAM 6,859 MiB, swap used 31,569 MiB; memory PSI full avg10 5.98 | Improvement coincided with the historical actions and ongoing workload activity. This does not establish exclusive causation or completed autosaves. |
| 14:03:57 / 55 | Available RAM 4,260 MiB; swap used 31,201 MiB; load averages 20.94, 43.57, 46.09 | A later point observation, not a sustained recovery time series. The attempted PSI extraction printed no PSI values. |

## What the guard actually did

The historical PID in the journal matches the current unit's PID, but no historical executable digest or loaded policy snapshot was captured. Behavioral journal evidence is stronger than assuming the inspected source or current policy was loaded unchanged.

Exact examples in [the journal extracts](raw/journal-extract.json):

- **13:49:53:** `oom-bias` changed two workload scores from 200 to 250 under `critical pressure`. This steers a possible kernel OOM choice; it neither kills a process immediately nor reclaims its memory.
- **13:50:59:** renice was skipped for a `rust-lld` process because the guard could not restore priority in that process's user namespace. Refusing an irreversible priority downgrade is the intended safety behavior, not permission to bypass the check.
- **13:50:24:** CPU caps were refused because a scope contained exempt `pi`, or because no isolated per-job scope was established. The guard must not throttle a shared interactive scope to punish one descendant.
- **13:55:31:** the guard emitted `Error: memory/pressure - critical: observed=critical, mem available 4%, swap used 56%, PSI full Some(56.31)` and listed `.xdg-desktop-po=666MiB` among the offenders. This directly contradicts the theory that pressure/portal monitoring was simply absent.
- Browser-tree vetoes explicitly protected a normal Chrome session without a disposable test profile.

No PID-specific portal mitigation was found in this bounded capture; the name-based pressure report did include a portal. Its full swapped footprint and why it did or did not receive other mitigation cannot be reconstructed from these records alone. The capture proves warning emission, not that the operator saw a desktop notification.

## Existing mechanisms and the actionable source gap

The inspected `src/main.rs` memory path already combines available-memory thresholds, PSI and independent swap-in velocity; occupancy alone is contextual. Stabilization delays pressure transitions. Current policy selection records a 120-second sustain setting, but the exact historical setting is not proven.

The candidate sampler initializes `swap_mb` to zero in `parse_ps_output`. `check_memory_pressure` then shortlists five candidates before reading their swapped footprint and reranking only those candidates. Consequently, a mostly swapped process outside the RSS shortlist can remain invisible to the final ranking. This is a concrete selection coverage gap, **not proof it caused this incident**: the portal was demonstrably reported by name. The next repair must consider swapped footprint before exclusion, retain incarnation/exemption checks, represent unreadable measurements honestly and bound sampling cost.

No shared pressure-aware launch queue was established by this audit. Workload admission is outside the confirmed repair list. Priority/OOM changes alone do not reduce the aggregate memory demand of concurrently running agents, compilers and browsers.

## Corrections to the original explanation

1. **Zram is not preallocated at its logical device size.** The configured 200% capacity does not itself consume twice physical RAM. The measured compression saved substantial resident space. A smaller populated device may require evacuating pages to disk or temporarily into RAM, with latency/OOM risks. Guaranteed 4–8 GiB relief was unsupported; no resize is recommended on that premise.
2. **The swap decline was not roughly 39 GiB returning to RAM.** That explanation is incompatible with the host's total RAM and simultaneous increase in available memory. Termination may have freed swapped allocations; other workloads may also have ended. Portal virtual size is suggestive, not proof of its swapped ownership. A before/after per-process swap or job accounting record is missing.
3. **Zombies were not a significant memory-reclamation opportunity.** They retain small kernel bookkeeping rather than a live application heap. Parent reaping is a health concern, not an explanation for tens of GiB of swap.
4. **Stopped persistence processes need outcome monitoring.** A blocked/pending SIGCHLD does not establish why a process stopped; SIGCONT does not automatically clear its signal mask. A later `S` state and fewer zombies do not prove that a valid tmux save occurred. Last successful save and correct server association were not verified.
5. **Cumulative counters and CPU averages were mixed.** OOM totals, compaction counts and accumulated `kswapd0` CPU time cannot date events without deltas. The later `ps %CPU` is a lifetime average and is not directly comparable to the earlier interval `top` CPU sample.
6. **Process-count inconsistency is unresolved.** Entry 18 reports 921 `/proc` directories but 31 `ps` rows; entry 21 later reports roughly 900 rows from both. Pressure alone does not establish that `ps` output was truncated. Command/output transformation, collection errors and races need consideration. Do not use the anomalous count for attribution.

## Remaining evidence limits and next repairs

The incident diagnosis is bounded by missing historical executable/policy identity, absent portal `VmSwap`/PSS measurements, no job-level attribution of swap release, no proof of useful tmux autosave recovery, and short rather than sustained post-action sampling. Those omissions are disclosed rather than filled with retrospective certainty.

The confirmed next items are swap-aware selection, report-only sustained service-growth and persistence-health diagnostics, and an evidence-based zram assessment. They do not grant restart/kill/resume authority, memory caps, zram changes, browser cleanup or workload-admission scope. Full implementation/qualification belongs to those later items; this report does not claim that the installed guard was repaired.
