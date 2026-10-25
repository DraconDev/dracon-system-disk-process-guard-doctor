# Changelog

All notable changes to `dracon-system` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: prior to 0.112.12, `dracon-system` was developed inside the
> [`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
> monorepo. Releases 0.0.0–0.112.11 are recorded in
> [`dracon-utilities/CHANGELOG.md`](https://github.com/DraconDev/dracon-utilities/blob/main/CHANGELOG.md)
> under the `dracon-system` heading. From 0.112.12 onward, this CHANGELOG
> is the canonical record.

## [Unreleased]
## [0.112.50] - 2026-10-25

### Fixed

- **The trash credential heuristic no longer keeps a whole deleted item because of one incidental filename.** The rule matched a loose SUBSTRING anywhere in a filename, and ONE match anywhere inside a deleted item protected ALL of it. On this host that kept **112 items and 28.0 GiB** — including a 5.0 GiB Rust build tree kept alive by a compiled artefact named `…-trust_token.o`, and 1.8 GiB of the operator's own manuscripts kept alive by a chapter called `02-borrowed-credentials.md` — while the disk filled at over 100 GiB/h. The rule is now narrow: a name is credential-like when the file IS a credential file by its own name — by extension (`.env`, `.pem`, `.key`, `.age`, `.p12`, `.pfx`, `.keystore`, `.jks`, `.ppk`, `.asc`, `.gpg`, `.kdbx`, `.netrc`, `.npmrc`), by whole name (`id_ed25519`, `hosts.yml`, `.git-credentials`, `credentials.json`, `service_account.json`, `Login Data`), or by a name whose whole stem is a credential word (`secrets.yaml`, `client_secret.json`). A credential word must be the whole stem or LEAD the name: `credentials_backup` qualifies, `02-borrowed-credentials` does not. This reverses the deliberately broad rule of 2026-10-02, by the operator's decision of 2026-10-25.
- **The trash credential scan no longer stops at depth 8.** The deepest deleted tree on this host reached depth 15, so 42,029 files were never examined — and a directory sitting AT the cap kept its whole top-level entry: 3,977 subtrees holding the bulk of the trash. The cap is now 16, which covers the measured maximum with a level of margin. The deeper walk costs 6 seconds on a 213,000-file trash, and the purge runs in the background cleanup tier. A tree deeper than the cap still fails CLOSED and is kept.
- **A committed `.env` template is not a credential.** `.env.example`, `.env.sample`, `.env.template` and `.env.dist` are the conventional names for a COMMITTED file of placeholder values — they ship in every repository that documents its environment. On this host they held six deleted projects of 0.84 GiB each: 5 GiB of build trees kept alive by a template file that contains no secret at all. A real `.env`, `.env.local` or `.env.production` is still matched.
- **A credential word that merely LEADS an ordinary compound name is not a credential file.** `secret` and `token` are credential words, but they also lead ordinary names — the cargo artefacts `secret_scan-<hash>` and `token_bucket.rs`. Admitting them as leading words kept a 5.0 GiB build tree alive on the strength of a test name. The set of words that qualify when they lead a name is therefore smaller than the set that qualifies as a whole stem: `credentials`, `password`, `client_secret`, `id_rsa` and their kin still qualify; `secret` and `token` only qualify as the WHOLE name.

**Trade-off, stated plainly:** a trashed file named `my-api-key.txt` or `token_backup.txt` is no longer protected by name alone. The files the guard still protects by name are the ones that are credential files in themselves. The real key material on this host (`cli.key` and friends, 783 MiB across 26 deleted items) is still matched.

### Tests

- The heuristic's contract is pinned in both directions: `cli.key`, `.env`, `id_ed25519.key`, `client_secret.json`, `secrets.yaml` and `credentials_backup` are credential files; `chrome.css`, `trust_token.o`, `02-borrowed-credentials.md` and `wallet-token-cache` are not.
- The exact 2026-08-10 scenario is pinned as a REGRESSION: a nested `example-mint_dev_token` inside a deleted `target/` tree no longer keeps that tree alive.
- The depth-capped fixture is rebuilt at the new cap, so the fail-closed rule is still tested.

## [0.112.49] - 2026-10-24

### Fixed

- **The scratch report no longer claims the disk is not filling when it has not measured it.** The hours-until-full figure for the held-back scratch was computed from a byte-precise fill rate that returned "no rate" for two very different situations: the disk genuinely not filling, and the rate simply not being measurable yet. Both were rendered as one string — "never (the disk is not filling)" — so the guard stated as a fact something it had not measured, on the one line the operator reads to decide how urgent the disk is. The rate is now a three-state value — unmeasured, not filling, filling — and the report says which one it has. An unmeasurable rate says so plainly instead of asserting a calm disk.
- **The scratch report and the rapid-fill alarm now quote the SAME fill rate.** The two figures were measured twice, from two different sample windows: the alarm measured the rate in the pass, and the background cleanup recomputed it minutes later from a FROZEN CLONE of the history — taken before that pass's own deletions, and so always one sample stale. On this host the scratch line printed "never (the disk is not filling)" at 22:58:06 and the alarm printed "Disk growing at 51.3 GiB/h (84% used); 137.6 GiB available, estimated 2.7 hours until full" at 22:58:46 — the guard contradicting itself 41 seconds apart, on the same disk, in the same pass. The rate is now measured ONCE per pass and stored in the runtime state, so the two lines agree by construction.
- **The scratch report no longer borrows the disk alarm's wording for its own number.** The figure on the scratch line is `held_back_bytes / rate` — how many hours of writing the held-back scratch would absorb. It is NOT the time until the disk is full, which is `free_space / rate` and is what the rapid-fill alarm reports. The 0.112.47 rewrite relabelled it "hours until full" without changing the number, so the journal read "Tmp: 16.8 GiB held by the age floor ... — hours until full: 0.3" beside "estimated 2.7 hours until full": the same words on the same disk, meaning two different quantities, with the alarming one understated by a factor of nine. The line now says what it measures — "0.3 hours of writes at 51.3 GiB/h" — and the two no longer read as contradictions.

### Tests

- The fill rate's three states are pinned: too few samples, a window spanning under 60 s, a fresh growth window after a reclamation, and a shrinking disk are all "unmeasured"; a flat disk is "not filling"; a sustained growth is a rate.
- The pass stores the rate it measured, and the stored figure is the same one the alarm reports — so the two lines cannot drift apart again.
- The scratch figure must not contain "hours until full", and an unmeasurable rate must not claim the disk is not filling.

## [0.112.48] - 2026-10-24

### Fixed

- **A refused CPU-cap is retried on the next pass again.** The 0.112.47 fix remembered a refused CPU cap and retried it only after a ten-minute cooldown, to stop the per-pass retry of a refusal no retry could fix ("scope contains exempt process chrome"). But the cap layer's own refusals ask for the opposite: "incomplete CPU lease recovered; retry next pass" — a lease the guard could not tear down cleanly is REPAIRED by the next attempt, not by waiting — so the cooldown delayed the CPU cap on a memory-critical host by ten minutes, for the one refusal that a retry actually fixes. The attempt runs every pass again, as it did before 0.112.47; what stays fixed is the LOG, which is quiet while the reason is unchanged and says so when the reason changes, and the refusal is still named in the report so the operator can see that the guard TRIED and was refused.

### Tests

- A new regression test pins the log rule: the first refusal is news, the same refusal repeated is not, and a refusal whose reason changed is news again.

## [0.112.47] - 2026-10-24

### Fixed

- **The size-aware `/tmp` floor no longer deletes entries it must not.** The short floor for large entries removed an entry BEFORE the three liveness and protection gates ran — the open-file/cwd/exe scan, the live unix socket check, and the protected-path and tmp-root containment check — and `remove_tmp_entry` performs no checks of its own, so the ORDER of the calls was the entire defence. A large entry could therefore be deleted while a live process held it open, while it carried a bound unix socket (the failure mode that cost the codex daemon its control socket on 2026-10-21), or while it sat under a protected path. On this host the disk sat at 81% — above the 80% proactive threshold, so the size-aware floor was live — with **6 entries totalling about 9.7 GB** inside that bypass window at the time of the fix. The three gates are now ONE named helper (`tmp_entry_block`) that every removal path must pass, so a second path cannot forget one of them. The size-aware floor may only make the AGE floor shorter; it must never make the safety gates weaker.
- **The held-back-scratch report counts only what the age floor is really holding back.** An entry that a live process holds open, that carries a bound unix socket, or that sits under a protected path is not reclaimed by lowering the floor, so counting it told the operator "lower `tmp_min_age_hours` to free 30 GiB" when lowering it frees nothing — and the hours-until-full figure was computed from the inflated number.
- **The held-back-scratch report is no longer conditional on the size-aware floor being consulted.** The accounting sat inside `if large_floor_active`, so below the proactive threshold — and with the size-aware floor disabled — the guard said NOTHING about the scratch it was holding back. The report is produced on every sweep now, and the size-aware share is counted SEPARATELY, because `tmp_min_age_hours` and `tmp_large_min_age_hours` are different knobs and one total cannot give one piece of advice.
- **`tmp_large_min_age_hours` can no longer be a silent no-op.** The knob exists to reclaim bulk scratch SOONER, so it only applies when it is SHORTER than `tmp_min_age_hours`; a configured value that is not shorter cannot shorten anything, and it was silently ignored with no way for the operator to find out why. The configuration is classified at policy load and a misconfiguration is now reported by name ("`tmp_large_min_age_hours` (48) is not shorter than `tmp_min_age_hours` (24)"), once per distinct state rather than on every pass. The documentation that claimed the size-aware floor was "off by default" while the shipped values enabled it is corrected in the crate, in the example policy and in the comment on the default function: a knob whose real behaviour differs from its documentation is worse than no knob at all, because the operator reads the file, believes the floor is inert, and does not set it.
- **A dry run no longer overstates reclaimable space.** The protected-path refusal in the proactive rust-target path and in the node_modules path sat inside `if apply`, so a dry run skipped it entirely: every protected tree was listed as reclaimable AND its bytes were counted. This is the same defect that was fixed in the action-tier rust path on 2026-10-02 ("overstating reclaimable space ... misled the operator twice in one session"), in the two paths that were missed. Both refusals are hoisted so preview and apply go through ONE check and cannot disagree, and the refused trees are named in the report instead of being silently counted.
- **The OOM ledger no longer lies about a process it has already restored.** The release path restores the live `oom_score_adj` FIRST and only then asks for the ledger record to be dropped — and the drop was REFUSED while any pending descendant still referenced the root. So the ledger went on claiming "biased" for a process whose live value was already back at its original, and the guard was never steered to fix it: `apply_oom_bias` skipped any pid the ledger believed was already biased, so the stale record silently disabled OOM steering for a live top-RSS process, and the process was never re-biased, because the guard trusted its own bookkeeping. Two live processes carried that stale record on this host when the audit found it (measured 2026-10-24: the ledger said "biased", the live `oom_score_adj` was back at 200, and the host was at 2% available memory with PSI at 28.85). Two changes, because either alone leaves a way in: the root's record is no longer REFUSED once the bias has been released (the descendants' restoration debts are self-contained in the pending map, which carries its own root pid and original value), AND a recorded bias is re-checked against the LIVE `oom_score_adj` before the guard believes it — so a leftover record can no longer make the guard skip a live top-RSS process on every pass. The ledger's record remains an OBLIGATION that survives a restart (the release path has to be able to restore a bias a previous run applied), so the live value, not the record, is what decides.
- **A released root's children are restored too.** A child forked while its parent was biased INHERITED the bias target, and the sweep restored those children only while the parent was still a biased root. Once the parent was released, a child forked in the gap could no longer be attributed to it and stayed pinned at 250 for the rest of its life — the same class of strand as the priority one the operator already had to clean up by hand, and the reason a long-lived browser session on this host is still sitting at 250 although the guard has no record of ever having touched it. The pending restoration debts carry the root's original value, so a released root stays in the sweep's scope for as long as it owes a restoration, and its children are restored with it.
- **A defunct child no longer blocks its parent's release forever.** A zombie still has a `/proc/<pid>` directory and a matching identity, so every identity check treats it as alive — but the kernel refuses `oom_score_adj` writes for a process with no address space, so the restoration could never succeed. The pending entry was retried forever, and it kept the biased parent from ever being released. The obligation is discharged by the child's death: there is no `oom_score_adj` left to restore. A defunct child is now dropped from the pending set, and the settled state is PERSISTED — the removals were never written to disk, so dead entries came back on the next daemon start and blocked the parent again.
- **A refused CPU-cap is remembered instead of retried forever.** "scope contains exempt process chrome" cannot be resolved by retrying: it clears when that exempt process exits. The old code retried on every pass and logged the identical refusal on every pass, and nothing durable said the guard had tried and been refused. The refusal is now remembered with its reason, retried after a cooldown, logged when it is new or has CHANGED, and named in the report so the operator can see that the guard TRIED.
- **The stash TTL is the TTL.** The expiry test compared TRUNCATED whole days against the TTL, so an entry only expired once it was a whole day PAST it: `stash_ttl_days = 7` actually kept entries for eight days, on every stash. The comparison is in seconds now.
- **An absurd `node_modules_max_age_days` can no longer overflow.** It was the only non-saturating age computation in the crate: it wrapped in release builds and panicked in debug builds, turning a nonsense knob into a wrong age — and, in debug builds, a crash. Saturated, an absurd age simply means "never old enough", which is the safe direction.

### Tests

- Ten regression tests pin the behaviour above, each written so it fails against the pre-fix code: the size-aware floor must respect the open-file and protected-path gates; the held-back accounting must ignore a protected entry; the held-back report must appear with the size-aware floor OFF; the size-aware share must be counted separately from the general floor; the large-floor knob configuration must classify a non-shorter floor as a misconfiguration rather than a no-op; the shipped default must be a configuration that actually works; a dry run must not count a protected node_modules tree; an absurd `node_modules_max_age_days` must not overflow; the stash TTL must expire at the configured age; and a defunct child must leave no pending restoration behind.

## [0.112.46] - 2026-10-24

### Added

- **The invariant the whole pass rests on, pinned as a test: no heavy browser tree may be silently dropped.** Five individual fixes are five chances to forget the point, so the point itself is now asserted — every heavy browser tree the scan looks at must end up in EXACTLY ONE of the three lists (candidate, vetoed, protected), and the test says so for a fixture holding all the shapes at once: a heavy headless orphan nobody is driving, one with a client attached, a heavy busy desktop session, a light tree and a young tree. The light and young trees must appear in NONE of the lists; the heavy ones must appear in exactly one each. The test fails if the lifetime-CPU gate returns to the veto path, if the exempt list starts carving up a tree again, or if any NEW gate is added that drops a heavy tree on the floor.
- Ending a forgotten browser tree now also reclaims the PROFILE DIRECTORY that tree owned. A leaked browser is two halves: the processes and the ~850 MiB profile they leave behind in `/tmp`. The guard now ends the processes promptly, but the profile waited for the general tmp age floor (6 h on this host) while the CI leaked a browser tree every few minutes — measured on 2026-10-24 as seven orphaned profiles holding **5.9 GB** after five hours, growing at roughly 1.2 GB/hour, with `/` at 87%. Ending the processes but leaving the profile reclaims only half of the leak. The guard may reclaim it NOW because it has just PROVED the browser is dead and abandoned, which is a stronger proof of "nobody is using this profile" than an age floor can ever give — but the proof is re-checked at the moment of removal: the tree must have been fully ended (a tree with a surviving member is not proof), the profile must live under one of the guard's VALIDATED TMP ROOTS (whose allowlist is hard-coded to `/tmp`, so the operator's real `~/.config/google-chrome` is unreachable by construction, whatever `tmp_search_paths` says), no live process may name it in its command line (a browser that started on the same path after the old one died), nothing may hold a file open under it, no live unix socket may live in it, and it must pass the normal protected-path and symlink checks. Every check that can fail closed does: if the open-file scan cannot run, nothing is removed — an empty set would silently disable the protection, which is the failure mode that cost the codex daemon its control socket on 2026-10-21. The new knob `reclaim_reaped_browser_profiles` (default true) turns the behaviour off and leaves the profile for the tmp sweep to age out.
- Forgotten heavy orphans are weighed as a TREE, not as single processes, and the tree is ended as a unit. The 2026-10-23 leak was 792 orphaned browser processes holding 8.6 GB between them, with a median single-process weight of 3 MiB: under the per-process floor of 2 GiB not one of them ever qualified, so the guard logged critical memory pressure on every pass and reaped nothing. The weight that decides is now the AGGREGATE over the tree's root and every member, gated by the new `forgotten_orphan_tree_footprint_mb` (256 MiB); a tree qualifies when either its aggregate crosses that floor or its root crosses `forgotten_orphan_footprint_mb` (2048 MiB), so the per-process trigger keeps its meaning and the aggregate trigger is separately controllable (0 disables it). The floor was measured, not guessed, and measured again by the live proof that reaped one: a single orphaned headless Chromium tree on this host held 807 MiB when FRESH and settled at 293 MiB once the kernel had swapped most of it out, so a 1 GiB floor would MISS a single leaked tree entirely — the natural unit of leakage — and a 512 MiB floor would catch one only while it was young, missing the state a real leak spends most of its life in. 256 MiB catches one tree in its steady state. A browser's tree is the root, its zygote, renderers, GPU and network helpers, and the crashpad handler that detaches out of the tree via `setsid` and is then reparented to init — the last is tied back to the tree by the `--crashpad-handler-pid` its siblings carry, so it is weighed and ended with the tree instead of surviving it. A tree is ended DEEPEST FIRST and every member is signalled in one round: the old per-process path waited up to ~7s for each kill, so a 792-process tree would have held the guard's blocking thread for over an hour.
- Forgotten heavy orphan trees must be OLD ENOUGH before the guard will touch them. Measured while verifying this change: an orphaned headless Chromium root had burned only four seconds of lifetime CPU, so the CPU gate cannot tell "forgotten" from "just detached and still in use" — a browser a harness detached on purpose and still drives over CDP is orphaned, headless and idle-looking by design, and only TIME separates it from a leak. `FORGOTTEN_MIN_TREE_AGE_SECS` (300) is the floor: a leak is reaped on the first pass after it ages past five minutes, and a browser detached a moment ago is left alone. It is a constant, not a knob, because a safety floor an operator can casually tune is not a floor; the operator's lever for "do not touch these trees at all" is the footprint floor and the exempt list, both of which are knobs.
- The tree pass reports what it weighed: the tree's root PID, how many processes it held, its aggregate resident-plus-swapped footprint, the root's own footprint, its age and CPU, and why it qualifies. The table output gained rows for the trees, the trees that were ended, and the trees that were VETOED as a live desktop session.
- Forgotten heavy orphans are detected and reported. A process that is orphaned (its owner is provably dead), detached from any terminal, sleeping, idle by measurement, and heavy — resident PLUS swapped footprint at or above `forgotten_orphan_footprint_mb` (2048 MiB) — is reported on every pass with its evidence, but only when its command line shows it is a TEST job (headless, or a temporary/test profile). A browser that is neither is the operator's own session: it is VETOED, whatever its weight, and the veto is reported too. Ending one happens only under warn/critical memory pressure, through the existing pressure-orphan path, and only after every gate is re-verified live at kill time (including the starttime nonce, so a recycled PID is never signalled, and including the desktop veto). The pressure gate is the named function `forgotten::forgotten_reap_allowed_under_pressure` and it fails closed: an unrecognised pressure level is not permission to end a process, and `the_forgotten_reap_is_skipped_without_memory_pressure` pins that by name.
- A size-aware floor for the `/tmp` sweep: an entry of at least `tmp_large_entry_bytes` (1 GiB) is judged against `tmp_large_min_age_hours` (2 h) instead of the general floor, so a bulk scratch clone does not have to wait the general floor out. It is consulted only when the disk is filling, because deciding it costs a size measurement per entry.
- The sweep reports the scratch the age floor is holding back — the bytes, the number of entries, and how many hours of disk that is at the sustained fill rate — and warns when it passes `tmp_scratch_warn_gib` (20 GiB). The disk alarms are percent-used and rapid fill; neither of them saw the deletable litter waiting for its floor.
- `scripts/check-guard-knob-parity.sh` asserts that every knob this work added carries the same value in the crate's defaults, the NixOS operator module, the crate's example policy and the live policy, and that the daemon's task budget still agrees between the shipped unit and the NixOS module. It fails on a drifted source — in the module as well as in the crate.
- The NixOS operator module (the parent repository's `flake.nix`) now DECLARES these knobs in `services.dracon.system.policy`, under their TOML key names verbatim, with the crate's own defaults: a NixOS install with no policy file runs the crate's defaults, so the declared values are what such an install actually gets. `services.dracon.system.managePolicy` (default `false`) lets Nix own the policy file — the pinned example policy plus the declared knobs, rendered into `~/.dracon/utilities/system/dracon-system.toml` with the knobs inside the `[guard]` table — and the parity check asserts that switch is off by default, because the operator's policy file is the operator's (the general tmp floor is 6 h on this host against the example's 24 h). `scripts/check-flake.sh` renders that policy during the check and asserts every declared knob appears exactly once, with the declared value, inside the `[guard]` table — a duplicate key is invalid TOML and the guard refuses to start on a malformed policy.
- `scripts/prune-nix-system-profile.sh` is the apply-ready privileged path for the root-owned Nix system profile, which a user-session guard can never prune.

### Changed

- The credential guard now says what keeping something COSTS. It reported "keeping 55 flagged entries" and nothing else — and a COUNT looks like a rounding error when it is not: on 2026-10-24 those 55 entries held **11 GiB**, and they grew every time the CI trashed a repository whose test files are named `…-token-…test.ts` (the operator's own manuscript was in the set too, kept because a chapter is called "borrowed credentials"). The purge now reports the total the kept entries hold and names the biggest three, alongside the existing counts. This is REPORTING ONLY — no behaviour changed — and it is the same rule the rest of the guard follows: a guard that keeps something must never hide the cost of keeping it. The heuristic itself is deliberately unchanged: it is a safety mechanism, and narrowing it is a decision to be made consciously, not by accident in a refactor. A test now pins the known false positives, so a future narrowing has to be a deliberate edit.
- The hold-then-delete staging area is now called **stash**, not **quarantine**. The old name described something the feature never did: "quarantine" means keep a thing in place, isolated, to stop it spreading — and the staging area does the opposite. It MOVES the directory to the second disk (a `rename()` where the filesystems allow it, a copy-then-delete where they do not), so the original is gone from `/` as soon as the copy lands, and the entry is held on the other disk only until its TTL expires. Nothing is ever isolated in place. `stash` says what it does: put it aside, restorable until it expires. The rename is complete and there is no alias for the old name: the CLI command (`dracon-system stash move|list|restore|expire|purge`), the module, the config keys (`stash_dir`, `stash_ttl_days`, `stash_expire_interval_secs`, `stash_max_bytes_per_pass_mb`, `clean_stash_first`), the manifest file (`.stash.json`), the unit's writable path, the example policy, the NixOS module, and the docs. A policy file still carrying the old keys is reported by the daemon's existing unknown-key warning, so the change is visible rather than silent.
- The guard no longer lowers the priority of a process whose priority it could not restore. This follows the guard's own long-standing rule — "do not apply a reversible limiter unless the service can complete both halves of that lifecycle" — which the namespace-blind capability probe had been letting it break: the mitigation was a ONE-WAY ratchet that left long-lived processes stuck at a degraded `nice` for the rest of their life. On this host that means the memory-pressure renice no longer applies to the heavy processes outside the guard's own user namespace, and the guard reports each one it skips and why. The other two memory-pressure levers are unchanged and both remain reversible without any capability: OOM biasing (which only steers the kernel's last-resort choice) and CPU quota capping.

### Fixed

- The candidate list and the veto list now answer "what is heavy?" the SAME way. The veto report gated the tree's AGGREGATE footprint with `forgotten_orphan_footprint_mb` — a knob documented everywhere else as **the ROOT's own weight floor** — so a knob was being used for something its name does not say, and the two lists answered the question differently: a 500 MiB desktop tree was invisible while a 500 MiB test tree was a candidate. "If this had been mine to reclaim, would I have reclaimed it?" has to have the same answer whichever answer it gives, so the veto list now uses the same compound rule as the candidate list: the tree's aggregate against the tree floor, OR the root's own weight against the per-process floor. Reporting only; nothing is ended any differently.
- The guard now reports the operator's own heavy desktop session. The veto path skipped any tree whose root had burned more than `reap_report_max_cpu_seconds` of lifetime CPU, so a busy desktop tree was skipped before it was ever WEIGHED — and on this host that meant the operator's own Chrome (65 processes, 10.2 GiB, 8.3 hours of lifetime CPU) had never been reported by the guard, not once in the whole of the journal. It is the same lifetime-CPU-as-a-proxy error this work removed from the REAP path, applied to the REPORT path, and it failed in the same direction: the busier and heavier a tree was, the more certain the guard was to hide it, so the single largest tree on the machine was the one you could never see. The veto report is now gated on WEIGHT — the same per-process floor the "heavy trees left alone" report uses — so only genuinely heavy desktop sessions are listed, and the operator's browser is now weighed and named instead of silently skipped. Reporting only: a desktop session is still never a candidate and is never ended.
- A browser's own helpers are no longer carved out of its tree by the exempt list. The exempt list names APPLICATIONS — `chrome` is on the operator's list — and every process of a chrome tree is named `chrome`. The ROOT was already admitted despite the name exemption (for a browser the desktop veto decides instead of the name), but the exemption was still applied to the tree's DESCENDANTS while the tree was being built, and that had two effects, both wrong: the tree's AGGREGATE weight was measured as the ROOT'S alone — a 379 MiB, 9-process `chrome` tree weighed **122 MiB**, below the 256 MiB floor, so the guard could not see a leak that was plainly there, in the very pass that exists to see it — and when such a tree WAS ended, its helpers were not in the tree, so they were never signalled and survived it. The kill-time gate already had this right (a browser helper inside a dead test's tree is that job's own machinery, not one of the operator's applications); the tree builder now agrees with it, so the tree that is weighed is exactly the tree that is ended. A NON-browser tree keeps the exemption unchanged: an exempt daemon inside a dev-server tree is still the operator's, and is neither ended nor counted as reclaimable.
- The forgotten-orphan pass no longer hides the leaks it exists to catch. It decided "is anybody still using this?" with LIFETIME CPU: a tree that had burned more than `reap_report_max_cpu_seconds` (60) was skipped ENTIRELY — not reported, not weighed, not reaped, and with no message saying why. That is the wrong measurement, and it fails in the worst possible direction: the leak that MATTERED is a leak that did real work, so the more work a leaked browser did the more certain the guard was to ignore it. Every tree the guard reported on 2026-10-24 had two seconds of CPU, and the audit found one it had never reported at all: an orphaned, `--headless=new` browser tree of 20 processes holding 1523 MiB with 216 seconds of lifetime CPU, still burning half a core, whose DevTools port had no client attached. The question "is it still in use?" is now answered by asking whether a client is actually connected: a browser driven over the DevTools protocol holds an ESTABLISHED connection to its debugging port, and a browser nobody drives holds none (the port being open proves nothing — Chromium keeps listening either way). The port comes from `--remote-debugging-port=`, or from the profile's `DevToolsActivePort` file when the flag is `0` and the OS picked it. A tree that exposes no DevTools endpoint cannot be driven over one, so the lifetime-CPU proxy still decides there, unchanged and conservative.
- A heavy orphan tree that the guard deliberately leaves alone is now REPORTED instead of skipped in silence. The pass used to `continue` past a tree it had decided not to touch, so the operator's own Chrome, an in-use browser and a busy tree all looked identical to "nothing there". The report carries a `protected` section — the tree, its weight, and WHY it was left alone ("a client is still connected to it" or "busy — 900s of CPU and no debugging port to check for a client") — and the pass log says so. Silence is the one thing this pass must never do: the operator's `chrome` exemption and the browser veto mean a heavy tree can be left alone for good reasons, and there has to be a way to see that it was weighed rather than lost.
- The kill-time gate re-checks client liveness immediately before the signal, not only at scan time. A client can attach between the scan and the reap, and a tree that was undriven when it was weighed may be in use by the time the guard reaches for it. The re-verification is all-or-nothing for the root: a tree with a client attached at that moment is not ended at all.
- The `/proc/net/tcp` parser read the wrong columns. A row begins with the SLOT column (`42:`), which is not an address, so the address columns were never compared and no live client could ever be detected — the "is anybody driving it?" answer would have been a permanent "no". The columns are now located by shape rather than by position, and the port column is read as HEX. Getting that hex wrong turns a live client into an invisible one: the leaked browser's DevTools port 41549 appears in `/proc/net/tcp` as `A24D`, and an earlier fixture used `A2A5` (41637) while asserting the port was 41549 — an assertion that could only ever have been satisfied by a bug. A test now pins the conversion.
- The guard can be reconfigured again. The daemon refused the SIGHUP that was supposed to pick up a changed policy: `SIGHUP reload deferred: keeping previous policy so pending process adjustments are retried`. The guard applies a nice value to a heavy process to slow it down and restores it later, and the restore needs `CAP_SYS_NICE` — but `CAP_SYS_NICE` is NAMESPACED: it applies in the namespace that holds it and in its DESCENDANTS, never in an ancestor. The guard's unit runs in a nested user namespace (`user:[4026532749]`) while the processes it renices live in the initial one (`user:[4026531837]`), so it could always LOWER a process's priority (which needs no capability at all) and never RAISE it again. The probe that decided "can I restore this?" read only the guard's own `/proc/self/status` and said yes for processes the guard provably could not touch, so every restore failed with `Permission denied`, every restore was retried, and — because the reload is deferred while a restore is outstanding — EVERY later SIGHUP was deferred too. One un-restorable renice made the whole guard impossible to reconfigure. The probe is namespace-aware now and fails closed when either namespace cannot be read, and a restore that fails permanently (the guard is not permitted to set that process's priority at all) is dropped instead of retried forever, so it stops holding the reload hostage. The process still keeps its degraded priority until it exits; what changes is that the guard stops claiming it can undo it.
- The disk cleanup no longer starves the protection loop. It ran INLINE, ahead of the memory checks, in the same pass, and on this host one pass takes minutes — a `/tmp` with thousands of entries, a Trash the CI's `safe-delete` keeps refilling. The daemon completed exactly ONE pass in thirteen minutes, and a forgotten orphan tree that had been eligible for the whole of that time was reaped only when the cleanup finished and the next pass ran: the guard was doing housekeeping while the memory it exists to protect drained away. The cleanup now runs in its own task and the pass never waits for it, so the protection loop keeps its `interval_secs` cadence whatever the filesystem is doing; the cleanup's summary comes back through a shared slot and is reported on a later pass.
- For a BROWSER, the forgotten-orphan pass is no longer blocked by the `process_exempt_names` exemption — the desktop-session veto decides instead. The exempt list names whole APPLICATIONS, and the token `chrome` protected the operator's live session AND every headless test browser; since the headless test browsers are precisely the leak (two orphaned google-chrome trees held 2.1 GB that the guard could never see), the blunt name exemption was hiding the leak it was meant to leave alone. The veto tells the two apart exactly: a live desktop session carries no `--user-data-dir`, a test job is headless or runs from a temporary profile. The operator's own Chrome is protected twice over — the veto classifies it as a live session, and every other guard path (renice, OOM bias) still consults the exempt list by name. Non-browser processes keep their name exemption unchanged.
- The executable name the exempt list is matched against is now read from the `/proc/<pid>/exe` symlink, with the command line as the fallback. Chromium REWRITES its own argv — it reuses the memory after its argv for its own purposes — so `/proc/<pid>/cmdline` comes back for a Chrome process as ONE NUL-free blob instead of NUL-separated fields. Reading argv[0] out of that blob yields the basename of whatever followed the last slash in it, not the executable's name, which silently un-exempted the operator's own Chrome: the exact protection the exempt list exists to provide. The exe symlink is authoritative and unaffected by the rewrite, and the command-line fallback now takes the first whitespace-delimited token of the blob, which IS the executable path.
- The exempt list no longer matches as a substring of the WHOLE command line in the reaper. It matched the token `chrome` against `--user-data-dir=/tmp/puppeteer_dev_chrome_profile-*`, so every Puppeteer test browser was exempt — and an exemption is checked BEFORE any other gate, so the browser family that leaked 8.6 GB on 2026-10-23 was invisible to the guard: not reported, not weighed, not reaped. The reaper now matches an exempt entry against the executable NAME, exactly as the renice path always did (audit F119 recorded the reaper's whole-command-line substring as the odd one out), so the two paths that share `process_exempt_names` protect the same processes. A token still covers a name that extends it with a `-` (`wezterm` covers `wezterm-gui`), and deliberately not with a `_`, so `chrome` protects the operator's own browser while a `chrome_crashpad_handler` stays visible to the forgotten pass.
- The forgotten-orphan pressure gate is now the named function `forgotten::forgotten_reap_allowed_under_pressure` that the design doc and this changelog already described. The function did not exist: the pass compared the pressure level inline, so the property the documentation promised — "it fails closed, and a test pins the closed side by name" — was enforced nowhere. It fails closed, and `the_forgotten_reap_is_skipped_without_memory_pressure` now pins it.
- External cleanup commands (`nix-collect-garbage`, `docker prune`) are bounded. They ran INLINE in the guard's loop, so a GC that runs for minutes blocked the loop entirely — and the memory monitoring that would notice a swap spike never ran in the meantime. The child is killed when its bound expires.
- The memory-pressure ranking uses resident PLUS swapped footprint, and the report carries both. RSS alone misses the largest processes once the kernel has evicted their pages: a process can be down to a few MiB resident while holding tens of GiB in swap.
- The root-owned Nix system profile is only pruned when it has generations to prune, so the expected "needs root" failure is no longer a line on every cycle of the guard's life; when the prune IS needed it names the apply-ready privileged script.
- Entries the guard cannot remove (they belong to another user) are aggregated into one counted line per cycle instead of one failure line per entry per cycle, which buried the lines that matter.
- `doctor` and `status` ask about the daemon's real units. They checked `dracon-sync.service` and `dracon-sync-watchdog.timer` — the fleet moved the daemon to the operator units and masked the old ones, so two REQUIRED checks could never pass and their remediation told the operator to enable a masked unit.
- The "legacy config absent" check asks whether a legacy DAEMON CONFIG FILE is present, not whether the shared `~/.config/dracon` directory exists. That directory holds other tools' state and status tokens; failing a REQUIRED check because they exist, and telling the operator to move credentials, was worse than the stale-config risk the check was written for.
- Rapid disk-fill detection starts a fresh sample window after reclaimed space, so a large cleanup cannot mask subsequent growth. Recovery also re-arms the rapid-fill notification. Warnings now report user-available GiB and conditional hours until full.
- `recover_loop` moved above the test modules in `managed_oom.rs` (clippy's `items_after_test_module`), the CPU-quota round-trip assertion no longer folds a literal into `min` (`unnecessary_min_or_max`), and the managed-job test asserts `is_err()` instead of `!is_ok()` (`nonminimal_bool`). The last one is only reported by the 1.89 MSRV toolchain; the newer clippy no longer flags it.

## [0.112.45] - 2026-10-12

### Fixed

- Swap-in velocity is now a first-class memory-pressure signal: the guard measures swap-in pages/s on every pass (previously only as a PSI-absent fallback) and reads thrashing at or above the new `mem_swapin_warn_pages_per_sec` knob (default 1000, the legacy fallback constant). On 2026-10-04 the box thrashed at 100k+ pages/s while PSI sat at 3.9 and the verdict read "ok" with no mitigation — this signal catches exactly that shape. Unmeasured (`None`) fails closed to no-thrash; swap-out stays a non-signal (reclaim working, not pressure).
- Pressure-gated orphan reap (`reap_orphans_on_pressure`, default OFF): under warn/critical memory pressure, terminate parent-dead (reparented to init/systemd), terminal-less, allowlisted dev-server processes with NO age/CPU/state gates. The 24h auto-reap floor cannot catch a runaway whose owner just died (2026-10-04: an 8G orphaned `bun test`, 10 min old); orphanhood under pressure is the replacement proof, re-verified at kill time (same PID incarnation, parent still init, still detached, still allowlisted). Kills and skips are recorded in the memory report's `pressure_reaped` array and the `limited` action log.

- Replace-on-re-quarantine stops the treadmill that filled `/mnt/data/quarantine` with 192G in 3 days (terhub/target was quarantined 9 times): an auto move now records `auto: true` provenance in its manifest and deletes older AUTO generations of the same origin after the new copy is secured. Manual CLI moves never trigger replacement and accumulate as before; pre-flag manifests (unknown provenance), other origins, unreadable manifests, and anything outside the quarantine root are always kept. A replacement failure is loud but non-fatal — TTL expiry remains the backstop. See `docs/design/quarantine-treadmill-2026-10-04.md`.

### Added

- Opt-in guard auto-reap of abandoned dev servers (`reap_stale_dev_servers`, default OFF): with it set, reap candidates are re-verified live at kill time (sleeping, no tty, allowlist signature, exemptions, CPU ceiling, plus a starttime check against PID reuse) and then SIGTERMed, escalating to SIGKILL. Reserved PIDs and the guard itself are refused outright; zombies count as dead. Kills AND skipped re-verifies are recorded in the report's `reaped` array and echoed to stderr. The pass runs on a blocking thread so grace-period sleeps never stall a tokio worker. See `docs/design/guard-auto-reap-2026-10-04.md`.
- /tmp e2e-litter fast path: top-level tmp entries whose basename matches `tmp_e2e_patterns` (default covers `reconcile-*`, `deathrun-shield-*`, `cw-browser-test-*`, `puppeteer_dev_chrome_profile-*`, `playwright_chromiumdev_profile-*`, and other machine-generated test-copy namespaces) are judged against `tmp_e2e_min_age_hours` (default 6) instead of the general 24h gate, and the gate lifts entirely at the critical disk tier. Owns only the window the general pass skips (no double-counting); open-fd/cwd/exe, symlink, and protected-path checks are unchanged; deletion is direct, never quarantine. See `docs/design/obvious-fat-prune-2026-10-12.md`.
- Per-pass quarantine-move budget (`quarantine_max_bytes_per_pass_mb`, default 10240): caps cross-device quarantine-copy bytes per cleanup pass in the action and proactive rust-target paths, after the 2026-10-04 incident where repeated 10-30 GiB copies stalled the desktop and got `auto_cleanup_rust` disabled outright. Over-budget candidates defer with an explicit operator note in both dry-run and apply; direct deletes are uncapped. 0 = unlimited.

## [0.112.44] - 2026-10-03

### Fixed (workspace audit 2026-10-03, ROUND3)

- The tmp-cleanup freshness probe fails closed on any walk error (R3-L22): a metadata, readdir, iteration, or mtime failure now reads as fresh (keep) instead of skipping toward a delete verdict — transient I/O or an unreadable subtree defers cleanup to the next cycle, never authorizes deleting unknown content.
- Tmp cleanup refuses to run blind when `/proc` is unreadable (R3-L31): the open-file protection scan returns `None` on failure and the pass aborts loud in both apply and dry-run (dry-run would otherwise over-report reclaimable). Per-process fd misses stay skips — other users' fd tables are routinely unreadable to the unprivileged guard.
- Relocation treats unknown destination space as not-fitting (R3-L28): the old `unwrap_or(true)` treated a `df` failure as "fits", so ENOSPC mid-copy stranded a partial destination blocking retries. Not-ready plans now carry an explicit issue and refuse in `apply_relocate`.
- `doctor` checks the three M8 watchdog timers as required checks (R3-L26): missing backstops fail `--strict` instead of reporting all-green; without systemctl they are n/a like the services.
- The guard deployment checker matches symlinked roots on their canonical path and mirrors the 3b companion loop (R3-L23/R3-L24): stale or missing `dracon-system-guard-watchdog` units are detected byte-for-byte like the sync checker's, with the same case numbering where behavior is shared.

## [0.112.43] - 2026-10-02

### Fixed (workspace audit 2026-10-02)

- Refreshed the standalone lockfile from the tested parent dependency versions and verified isolated `cargo metadata --locked --offline`, so a standalone clone retains the same reproducible dependency graph.
- Interactive storage cleanup rechecks active Rust and Node/package-manager processes and recent artifact modification immediately before deletion, retaining candidates when inspection fails (audit A2, 2026-10-02). Package-cache apply refusal remains enforced.
- Quarantine refuses source directories already containing `.quarantine.json`, including dangling symlinks, and publishes new metadata atomically without replacement or link following (audit A5). Original contents stay intact on refusal; existing quarantine entries remain restorable.

### Fixed

- **The quarantine TTL was enforced by nothing (2026-10-02)** —
  `quarantine_expire` had exactly one non-test caller: the CLI `Expire` arm. No
  systemd timer, no cron entry, and no daemon pass ever called it, so
  `quarantine_ttl_days` (default 30) was a number nobody read and entries
  lived until a human ran the command. That was harmless while the reclaim path
  could not write, but the `ReadWritePaths` fix below made quarantine work, and
  the two combine badly: a quarantine move FREES `/` at copy time (the origin
  is deleted) and only frees `/mnt/data` when the entry expires. An expiry that
  never runs is not a safety copy — it is a leak, and the guard had started
  converting deletes into permanent second-disk usage.
  The guard now runs expiry itself on its own cadence, via the new
  `quarantine_expire_interval_secs` (default 86400, 0 = never expire
  automatically) and a `last_quarantine_expire` cooldown that deliberately does
  NOT share `auto_cleanup_interval_secs`, so tuning how often cleanup runs
  cannot silently change when a TTL is enforced. Three deliberate properties:
  it runs OUTSIDE the disk-pressure gate, because it drains the second disk
  rather than responding to pressure on `/` — gating it on `/` would mean the
  backlog only ever clears during a crisis; it is gated on
  `clean_quarantine_first`, so entries from a manual `quarantine move` never
  become an unattended delete while quarantine is unarmed; and every deletion
  is logged individually with entry name, origin and bytes, because once the
  entry is gone the journal is the only record it existed.
  `quarantine_expire_detailed` replaces the single walk the CLI was already
  doing twice, returning origin and size alongside the name so the log line is
  actionable and `pinned` entries are reported on every pass (an unreadable
  manifest means no TTL is computable, so such entries need a human and would
  otherwise accumulate silently) — including when `quarantine_ttl_days = 0`,
  where nothing is removed but the report is still produced. 13 new tests
  overall; every one mutation-checked, including dropping
  `check_safe_to_delete_guard` from `quarantine_move` — which is the single
  call the whole `protected_paths` mechanism rests on.

- **The action tier could quarantine the guard's own build cache (2026-10-02)** —
  `auto_cleanup_rust = true` protects ACTIVE builds (a running cargo/rustc, or a
  target written in the last 60s) and nothing else, so at the 85% action
  threshold the guard would have quarantined `dracon-utilities/target` and the
  three nested utilities' targets — 51.5 GiB of cache for the very repo that
  ships the guard, unattended. The live policy now sets
  `protected_paths = ["~/Dev/dracon-utilities"]`, which `check_safe_to_delete_guard`
  already honours with ancestor matching.
  It took TWO fixes, not one. `protected_paths` turned out to be the only
  path-valued guard knob that never expanded `~`: every sibling
  (`quarantine_dir`, `relocate_cold_root`, `rust_search_roots`, the guard log
  path) expands at point of use, but this one passed the raw string to
  `canonicalize`, which resolves `~` against the process CWD, failed NotFound,
  and was skipped SILENTLY — so the shipped `~` entry parsed, reported as
  configured, and protected nothing, while the absolute-path test stayed green.
  Verified with a read-only proof: `relocate …/dracon-utilities/target` printed a
  64.0 GiB move plan with the `~` form and refused with "refusing to delete
  protected path" once expanded. `~` is now expanded in
  `normalize_guard_policy_with_home` (the single normalization boundary, and
  idempotent), and an unresolvable
  protected entry now WARNS once per entry naming the entry instead of being
  skipped in silence. The expansion is deliberately NOT reported through
  `adjusted`: that list means "clamped to a legal range", and the warning built
  on it says so verbatim, so reporting a perfectly legal `~` form would print a
  false "clamped" line on every policy load — the same ignore-the-warnings trap
  this fix was removing. A test pins that it stays unreported; the visibility
  that matters is the per-entry warning below. It deliberately still fails open rather than refusing
  every candidate: a typo must not become a disk that fills and never reclaims.
  4 new tests, including one that drives the whole policy path — parse TOML,
  normalize, ask the real classifier — so the config can no longer be inert
  while an absolute-path test stays green. The dry-run preview disagreed for a
  second reason and is fixed with it (2026-10-02): the protected-path refusal
  lived INSIDE the `if apply` block, so `guard clean --rust` preview LISTED
  every protected tree as reclaimable and refused it only at apply time (as
  `Protected:`) — overstating reclaimable space by however large the protected
  workspace is (~66 GiB on this host), which misled the operator twice in one
  session. The check is hoisted so preview and apply go through ONE verdict and
  cannot drift, and the reason travels with the verdict.

- **The guard's auto-reclaim reclaimed zero bytes for four days, silently (2026-10-01)** —
  a second, larger consequence of the `ReadWritePaths` gap below, recorded here
  because the log never said so. When a quarantine move fails,
  `auto_cleanup_rust_targets` logs the failure and `continue`s rather than
  falling back to a direct delete — the right call for safety, but it means a
  reclaim that cannot be made reversible becomes one that does nothing at all.
  The 2026-09-30 23:41 pass reached the 85% action threshold, selected 11 real
  trees (including `dracon-platform/target` and `dracon-utilities/target`), and
  every move failed on a read-only filesystem. No `📦 Rust quarantined:` line was
  ever printed, and the single `⚠️ failed to quarantine` line competed with
  ~20 `failed to remove tmp entry … Permission denied` lines per 30s pass.
  `/mnt/data/quarantine` stayed empty for the whole period.
  `scripts/check-unit-deployment.sh` now has a sixth step that asserts the
  *running* service can write the storage roots its own policy names, by
  resolving each root against `/proc/<pid>/mountinfo` — the longest mount point
  that prefixes it, later lines shadowing earlier ones. This needs no
  `CAP_SYS_ADMIN`, so the namespace check that was previously written off as
  not performable is now a repeatable step in the release pipeline. "Cannot ask"
  stays a note, never a failure. `scripts/test_check_unit_deployment.sh` gains 8
  cases (24 total) covering the pre-fix shape, a grant on the disk but not the
  subtree, shadowed mounts, commented-out and relative knobs, a stopped service,
  and a root resolved through `/`; all were mutation-checked. That work also
  fixed two defects the suite was hiding: it was not hermetic (case 9 compared
  its synthetic fixture against the host's **real** deployed unit and reported
  drift that did not exist), and a root mount never matched as a prefix, so any
  ordinary path looked "not covered by any mount". See
  `docs/design/guard-namespace-contract-2026-10-01.md`.

- **The guard's quarantine and cold relocation were dead under the shipped unit's hardening (2026-10-01)** —
  `dracon-system-guard.service` runs with `ProtectSystem=strict` and
  `ProtectHome=read-only`, and its `ReadWritePaths=` granted only `%h` paths and
  `/tmp`. `quarantine_dir` and `relocate_cold_root` are POLICY values, so an
  operator who points them at a second drive gets a unit that silently cannot
  write there: every quarantine move failed with `Read-only file system (os
  error 30)`, one line per candidate per pass, and — because expiry deletes from
  the same directory — the entries already in quarantine could never expire
  either. Measured on this host with `quarantine_dir=/mnt/data/quarantine` and
  `relocate_cold_root=/mnt/data/cold`: `/` at 83% against an 85% action
  threshold with the reclaim path inoperable, and ~20G of quarantined build
  artifacts frozen since 28 Sept.
  Both roots are now listed (`-`-prefixed, so a host without a second drive
  still starts), which mirrors the existing decision to put the default log dir
  inside the unit's `ReadWritePaths` — `/var/log` "fails EROFS under the shipped
  hardening" (see the `log_dirs` default's rationale). Three changes keep it
  honest: `the_example_configs_storage_roots_are_inside_the_units_readwritepaths`
  fails if a storage root the example config teaches is not granted (it fails on
  the unit as it stood before this fix);
  `a_repointed_storage_root_outside_readwritepaths_is_reported` pins the
  reporting half by stripping the entry from a copy of the unit and requiring
  the root to surface; and the daemon now prints ONE startup line, on stderr so
  `guard once --json` stays parseable, naming the exact `ReadWritePaths` entry
  to add — replacing a per-candidate EROFS storm that buried the one actionable
  fact.

- **The guard's own diagnostics were blind to the guard, and several percent
  knobs had no floor (2026-10-01 audit)** —
  `dracon-system doctor` audited only `dracon-sync`'s policy and service, never
  the guard's own policy (and hardcoded a path instead of honouring
  `DRACON_SYSTEM_POLICY`), so the guard utility's diagnostic said nothing about
  whether the guard was even configured; `doctor --strict` counted
  `dracon-libs`, whose own remediation text calls it "Optional for installed
  binaries", so strict mode could never pass on a host installed from
  crates.io; a missing `systemctl` was reported as a failed service check
  rather than an unanswerable one. `unfreeze_below_percent` had no lower bound
  and its test is `used <= value`, so a legal `0` froze sync until the external
  freeze watchdog cleared the marker; `disk_early_warn_percent` was the only
  percent threshold without a 1..100 band, so `0` warned on every pass;
  `notify_cooldown_secs` had no ceiling, and at 2^63 or more the
  `Instant - Duration` in `cleanup_stale_cooldowns` panics and kills the daemon
  on its first pass (the subtraction is now `checked_sub` regardless).
- **`link apply --force-replace` was dead for every real-world link (2026-10-01
  audit)** — it called the strict `check_safe_to_delete`, whose
  `SYSTEM_PROTECTED` list contains `/home` and whose test is a descendant test,
  so every link under `$HOME` was refused (including the one the example config
  ships). The guard-specific safety variant is used now. The same function
  renamed the user's file to a backup and then symlinked with no rollback, and a
  single failing entry aborted the whole batch without producing a report.
- **The oom and CPU limiters could be applied and never released (2026-10-01
  audit)** — their release sat behind the `can_restore_nice` gate while their
  apply did not, so on a host without `CAP_SYS_NICE` (a manual `guard daemon`
  run, or a unit predating v0.112.39) a critical-pressure episode left
  `oom_score_adj=250` and a CPU cap in place with no way to undo them except
  SIGHUP or a restart. Only the renice half is gated now.
- **The shipped unit could not start on a host missing an optional directory
  (2026-10-01 audit)** — systemd fails a unit whose `ReadWritePaths` entry does
  not exist (verified: `Result=exit-code`, `ExecMainStatus=226`) unless the
  entry is prefixed with `-`, so a host that never trashed a file or never ran
  `nix-env` had no guard at all. The optional toolchain paths carry the prefix;
  the guard's own state roots stay required.
- **A freeze marker cleared by anything but the guard left the daemon frozen
  forever (2026-10-01)** — the watchdog's auto-clear (or an operator
  `dracon-sync resume`) removed the marker while the in-memory flag stayed
  true, so every later pass logged a remove failure for a file that no longer
  existed. The flag now resyncs; an operator-owned marker is still never
  touched.
- **Release tooling and fixtures (2026-10-01 audit)** — the release-notes
  generator re-emitted the broken compare link the previous pass hand-fixed in
  the shipped `.md`; nothing stopped a release closing an empty `[Unreleased]`;
  the generated install instructions produced a unit whose `ExecStart` did not
  match where `cargo install` puts the binary; `--abort`'s header read as a full
  undo though it only reverts local files; the release commit used
  `--no-verify`, bypassing the warden pre-commit hook; the version bump
  rewrote the first `^version =` line rather than the `[package]` one; and
  `release.sh` would publish a version that was not newer than the current one.
  `test_release_pipeline.sh` had been red since the standalone-repo flip
  because it asserted the pre-nested unprefixed tag names, and
  `test_release_standalone.sh` because it asserted a monorepo layout that no
  longer exists; its unique cases are folded into the pipeline fixture and the
  file is deleted. Both fixtures now print the release output on failure instead
  of exiting 1 in silence.
- **Smaller items (2026-10-01 audit)** — `events.rs` pushed every event into a
  process-global buffer nothing ever read; `check-unit-deployment.sh` treated a
  dangling unit symlink as "not deployed" (a silent false pass), printed
  unquoted paths, and `verify-install.sh` neither checked for `python3` nor
  compared the binary's version against the release under test; the freeze
  state machine, the `notify_cooldown` ceiling and the `ReadWritePaths` change
  are documented in `BLUEPRINT.md` and the example template.

### Added

- **A unit-drift check, because a shipped unit change was never actually
  deployed (2026-09-30)** — `scripts/check-unit-deployment.sh` fails when
  `~/.config/systemd/user/dracon-system-guard.service` is not byte-identical
  to the unit in this repo, when systemd is running a unit without an
  `ExecReload` that the shipped one has (file copied, `daemon-reload`
  forgotten), or when `systemd-analyze --user verify` rejects the deployed
  file. `scripts/release.sh` runs it at the end of a release and prints the
  redeploy command when it fails — advisory, because host deployment state
  must never block a release. Everything the check cannot know is a pass, not
  a false alarm: a host that never installed the unit reports "nothing to
  compare", a host with no reachable user manager (CI, a container, a bare ssh
  session) skips both systemd-facing steps, and a host with neither
  `XDG_CONFIG_HOME` nor `HOME` has no directory to compare. The unit is
  located in *both* directories systemd searches
  (`$XDG_CONFIG_HOME/systemd/user` and `~/.config/systemd/user`), because
  checking only one would let a drifted, deployed unit hide behind the other
  and report a silent false pass — the exact failure this check exists to
  prevent. Its own regression suite (14 cases) stubs `systemctl` and
  `systemd-analyze` so it passes identically with and without a user bus.
  This is the second silent case of the same class: audit F94 widened
  `ReadWritePaths` with `~/.local/share` on 2026-09-27 and `ExecReload`
  landed on 2026-09-29, and both sat in the repo while the live unit stayed
  at its 2026-09-20 copy — the deployed guard could neither truncate its
  logs nor be reloaded, and only a failing command revealed it. The live
  unit has been redeployed from the repo copy; the previous file is kept at
  `~/.config/systemd/user/dracon-system-guard.service.bak-20260930-203742`.

### Fixed

- **The intermittent `ETXTBSY` failures in the guard fixture tests are
  gone, and the cause is identified (2026-09-30)** —
  `renice_process_with_bin_reports_success_and_failure`,
  `nix_cleanup_apply_preserves_configured_profile_generations`,
  `restore_runtime_adjustments_composes_overlapping_nice_limiters` and
  `restore_runtime_adjustments_preserves_current_pid_incarnation` failed
  roughly once per 30-250 full-suite runs with
  `failed to invoke <tmpdir>/renice: Text file busy (os error 26)`.
  `restore_runtime_adjustments_restores_renice_and_oom` and
  `restore_runtime_adjustments_retains_failed_cpu_cap` were hit by the
  same cause and are fixed with them. The cause is in the *tests*, not
  the production path: `execve()` of a `#!/bin/sh` fixture that the test
  process itself created milliseconds earlier returns `ETXTBSY` while the
  kernel still considers that inode write-busy, and `libtest` runs tests
  in parallel threads inside one process, so several fixtures are written
  and exec'd concurrently by construction. Production is unaffected and
  unchanged: it only execs resolved system binaries (`renice`, `nix-env`,
  `systemctl`, `ps`), never a file it just wrote.

  The fix has two halves, because the first one alone was measured to be
  necessary but not sufficient. (1) A documented process-wide lock in
  `src/tests.rs` held across fixture creation *and* the exec removes the
  in-process collision, which is the reported root cause; holding only
  the exec half is not enough, since a probe written that way still
  failed twice in ~89 suite runs. (2) `write_test_script` now settles
  each fixture: it execs the fixture once with a marker argument that
  makes the fixture a no-op, which proves the inode has left the kernel's
  exec-busy window, and a fixture that has been exec'd once is provably
  no longer write-busy. Half (1) alone still produced 3 failures in 48
  full-suite runs while this host was at load average 87, where
  descheduling between the write and the exec can stretch the window even
  with the lock held.

  The three guards are deterministic by design — deliberately *not*
  assertions of "zero ETXTBSY", which is statistical and which host load
  alone broke 3 times in 48 runs:
  `fixture_writing_tests_hold_the_fixture_exec_guard` fails instantly and
  names the test if any fixture-writing test forgets the lock (the same
  shape as this fleet's other coverage tripwires);
  `settle_fixture_probes_without_side_effects_and_fails_loudly` checks
  that settling cannot pollute the log a test asserts on and that a
  fixture it cannot exec panics with a named reason instead of being
  swallowed; and `settled_fixture_survives_parallel_reexec` is the
  runtime smoke check. Each was verified to fail when its mechanism is
  removed.

  Scope was settled by measurement rather than assumption. The contended
  resource is the fixture inode of the test process itself, so the lock is
  process-local: 24,000 external `/bin/sh` execs against a serialized
  fixture test produced zero failures, and a private per-thread
  interpreter copy does not help either (so it is not a shared `/bin/sh`
  inode). A standalone reproducer is committed at
  `scripts/etxtbsy-repro.c` and documents every variant measured: 1 thread
  0, 4 threads 27-155, 8 threads ~25, whole window serialized 0, only the
  write serialized 5, `fsync` before close ~3033 (about 70x worse), plus
  non-fixes (reopen, directory fsync, rename, skipping chmod).

  Two long-standing misreadings are corrected by the same evidence. The
  `Is a directory (os error 21)` and `process identity unavailable`
  messages that appeared beside these failures are **not** additional
  failure modes: they are by-design fixtures in
  `restore_runtime_adjustments_restores_renice_and_oom` (a missing proc
  root, a `renice` that exits 1, and a directory standing in for an
  unwritable `oom_score_adj`), and `libtest` prints captured test stdout
  only for *failing* tests, so they surface next to whatever real
  assertion failed. External interference was also ruled out: 13,523
  planted fixture directories in `/tmp` survived with zero deletions while
  the flake fired 10 times in 293 full-suite runs, and the guard daemon's
  age-based `/tmp` cleanup (`tmp_min_age_hours`, default 24) skips freshly
  written content. The earlier "`nproc`-style parallel spawn is ruled
  out" note is consistent with this: spawning a pre-existing binary
  concurrently is safe, which is exactly why it never reproduced.

## [0.112.42] - 2026-09-29

### Removed

- **The per-utility `deny.toml` is deleted (audit decision D5, 2026-09-27)** — the workspace root's `deny.toml` is the single real gate, and this file was a *shadowing* weaker duplicate of it: `multiple-versions = "warn"` here versus `"deny"` at the root, and a shorter advisory/license list. cargo-deny 0.19 resolves a config by searching parent directories, so while this file existed, `cargo deny check` run from this repo (including the `run_gate cargo deny check` in `scripts/release.sh`) silently enforced the WEAKER policy while CI enforced the real one. With it gone, the release gate and CI read the same config, so the gate is strictly stronger than before. Verified after removal: `cargo deny check` passes from the workspace root, from this repo, and from `dracon-sync` (which never had a config). The `test -f "$clone/deny.toml"` assertion in `scripts/test_release_standalone.sh` was removed with it; that script still predates the 2026-09-11 nested-repo flip (it asserts a `dracon-system/` subdirectory inside a standalone clone), which is pre-existing and deliberately not fixed here.

**Standalone-clone caveat, measured rather than assumed**: with no `deny.toml` in scope, `cargo deny check` does NOT fail for the missing file — cargo-deny 0.19.9 logs `[WARN] unable to find a config path, falling back to default config` and then applies its BUILT-IN policy, i.e. a third policy that is neither this repo's nor the workspace's. Reproduced in a bare clone of this repo: `advisories FAILED, bans ok, licenses FAILED, sources ok` (exit 5), with 478 `license is not explicitly allowed` rejections because the default allow-list rejects dual expressions such as `MIT OR Apache-2.0` that both of our configs permit. (The bare-clone path was already not a clean one before this change: with the workspace config, the same clone reports `advisories FAILED` and exits 1. The change is what policy runs there, not whether it passes.)

`scripts/release.sh` now makes that case an explicit, fatal error instead of a silent third policy: the deny gate captures cargo-deny's output and calls `die_pre` if it sees the fallback warning, with a message pointing at the parent workspace or an explicit `--config`. The same guard is applied to `dracon-sync/scripts/release.sh`, which has never shipped a config and was therefore already exposed to the fallback. The guard is verified with a stubbed `cargo` in all four cases: fallback warning + failing checks → exit 2 (`die_pre`); real config, all ok → exit 0; real config, advisories FAILED → exit 1 (`die`); fallback warning on stdout with exit 0 → still exit 2.

### Added (space tiers, Phase 1, 2026-09-26)

- **`relocate` command**: move a cold directory to another root (typically
  the second disk), verify the copy by file count + bytes, remove the
  source, and leave a symlink. Dry-run by default, `--apply` performs the
  move; prints a `[[links.entries]]` snippet to keep the link managed.
- **Per-mount guard visibility** (new `disk_extra_mounts`, comma-separated):
  `guard once` reports each extra mount's usage + state alongside the
  primary mount in human and JSON output. Cleanup and freeze decisions
  still key off the primary mount.
- **`quarantine` command** (`move`/`list`/`restore`/`expire`): hold-then-delete
  staging under `quarantine_dir` (default
  `~/.local/share/dracon/quarantine`) with a TTL (`quarantine_ttl_days`,
  default 30, 0 disables expiry). Each entry carries a manifest with its
  origin; `restore` moves it back, `expire` deletes past-TTL entries
  (dry-run unless `--apply`).
- Design record: `docs/design/space-tiers-second-disk-2026-09-26.md`.
- `relocate` refuses git-tracked directories unless `--allow-tracked` is
  passed (moving one would replace it with a symlink and break the repo).
- **Automatic cold relocation** (Phase 2): at action/critical the guard
  scans `relocate_candidate_roots` for big idle untracked dirs and moves
  them to `relocate_cold_root` before deleting anything rebuildable.
  Report-only unless `auto_relocate_apply`; tracked content is never a
  candidate; moves are re-verified, recorded in a daemon-owned state file,
  and pressure is re-read between moves (max `relocate_max_moves_per_pass`
  per pass). `guard once` lists candidates in human and JSON output.
- **Quarantine-first cleanup** (`clean_quarantine_first`, default off):
  daemon rust-target and node_modules cleanup moves candidates to
  quarantine (TTL'd, restorable) instead of deleting; reclaimed bytes are
  reported honestly (0 for same-filesystem moves). Explicit `guard clean`
  always deletes.
- **Link drift monitoring**: the daemon checks managed + auto-relocated
  links every pass and notifies once on drift (`link status` merges both
  sources).
- **`setup` command**: reports space-tier readiness (policy, mounts,
  candidate roots, cold root, quarantine); `setup --apply` creates missing
  cold/quarantine dirs. No machine path is assumed — the seed of
  multi-user onboarding.

## [0.112.41] - 2026-09-14

### Fixed (target-cleanup eagerness, 2026-09-14)

- **Action-level Rust target cleanup is age-gated** (new
  `rust_target_action_min_age_days`, default 7 days): targets touched
  within the window are never candidates at any pressure level, not just
  in the proactive tier. Previously the action tier deleted ANY target
  above `cleanup_min_size_mb` (only a 60s mtime backstop), which thrashed
  daily-driver projects — `dracon-platform/target` was deleted ~20x in
  2 days with each rebuild regrowing 4–8 GiB straight back over the
  action line. A just-rebuilt target is fresh by definition, so the gate
  doubles as a re-clean cooldown. `0` disables the gate and restores the
  old delete-anything posture.

### Fixed (audit pass 2026-09-09, F36–F48, F58–F69)

- **Persistent event storage is bounded** (F69): the JSONL event log keeps a
  10 MiB active segment plus one rotated segment, caps individual records, and
  streams bounded tails instead of loading the whole file into memory. Tail
  requests are capped at 1,000 records, and oversized legacy segments are
  discarded on the next event write.
- **Empty event JSONL results stay machine-readable** (F68): `events --json`
  emits no human-only no-match sentinel when the log is missing, empty, or
  filtered to zero records; human output retains its existing message.
- **Status reports the effective system policy path** (F67): `status` now
  honors `DRACON_SYSTEM_POLICY` in both human and `--json` output, including
  an accurate existence flag for an overridden path. Without an override it
  reports the first discovered policy path, or the canonical default when no
  policy exists.
- **Guard log paths expand `~` consistently** (F66): logging and startup
  rotation now resolve `guard_log_file` through one shared path resolver, so
  the shipped `~/.local/state/dracon/dracon-system-guard.log` configuration
  writes and rotates the intended state file. Blank paths disable both;
  relative paths are documented as process-working-directory paths, with the
  shipped user service anchored at `%h`.
- **Tmp cleanup accepts only the explicit temporary root** (F65):
  `tmp_search_paths` is canonicalized and restricted to `/tmp` (or a
  descendant) before scanning, and each candidate must remain under its
  validated root, so apply cannot recursively remove old top-level home data.
- **Package-cache cleanup is race-safe by refusing uncoordinated apply deletion**
  (F63): process inspection detects cargo/rustc, npm, pip, and go operations,
  including common script/interpreter wrappers, for dry-run protection and
  diagnostics. Apply never recursively deletes package caches because external
  package managers provide no shared lock; failed or incomplete process
  metadata also fails closed.
- **Tmp cleanup protects process working directories** (F64): `/proc/*/cwd`
  targets are included alongside open file descriptors, so an old top-level
  tmp directory containing a live process's current working directory is not
  recursively removed.
- **Auto-renice respects a process's current nice value** (F62): graduated
  targets are now floors, so a process already at nice 10 is never raised to
  priority by resetting it to a lower tier such as nice 5.
- **Guard service restart policy distinguishes disablement, config errors,
  and crashes** (F61): the shipped and generated units use
  `Restart=on-failure`; valid `enabled = false` exits 0 without restarting,
  malformed/unreadable startup policy exits 78 (`EX_CONFIG`) and is prevented
  by `RestartPreventExitStatus=2 78`, while crashes remain restartable.
- **Bare `guard clean` selects all cleanup targets** (F60): a no-flag
  invocation now previews or applies Rust, Trash, Nix, cache,
  `node_modules`, and Docker cleanup instead of succeeding as a no-op.
  Explicit target flags remain selective; `--all` additionally enables
  Docker's all-unused-images mode.
- **Host `/tmp` cleanup matches the guard service namespace** (F59):
  the shipped unit and Home Manager-generated service now set
  `PrivateTmp=false` and grant `/tmp` as a narrow `ReadWritePaths`
  exception, allowing `clean_tmp` to reclaim the host filesystem while
  retaining the rest of the sandbox. `scripts/check-flake.sh` evaluates the
  generated service and guards both settings.
- **Nix generation retention honors `nix_keep_generations`** (F58):
  cleanup now passes `+N` to `nix-env --delete-generations` and avoids
  `nix-collect-garbage -d`, so applying cleanup keeps the newest configured
  profile generations instead of deleting all older generations.
- **`--kinds` help lists real cleanup kinds** (F36): `(targets, trash,
  nix...)` → `(rust-build, node-deps, build-output, cache)`.
- **`storage --json --cleanup` no longer silently drops cleanup**
  (F37): the combination now errors instead of ignoring the flag.
- **`.git` backstop covers subpaths** (F38): `/repo/.git/objects` is
  now refused, not just paths ending in `.git`.
- **Log truncation uses the guarded deletion check** (F39) so
  `protected_paths` apply.
- **Tmp symlink pruning races safely** (F40): `file_type()` pre-filter
  instead of a TOCTOU-prone `metadata()` call.
- **No more underflow panic on huge `--older-than`** (F41): cutoff uses
  `checked_sub` with a delete-nothing fallback.
- **SIGHUP message no longer claims defaults** (F42): keeps the
  previous policy, message says so.
- **`disk_early_warn_percent` clamped to `disk_warn_percent`** (F43)
  so the early band cannot be configured empty.
- **`guard clean --all` prunes all Docker images** (F44): the flag
  combination is plumbed through.
- **`prune --json` emits pure JSON** (F45): no-flag invocations no
  longer print human text before the report.
- **zram `--algorithm` help lists all 7 accepted values** (F46);
  **events `--severity` help drops `critical`** (F47), which no emitted
  event can match.
- **Example policy documents 9 previously undocumented knobs** (F48),
  including the default-true `clean_node_modules` / `clean_tmp`.

## [0.112.40] - 2026-09-01

- **`clean_node_modules` policy knob added** (audit M3, 2026-08-21):
  node_modules cleanup was the only auto-cleanup kind with no feature
  flag — it ran whenever `auto_cleanup_apply` was on. It is now gated by
  `clean_node_modules` (default `true`, preserving behavior) alongside
  its `clean_trash` / `clean_nix_garbage` siblings; set `false` to
  disable (e.g. projects resumed after long pauses lose deps mid-session
  since node_modules mtimes are idle). Explicit
  `guard clean --node-modules` remains available regardless.

- **Report-only storage kinds can no longer be selected for deletion**
  (audit M2, 2026-08-21): `storage --cleanup --kinds git-db --apply`
  could previously reach `remove_dir_all` on a project's `.git` directory
  — project history, not a regenerable artifact — because the tracked-dir
  gate cannot protect it (git never tracks its own database). Kind
  selection now filters report-only kinds (`NON_CLEANUP_KINDS`) from both
  the CLI flag and the policy default with an explicit warning, and
  `validate_storage_cleanup_path` refuses any `.git` path as a backstop.

## [0.112.38] - 2026-08-21

- **`storage --cleanup --apply` now uses the guard's deletion rules**
  (protected-path inconsistency fix, 2026-08-21): the interactive cleanup
  validated targets with the strict classifier whose system-root ancestor
  check rejects everything under `/home`, so on a normal workstation every
  candidate was refused while the guard's own auto-cleanup path deleted the
  identical class of artifact dirs. Both paths now share
  `check_safe_to_delete_guard`: /home-under artifact dirs are deletable,
  exact system roots / user-protected paths / symlinks remain refused.

- **Active-build detection now recognizes long-lived Rust tooling**
  (detection-gap fix, 2026-08-21): `detect_active_rust_builds` classified
  only cargo/rustc/clippy-driver processes as active builds, so a
  disk-pressure cleanup could delete a target dir out from under a live
  `rust-analyzer` or `cargo-watch` session (which hold the dir but were
  invisible to the cwd-based protection). Classification moved into a pure
  `is_rust_build_process` helper with unit-test coverage of new and
  pre-existing classes plus negative cases.

- **Guard cleanup target selection is now explicit** (audit M21, 2026-08-14):
  `guard clean` with no target flags no longer silently means “all”, and
  `--all` now wins over individual target flags as documented.

- **Workspace cleanup continues after individual deletion failures** (audit
  M23, 2026-08-14): `storage --cleanup --apply` now reports failed paths and
  still attempts the remaining selected paths instead of aborting on the
  first protected or I/O error.
## [0.112.37] - 2026-08-14

### Added

- **State-aware guard reporting** (`report_state_transition`): repeated
  unchanged observations are coalesced into a per-condition state machine
  instead of firing every guard cycle. Entry, escalation, and recovery
  transitions notify immediately; unchanged non-`ok` conditions repeat at
  most every `report_repeat_secs` (default 1800 = 30 min). Structured events
  and desktop notifications share the same backoff.
- **Memory-pressure classification with hysteresis**: swap occupancy alone is
  no longer treated as pressure. An observation only counts when free memory
  is low (`mem_available_warn_percent`) and/or PSI/swap-in thrash is active,
  and a candidate state must persist for `memory_pressure_sustain_secs`
  (default 120 s) before any notification or process mitigation fires.
  `guard once --json` now reports both `observed_pressure` and the stabilized
  `pressure`. Added regression coverage for classification and persistence.
- **Heavy-process alert keying**: per-process alert state is keyed by
  `pid + /proc starttime`, so a process incarnation gets at most one
  notification on entry (and one on stuck-candidate escalation), and a
  recycled PID cannot inherit silence from a previous incarnation. Alert
  state is dropped when the incarnation is no longer heavy.
- **Action-level cleanup scan cadence** (`auto_cleanup_interval_secs`,
  default 1800): the action/critical-level Rust/Trash/Nix/node_modules
  scan is bounded even in report-only mode, so persistent disk pressure no
  longer re-walks large trees every guard cycle. The timestamp is set before
  the scan so a filesystem error cannot retry-loop.

### Changed

- **Proactive cleanup now starts at 80% disk usage** (was 50%): stale
  `target/` scans only run when the disk actually needs attention. Codified
  in the example policy with the 120-cycle hourly cadence.
- **Release pipeline hardening**: `scripts/release.sh` now runs the locked
  test/build/clippy/deny gates before mutation, verifies the packaged binary
  through an install fixture, handles already-published/committed/tagged/
  released reruns, and prints exact mirror-tag push reminders. The standalone
  repository also ships a synchronized `Cargo.lock` and `deny.toml`, with a
  clean-clone regression covering all release gates so the parent workspace
  cannot mask failures. Dry-runs now update Cargo.toml/Cargo.lock/changelog/
  release notes for inspection, and `--abort` rolls back that complete set.

### Fixed

- **Disk state-change notification on restart**: starting the guard while
  disk usage is in the ordinary `warn` band no longer fires a spurious
  "state changed to warn" notification. Only a transition from a previous
  in-process state notifies; an initial `critical` reading is still
  actionable and notifies.
- **Heavy-process notification spam**: the per-process notification now
  honors the process-incarnation state machine instead of re-notifying every
  `notify_cooldown_secs` while the same heavy process persists. Escalation to
  `stuck` is still reported once.

- **Swap fallback now retains `pswpout`**: the previous swap-counter sample
  stores both parsed `/proc/vmstat` counters instead of replacing `pswpout`
  with zero, matching the runtime-state contract and regression coverage.
- **CPUQuota offender caps now clamp to 100%**: values above one CPU are
  normalized before the critical-pressure loop, preventing an invalid
  configuration from producing a failed cap warning for every offender on
  every guard interval. Added regression coverage for oversized values.
- **OOM bias no longer strands forked descendants at 250**: critical-pressure
  biasing records the existing descendant incarnations, then sweeps newly
  forked descendants on each guard pass and restores them to the nearest
  biased parent's pre-bias `oom_score_adj`. Failed or unreadable child writes
  remain pending so the root adjustment is retained and retried. The sweep is
  ancestry-, identity-, exemption-, and kernel-aware, with nested-root and
  retry regression coverage.
- **Process-management documentation now matches v0.112.36 behavior**: the
  guard's comments and README describe reversible renice, optional
  `oom_score_adj` biasing, and optional CPUQuota throttling instead of
  incorrectly claiming renice is the only process action.
- **Doctor status output now reports failures correctly**: failing checks are
  labeled `fail` instead of the inverted `present`, and the canonical
  `~/.dracon` system-root check is included in the table. Added regression
  coverage for the root row and status mapping.
- **SIGHUP policy reload and graceful shutdown now restore every active
  process limiter** before discarding runtime state: legacy and memory renice
  adjustments, OOM bias, and transient CPUQuota cgroups are all restored or
  retained for retry. Restorations use `/proc` starttime identities rather
  than cmdline argv0, treat unavailable proc/cgroup trees as indeterminate,
  avoid stopping transient services until cgroup state is readable, and report
  unverified systemd or cgroup operations instead of dropping the tracking
  entry.
- **Memory-pressure renice now preserves the original nice value**: runtime
  state stores both the pre-pressure and applied nice values, and release
  restores the captured pre-pressure value instead of hardcoding nice 0.
- **Overlapping limiter cleanup now restores the pre-limiter nice value once**:
  legacy and memory renice state compose for the same process incarnation,
  stale PID-reuse entries cannot discard a current entry, failed restoration
  remains retryable, and `guard once` restores process limiters on success,
  JSON output, and error paths instead of dropping its local runtime state.
- **Trash credential scanning now applies to dry-run estimates**: a
  credential-like filename blocks both deletion and the reported reclaim
  estimate, with the same warning/event path used by apply mode.

## [0.112.36] - 2026-08-10

### Added

- **Memory-pressure limiter** (`auto_renice_on_memory`, default `true`): when
  memory pressure is warn/critical, the top RSS offenders are **reniced**
  (graduated: 4 GiB → nice 5, 8 GiB → nice 10) so interactive apps win CPU
  back during a choke. Reversible: restored to nice 0 after `release_after_secs`
  of recovered pressure. Fixes the "system unresponsive" symptom without
  killing anything. Whitelist via `process_exempt_names`.
- **OOM-killer bias** (`bias_oom_on_pressure`, default `true`): during CRITICAL
  pressure, top offenders get `oom_score_adj` raised to 250 so the kernel's
  last-resort OOM kill picks THEM instead of an innocent process. Writing
  `oom_score_adj` never triggers a kill — it only steers the victim choice IF
  the kernel kills anyway. Restored on recovery. Deliberately protected
  processes (adj ≤ -500, e.g. -1000 unkillable) are never touched.
- **Optional CPUQuota offender caps** (`cap_offenders_cpu_percent`, default
  `0` = off): during CRITICAL pressure, top offenders are moved into a
  transient user systemd unit with `CPUQuota=N%` — hard-throttles a stuck
  busy-loop that nice 19 still lets burn a core. CPU throttling never kills;
  the process is moved back and the unit stopped on recovery. Off by default
  because it needs a user systemd manager and moves processes between
  cgroups; verified live (100% → ~51% at `CPUQuota=50%`).

### Security

- Memory limiter and OOM bias both skip kernel threads and
  `process_exempt_names` entries; OOM bias additionally skips processes with
  `oom_score_adj <= -500` (deliberate unkillable/protected).

### Fixed

- None (behavioral additions only).

## [0.112.35] - 2026-08-10

### Added (2026-08-10, v0.112.35)

- **Memory/swap pressure guard** (`monitor_memory`): reads
  `/proc/meminfo` + PSI (`/proc/pressure/memory`) every guard pass;
  warns when free memory is low, swap usage is high, or the system is
  swap-thrashing (PSI `full avg10`). Notifications include the top-5
  RSS offenders so the operator knows what to kill. Never kills
  anything itself. Knobs: `mem_available_warn_percent` (default 10),
  `swap_used_warn_percent` (default 50), `mem_psi_full_warn`
  (default 10.0). Falls back to a pswpin-rate check when PSI is
  unavailable. This is the failure mode from the 2026-08-09/10
  incidents (RAM exhausted, kswapd thrashing at 86% CPU, 19 GiB swap
  used) that previously had NO guard at all.
- **Sustained-heavy "stuck candidate" escalation**
  (`process_stuck_after_secs`, default 600): a process still heavy
  after the sustain window plus this many seconds is reported as
  "POSSIBLY STUCK" (e.g. the 4 svelte-check processes at ~285% CPU
  holding 6 GiB that never finished). Notification only; no auto-kill.
- **Zombie process detail**: zombies are now
  enumerated per-pid with comm, ppid, parent command, whether the
  parent is still alive, and time since first seen in Z state; the
  report and notification include the oldest offenders instead of a
  bare count. Zombies are still not killable — this is diagnostic.
- **Rapid disk-fill alert** (`disk_rapid_fill_gbph`, default 20):
  byte-precise df history (percent deltas are too coarse on large
  disks) alerts "disk filling at X GiB/h" when the sustained fill
  rate crosses the threshold, long before the percent thresholds.
- **Trash credential guard** (`trash_credential_guard`, default
  true): before emptying the trash, a recursive scan checks for
  credential-signal filenames (chrome/credential/password/secret/
  token/*.env/*.pem/*.key/*.age/etc., per
  docs/design/disk-full-credentials-2026-08-10.md). Any match aborts
  the deletion — the 2026-08-10 scan found 665 credential-pattern
  matches in a 56 GiB trash.
- `guard once` report now includes Memory Pressure, Zombies, and
  Disk Fill Rate rows (and the same fields in `--json`).


### Fixed

- **`evaluate_link` accepts equivalent non-canonicalized targets**
  (audit LOW, 2026-08-10): `normalize_path` fell back to RAW path
  strings when `canonicalize` failed, so a link whose actual target is
  written `~/a/../b` (with the intermediate `a` missing/broken) was
  reported `link_target_mismatch` against a configured `~/b` even
  though it points at the same file. The fallback is now
  `lexical_normalize`, which collapses `.`/`..` components without
  touching the filesystem (never dropping a leading `..` and never
  climbing above the root). Tests: lexical collapse cases (incl.
  `..`-above-root preserved and `a/b/../../c` → `c`) plus an
  `evaluate_link` regression test with a real `..`-form symlink whose
  intermediate is missing — now in-sync, while a genuinely different
  target still reports mismatch.

- **`scan_broken_symlinks` comment corrected** (audit LOW, 2026-08-10):
  the note claimed `fs::metadata` "doesn't follow symlinks" — it does
  (that is `symlink_metadata`). The call itself is correct: metadata
  resolves the whole chain, so a chain (L → T → missing) fails and L
  is reported broken. The corrected comment documents why a future
  "simplification" to `symlink_metadata` must NOT happen (it would
  break chain-following detection), and a new test pins the behavior:
  a broken chain (leaf → mid → missing) reports BOTH as broken, while
  a healthy chain (→ real file) is not.

- **force_replace backups can no longer collide within the same second**
  (audit LOW, 2026-08-10): `backup_path_for` used a second-resolution
  timestamp (`as_secs()`), so two backups of the same basename in one
  directory within the same second (same link listed twice, or a daemon
  pulse plus a manual `link apply`) produced the SAME backup path and
  `fs::rename` silently overwrote the earlier backup. The timestamp is
  now nanosecond-resolution and a new `unique_backup_path` helper bumps
  a `-1`, `-2`, … suffix until the name is free (`symlink_metadata`,
  so leftover broken symlinks count as occupied too). Tests:
  suffix-bump helper (incl. broken-symlink occupancy), never-reuse of
  an occupied backup name, and a force_replace behavioral test that
  pre-places a file at the exact second-resolution name and asserts both
  backups survive.

## [0.112.34] — 2026-07-26 — full-audit remediation batch 4 (2 HIGH fixes)

From `AUDIT_FULL_2026-07-26.md`:

- **SYS-H1 — guard daemon busy-looped forever after the first
  interval**: `elapsed` was declared once before the outer daemon
  loop, so after the first full interval the inner 1-second sleep
  loop never ran again — `run_guard_once` executed back-to-back
  continuously (df/ps/du + walkdir scans every pass). `elapsed` is
  now reset inside the outer loop, every pass.
- **SYS-H2 — `link apply` could never fix a drifted symlink**:
  existing symlinks were routed through `check_safe_to_delete`,
  which ALWAYS refuses symlinks — so `apply` errored on every
  existing symlink, including the drifted ones it exists to repair
  (and even in-sync ones, since there was no short-circuit). Now
  in-sync entries are skipped and drifted symlinks are unlinked
  directly (unlinking a symlink never touches its target) before
  re-creation. Regression tests added (`links_tests.rs`).

## [0.112.12] - 2026-06-21

### Changed
- **Standalone repo**: `dracon-system` is now a first-class standalone git
  repository at
  [`DraconDev/dracon-system-disk-process-guard-doctor`](https://github.com/DraconDev/dracon-system-disk-process-guard-doctor).
  Previously this code lived in
  [`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
  as a workspace member. Source-of-truth has moved to the standalone repo;
  future releases are cut from there via `scripts/release.sh`.
- **`scripts/release.sh`**: new per-repo release script. Same interface as
  the parent monorepo's `release.sh` (`<version> --yes [--dry-run] [--abort]`),
  scoped to the standalone repo's Cargo.toml, CHANGELOG, crates.io publish,
  and GitHub release. Each utility now releases independently on its own
  cadence.
- **Push-protected remotes**: the verbose repo name
  (`dracon-system-disk-process-guard-doctor`) is the public-facing
  identity. Local directory is `dracon-system/` for ergonomics. The
  4-keyword description in the repo metadata ("disk, process, guard,
  doctor") is the canonical public description.

### Verified
- `cargo info dracon-system` confirms version 0.112.12 on crates.io
- `gh release view v0.112.12` (verbose repo) shows the github release
- Daemon's `dracon-sync repos` continues to see this repo and pushes to
  the 3 remotes (github + gitlab + codeberg) on its own cycle

[Unreleased]: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/v0.112.12...HEAD
[0.112.12]: https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/releases/tag/v0.112.12
