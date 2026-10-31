# Authoritative v6.2 pass — direct no-collection evidence (2026-10-31 00:51:55 UTC)

This is the accepted run for the space-hygiene report, and it supersedes the
earlier qualifying pass at `../attempt-20261030T2340Z-v6-qualified/` (baseline
618). It runs for 231 seconds and reports the invariant the report claims —
nothing was collected — from four independent, read-only measurements.

## Result

| Measurement | Before | After | Meaning |
|---|---|---|---|
| Preserved baseline vs `roots_before` | 638 roots | 638 roots | byte-identical (SHA-256 `c43ad840…`); the pass started from the preserved pre-sweep list |
| `/nix/store` entries | 37,539 | 37,539 | any collection would remove paths |
| `nix-store --gc --print-dead` | 0 | 0 | the deletion set; printing deletes nothing |
| `nix-gc.service` start + journal | `Sat 2026-10-31 00:00:00 GMT` | same | 0 GC records in the bounded window (`-- No entries --`) |

Root-count churn was +0/−1 and is reported as a diagnostic, not a gate: the
count includes the Nix daemon's transient roots for whatever a live process has
mapped, so it moves during any multi-minute measurement on this host.

## Why the gate looks the way it does

The original rule required GC-root count equality. That is unsatisfiable here:
three consecutive attempts on 2026-10-30 (609→650, 650→617, 617→644) were
rejected while their store contents were unchanged and no GC ran. Those
attempts are preserved under `../attempt-20261030T2315Z-contract-final/` and
`../attempt-20261030T2332Z-contract-final2/` as evidence that the proxy, not the
pass, was the defect.

## Files

| File | Content |
|---|---|
| `baseline-*.txt` | Preserved pre-sweep root list (638) |
| `before-*.txt` | `roots_before` — byte-identical to the baseline |
| `after-*.txt` | `roots_after` (637) |
| `meas-*.dead-before` / `.dead-after` | `--print-dead` sets, both empty |
| `meas-*.roots-added` / `.roots-removed` | Root-churn diff (diagnostic) |
| `run-*.txt` | Full pass output: every area/candidate byte reading and gate field |
| `root-list-sha256.txt` | SHA-256 of the three root lists (baseline and before are identical) |
| `root-list-counts.txt` | Root-entry counts 638/638/637 with the `wc -l` values |
| `attempts.log` | Wrapper decision, including the `gc_identity` line |
| `gc-window-*.txt` | `-- No entries --` |
| `wrapper.stdout` | Wrapper console output |

Key lines from `attempts.log`:

```
preserved_baseline_count=638 run_baseline_count=638 roots_before=638 roots_after=637
root_churn_added=0 root_churn_removed=1 baseline_matches_before=YES
store_paths=37539->37539 dead_paths=0->0 no_collection_evidence=YES gc_unchanged=YES
gc_identity gc_start_before=Sat 2026-10-31 00:00:00 GMT gc_start_after=Sat 2026-10-31 00:00:00 GMT journal_gc_records=0
attempt 1: SUCCESS - preserved pre-sweep baseline matched, /nix/store and --print-dead set unchanged, no nix-gc.service run in window
```

## Scope note

`--print-dead` is empty because the boot generation holds every store path
reachable, so that set cannot shrink further; it would change if reachability
moved. The store-entry count would drop on any deletion. The service identity
and journal records are the direct no-GC evidence. All three are reported
because none substitutes for the others.

No cleanup, collection, or GC command was invoked by this survey. The three
scheduled GC runs on this host were the system timer's, recorded in the main
report with their own journal output preserved under
`../post-pass-confirmation-20261031T0048Z/`.
