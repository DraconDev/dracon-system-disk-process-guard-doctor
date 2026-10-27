# Nitpicky disk cleaner — `dracon-system disk` (2026-10-27)

Supersedes the earlier write-up of the same session. That version described an
extension of `guard clean` and claimed the objective was met; it was not. The
auditor was right on six counts, and this document records what actually exists
now, including the defects live testing exposed.

## The command

```
dracon-system disk audit   # report mode: every trash class, per-path disposition
dracon-system disk clean   # freeing mode: auto-applies the SAFE classes only
dracon-system disk         # same as audit
```

Global flags: `--class git,tmp,caches,strays`, `--json`, `--detail N`
(per-path lines printed per class, default 15, `0` = all),
`--tmp-min-age-hours N`.

Mode semantics follow the objective: **the default frees space** and applies only
the SAFE classes; `--report` is the dry run and frees nothing; `--apply` is
accepted as an explicit synonym for the default. (The earlier version made
dry-run the default, which the objective does not describe.)

## Classes and dispositions

| # | Class | Detector | Disposition |
| --- | --- | --- | --- |
| 1 | Orphaned git scratch (`tmp_pack_*`, `tmp_idx_*`, `tmp_obj_*`) | `src/git_temp.rs` | `APPLY` when no pack-writing git process owns the repo and the file is older than `git_temp_min_age_hours` (default 6); otherwise `SKIP` with the rule named |
| 2 | `/tmp` + `/var/tmp` cruft, oversized/rotated logs | `src/disk_tmp.rs` | `APPLY` past `tmp_cruft_min_age_hours` (default 24) |
| 2 | Dead swapfiles | `src/space_report.rs` | `REPORT` with the exact `sudo rm -f` line |
| 3 | Caches: cargo, npm, bun, pip, go, playwright, puppeteer, chrome | `src/disk.rs` | `SKIP` with the owning process names + PIDs when in use; `REPORT` when idle |
| 3 | Old Nix generations | `src/main.rs` | spread plan, see below |
| 4 | Stray dirs + backup bundles | `src/space_report.rs` | `REPORT`, never auto-deleted |

`APPLY` = this class is safe and allowed in this mode. `SKIP` = a safety rule
refused it, and the reason says which rule and which PID. `REPORT` = never
automatic; a human decides.

## Nix generations

Three separate defects were fixed here, each found by the auditor and each
reproduced:

**1. The sweep could never run.** The plan was joined with spaces into a *single*
argv element and passed to `nix-env --delete-generations`. nix rejects that:

```
$ nix-env -p /tmp/probe --delete-generations "3 6"
error: invalid generation number '3 6'          exit=1
$ nix-env -p /tmp/probe --delete-generations 3 6
removing profile version 3
removing profile version 6                       exit=0
```

The old unit test used a fake `nix-env` that echoed `$*`, which cannot tell one
argument from two, so the test passed while the feature was broken. The plan is
now a `Vec<u32>` and `nix_delete_generations_argv` emits one element per
generation. The test fixture now records **each argv element on its own line**
(`nix-env-argv:<count>`, `nix-env-arg:<value>`) and the assertions check the
argument *shape*, including an explicit check that no element contains two
numbers separated by a space.

**2. "Current" was inferred as "highest".** `parse_nix_generation_numbers`
discarded the `(current)` marker, so after
`nix-env --switch-generation N` a cleaner protecting the highest number would
delete the generation the machine actually boots. `parse_nix_current_generation`
now reads the marker, and a listing without one causes the prune to be skipped
with a warning instead of guessing.

**3. The running closure was never consulted.** For the NixOS system profile the
generation links are world-readable even though the profile itself is root-locked,
so the booted generation can be identified exactly:
`/run/current-system` resolves to the same store path as `system-<n>-link`.
`system_profile_spread_plan` reads those links, protects every generation whose
resolved store path equals the running closure, always protects the highest as a
floor, and protects any generation whose link cannot be resolved (unknown is not
"unused"). The first attempt at this compared whether the link *target existed* —
which is true for every generation of a live profile, so it protected the entire
profile and silently made pruning impossible; it now compares resolved store
paths.

The system profile is no longer pruned with `+N`. `+N` keeps only the newest N,
which is exactly the shape the objective rejects.

