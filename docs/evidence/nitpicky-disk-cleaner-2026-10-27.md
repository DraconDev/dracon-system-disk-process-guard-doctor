# Nitpicky disk cleaner — implementation and first live run (2026-10-27)

Follows `system-disk-analysis-2026-10-26.md`, which found that the root disk filled
from **git temp-pack churn**, not from media, and that the existing guard had no
visibility into three trash classes at all.

## What was built

Extended the existing `dracon-system guard clean` framework rather than adding a
parallel tool. The framework already owned rust/trash/nix/caches/node_modules/
docker cleanup with dry-run, min-size floors and protected paths; it was missing
three detectors, which is the real gap this work closes.

| Addition | Where | Behaviour |
| --- | --- | --- |
| Orphaned git scratch files (`tmp_pack_*`, `tmp_idx_*`, `tmp_obj_*`) | `src/git_temp.rs` | **Auto-applied** when no pack-writing git process owns the repo and the file is older than `git_temp_min_age_hours` (default 6) |
| Dead swapfiles | `src/space_report.rs` | **REPORT-ONLY**, prints the exact `sudo rm` line |
| Stray dirs + backup bundles | `src/space_report.rs` | **REPORT-ONLY**, needs explicit per-path approval |
| Nix generation thinning | `src/main.rs` | Changed from "keep newest N" (`+N`) to **spread across the range** |

New policy knobs: `git_temp_min_age_hours` (default 6), `dead_swapfile_candidates`
(default `/var/lib/swapfile`), `stray_scan_roots` (default `~`).

## Safety model, and the three bugs live testing caught

The design rule was "never delete under a running process". Three successive
live runs showed the first three implementations were wrong in ways unit tests
could not see:

1. **`~` was not expanded.** `PathBuf::from("~/Dev")` is a literal directory
   named `~`, so the first run printed `Nothing to clean.` on a host holding
   33 GiB of git scratch. Fixed by routing every scan root through the existing
   `expand_tilde`.
2. **The new blocks ran after the report.** They were appended below the
   `if json` / `Nothing to clean` reporting section and were never reached.
   Fixed by moving them above it.
3. **"Any git process blocks everything" never allows anything.** The sync
   daemon runs `git add`/`commit` on a one-second pulse, so a git process is
   *always* alive. First correction was per-repo attribution; the second live
   run proved that insufficient too (the daemon's `commit` inside each repo
   matched). The final rule matches the **subcommand**: only `gc`, `repack`,
   `pack-objects`, `index-pack`, `prune`, `unpack-objects`, and
   `git-remote-*`/`*-pack` helpers count as pack writers. Routine
   `add`/`commit`/`status`/`push` do not block cleanup.

A fourth bug was caught by the existing test suite, not by review: the
system-profile prune was reusing `gen_arg`, which by then could hold the user
profile's **spread plan** (`"3 6"`). Passed with `-p /nix/var/nix/profiles/default`
that would have deleted generations 3 and 6 of the *system* profile — numbers
chosen by a different profile's layout. Fixed: the system profile always keeps
the `+N` form, since it is root-owned and cannot be enumerated for its own plan.

## Nix generation spread

`+N` keeps only the newest N and discards every older rollback anchor: with 18
generations and `keep=5` it retains 14–18. The spread rule retains one anchor near
each end and spaces the rest: generations `1..=18`, `keep=5` → kept `{1, 5, 9, 13, 18}`,
deleted `{2,3,4,6,7,8,10,11,12,14,15,16,17}`. The current generation is never
deleted, even at `keep=0`. Covered by `nix_spread_tests` including the
oldest-anchor assertion that distinguishes it from `+N`.

## Gates

```
cargo test  -p dracon-system --locked   665 passed, 0 failed
cargo clippy -p dracon-system --locked -- -D warnings   clean
cargo build  -p dracon-system --release --locked        ok
```

Test breakdown: 649 unit + 4 CLI + 12 service. New coverage: git scratch
detection (pack + loose fanout, symlink refusal, real packs ignored), age floor,
unknown-mtime refusal, dry-run vs apply agreement, per-repo ownership, subcommand
gating, swapfile active/inactive, stray classification (including that
`Dev`, `Documents`, `Pictures`, `Videos` are never flagged), and the nix spread
selection.

## First live run

Observed filesystem movement (a single filesystem with heavy concurrent activity;
these are NOT exact per-path attribution):

- Before: `972992737280 total, 159618600960 available`
- After:  `972992737280 total, 160449847296 available`
- Final:  `972992737280 total, 763042111488 used, 160449847296 available (83%)`

Deleted (all six were zero-byte stale loose-object scratch files):

| Path | Bytes |
| --- | ---: |
| `dracon-strategy/.git/objects/37/tmp_obj_d2zqC6` | 0 |
| `dracon-strategy/.git/objects/37/tmp_obj_NtSAC8` | 0 |
| `dracon-strategy/.git/objects/25/tmp_obj_0WYt1C` | 0 |
| `dracon-strategy/.git/objects/f0/tmp_obj_dpD5Ce` | 0 |
| `pi-plugins/.git/objects/9a/tmp_obj_Hg2co9` | 0 |
| `pi-plugins/pi-goal-list-loop-audit/.git/objects/73/tmp_obj_5TdrI5` | 0 |

Verified absent after deletion.

**Deliberately protected, not deleted:** 16 `tmp_pack_*` files totalling
~13.6 GiB in `dracon-strategy/ai-auto-writer` and `browser-extensions-shared`.
All were newer than the 6-hour age floor, i.e. plausibly in-flight pack
operations. The tool protected them on every pass and will reclaim them once they
age past the floor with no owning process — which is the correct outcome, not a
miss.

**Report-only findings (nothing deleted):**

| Item | Size | Action offered |
| --- | ---: | --- |
| `/var/lib/swapfile` (inactive; not in `/proc/swaps`, not in fstab or NixOS config) | 32.0 GiB | `sudo rm -f /var/lib/swapfile` |
| `/home/dracon/dracon-lfs-proof` | 119.2 MiB | needs approval |
| `/home/dracon/tmp-shield` | 8.8 MiB | needs approval |
| `/home/dracon/eve-audit-new` | 20 KiB | needs approval |
| `/home/dracon/.dracon-git-recovery-20260831-1610` | 40 KiB | needs approval |

Idempotency: a second `--apply` pass reported 0 git-temp deletions, so the
detector does not re-report what it already cleaned.

## Limitations

- Available bytes went **up** during this window largely because the sync
  daemon's own `auto_gc` kept reclaiming git garbage; this cleanup's own
  contribution was six empty files. The value delivered here is the *tooling*
  that prevents the next 33 GiB accumulation, not the bytes freed today.
- The root-owned swapfile still requires a human with sudo; unprivileged tools
  correctly refuse it.
- `db-gateway` and other capped trees were not re-tested here.
- `guard once` refuses to run concurrently with the live daemon
  (`lock file is held by another process`), so the health check used
  `dracon-sync health` plus `cargo` gates instead of a guard pass.
