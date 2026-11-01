# Swap-aware offender selection — source/test qualification

## Result and boundaries

The shared process sampler now attempts a swapped-footprint measurement for
**every identity-verified row before memory shortlisting**. A low-RSS process
can therefore rank on its swapped allocation instead of disappearing before
that allocation is read. This is the source correction identified in the
[incident audit](../incident.md), not retrospective proof of portal ownership
of the incident's swap.

No binary was installed, no live configuration was edited and no live process
or service was signalled. Memory caps and mitigation authority were not added.
The installed guard and paused reclamation work were left untouched.

## Implementation and safety

- `src/main.rs::process_samples` reuses the existing bounded process-table
  sampler and `verified_process_samples_with`; it does not introduce a second
  process monitor. Identity is checked before and after reading swap. Exited,
  recycled, exec-renamed and unverifiable processes cannot contribute the old
  sample's footprint across that read.
- `read_proc_swap_mb_from` bounds the status read and requires a valid single
  `VmSwap` value with kernel units. Missing fields, bad units, read errors,
  malformed UTF-8 and oversized status data produce `None`, not `Some(0)`.
- `ProcSample.swap_mb` is optional. JSON preserves numeric known values but
  uses `null` for unknowns. The existing `memory.top_rss` key remains for
  consumers; rows now rank by saturating RSS-plus-known-swap lower bounds.
  `memory.swap_samples_unavailable` discloses eligible unknowns, including
  those outside the shortlist. Notifications/events also expose uncertainty.
  **Unknown allocations may exceed their observed lower bound:** this is not
  a claim of exhaustive true-footprint attribution when proc data is missing.
- `memory_pressure_offenders` retains the existing five-row cap and
  kernel/exempt-name filtering, with deterministic PID tie-breaking. Selection
  retains only a constant-size shortlist and does not clone the full table.
  The scan's work is linear in the already size-capped process-table input;
  swap reads are capped per row. This necessarily does more status reads than
  the former incorrect five-candidate-only enrichment.
- Command-line enrichment still occurs only after selection. Existing
  mitigation incarnation checks, reversible-priority capability checks,
  durable-ledger gates, OOM protections, CPU-scope/exemption checks and
  opt-in destructive paths were not relaxed. Legacy descendant/probe fixtures
  use explicit measured-zero values without changing their protected behavior.

The [captured source diff](source-diff.patch) is relative to the earlier
incident provenance and also includes formatting in the touched files; source
hashes used for qualification are in [qualification.json](qualification.json).
This is source/test evidence, not historical loaded-image identity.

## Contract-to-evidence map

| Protected behavior | Evidence |
|---|---|
| A mostly swapped process outside the original RSS shortlist is considered | `src/memory_sampling_tests.rs::swapped_offender_outside_rss_shortlist_is_selected`, [focused output](focused-tests.log). The fixture has a low-RSS offender with 32 GiB swapped and an old-algorithm control that misses it. |
| Unreadable swap remains unknown; omission does not erase incomplete coverage | `unknown_swap_is_not_a_measured_zero_even_outside_shortlist` checks JSON null versus numeric zero, distinct descriptions for the same process, and unavailable coverage outside the shortlist. |
| Process churn/unavailable identity cannot transfer a footprint | `process_churn_across_swap_read_discards_the_sample` and `initial_identity_failure_does_not_read_swap_or_fabricate_samples`. All changes occur in owned disposable proc fixtures. |
| Exempt/kernel processes remain excluded; overflow and parser bounds are safe | `exemptions_and_kernel_threads_never_become_memory_offenders`, `empty_selection_and_footprint_overflow_are_safe`, `swap_parser_rejects_missing_malformed_and_oversized_status`. |
| Harmless input changes preserve selection | `swapping_and_equal_footprints_are_independent_of_input_order` rotates/reverses the same fixture rows, including equal-footprint rows. |
| Tests reject meaningful regressions rather than only matching source text | [Qualification script](qualify.py), [baseline](baseline.log), [early-shortlist mutant](early-rss-shortlist.log), [missing-incarnation-check mutant](missing-incarnation-check.log). Both mutants compile successfully and fail the intended behavioral assertions; live source is never mutated. |
| Required package gates pass | [Package tests](package-tests.log): 720 unit tests, 4 event CLI tests and 12 service tests pass, zero failures. [Strict Clippy](package-clippy.log) finishes successfully. These totals describe this run, not permanent requirements. |

Exact command exit statuses and tested source hashes are retained in
[gates.json](gates.json). An additional [all-targets strict Clippy run](all-targets-clippy.log)
also passed. [Final verification](final-verification.txt) checks source/hash
agreement, expected mutant outcomes, test results, citations and source diff
whitespace.

## Reproduction

Run from the nested `dracon-system` checkout:

```sh
timeout 600 env CARGO_BUILD_JOBS=2 cargo test --locked -p dracon-system
timeout 600 env CARGO_BUILD_JOBS=2 cargo clippy --locked -p dracon-system -- -D warnings
timeout 180 python3 docs/evidence/performance-2026-11-01/swap-selection/qualify.py
git diff --check
```

The optional one-off mutation harness compiles actual current production
sampling/ranking function bodies plus actual fixture tests in a disposable,
std-only Rust harness. Its only substitute is an owned fixture-directory
allocator in place of `tempfile`; no live processes or `/proc` entries are
created or altered. It does not replace the full Cargo gates, is not invoked
from collected tests, and needs source-extraction/mutation sites reviewed if
those functions evolve. A compile failure is explicitly rejected as mutation
proof. Retained scratch paths and generated-source hashes are recorded in
`qualification.json` for inspection.

A [fresh-context reviewer](review.md) found no substantive safety/wiring
issues. Its tool set could inspect source/logs but not rerun the commands;
command execution evidence is the parent's retained output. Full-workspace
qualification and the later service-health work remain separate queued items.
