# Cleaner coverage for the audit gaps (2026-10-27)

Follow-up to `leftover-audit-2026-10-27.md`: close the three coverage gaps that
made `/tmp` litter and the agent-tool caches invisible to the one-shot cleaner.

## Gap 1 — e2e/test-litter names the fast path did not cover

The guard daemon already had a short-floor fast path (`tmp_e2e_patterns` +
`tmp_e2e_min_age_hours`, 6h), but six families observed on this host were not
in the default list, so they waited out the general 24h floor:

`ai-tts-*`, `iso-route`, `base-copy`, `synth4*`, `junk-runner-e2e.*`,
`node-compile-cache`.

`junk-runner-e2e.*` uses a DOT before the random suffix
(`junk-runner-e2e.qVEdrH`), so the dash form would miss every one. A
regression test pins both the match and the non-match.

## Gap 2 — the one-shot audit never consulted the fast path

`dracon-system disk audit` classified `/tmp` only against
`tmp_cruft_min_age_hours` (24h), so everything between 6h and 24h was invisible
— the earlier full audit reported 63 MiB of `/tmp` while much more was there.

`DiskOptions` now carries both fast-path knobs, and each entry is adjudicated by
one pure classifier:

```rust
pub(crate) fn tmp_cruft_floor(
    file_name: &str, age_hours: u64,
    e2e_patterns: &[String],
    general_floor_hours: u64, e2e_floor_hours: u64,
) -> Option<u64>
```

The walk uses the LOWER of the two floors, then each entry is adjudicated by its
governing floor; entries clearing neither are dropped uncounted. Empty patterns
(or `tmp_e2e_min_age_hours = 0`) disable the fast path — same contract as the
daemon. The finding reason names which floor applied.

## Gap 3 — live-churn safety (the important one)

**A directory mtime does not move when its contents are rewritten.** Measured
during this work on `/tmp/node-compile-cache`: top-level mtime 17–18h old,
**317 files newer than 6h**, newest written seconds before the reading. Aging a
tree by its top mtime therefore marked a directory under active writes as
`APPLY`, and `disk clean` would have deleted it.

The daemon never had this hole — it refuses any tree with fresh nested content
(`tree_has_fresh_content`). The one-shot path must not be a weaker route to
deletion, so `disk_tmp` now measures content age in the SAME walk that sizes the
tree (no extra `lstat`) and `disk::audit` applies the daemon's rule:

```rust
pub(crate) fn tree_is_live_churn(
    newest_content_age_hours: Option<u64>,   // None = walk incomplete
    threshold_hours: u64,
) -> bool
```

- Fresh nested content under the governing floor → **REPORT**, never APPLY, with
  a reason stating both ages so the difference is visible.
- Unreadable/truncated walk → treated as FRESH (fail closed), matching the
  daemon's walk-error behavior.
- A readable but EMPTY tree has no content to be fresh, so it falls back to its
  own mtime — otherwise such a dir could never be reclaimed.
- An UNREADABLE tree fails CLOSED and is reported, not deleted. On this host
  that is the `systemd-private-*` family: root-owned, mode 0700, so the walk
  cannot list them at all. Roughly 20 of the ~32 live class-2 findings are
  exactly these 0-byte dirs, each reported as *"nested content is unreadable
  (permissions) — cannot prove the tree is idle"*. They are owned by running
  services and are never deletable by the user account, so REPORT (never
  APPLY) is the correct disposition. The daemon behaves the same way — its
  `tree_has_fresh_content` returns "fresh" on a read error and skips the
  entry entirely.

Live result after the fix:

```
• REPORT /tmp/node-compile-cache (145.4 MiB) — tmp-dir top-level mtime 18h old
         but newest nested content is 0h old — live churn, under the 6h floor;
         never auto-deleted
```

## Gap 4 — agent-tool caches

Added five cache families, each scoped to a cache SUBDIRECTORY (a test forbids
bare dotdirs): `.codex/cache`, `.codex/plugins/cache`, `.hermes/cache`,
`.hermes/audio_cache`, `.hermes/image_cache`.

Deliberately NOT covered, with reasons:

- `.codex/packages` (1.9G) and `.codex/sessions` (1.1G) — installed tool
  packages and conversation history. Neither is a cache; deleting them
  uninstalls the daemon or destroys user data.
- `.hermes/hermes-agent` (2.3G) — the agent's own installation and source tree,
  not a cache.
- `.hermes/backups`, `.hermes/cron`, `.hermes/hooks` — durable operator state.

Live report now shows real bytes rather than three zeros:

