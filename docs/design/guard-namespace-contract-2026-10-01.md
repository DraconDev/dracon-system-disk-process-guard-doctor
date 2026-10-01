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
