# Disk-full recovery and rapid-fill warnings — 2026-10-20

## Live findings

The guard journal records root at 99% at 17:00 and 100% at 17:30 and
18:00 BST. At investigation time (20:21 BST), root had recovered to
168 GiB available (81% used); `/mnt/data` had 159 GiB available. Inodes
were 12% used. The immediate block and inode exhaustion had cleared.
No additional deletion was performed by this investigation. The existing
workspace `target/` was absent at inspection; attribution of its removal
is not established. Do not credit this patch with recovering those bytes.

The active guard is `dracon-system-guard.service`. It was running, and
its journal shows repeated rapid-fill warnings (20–50 GiB/h), tmp cleanup,
and correctly retained credential-flagged Trash content. Its normal tmp
passes reclaimed tens to hundreds of MiB while the filesystem remained full;
that alone did not resolve the larger storage pressure. Current policy has
`auto_cleanup_rust = false`. Broad deletion or enabling unattended Rust
cleanup is not part of this repair.

A read-only shallow size survey found `/tmp` about 28 GiB and `~/.cache`
about 15 GiB. These contain active browser profiles, audit checkouts and
other agents' work, so they are not blanket deletion candidates.

## Bounded guard improvement

A large reclaim can make the rolling byte-history regression negative and
hide rapid growth after recovery until the older samples expire. Compute
rate only from samples since the most recent drop in used bytes. Retain
the existing minimum of three samples over 60 seconds, so a cleanup alone
cannot invent growth. Reclamation also clears the previous rapid verdict so
fresh rapid growth can send a new entry notification. Small fluctuations can delay an estimate; disk usage
threshold alerts continue independently.

Rapid-fill notifications and structured events now include user-available
GiB and estimated hours until full if that rate continues. Use df's
`avail_bytes`, excluding reserved blocks, rather than total minus used.
The estimate is conditional, not a promise. Existing alert cadence and
cleanup policy stay in force.

Regression coverage exercises a 201 GiB reclaim followed by 120 GiB/h
growth, minimum post-reclaim samples, shrinking disks, ordinary growth,
and warning text at both positive and zero available space.

## Further work

- Attribute writes by producer and mount at warning time using bounded
  sampling; recursive disk scans inside every guard pass would add I/O
  pressure during an incident.
- Record before/after available bytes for every cleanup pass, including
  partial failures, so a busy cleanup log cannot imply meaningful relief.
- Surface blocked/protected cleanup candidates separately from reclaimable
  totals; preserve credential guards and active-build checks.
- Give the second disk its own rapid-fill history. Extra mounts currently
  provide visibility while primary-mount pressure controls cleanup.

## Dependency validation

The required `cargo deny check` found RUSTSEC-2026-0382 in
`rustls-webpki 0.103.15` and RUSTSEC-2026-0381 in `webpki-roots 1.0.9`.
The parent workspace lockfile now selects the advisory's patch fixes,
0.103.16 and 1.0.10 respectively. No dependency manifest constraints or
other locked versions were changed. Validation and deployment use this
updated lockfile.