## Pre-delete safety capture

The objective requires the "never delete under a running process" check to be
verified via `/proc` **exe, cwd and fd** references captured *before* the unlink,
and states that a `(deleted)` suffix alone is not proof either way. That matters
because unlinking adds ` (deleted)` to every fd symlink pointing at the file, so
a post-delete scan cannot distinguish "was in use" from "was not".

`src/disk_refs.rs` implements `ProcSnapshot::read`, which reads `/proc` **once**
and indexes every `exe`, `cwd` and `fd` target, stripping any ` (deleted)` suffix
before comparison. Each finding carries its capture; `apply()` re-captures
immediately before each unlink and re-checks, so a file that became in use
between audit and delete is skipped rather than removed. An unreadable `/proc`
is treated as "unknown" and blocks deletion — a failed scan is never evidence of
"unused".

## Live run

```
date           2026-10-27T03:54:02Z                2026-10-27T03:54:27Z
df -B1 / avail 155001503744 (used 768490455040)     157865975808 (used 765625982976)
```

Recorded before/after `df -B1 /` from the tool's own report: 154980950016 →
157869846528 available. **Available-byte movement on this busy filesystem is not
exact per-path attribution** — the sync daemon's own `auto_gc` ran during the same
window.

Deleted: **33,702 paths, 2,021,691,773 bytes (1.88 GiB)**. Every path is listed in
the run output with its pre-delete bytes, class and pre-delete `/proc` capture.
The table is mechanical, so it is reproduced here by top-N rather than in full:

| path | pre-delete bytes | class | pre-delete /proc refs |
| --- | ---: | --- | --- |
| `/var/tmp/fh` | 1545884624 | 2 tmp + logs | none (no /proc exe, cwd or fd reference) |
| `/var/tmp/jiti` | 261689442 | 2 tmp + logs | none (no /proc exe, cwd or fd reference) |
| `/var/tmp/flatpak-cache-PTWLW3` | 54644456 | 2 tmp + logs | none (no /proc exe, cwd or fd reference) |
| `/var/tmp/glla-behavioral-STz7Ku` | 19710000 | 2 tmp + logs | none (no /proc exe, cwd or fd reference) |
| `/var/tmp/node-compile-cache` | 11320000 | 2 tmp + logs | none (no /proc exe, cwd or fd reference) |

No row carried a live reference, which is the invariant the capture exists to
prove. The remainder were `pi-*` and `glla-*` fixture directories past the age
floor.

**Idempotency:** an immediate second `disk clean` reported
`Deleted 0 path(s), 0.0 B freed`, so the cleaner does not re-report or re-delete
what it already cleaned.

### Deliberately NOT deleted

| Item | Size | Why it survived |
| --- | ---: | --- |
| `/var/lib/swapfile` | 32.0 GiB | REPORT-only. Verified present after the run: `stat` = 34359738368 bytes, root:root, mode 600. Reported with `run: sudo rm -f /var/lib/swapfile` |
| `/home/dracon/dracon-lfs-proof` | 122.2 MiB | REPORT-only stray dir, needs per-path approval. Still present. |
| `/home/dracon/tmp-shield` | 7.0 MiB | same. Still present. |
| `/home/dracon/eve-audit-new` | 667 B | same. Still present. |
| `.dracon-git-recovery-20260831-1610` | 26.2 KiB | same. |
| 16 `tmp_pack_*` | 12.5 GiB | `SKIP` — younger than the 6-hour floor, i.e. plausibly in-flight packs |
| `~/.cache/ms-playwright`, `~/.npm`, `~/.bun/install/cache`, `~/.cache/google-chrome`, `~/.cache/puppeteer`, `~/.cargo/registry/cache` | 12.1 GiB total | `SKIP` — in use, with owning PIDs shown |

`.pi-glla`, the three canonical checkouts and all media paths are untouched.

## Defects found by running it (not by review)

1. **`~` was not expanded** — `PathBuf::from("~/Dev")` is a literal directory
   named `~`, so the first run printed `Nothing to clean` on a host holding
   13 GiB of git scratch. All scan roots now go through `expand_tilde`.
2. **Blocks ran after the report** — initially appended below the
   `if json` / `Nothing to clean` section, so they never executed.
