# Qualified v6 pass — direct no-collection evidence (2026-10-30 23:40:55 UTC)

This is the acceptance run for the space-hygiene report. It replaces GC-root
count equality with the invariant the report actually claims: nothing was
collected.

## Why the earlier bracket was wrong

`nix-store --gc --print-roots` includes the Nix daemon's **transient** roots for
any store path a live process currently has mapped. On this host that list churns
during any multi-minute measurement even when nothing is collected. Three
consecutive passes on 2026-10-30 (23:15–23:39 UTC) were rejected by that proxy
while their store contents stayed identical and no GC ran:

| Attempt | baseline | before | after | `/nix/store` | GC run | Gate |
|---|---:|---:|---:|---|---|---|
| contract-final #1 | 609 | 609 | 650 | 37,539→37,539 | no | rejected |
| contract-final #2 | 650 | 650 | 617 | 37,539→37,539 | no | rejected |
| contract-final2 | 617 | 617 | 644 | 37,539→37,539 | no | rejected |
| **v6 qualified** | **618** | **618** | 638 | **37,539→37,539** | no | **accepted** |

The churning entries were a video-encode toolchain (`ffmpeg`, `libvpx`,
`svt-av1`, `libass`, `libopenmpt`, `zvbi`, `openjpeg`, `wl-clipboard`) appearing
and disappearing as transient runtime roots; the v6 pass recorded +26/−6 churn.
Those non-qualifying attempts are preserved under
`../attempt-20261030T2315Z-contract-final/` as evidence of the proxy defect.

## Evidence in this folder

| File | Content |
|---|---|
| `baseline-*.txt` | Preserved pre-sweep root list (618 roots) |
| `before-*.txt` | `roots_before`, byte-identical to the baseline |
| `after-*.txt` | `roots_after` (638 roots; churn only) |
| `meas-*.txt.dead-before` / `.dead-after` | `--print-dead` sets, both empty |
| `meas-*.txt.roots-added` / `.roots-removed` | Root-churn diffs (diagnostic) |
| `run-*.txt` | Full pass output: all area/candidate byte readings and gate fields |
| `attempts.log` | Wrapper acceptance decision |
| `gc-window-*.txt` | `-- No entries --` for the bounded journal window |

Key fields from `run-*.txt`:

```
pre_sweep_baseline_count=618
roots_before=618
store_paths_before=37539
dead_paths_before=0
roots_after=638
store_paths_after=37539
dead_paths_after=0
no_collection_evidence=YES
```

## Scope note

`--print-dead` is empty because the boot generation holds every store path
reachable, so that set cannot shrink further. The three checks are
complementary, not interchangeable: any deletion would reduce the `/nix/store`
entry count, any reachability change would alter the dead set, and the
journal/service identity proves no `nix-gc.service` run occurred in the window.

No cleanup, collection, or GC command was invoked by this survey. The only GC
runs on this host were the system timer's, documented in the main report.
