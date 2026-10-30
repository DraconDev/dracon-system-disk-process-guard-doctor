# Endpoint-only sampler regression fixture

The disposable mock run exercises the new `SAMPLE_ROOTS=NO` branch of `anchored-pass.sh`.

- Stable endpoint case: one `nix-store --gc --print-roots` call inside the anchor (the after-count), `root_series_samples=0`, and `roots_unchanged=YES`.
- Changed endpoint case: the same single root-list call returns one extra root; the anchor reports `roots_unchanged=NO`.

This checks that disabling concurrent sampling does not bypass endpoint comparison. The retry-gate fixture suite is preserved separately in `../retry-gate-rerun/` and still verifies baseline match/mismatch, same-count root-set churn, GC-event rejection, and fail-closed journal handling. All dependencies here are mock binaries; these tests do not read or modify Nix state.

The current helper copies are included for audit. The live retry wrapper disables sampling only inside its accepted bracket; the standalone anchor retains optional sampling by default.
