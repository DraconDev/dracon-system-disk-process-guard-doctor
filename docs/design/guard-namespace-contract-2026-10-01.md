# The guard's reclaim path was a silent no-op for four days (2026-09-28 → 10-01)

Status: closed. The `ReadWritePaths` grant shipped 2026-10-01 and is live; the
runtime check that would have caught it is now part of
`scripts/check-unit-deployment.sh`.

## What happened

`dracon-system-guard.service` runs with `ProtectSystem=strict`, which remounts
the whole filesystem read-only and then re-opens write access only for the paths
named in `ReadWritePaths=`. A path that is *not* named is not writable, even
though the disk it lives on is a perfectly ordinary read-write ext4 mount
outside the sandbox.

The policy named two storage roots on the second disk:

```toml
quarantine_dir     = "/mnt/data/quarantine"
relocate_cold_root = "/mnt/data/cold"
```

`clean_quarantine_first = true` routes every auto-reclaimed candidate through
`quarantine_first_remove()`, which *moves* the tree to `quarantine_dir` with a
manifest. The unit granted `/mnt/data` — not the two subtrees — and
`ProtectSystem=strict` had made `/mnt/data` itself read-only inside the service
namespace. So the move target did not exist as writable anywhere in the guard's
world.

## Why it was invisible

`auto_cleanup_rust_targets` treats a failed quarantine as a skip, not as a
reason to fall back to a direct delete:

```rust
Err(e) => {
    eprintln!("⚠️ failed to quarantine {}: {}", target.path.display(), e);
    continue;
}
```

That is the right call for safety — a reclaim that cannot be made reversible
should not silently become an irreversible one. But combined with the missing
grant it means **the entire reclaim path reclaimed zero bytes while looking
busy**. The journal for 2026-09-30 23:41, the first pass at the 85% action
threshold:

```
⚠️ failed to quarantine /home/dracon/Dev/folder-auto-banner/target
⚠️ failed to quarantine /home/dracon/Dev/dracon-log/target
⚠️ failed to quarantine /home/dracon/Dev/dracon-platform/target
⚠️ failed to quarantine /home/dracon/Dev/dracon-utilities/target
⚠️ failed to quarantine /home/dracon/Dev/eve/target
⚠️ failed to quarantine /home/dracon/Dev/dracon-strategy/ai-auto-video/target
... 11 real trees, all EROFS
```

The `📦 Rust quarantined:` line that a working reclaim prints never appeared,
and `disk/rapid-fill` kept reporting growth — 32.1 GiB/h on 2026-10-01 — because
nothing was ever moved. The one line that mattered was also competing with
roughly twenty `⚠️ failed to remove tmp entry … Permission denied` lines per
30-second pass, most of them systemd-private `/tmp` directories no user can
remove and nobody expects to.

`/mnt/data/quarantine` stayed empty across those four days. The 12 entries in it
now were created on 2026-09-28 by a *manual* run outside the service namespace,
where `/mnt/data` is writable and the move succeeds.

## The fix

`ReadWritePaths=` must name the paths the guard writes, not the disk they sit
on. The `-` prefix keeps both entries optional so a host with no second drive
still starts:

```
ReadWritePaths=%h/.dracon %h/.local/state/dracon %h/.local/share -%h/Dev -%h/.local/share/Trash -%h/.cargo -%h/.cache -%h/.npm -%h/.local/state/nix /tmp -/mnt/data/quarantine -/mnt/data/cold
```

Note that `-` only means "ignore this entry if the path is missing". On a host
where `/mnt/data/quarantine` *does* exist — every host that has configured one —
the entry is a real grant, and a read-only parent still wins over a read-write
superblock.

## The check that closes the loop

Steps 1-5 of `scripts/check-unit-deployment.sh` compare the shipped unit to the
deployed file and ask systemd whether the result loads. None of them can see
whether the running service can actually *write* anything, which is the only
question that matters here. Step 6 answers it against the live namespace.

Joining the namespace to poke at it needs `CAP_SYS_ADMIN`, which an
unprivileged operator shell does not have — that is why this was previously
written off as "not performable". It is performable, and the reason is that the
kernel already publishes the answer: every mount's flags are in
`/proc/<pid>/mountinfo`, and reading another same-uid process's mountinfo needs
no privilege beyond ptrace-mode read. For a path, the mount that decides access
is the **longest mount point that prefixes it**; among equal prefixes the later
line shadows the earlier one.

