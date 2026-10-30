# 2026-10-30 19:20 root-count mismatch

A complete read-only measurement ran from 19:20:26 to 19:22:09 UTC with the continuous root sampler enabled. It preserved a 640-root baseline and matched `roots_before=640`, but ended at `roots_after=636`; four entries were removed from the root list and none added. The root-series samples ranged 636–641 with nine transitions. Store paths remained 37,987 before/after and reachability remained 4,610 before/after. The service/journal check reported no `nix-gc.service` run during the bounded window.

This attempt was rejected by the baseline gate. Its observations motivated disabling continuous root polling inside the accepted measurement bracket; the separate endpoint-only pass at 19:40 is retained in the adjacent evidence folder. The cause of the four root-list removals is not established.