```
• SKIP /home/dracon/.codex/cache (46.3 MiB) — owned by codex (3 processes)
• SKIP /home/dracon/.codex/plugins/cache (27.5 MiB) — owned by codex (3 processes)
• REPORT /home/dracon/.hermes/audio_cache (0.0 B) — no owning process
```

## Contract preserved

All new surfaces are REPORT/SKIP-first. No `--apply` path was added for any new
family, and the audit now refuses to mark a live-churn tree deletable at all.

## Verification (observed on this tree)

Workspace gates, run from the parent workspace:

- `cargo clippy --workspace --locked --all-targets -- -D warnings` — **exit 0,
  clean.** (An earlier revision of this document claimed this gate was blocked
  by a dead helper in `dracon-warden`; that was wrong. The helper is not present
  in `dracon-warden/tests/integration_test.rs` and the gate passes. Corrected.)
- `cargo deny check` — exit 0, advisories/bans/licenses/sources ok.
- `cargo test -p dracon-system --locked` — all suites ok, exit 0.
- `cargo test --workspace --locked` — 30 suites ok; 7 failures, all in
  `dracon-sync` (`test_push_to_named_remote_*`, `bucket_qualification_*`,
  `excluded_revert*`), all resource-related: the host was at load 80–99 with
  ~1 GB available during that run. Re-running `dracon-sync` alone immediately
  after passed **1694 tests, 0 failed**.

Live checks:

- `disk audit --class tmp` → 32 findings, 211.6 MiB, with
  `/tmp/node-compile-cache` correctly REPORTed as live churn instead of APPLY.
- `disk audit --class caches` → the new families report real bytes (above).

## Delete-time re-check (TOCTOU)

The audit refusing to mark a live tree APPLY is not sufficient on its own: a
tree idle during the scan can be written to before `disk clean`'s delete loop
runs. `disk::apply_with_floors` therefore re-walks every directory candidate
immediately before `remove_dir_all` and keeps the tree if any nested content is
newer than **the floor that qualified that finding**.

The qualifying floor travels ON the finding (`Finding::qualifying_floor_hours`,
set in the class-2 audit loop), not as a global window. A single global
`min(e2e, general)` window would apply the 6h e2e floor to 24h general-floor
findings, so a general-floor tree whose content was refreshed to 12h during
the audit→delete gap — by an mtime-PRESERVING restore (`cp -a`, `rsync -a`,
`tar -x`, `unzip`), exactly how these test trees are rebuilt — would be
deleted, while the daemon's own general pass keeps it. A finding with no
recorded floor FAILS CLOSED: the tree is kept.

Tests for this behaviour:

- `the_delete_time_check_uses_the_floor_that_qualified_the_finding` — the
  discriminating case. Both fixtures hold content aged 12h (inside the 24h
  general floor, outside the 6h e2e floor). The **general-floor (24h)**
  finding must be KEPT; the **e2e-floor (6h)** finding with the same 12h
  content must still be DELETED — so the test proves the per-finding floor and
  not a fixed 6h window, and it cannot be satisfied by a blanket
  "never delete" rule.
- `a_tree_that_became_live_churn_after_the_audit_is_kept_at_delete_time` —
  an **e2e-floor (6h)** finding whose content was written during the gap
  (0h) → 0 bytes freed, tree survives, disposition downgraded to REPORT with
  the reason recorded.
- `a_finding_without_a_known_floor_fails_closed_and_keeps_the_tree` — a
  finding with no recorded floor is never deleted on a guessed window.

## Final observed numbers (last run on this tree)

- `cargo test --workspace --locked` — **exit 0, 44 suites ok**.
- `cargo clippy --workspace --locked --all-targets -- -D warnings` — exit 0.
- `cargo deny check` — exit 0.
- `cargo build --release --locked` — exit 0.
- `disk audit --class tmp` — 37 findings, 211.7 MiB, with
  `/tmp/node-compile-cache (146.6 MiB)` REPORTed: *top-level mtime 19h old but
  newest nested content is 0h old — live churn, under the 6h floor; never
  auto-deleted*.
- `disk audit --class caches` — `.codex/cache` 46.3 MiB and
  `.codex/plugins/cache` 27.5 MiB both SKIP as codex-owned; hermes audio/image
  caches REPORT as unowned.

Counts differ between runs because `/tmp` is live: the audit-observed trees
(`iso-route`, `ai-tts-*`, `junk-runner-e2e.*`) were reaped by their owning test
runs between observations. The finding count is therefore not a contract — the
behaviour (which disposal each entry receives) is.