```
$ scripts/check-unit-deployment.sh
  ✓ quarantine_dir=/mnt/data/quarantine is read-write in the running namespace (mount: /mnt/data/quarantine)
  ✓ relocate_cold_root=/mnt/data/cold is read-write in the running namespace (mount: /mnt/data/cold)
✓ OK: deployed unit matches the shipped unit (~/.config/systemd/user/dracon-system-guard.service).
```

The live namespace, for the record — the parent read-only and the two subtrees
read-write, which is what a `ReadWritePaths` entry looks like from inside:

```
622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
650 622 8:2 / /mnt/data ro,nosuid,noatime shared:527 master:129 - ext4 /dev/sda2 rw
675 650 8:2 /cold /mnt/data/cold rw,nosuid,noatime shared:528 master:129 - ext4 /dev/sda2 rw
676 650 8:2 /quarantine /mnt/data/quarantine rw,nosuid,noatime shared:529 master:129 - ext4 /dev/sda2 rw
```

"Cannot ask" stays a note, never a failure: a stopped service, a pid that exited
between the query and the read, and a non-dumpable process all mean "no
opinion" — the same rule the loaded-unit step already follows. A host with no
storage roots configured has nothing to check and is likewise not a failure.

## Test seams and regression coverage

`scripts/check-unit-deployment.sh` takes `POLICY_FILE`, `GUARD_MAINPID` and
`GUARD_MOUNTINFO` so `scripts/test_check_unit_deployment.sh` can drive the
runtime step from mountinfo fixtures on a host with no live service. The cases
that matter:

| Case | What it pins |
|---|---|
| 17 | the pre-fix shape (disk read-only, no sub-mounts) is a **failure** |
| 18 | granting the disk but not the subtree is still a failure, and the granted one still passes |
| 19 | the fixed shape passes and names the mounts it relied on |
| 20 | a later mount at the same point shadows an earlier read-write one |
| 21 | a root under an ordinary read-write mount passes; an uncovered root is reported |
| 22 | a commented-out or relative knob is not a configured root |
| 23 | a stopped service and an unreadable mountinfo source are notes, not alarms |
| 24 | a root under `/` with no mount of its own resolves through the root mount |

Case 24 exists because the first implementation got it wrong: `case "$target"`
in `"$point"/*` compiles the pattern `//*` for the root mount, which matches
nothing, so every ordinary path looked "not covered by any mount". The fix
handles `/` before the `case`.

Every case was mutation-checked — disabling the read-only verdict, taking the
first match instead of the longest, treating an unreadable mountinfo as a pass,
dropping the root-mount special case, and short-circuiting the check entirely
each produce a specific failure.

## Two unrelated defects the suite was hiding

Found while adding case 9, both worth keeping fixed:

- The suite's header claims "every case drives the script with explicit
  repo/deployed paths so the host's real unit is never touched". Case 9 did not,
  and a case before it left `$deployed` mutated. On any host with the guard
  installed, case 9 compared the synthetic fixture against the **real deployed
  unit**, failed at the byte comparison, and reported a drift that did not
  exist. The suite now runs against an isolated `HOME` so unit discovery can
  only ever find a fixture, and case 9 resets `$deployed` first.
- `ReadWritePaths` naming the disk is not a weaker version of naming the path;
  under `ProtectSystem=strict` it grants nothing at all. Worth stating because
  it is the natural mistake to make when reading the directive.

## Verification

```bash
scripts/test_check_unit_deployment.sh   # 24 cases
scripts/check-unit-deployment.sh        # live service + real policy
cargo test --locked                     # 397 tests
```

---

## Follow-on (2026-10-02): the TTL was never enforced

The `ReadWritePaths` fix above made quarantine able to write. It did not make
the TTL mean anything, and the two changes combine into a leak.

`quarantine_expire` had exactly one non-test caller — the CLI `Expire` arm in
`src/quarantine.rs`. There was no systemd timer, no cron entry, and no daemon
pass. So `quarantine_ttl_days` (default 30) only ever gated a command a human
had to remember to run.

The reason that matters is what a quarantine move actually costs. It copies the
tree to the second disk and deletes the origin, so:

| | `/` | `/mnt/data` |
|---|---|---|
| at move time | freed | consumed |
| at expiry | unchanged | freed |

`/` is freed immediately, so the guard's job looks done. `/mnt/data` is only
given back if expiry runs. With expiry never running, every reclaimed tree is
paid for twice, permanently — the system trades disk on one filesystem for
disk on another and never completes the trade. An expiry that never fires is
not a safety copy.

