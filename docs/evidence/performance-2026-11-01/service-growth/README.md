# Report-only service memory growth — source/test qualification

## Result

The existing guard pass now carries a separate `service_memory_growth` report
and rate-limited `memory/service-growth` events. It observes configured services
independently of host pressure or offender shortlisting. It never feeds
mitigation eligibility or invokes service recovery, quota or admission APIs.
No live policy, service, process, installation or paused reclamation state was
changed for this item.

[Design and invariants](../../../design/service-memory-growth-2026-11-01.md),
`src/service_growth.rs`, `src/service_growth_tests.rs`, the shipped
`dracon-system.example.toml` and root README describe the implemented model.

## What is measured

- Explicit `system:`/`user:` service targets, not ambiguous names or manager
  fallback. Defaults cover the system sync operator and user guard/portal.
- Cgroup-v2 subtree charged RAM (`memory.current`, including cache/kernel,
  **not RSS/PSS**) and swapped bytes (`memory.swap.current`). These are
  sequential point readings, not atomic ownership measurements or proof of
  a leak. Missing counters remain unknown, not zero.
- A generation binds boot, explicit manager/UID, canonical service Id,
  InvocationID, ControlGroup, directory device/inode and main PID/starttime.
  Manager, directory, process identity and membership are revalidated after
  collection. Unsupported namespace/accounting or unsafe paths are unavailable.
- Growth uses elapsed monotonic time and measured baseline/current counters.
  Both window halves must contribute at least one quarter of the total delta
  and configured minimum. A high endpoint, exact/noisy burst plateau, or
  resident-to-swap movement with unchanged total is insufficient.
- Drops, missing observations, incarnation changes, duplicate/backward time
  and large gaps reset comparison. This conservative model may miss noisy or
  short-lived growth; sustained charged-memory growth is not a heap-leak
  diagnosis. A one-shot guard cannot inherit daemon history.

Limits are implemented/tested in `service_growth.rs`: at most 16 targets and
32 points per target; capped batched manager output, per-command and overall
asynchronous deadlines; bounded counter/path reads and no-symlink descriptor
walks. There is no recursive cgroup/process scan. Kernel filesystem calls are
bounded by count/bytes/depth, not a promise of preempting a stalled kernel
syscall. [History/budget fixtures](focused.log) exercise those limits.

## Contract-to-evidence map

| Requirement | Behavioral evidence |
|---|---|
| Sustained versus temporary growth | `sustained_growth_exposes_counter_baseline_elapsed_window_and_rate`, `a_single_burst_plateau_or_resident_swap_migration_is_not_sustained_growth`, `swap_only_growth_and_irregular_intervals_use_elapsed_time_not_poll_count` in the [focused results](focused.log). |
| Restart/PID/cgroup changes and missing/reset samples | `service_and_process_generation_changes_never_bridge_history`, `release_missing_data_disappearance_clock_and_sampling_gaps_reset_comparison`, `manager_and_process_races_or_cgroup_replacement_make_sample_unavailable` include migration during final manager read. |
| Explicit unavailable provenance and bounded read-only collection | Fixtures cover missing namespace/boot/counters, malformed/overflow values, directory/counter symlinks, output/batch caps and query-only manager argv. File contents and nonempty mitigation state are unchanged by diagnostic evaluation. |
| Supporting report footprint/window/rate | The sustained-growth fixture serializes and checks current resident/swap, baseline counters, elapsed seconds, byte delta and MiB/min, and checks the same window/rate in human summary. Missing-counter JSON stays null. |
| History/reload/policy compatibility | `history_and_event_signatures_stay_bounded_and_quiet`, `successful_reload_clears_history_but_deferred_reload_keeps_old_observation_state`, `old_policies_defaults_new_knobs_and_normalization_are_backward_compatible`. Root example legality/unknown-key tests remain in the full package suite. |
| Harmless input changes versus real regressions | `equivalent_target_reordering_preserves_observed_growth`; [actual-tracker qualification](qualify.py) passes the [baseline](baseline.log), while compiled [negligible-drift](negligible-drift.log) and [bridged-incarnation](bridged-incarnation.log) mutants fail intended behavioral assertions. Live source is never mutated. |
| Required package gates | [Package tests](package-tests.log): 735 unit tests and 16 integration tests pass with no failures. [Strict package Clippy](package-clippy.log) and additional [all-targets strict Clippy](all-targets-clippy.log) finish successfully. Counts describe these runs, not permanent requirements. |

[Exact required commands/statuses and source hashes](gates.json),
[qualification metadata](qualification.json), and
[fresh-context review/fixes](review.md) retain supporting evidence. Review was
source/log inspection, not an independent command rerun.

## Reproduction

From the nested `dracon-system` checkout:

```sh
timeout 600 env CARGO_BUILD_JOBS=2 cargo test --locked -p dracon-system
timeout 600 env CARGO_BUILD_JOBS=2 cargo clippy --locked -p dracon-system -- -D warnings
timeout 600 env CARGO_BUILD_JOBS=2 cargo clippy --locked -p dracon-system --all-targets -- -D warnings
timeout 180 python3 docs/evidence/performance-2026-11-01/service-growth/qualify.py
git diff --check
```

The optional one-off qualification extracts the actual pure tracker and actual
fixture checks into a std-only harness; it adapts the large GuardPolicy to the
relevant fields with defaults extracted from source and strips serde derives.
It deliberately does **not** stand in for full Cargo serialization, collector,
namespace or descriptor tests. Compilers must succeed before negative outcomes
count as regression proof. Scratch paths and generated hashes are retained;
future helper changes require reviewing the extraction/mutation sites, not
refreshing expected literals blindly. No test invokes the package runner.

## Configuration and remaining scope

Four backward-compatible guard settings are documented in the root README and
example: monitor flag, window seconds, minimum increase MiB and target list.
The existing Nix scalar override renderer supports the bool/integer settings;
target arrays are TOML settings via its existing unmanaged/custom-policy path.
Neither a Nix pin nor live configuration was changed.

The installed guard is unchanged. Deployment, automatic recovery, workload
admission and tmux save-health diagnostics were not part of this item. Whole
workspace qualification remains the separately queued final gate.
