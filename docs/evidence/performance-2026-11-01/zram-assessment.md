# Zram assessment — 2026-11-01

## Conclusion

**The evidence does not justify a resize or a promised RAM saving.** Zram's
logical capacity is not preallocated physical RAM. The incident had both large
stored data and severe contemporaneous paging/stalls; the later short capture
still shows active paging, but much lower sampled memory stalls. Occupancy
alone proves neither active thrashing nor successful recovery. Shrinking a
populated device can move or temporarily increase memory demand and stall/OOM
risk; it is not guaranteed RAM relief.

This is an assessment only. No resize/reset, swapoff, synthetic pressure,
configuration change, deployment or process/service signalling was performed.
Only bounded proc/sysfs reads, read-only CLI status and evidence calculations
were used. Production `src/zram.rs` is unchanged.

## Evidence, units and timing

- **H25/H27/H12/H38:** [original tool observations](raw/export-observations.json),
  entries 25/27/12/38, with original UTC timestamps; context and attribution
  limits are in the [incident record](incident.md). H25 swap accounting is
  **13:53:20.681 UTC**; H27 zram byte counters are **13:53:51.550 UTC**, not an
  atomic joint sample.
- **S0–S2:** [raw host capture](zram/host.json), three sequential observations
  from **19:03:32.784149 to 19:03:42.822519 UTC**. Each individual read has start/end
  UTC and monotonic timestamps, source path, raw text/hash or explicit error.
  The capture is bounded to three observations, five-second waits, four devices
  and 256 KiB/read; the outer command deadline was 45 seconds. These reads do
  not establish sustained behavior over a day or the incident's full duration.
- [Derived values/formulas](zram/derived.json) come from the [evidence-only
  calculator](zram/analyse.py); [fixtures](zram/test_analyse.py) and
  [results](zram/tests.log) protect unknowns, units and counter distinctions.
  `frames[0]` is S0; `frames[2]` is S2; `incident` retains H25/H27 timestamps.
- [Commands/statuses/source provenance](zram/commands.json), [bounded collector](zram/capture.py),
  [later mount/kernel observation](zram/supplement.json), and illustrative
  [existing CLI stdout](zram/status.stdout)/[stderr](zram/status.stderr).
  CLI status was separately captured at **19:09:13.457 UTC**; its rounded
  figures are not substituted for the earlier raw observations.

**GiB = bytes / 1,073,741,824; MiB = bytes / 1,048,576.** Kernel `kB` in
meminfo and KiB accounting in `/proc/swaps` are multiplied by 1,024. The current
base page size is **4,096 bytes** (`host.json.page_size_bytes`); swap rates
below use that observed value, not an assumption about historical page size.

## Five quantities that must stay separate

| Quantity | Meaning | S0 observation, approximately 19:03:32 UTC |
|---|---|---|
| Logical capacity | `disksize`, addressable uncompressed block-device size; not committed RAM | **65,359,839,232 bytes = 60.871 GiB**. Approximately twice contemporaneous `MemTotal` (**32,680,579,072 bytes = 30.436 GiB**); not twice-RAM physical consumption. |
| Stored original data | `mm_stat[0]`, represented uncompressed data; not a physical allocation | **26,198,777,856 bytes = 24.400 GiB**. |
| Compressed payload | `mm_stat[1]`, compressed data size | **7,979,230,525 bytes = 7.431 GiB**. |
| Driver-reported RAM allocation | `mm_stat[2]`, physical allocation including allocator effects; not logical size | **8,168,927,232 bytes = 7.608 GiB**. Not a complete accounting of every host/kernel/compressor overhead or CPU cost. |
| Separate swapfile allocation | `/proc/swaps` Used, allocated logical swap slots on the lower-priority non-zram file | `/mnt/data/swapfile`: **1,909,608 KiB = 1,955,438,592 bytes = 1.821 GiB**, priority **−1**, versus zram priority **5**. Allocation is not an I/O rate or proof that every page lacks a RAM/cache copy. |