This was invisible while the reclaim path could not write. The moment the grant
landed, the guard would begin converting deletes into permanent second-disk
usage, unattended, at the 85% action threshold.

### What the daemon now does

`maybe_expire_quarantine` runs on every guard pass, outside the disk-pressure
gate, paced by its own cooldown. Three decisions worth stating:

**Outside the pressure gate.** Every other reclaim path runs at
action/critical because it responds to pressure on `/`. This one drains the
second disk. Gating it on `/` would mean a healthy `/` never drains and the
backlog only clears during a crisis — exactly backwards.

**Its own cooldown, not `auto_cleanup_interval_secs`.** Sharing the knob would
mean an operator tuning how often cleanup runs silently changes when a TTL is
enforced. `quarantine_expire_interval_secs` (default 86400, 0 = opt out) and a
separate `last_quarantine_expire` keep the two cadences independent. 0 is a
documented sentinel and is deliberately *not* floored: a floor here would
re-enable unattended deletion for an operator who explicitly opted out, which
is the opposite failure from the one a floor guards against.

**Gated on `clean_quarantine_first`.** If quarantine is not armed, anything in
the directory came from a human running `quarantine move` by hand, and that must
not silently become an unattended delete.

Every deletion is logged individually with entry name, origin and bytes. Once
the entry is gone the journal is the only record it existed, so an operator
cannot review it from a directory listing. Pinned entries — unreadable
manifest, so no TTL is computable — are reported on every pass for the same
reason: they need `quarantine purge`, and would otherwise accumulate quietly in
a directory whose contract is bounded growth.

### A pinned invariant, and one thing that is not

A pinned entry is `expired = false` by construction in `quarantine_list`, so it
can never reach the removal loop. That single arm in one match expression is
the only thing standing between a corrupt manifest and an un-datable deletion,
so it is now pinned by a test (`quarantine_expire_never_removes_a_pinned_entry`)
rather than by this prose.

The same applies to `protected_paths`: the entire protection rests on ONE call,
`check_safe_to_delete_guard` inside `quarantine_move`. Drop it in a refactor and
the config keeps parsing, the guard keeps listing candidates, and the protected
tree gets quarantined anyway with nothing failing. That is now pinned too.

But pinning the call was not sufficient, and the first version of this work got
that wrong in exactly the way this section warns about. `protected_paths` was
the **only** path-valued guard knob that never expanded `~`. Every sibling —
`quarantine_dir`, `relocate_cold_root`, `relocate_candidate_roots`,
`rust_search_roots`, the guard log path — expands at point of use. This one
passed the raw string to `canonicalize`, which resolves `~` against the process
CWD, returned NotFound, and was `continue`d **silently**. So:

- the shipped `protected_paths = ["~/Dev/dracon-utilities"]` parsed,
- the guard reported the 64.0 GiB `dracon-utilities/target` as a candidate,
- the `check_safe_to_delete_guard` test passed, because it hardcoded an absolute
  path.

Config inert, test green, protection absent. The empirical proof that does not
depend on reading any of this:

```
$ dracon-system relocate /home/dracon/Dev/dracon-utilities/target --to /mnt/data/cold
│ Size ┆ 64.0 GiB in 99653 files       │   ← not protected
```

The fix is two parts, and both were needed:

1. `~` is expanded for `protected_paths` in `normalize_guard_policy_with_home` —
   the single normalization boundary, and idempotent. Expanding at point of use
   would have fixed only the one caller that happened to call `expand_tilde`.
   The expansion is deliberately NOT reported through `adjusted`: that list means
   "clamped to a legal range" and `report_clamps` prints it verbatim, so a
   perfectly legal `~` form would emit a false "clamped" warning on every policy
   load — teaching the operator to ignore warnings, which is the very thing this
   section is about. `src/policy_tests.rs` pins that it stays unreported.
2. An unresolvable protected entry now **warns**, once per entry, naming the
   entry. The original `continue` had no diagnostic at all, which is precisely
   why the `~` form could fail invisibly for four days while the operator
   believed the tree was protected. It still fails **open** rather than
   refusing every candidate: a typo in one entry must not become a disk that
   fills and never reclaims.

The regression test drives the whole policy path — parse TOML, normalize, ask
the real classifier — so an absolute-path test cannot be green while the shipped
config is inert.

What is *not* fixed: `guard clean --rust` still lists protected candidates in
its dry-run preview and refuses them only at apply time, so the preview
overstates what is reclaimable. Cosmetic, but it is why the preview still shows
`dracon-utilities/target` as a candidate after `protected_paths` is set.
