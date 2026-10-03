# dracon-system v0.112.44 (2026-10-03)

ROUND3 audit remediation (5 LOW findings, all fail-closed hardening).

- Tmp cleanup fails closed twice over: the freshness probe treats any
  walk error as fresh (keep), and an unreadable `/proc` aborts the
  pass loud in both apply and dry-run instead of silently disabling
  open-file protection.
- Relocation treats unknown destination space as not-fitting, so a
  `df` failure can no longer strand a partial destination blocking
  retries.
- `doctor` checks the three M8 watchdog timers as required checks —
  missing backstops fail `--strict`.
- The guard deployment checker canonicalizes symlinked roots before
  mount matching and mirrors the sync checker's 3b companion loop.

Validation: workspace gates green (test/clippy/deny/fmt), both
deployment-checker suites pass including the new symlinked-root and
companion cases.

Install:

```bash
cargo install dracon-system --version 0.112.44 --locked
```

[Full changelog](https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.112.43...dracon-system-v0.112.44)