Sources: `host.json.samples[0].reads` paths `/sys/block/zram0/{disksize,mm_stat}`,
`/proc/meminfo`, `/proc/swaps`; `derived.json.frames[0]`. The **later**
19:09 mount observation maps `/mnt/data` to ext4 on `/dev/sda2`; it is not a
historical mount receipt for the incident. Zram's own `backing_dev` was `none`
and `bd_stat` was `0 0 0` in S0–S2: the separate swapfile must not be confused
with zram backing-device writeback. `comp_algorithm` selected `[zstd]` in
S0–S2, as historical H30 also reported; that is an observation, not proof that
this algorithm is optimal for the current workload.

Additional distinctions:

- At S0, compressed-payload ratio = original/compressed = **3.283×**;
  effective allocation ratio = original/allocated-RAM = **3.207×**.
  Their difference is not a reclaimable-memory promise. Both vary with data,
  same-filled/incompressible pages and allocator behavior.
- S2, about **19:03:42 UTC**, held **26,498,031,616 original bytes (24.678 GiB)**,
  **8,078,501,516 compressed bytes (7.524 GiB)** and
  **8,274,046,976 allocated bytes (7.706 GiB)**. The short growth is observed,
  not a long-term trend or a leak diagnosis (`derived.json.frames[2]`).
- S0's `mem_used_max` was **20,484,837,376 bytes (19.078 GiB)**, a historical
  high-water counter, **not current allocation** or a dated peak. `mem_limit`
  was **0**, meaning no configured zram allocator limit, not zero RAM use.
- S0 `/proc/swaps` reported zram Used **26,202,532 KiB (26,831,392,768 bytes)**,
  which is not identical to `orig_data_size`. Swap-slot accounting, stored
  data and swap-cache/discard lifetimes need not match, and reads are not
  atomic. Do not explain the difference as proven leakage. Usable swap Size
  is also slightly below the sysfs block-device size.

