# Cleaner coverage for the audit gaps (2026-10-27)

Follow-up to `leftover-audit-2026-10-27.md`: close the three coverage gaps that
made ~9 GiB of `/tmp` litter and the agent-tool caches invisible to the
one-shot cleaner.

## Gap 1 — e2e/test-litter names the sweep found but the fast path did not cover

The guard daemon already had a short-floor fast path
(`tmp_e2e_patterns` + `tmp_e2e_min_age_hours`, 6h), but six families observed
on this host were not in the default list, so they waited out the general
24h floor:

`ai-tts-*`, `iso-route`, `base-copy`, `synth4*`, `junk-runner-e2e.*`,
`node-compile-cache`.

Note `junk-runner-e2e.*` uses a DOT before the random suffix
(`junk-runner-e2e.qVEdrH`), so the dash form would silently miss every one.
A regression test pins both the match and that non-match.

## Gap 2 — the one-shot `disk audit` never consulted the fast path

`dracon-system disk audit` classified `/tmp` only against
`tmp_cruft_min_age_hours` (24h). Everything between 6h and 24h was simply not
seen — this is why the earlier full audit reported 63 MiB of `/tmp` while
~9 GiB was sitting there.

Fixed by giving `DiskOptions` the two fast-path knobs and routing each entry
through one pure classifier:

```rust
pub(crate) fn tmp_cruft_floor(
    file_name: &str, age_hours: u64,
    e2e_patterns: &[String],
    general_floor_hours: u64, e2e_floor_hours: u64,
) -> Option<u64>
```

The walk uses the LOWER of the two floors, then each entry is adjudicated by
the governing floor; entries clearing neither are dropped uncounted. An empty
pattern list (or `tmp_e2e_min_age_hours = 0`) disables the fast path and the
general floor governs everything — the same contract as the daemon.

The finding REASON names which floor applied, so an operator can tell a
6h e2e reap from a 24h general one:

```
• APPLY  /tmp/iso-route (812.1 MiB) — tmp-dir untouched for 10h
         (e2e fast-path floor tmp_e2e_min_age_hours=6)
```

## Gap 3 — agent-tool caches were not a cache family at all

Added `CodexCache` (`.codex/cache`), `CodexPluginsCache`
(`.codex/plugins/cache`) and `HermesCache` (`.hermes/cache`) to
`CacheFamily`. Each is scoped to a cache SUBDIRECTORY and a test forbids a
bare-dotdir path: `.codex` holds auth and plugin state, `.hermes` holds
backups, cron and hooks — none of that is a cache.

Live behavior, unchanged contract: unowned → REPORT with the manager's own
purge command (no `--apply` path); held by a live process → SKIP with pids.

```
• REPORT /home/dracon/.hermes/cache (0.0 B) — hermes cache; no owning process. …
• SKIP   /home/dracon/.codex/cache (0.0 B) — owned by codex (2 processes; pid 250296, pid 2846975)
```

Labels deliberately omit the word "cache" (the message already appends
" cache; …"), which a live run caught as "hermes cache cache".

## Contract preserved

Both new surfaces stay REPORT/SKIP-first: the fast path only widens which
entries are *reported* in audit mode, and `disk clean` still deletes nothing
held open by a live process. No new `--apply` path was introduced for any of
the three families.

## Verification

- `cargo test --workspace --locked` — 44 suites ok
- `cargo clippy --workspace --locked --all-targets -- -D warnings` — clean
- `cargo deny check` — ok
- `cargo build --release --locked` — ok
- Live: `disk audit --class tmp` reports the previously invisible 6.5 GiB
  across 85 findings; `--class caches` shows the three new families with
  correct owner attribution.
