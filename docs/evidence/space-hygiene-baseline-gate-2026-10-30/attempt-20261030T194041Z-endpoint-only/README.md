# Fresh post-GC endpoint-only sweep — 2026-10-30 19:40 UTC

This was a complete, read-only anchored measurement using the current preserved-baseline retry gate. The anchor ran from 19:40:57 to 19:42:18 UTC.

- Exact preserved pre-sweep baseline: **609 roots** at 19:40:57 UTC.
- Anchored endpoints: `roots_before=609`, `roots_after=609`; the sorted root lists are byte-identical.
- Store paths: **37,539 before and after**; reachability: **4,606 before and after**.
- `gc_unchanged=YES`; the recorded journal window is empty and the service start timestamp remained 2026-10-30 00:00 GMT.
- Continuous root polling was disabled during this accepted bracket; `root_series_samples=0`. The baseline and endpoint lists remain preserved.
- The attempt is a fresh post-GC baseline; it does not replace or reconstruct the earlier unpreserved 610-root observation. The report separately records the scheduled GC that ran before this pass.

`run-20261030T194041Z.txt` contains the exact byte readings for all areas/candidates measured in this pass, including the corrected Git-history aggregation. `attempts.log` contains the wrapper acceptance decision; `gc-window-20261030T194041Z.txt` contains the journal result. No cleanup or collection command was run by the survey.