The documented kernel ABI distinguishes these fields; see
[Linux zram statistics](https://docs.kernel.org/admin-guide/blockdev/zram.html)
and [PSI semantics](https://docs.kernel.org/accounting/psi.html). Raw evidence,
not a guessed compression ratio, is the basis of this assessment.

## Incident versus later observations

H27 directly measured **64,697,733,120 original bytes (60.255 GiB)**,
**12,972,688,106 compressed bytes (12.081 GiB)** and
**13,342,056,448 allocated bytes (12.425 GiB)**. Payload and effective
allocation ratios were **4.987×** and **4.849×** respectively
(`derived.json.incident`). The original export's formatted `disksize: 60.9 GB`
is rounded; no exact historical sysfs disksize was preserved there.

H25 independently recorded zram usable size **63,827,964 KiB**, Used
**63,582,644 KiB**, and swapfile Used **9,523,016 KiB (9.082 GiB)**. This shows
near-full zram swap-slot allocation and separate disk-swap allocation at that
time, not a simultaneous H27 occupancy percentage or proof of the cause/timing
of spillover. Higher-priority zram generally receives allocations before a
lower-priority file; extant file allocation can persist after pressure falls.

Historical **activity** evidence is stronger than occupancy alone: H12 at
**13:51:19.729 UTC** reported memory PSI `full avg10=78.09%` and I/O PSI
`full avg10=81.52%`; H38 at **13:56:14.441 UTC** reported a five-second sample
of **27,885 combined swap-in/out pages/s** and **10,872 major faults/s**.
Combined directions are not a swap-in rate; major faults also include
file-backed faults. These asynchronous observations support severe incident
paging/stalls, not application-specific ownership or an exclusive zram cause.

Current interval derivations (`derived.json.intervals`; counter values and
per-read timestamps in `/proc/vmstat` and pressure records):

| UTC vmstat endpoints | Monotonic duration | Swap-in | Swap-out | Major faults | Memory full stall share | I/O full stall share |
|---|---:|---:|---:|---:|---:|---:|
| 19:03:32.788 → 19:03:37.818 | 5.029389 s | 5,261.47 pages/s; 20.553 MiB/s | 11,428.03 pages/s; 44.641 MiB/s | 6,212.88 events/s | 2.133% | 0.440% |
| 19:03:37.818 → 19:03:42.822 | 5.004010 s | 3,168.86 pages/s; 12.378 MiB/s | 11,045.74 pages/s; 43.147 MiB/s | 4,513.98 events/s | 2.120% | 0.326% |

Formula: `(counter_after − counter_before) / elapsed_monotonic_seconds`;
MiB/s adds observed `page_size / 2^20`: this is logical uncompressed page
volume, **not compressed-device or physical-disk bandwidth**. PSI interval shares use **each PSI
file's own elapsed time**, and `delta(full.total_microseconds) / elapsed / 10^6 × 100`,
not subtraction of smoothed avg10 values. Missing/reset counters, invalid time
or unestablished boot continuity produce null, not zero.

Thus the host was **not paging-idle** in this brief capture, but the full-memory
stall share was much lower than the historical short-window peak. This is not
proof of no thrashing, durable recovery or a causal before/after experiment.
Global swap counters combine zram and disk traffic. The separate swapfile's
Used allocation stayed constant in S0–S2; that does **not** establish zero disk
swap reads/writes. I/O pressure and diskstats include unrelated filesystem work.
`MemAvailable` was about **3.157 / 3.421 / 3.009 GiB** across S0/S1/S2, not a
promise of evacuation headroom (`derived.json.frames[*].meminfo_bytes`).

## Existing status implementation: useful, not the calculation authority

`src/zram.rs` reads the first device's `disksize`, `comp_algorithm` and
`mm_stat`; it already separates original, compressed and allocated sizes.
However, its binary divisions are labelled `GB`, disksize is integer-truncated,
missing/invalid fields become zero, and `filter_map` can shift later positional
fields past a malformed token. Zero-denominator ratios are printed as zero.
It does not report disk-swap usage or interval paging/stall rates. Its generated
configuration's default percentage is not evidence of an optimal setting.

This assessment does not change that production parser or deploy a fix. The
evidence calculator instead preserves field positions/unknowns and tests the
meaningful malformed-column case, zero denominators, KiB/byte distinctions,
separate swap directions/base-page size, counter resets and clock/boot gaps.
Harmless whitespace/order/unrelated fields remain tolerated; bad required
fields do not silently become healthy zero. See [tests](zram/tests.log).

## What could justify future tuning, and how to validate safely

1. **Establish a representative baseline first.** Preserve new timestamped
   evidence separately rather than overwrite this capture. Observe normal and
   peak workloads long enough to correlate user/job latency, memory/IO PSI,
   directional paging, MemAvailable, allocated zram RAM, stored original data,
   incompressible/allocator behavior, CPU cost and disk-swap activity. Keep
   device/boot/configuration identity and report resets/missing data. This
   ten-second sample cannot identify an optimal capacity/algorithm/swappiness.
2. **State a falsifiable tuning hypothesis.** Examples are repeatable
   compression-CPU bottlenecks or zram allocation crowding an active working
   set while disk latency remains acceptable. Large capacity/occupancy alone
   is insufficient. Compare representative data and peak tail latency, not a
   single ratio. A smaller logical ceiling does not inherently shrink the
   current pool or improve the working set; a poorer compression ratio can
   increase its RAM cost. Memory limits can cause failed writes, not free relief.
3. **Require separate operator-approved maintenance or an isolated canary.**
   Confirm the exact device, populated slots, available RAM/disk reserve,
   filesystem health and swap priorities; preserve current configuration and
   a recovery path. Reducing populated zram capacity normally involves
   deactivation/reset/recreation, not a harmless online size edit. `swapoff`
   can require resident evacuation and heavy reclaim, is not a guaranteed
   direct transfer to another swap device, and can stall or fail/OOM. Current
   compressed pool size is not the uncompressed evacuation requirement.
   Do not attempt it merely because a smaller nominal size looks adequate.
4. **Change one variable only, with predefined abort/rollback criteria.**
   Use a controlled approved workload/window, compare throughput and tail
   latency as well as stalls, paging and memory allocation, and stop on
   degraded responsiveness, failed writes, OOM risk or missing telemetry.
   Restore the previous configuration through its supported maintenance path
   if needed; restoring a size does not instantly restore old page residency.
   No such experiment, resize or workload stimulus was executed here.

**No amount of RAM saving is promised.** Current and historical compression
ratios differ; workloads, allocator overhead, disk latency and evacuation costs
must be measured before any tuning recommendation can be justified.