3. **"Any git process blocks everything" blocks everything, always** — the sync
   daemon commits on a one-second pulse, so a git process is always alive. First
   correction: per-repo attribution by `cwd`. Second live run proved that
   insufficient too (the daemon's `commit` in each repo matched). Final rule:
   match the **subcommand** — only `gc`, `repack`, `pack-objects`,
   `index-pack`, `prune`, `unpack-objects`, `git-remote-*` and `*-pack` helpers
   count as pack writers; `add`/`commit`/`status`/`push` do not.
4. **`--class` did not restrict the scan** — filtering findings after a full
   walk made `--class git` slower (280s) than the whole audit. Selection is now
   applied during the scan.
5. **Strays were classified after sizing** — the scan ran `directory_bytes` on
   every entry under `~`, walking all of `~/Dev`, before discarding it. Now it
   classifies first and sizes only what it keeps.
6. **`/proc` was re-walked per finding** — the first audit took **244 seconds**
   and had to be killed once. Two causes: per-finding `/proc` walks (fixed by
   `ProcSnapshot`, plus a `stop` `break` in the prefix range loop that was
   `continue`-ing through the whole index) and serial `lstat` over 33k entries
   (fixed by `directory_bytes_parallel`). The full audit now runs in **3 seconds**.
7. **33k per-path lines buried the findings that needed a decision** — the
   report now prints per-class totals plus the top `--detail N` (actionable
   first, then largest), with the rest aggregated by count and bytes.
8. **Dot-prefixed `/tmp` entries were deletable** — `/tmp/.X11-unix` and friends
   are application runtime state, not litter. They are now `REPORT` and only
   shown above 256 MiB, so a real hog cannot hide behind a leading dot.

## Gates

```
cargo test   -p dracon-system --locked              703 passed, 0 failed
cargo clippy -p dracon-system --locked -- -D warnings   clean
cargo build  -p dracon-system --release --locked       ok
```

Breakdown: 687 unit + 4 CLI + 12 service. New coverage:
`disk_refs` (fd/cwd/exe blocking, unrelated process not blocking, unreadable
`/proc` = unknown, directory-cwd blocking, a live `/proc` scan finding a real
held descriptor, and the same file no longer blocking after release);
`disk_tmp` (age floor, symlink refusal, whole-tree directory size, rotated-log
naming, fresh-vs-old rotation, oversized live log); `disk` (all four classes
named in an empty report, incomplete scan never reading as clean, dead swapfile
report-only with the exact command, cache users carrying PIDs, bun/playwright/
puppeteer/chrome coverage, apply touching only APPLY findings, a path that
becomes used between audit and delete being skipped, evidence rows carrying
bytes and capture); `nix_current_generation_tests` (marker parsed rather than
inferred, no-marker → no current, **rolled-back current ≠ highest survives**,
closure comparison, unreadable link protected, argv one element per generation,
protection not inflating the keep count); `system_profile_spread_tests` (link
name parsing, missing profile → no plan, profile argv shape, and a live check
that the booted generation is never in the plan).

## Remaining limitations

- The root-owned swapfile still needs a human with sudo. An unprivileged tool
  correctly refuses it.
- Cache directories are `REPORT`/`SKIP` only — there is deliberately no `--apply`
  path, because purging `~/.npm` or `~/.cargo/registry/cache` out from under a
  package manager corrupts its index. The report names the owning PIDs so a
  human can decide when to run the manager's own purge.
- Nix *store* garbage collection still lives on the `guard clean` path; `disk`
  reports and prunes generations but does not run `nix-collect-garbage`.
- The system-profile prune still requires root to take effect; the tool computes
  the closure-safe plan and reports the privileged script path.

## Follow-up 2026-10-27: swapfile deleted by operator

The operator ran `sudo rm -f /var/lib/swapfile` interactively (sudo requires a
password; non-interactive `sudo -n` and `pkexec` both failed). Verified after:
`/var/lib/swapfile` no longer exists, `swapon --show` still lists `/dev/zram0`
+ `/mnt/data/swapfile`, and `df -B1 /` shows 180,198,932,480 bytes available
(up from ~147 GiB). The 32 GiB orphan on the nvme root is gone.
