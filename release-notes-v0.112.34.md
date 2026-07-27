# dracon-system v0.112.34 — full-audit remediation batch 4 (2 HIGH fixes)

Released 2026-07-26. Remediation batch 4 of
[`AUDIT_FULL_2026-07-26.md`](./AUDIT_FULL_2026-07-26.md): 2 HIGH
fixes (SYS-H1, SYS-H2). Each was independently reviewed against
the source before acceptance. Behavioral verification ran against
real scratch repos and a busy-loop reproduction in-process.

## Fixed

### SYS-H1 — guard daemon busy-looped forever after the first interval

`elapsed` was declared once before the outer daemon loop, so after
the first full interval the inner 1-second sleep loop never ran
again — `run_guard_once` executed back-to-back continuously
(df/ps/du + walkdir scans every pass). On a 35-repo fleet this
generated a noticeable CPU/IO footprint every second, masking
genuine spikes in the guard report.

`elapsed` is now reset inside the outer loop, every pass. The guard
now actually sleeps between scans at the configured interval.

### SYS-H2 — `link apply` could never fix a drifted symlink

Existing symlinks were routed through `check_safe_to_delete`, which
**always refuses symlinks** — so `apply` errored on every existing
symlink, including the drifted ones it exists to repair (and even
in-sync ones, since there was no short-circuit). Operators got the
"`link apply` failed: refuse to delete symlink" error with no
recovery path.

Now:

- In-sync entries are skipped (no syscall, no error).
- Drifted symlinks are unlinked directly via
  `fs::remove_file(&link)` (unlinking a symlink never touches its
  target, so the safety invariant is preserved without consulting
  `check_safe_to_delete`) before re-creation.

Regression tests added in `links_tests.rs`:

- `apply_link_policy_fixes_drifted_symlink_and_is_idempotent`
- `evaluate_link_handles_missing_and_sync_cases`
- `evaluate_link_missing_link_returns_missing`

## Tests

- 79 → 88 unit tests in `dracon-system` (added 9 link-related
  tests in `links_tests.rs` and the existing `tests.rs`).
- All 88 pass; clippy clean.

## Upgrade notes

- No operator action required. The guard daemon's behavior changes
  to (a) actually sleep between scans and (b) successfully repair
  drifted symlinks via `link apply`.
- If `link apply` had previously been failing in the operator's
  logs with "refuse to delete symlink", those entries will now
  succeed — the operator can re-run to confirm.

## Audit cross-references

- `AUDIT_FULL_2026-07-26.md` — §"SYS-H1 / SYS-H2"