# 2026-10-30 baseline-gate evidence

Report-only evidence for the latest Nix-root baseline attempts and the retry-gate fixture checks. No cleanup, collection, or surveyed-data mutation was performed.

## Full measurement attempt — 15:11:27–15:28:23 UTC

- Preserved pre-sweep root count/list: 656.
- Anchored pass: roots before 656, after 636; root sets differed. The pass is **not baseline-qualified**.
- Store paths: 37,983 before, 37,987 after. Reachability warm-up: 4,608 paths.
- 110 root-count samples ranged 623–2,719 with 79 transitions.
- The wrapper recorded `gc_unchanged=YES`; the journal window was empty and the service last-start timestamp remained 00:00 GMT. The count mismatch is still a failed acceptance gate.
- The full size readings are retained as point-in-time observations, not as a stable snapshot.

## Second bounded attempt — started 15:28:24 UTC

- Preserved baseline: 639; the anchored pass began at 15:28:49.
- The outer 30-minute timeout ended before the after-count or final journal comparison was captured. Treat all partial `du` readings as spot observations only; this attempt did not qualify.
- A read-only after-check found no `nix-gc.service` journal entries since the attempt start and the service last-start timestamp was still 00:00 GMT. See `attempt-20261030T152824Z-incomplete/no-gc-check-after-timeout.txt`.

## Fixture checks

The disposable fixture outputs and scripts are under `gate-fixtures/`. They
exercise acceptance of an unchanged count with no GC event, acceptance of
same-count root-set churn (the contract is count-based), rejection of a
changed count, rejection of a GC event, and fail-closed behavior when the
journal cannot be read. These are gate-logic tests, not observations of the
live host.

The archived `anchored-pass.sh` is the exact version used for the live
15:11 attempt; its `git_history` aggregation emitted an AWK parse error, so
that row was absent. The live cache helper was corrected to use `printf`; the
19:20 host pass included the corrected 27-repository total but failed the
root-count bracket. After disabling the concurrent sampler for the acceptance
bracket, the 19:40 host pass qualified at 609→609 roots with identical root
lists. Its full artifacts are in `attempt-20261030T194041Z-endpoint-only/`.
The optional-sampler regression fixture is in
`gate-fixtures/endpoint-only-sampler/`.

## Fixture rerun and cache-log qualification

The current retry gate's stable, count-mismatch, same-count-root-churn,
GC-event and journal-unavailable fixtures were rerun with explicit isolated
mock paths. Results and per-case logs are in
`gate-fixtures/retry-gate-rerun/`.

The shared cache `attempts.log` also contains entries at 19:37 and 19:39 from
an initial fixture integration invocation that used mocked `nix-store` but
incorrectly wrote to the default live log directory. Those records are not
host observations and are excluded from all claims. The validated 19:40 pass
uses a separate, unique log directory preserved in its own evidence folder.
