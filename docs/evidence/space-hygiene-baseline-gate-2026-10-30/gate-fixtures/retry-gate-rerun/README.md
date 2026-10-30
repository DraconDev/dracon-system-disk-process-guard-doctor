# Retry-gate regression rerun — 2026-10-30

The current `retry-anchored-pass.sh` was exercised with the disposable mocks under `../gate-fixtures/`:

| Case | Exit | Result |
|---|---:|---|
| Stable baseline, no GC event | 0 | Accepted |
| Changed root count | 1 | Rejected after bounded attempt |
| Same-count root-set churn | 0 | Accepted as count-only contract permits |
| GC journal event | 2 | Rejected; no-GC condition violated |
| Journal unavailable | 2 | Rejected; no-GC evidence unavailable |

The captured per-case `attempts.log`, root lists, mock output and stdout are included. These are logic tests against mock dependencies, not observations of live host state.
