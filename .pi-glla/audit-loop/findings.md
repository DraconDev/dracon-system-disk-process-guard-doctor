# Audit findings — dracon-system (pass of 2026-09-29)
# Append-only: never delete, rewrite, or reorder existing lines.

- [x] FIX: MEDIUM: capped_pids retain prunes only Gone, not Mismatch — a PID-reused cap entry survives while memory/oom prunes drop Gone|Mismatch (src/main.rs:4559) — fixed in 6279c90
- [x] FIX: LOW: zombie_details discards starttime and keys zombies_since by pid only — recycled PID inherits prior zombie age (src/main.rs:4687-4725) — fixed in 31fee01
- [x] FIX: LOW: oom-restore + cpu-uncap Unavailable defers with no retry cap, unlike mem/legacy unrenice bounded at 3 (src/main.rs:4453-4515) — fixed in f06cb16
- [x] FIX: LOW: renice/systemctl invoked via bare PATH-relative names in live and SIGHUP-restore paths — PATH poisoning redirects a privilege op (src/main.rs:1642,2230,4507) — fixed in c52815b
- [x] DECIDED: notify_command keeps single-path; no-flags limit documented in example template (2026-09-29)
- [x] DECIDED: SIGHUP keeps the full runtime; converges within sustain window, no reload-path change (2026-09-29)
- [x] FIX: HIGH: quarantine restore path traversal — canon_root.join(name) accepts ../ and /, no containment check; can read/copy/delete outside quarantine root (src/quarantine.rs:245) — fixed in c1b0ad8
- [x] FIX: HIGH: restore deletes manifest before move; failed rename/copy leaves manifest-less orphan that future restore refuses and expire never expires (src/quarantine.rs:259) — fixed in c1b0ad8
- [x] FIX: HIGH: quarantine_move writes manifest after destructive step; manifest-write failure after remove_dir_all(origin) or rename orphans data with no manifest (src/quarantine.rs:162-178) — fixed in c1b0ad8
- [x] FIX: HIGH: is_git_tracked misses repo roots — probes parent only; relocating ~/Dev/<repo> returns false so a whole repo can be moved leaving a symlink (src/relocate.rs:114-137) — fixed in 1f9215c
- [x] FIX: MEDIUM: link apply --force-replace ignores user protected_paths via check_safe_to_delete(&link, &[]) (src/links.rs:211) — fixed in 7129c4b
- [x] FIX: MEDIUM: clean_tmp judges whole tree by top-level mtime; stale top dir with fresh nested closed files is remove_dir_all'd, open-fd guard doesn't cover it (src/main.rs:~4900-4935) — fixed in c77e855
- [x] FIX: MEDIUM: copy_tree+verify only files+bytes, filter_map hides unreadable entries; verification can pass while data dropped (src/relocate.rs:48-100) — fixed in edde070
- [x] FIX: MEDIUM: setup --apply ensure_setup_dir does blind create_dir_all + probe write on policy paths; symlinked root writes into link target, no protected/nesting validation (src/setup.rs:40-53) — fixed in 23d8a7f
- [x] FIX: MEDIUM: apply_relocate deletes source before symlink; symlink failure leaves original path broken with no rollback (src/relocate.rs:248-250) — fixed in a034ac6
- [x] FIX: LOW: quarantine_expire ? on dir.canonicalize() aborts batch mid-run; earlier deletions done, removed list lost (src/quarantine.rs:284-291) — fixed in c1b0ad8
- [x] DECIDED: quarantine expiry keeps fail-safe; corrupt/missing manifests pin garbage rather than risk deleting unknown data (2026-09-29)
- [x] FIX: LOW: zram --gen-config --memory-percent unbounded u32; nonsense % emits broken NixOS snippet (src/zram.rs) — fixed in c868eb2
- [x] FIX: HIGH: release-notes compare link is broken and mis-scoped — endpoint v0.112.41 unprefixed does not exist (only dracon-system-v0.112.41 does) and URL points at parent repo instead of standalone (release-notes-v0.112.41.md:26) — fixed in dd9065c
- [x] FIX: MED: three live reap knobs undiscoverable — reap_report_min_idle_hours, reap_report_max_cpu_seconds, reap_report_signatures appear zero times in example TOML (src/policy.rs:89-97) — fixed in 7b9551d
- [x] FIX: MED: sync_freeze_marker undocumented — absent from example TOML despite freeze_sync_at_action/unfreeze_below_percent being documented (src/policy.rs) — fixed in 7b9551d
- [x] FIX: MED: example template has no [storage]/[links] sections — StoragePolicy and LinkEntry policy invisible while README documents those commands (dracon-system.example.toml) — fixed in 7b9551d
- [x] FIX: MED: BLUEPRINT.md:20 claims target protection "default: 30 minutes" — matches neither old 60s backstop nor current 7-day action gate (BLUEPRINT.md:20) — fixed in 6a1391d
- [x] FIX: LOW: BLUEPRINT.md:22-26 claims rust search roots "~/Dev, ~/dracon" — code default is "~/Dev" only (BLUEPRINT.md:22-26) — fixed in 6a1391d
- [x] FIX: LOW: README.md:20 says "version 0.112.40 on crates.io" — Cargo.toml is 0.112.41 (README.md:20) — fixed in dd9065c
- [x] FIX: LOW: release-notes body omits the release's actual content — 7-day age gate + F36-F48 audit batch (release-notes-v0.112.41.md:1-26) — fixed in dd9065c
- [x] FIX: MED: no tests for daemon wiring of recent behavior — reap_policy_from_guard/reap_candidates/GuardReport.reap_candidates, clean_quarantine_first branches, auto-relocate daemon path have zero coverage (src/main.rs:4655,4668,6039) — fixed in 4b29a71
- [x] FIX: LOW: CHANGELOG.md:[0.112.35] names "zombie_details" like a knob — no such policy field exists, it is a main.rs function (CHANGELOG.md) — fixed in 6a1391d
- [x] DECIDED: log monitoring documents "empty log_dirs = disabled" in README and example header; no default path assumed (2026-09-29)
- [x] DECIDED: README space-tier sections marked (unreleased); no 0.112.42 cut in this pass (2026-09-29)
- [x] DECIDED: dead print.rs helpers deleted with their tests; all output uses policy.rs::human_bytes (2026-09-29)
- [x] DECIDED: dracon-code added to code-default exempt list to match the example template (2026-09-29)
- [x] DECIDED: legal ranges documented per knob in the example template; no code clamps added (2026-09-29)
- [x] FIX: MEDIUM: restore copy-fallback copies the manifest into the restored origin and never removes it — rename path cleans origin.join(MANIFEST_NAME), copy path leaves a stray manifest (src/quarantine.rs) — fixed in 845f2bb
- [x] FIX: MEDIUM: expire still batch-aborts on escaped-root entries — the skip-and-collect fix covered vanished dirs only; the parent-containment bail discards already-collected removals (src/quarantine.rs) — fixed in e36c16d
- [x] FIX: MEDIUM: resolve_bin falls back to bare PATH-relative names off-NixOS — the renice/systemctl hardening does not hold where store paths miss (src/main.rs) — fixed in b2238c3
# --- DECIDE ratification, raised per-finding via ask_user_question (2026-09-29) ---
# The 8 DECIDED lines above were committed during the pass BEFORE they were
# raised to the operator — wrong process. They are re-raised here, one question
# per finding; the operator ratified all eight AS COMMITTED. No reverts, and no
# new in-pass work was chosen. The declined B-side of each is queued for a
# later pass rather than done now.
- [x] DECIDED (ratified 2026-09-29): #1 notify_command stays single-path, no-flags limit documented — ratified as committed (b7acd49)
- [x] DECIDED (ratified 2026-09-29): #2 SIGHUP keeps the full runtime, no reload path — ratified as-is (no code change)
- [x] DECIDED (ratified 2026-09-29): #3 quarantine expiry keeps the fail-safe on an unreadable manifest — ratified as-is (no code change)
- [x] DECIDED (ratified 2026-09-29): #4 empty log_dirs = monitoring disabled, documented opt-in — ratified as committed (b7acd49)
- [x] DECIDED (ratified 2026-09-29): #5 README space-tier sections marked (unreleased) — ratified as committed (b7acd49)
- [x] DECIDED (ratified 2026-09-29): #6 dead print.rs helpers stay deleted — ratified as committed (320f29b)
- [x] DECIDED (ratified 2026-09-29): #7 dracon-code stays in the code-default exempt list — ratified as committed (c58acb4)
- [x] DECIDED (ratified 2026-09-29): #8 legal ranges documented in the example template, no code clamps — ratified as committed (7b9551d, b7acd49)

# --- Audit pass of 2026-10-01 (fresh survey: 4 parallel scouts + parent lane) ---
# Every line below was verified against the source before it was recorded. Two
# scout claims were REFUTED by experiment and are deliberately not filed:
# `Instant - Duration` does not panic at 1e12 secs (only at >= 2^63), and
# `dirs::home_dir()` does honour $HOME here (the events/guard test-isolation
# premise holds).

## Guard live mitigation loop (src/main.rs)
- [x] FIX: HIGH: oom_score_adj and CPUQuota are applied without the capability gate that gates their restore — on a host without CAP_SYS_NICE the guard biases oom to 250 and caps CPU, then `pressure == "ok" && can_restore_nice` never releases either (src/main.rs:4361,4396 apply vs 4439 gate; release at 4529 and 4592) — fixed in 24fba49
- [ ] FIX: MEDIUM: oom descendant pending map has no retry cap — one child whose identity or oom_score_adj read stays EACCES pins its root at 250 forever, grows the map unbounded, and makes every SIGHUP reload permanently Deferred (src/main.rs:1792,1804,1818; pin at 4570)
- [x] FIX: MEDIUM: notify_cooldown_secs has a floor but no ceiling and `cleanup_stale_cooldowns` does `Instant::now() - Duration::from_secs(cooldown*2)` — a value >= 2^63 panics the daemon on its first pass (verified: "overflow when subtracting duration from instant"), and a merely huge value disables cooldown pruning forever (src/policy.rs:1206; src/main.rs:5695) — fixed in 9841ddd, 91232b6
- [ ] FIX: LOW: oom_known_descendants grows one entry per descendant incarnation ever seen and is only cleared when the root leaves oom_biased_pids — a long critical episode under a forking root accumulates thousands of dead keys (src/main.rs:4681)
- [x] FIX: LOW: after the freeze marker is cleared externally, the daemon's in-memory sync_frozen never resyncs and logs a spurious "failed to remove freeze marker" on every qualifying pass (src/main.rs:4889,6158) — fixed in 9841ddd, 91232b6
- [ ] FIX: MEDIUM: applied mitigations live only in memory, so a panic or MemoryMax kill during a critical episode restarts the daemon with empty maps and strands renice/oom_score_adj on live processes with no record that they need restoring (src/main.rs:7373; unit Restart=on-failure, MemoryMax=250M)

## Policy, setup and docs
- [x] FIX: MEDIUM: unfreeze_below_percent has no floor and the unfreeze test is `used <= value`, so a legal 0 (or 1) freezes sync until the 30m freeze watchdog clears the marker (src/policy.rs:1178; src/main.rs:4890,6158) — fixed in 9841ddd, 91232b6
- [x] FIX: LOW: disk_early_warn_percent is the only percent threshold without a 1..100 band — 0 makes the guard warn on every cycle (src/policy.rs:1151 vs 1183) — fixed in 9841ddd, 91232b6
- [ ] FIX: LOW: unknown-key detection stops at depth 1, so a typo inside a `[[links.entries]]` table is accepted silently while the same typo one level up is reported (src/policy.rs:1379)
- [ ] FIX: LOW: hint_for never considers section names, so the most common config typo (`guards` for `[guard]`) gets no "did you mean" at all (src/policy.rs:1416)
- [ ] FIX: LOW: the near-miss hint crosses a semantic tier — `rust_target_min_age_days` is suggested for a mistyped action-tier key, pointing the operator at the proactive gate (src/policy.rs:1416)
- [ ] FIX: LOW: BLUEPRINT.md pins normalize_guard_policy "at line 837" (it is at src/policy.rs:1087) and claims it bounds all values while 13 sentinel-zero knobs are deliberately left alone (BLUEPRINT.md:138)
- [ ] FIX: LOW: the template header lists cap_offenders_cpu_percent as NOT CLAMPED but the code clamps it to 100 (dracon-system.example.toml:36; src/policy.rs:1217)
- [ ] FIX: LOW: the setup report swallows a policy parse error and reports built-in defaults as "no policy exists", telling the operator to configure a file that is actually broken (src/setup.rs:111)
- [ ] FIX: LOW: setup's nesting check is lexical and neither it nor the daemon absolutises relocate_cold_root, so `setup --apply` can create ./cold under the CWD while the daemon resolves ~/cold (src/setup.rs:218; src/main.rs:5996)

## Storage, links and quarantine
- [x] FIX: HIGH: `link apply --force-replace` calls the strict check_safe_to_delete, whose SYSTEM_PROTECTED list contains /home — every link under $HOME is refused, so the flag is dead for every real-world link (src/links.rs:214; src/safety.rs:7,45) — fixed in 10554b3, 5a25763, 0e91aa4
- [x] FIX: MEDIUM: force_replace renames the user's file to a backup and then creates the symlink with `?` and no rollback, so a symlink failure leaves the path gone and the data only in a backup (src/links.rs:216,224) — fixed in 5a25763, 0e91aa4
- [ ] FIX: MEDIUM: apply_relocate removes the staging copy with `?` after the symlink is in place, so a removal failure reports the move as failed, never records the relocation, and leaves a full duplicate (src/relocate.rs:353; src/main.rs:6085)
- [x] FIX: MEDIUM: apply_link_policy aborts the whole batch on the first failing entry, skipping every later entry and the report (src/links.rs:187) — fixed in 5a25763, 0e91aa4
- [ ] FIX: MEDIUM: quarantine_move's verification walk propagates with `?` and leaves a fully copied entry dir with no manifest, which restore refuses and expire pins forever (src/quarantine.rs:179)

## doctor / CLI
- [ ] FIX: MEDIUM: doctor audits only dracon-sync's policy and service — it never checks the guard's own policy or service, and hardcodes a path instead of using effective_system_policy_path() (src/doctor.rs:36; src/main.rs:6416)
- [ ] FIX: MEDIUM: doctor --strict counts canonical_libs_exists, whose own remediation text calls it "Optional for installed binaries", so strict mode can never pass on a host installed from crates.io (src/main.rs:450; live: `doctor --strict` exits 1 here for exactly that)
- [ ] FIX: LOW: a missing systemctl is reported as a failed service check with "run systemctl --user enable" advice — cannot-ask is reported as an answer (src/main.rs:6422)

## Release and deploy tooling
- [x] FIX: HIGH: the shipped unit's ReadWritePaths lists paths a host may not have, and systemd fails such a unit to start with 226/NOPERM (verified with a scratch unit); ~/.local/share/Trash, ~/.local/state/nix, ~/.cargo, ~/.cache, ~/.npm and ~/Dev are all optional (dracon-system-guard.service:68) — fixed in the shipped unit and redeployed (live unit byte-identical, systemd-analyze clean, drift guard green); the namespace change takes effect at the next service start, which is left operator-timed so a restart cannot strand in-flight mitigations
- [x] FIX: MEDIUM: the release-notes generator still emits the broken compare link the 2026-09-29 pass hand-fixed in the .md — `git describe | sed 's/^v//'` cannot strip the crate prefix in a nested repo and the base repo no longer contains the crate (scripts/release.sh:374) — fixed in 2e39cd2
- [x] FIX: MEDIUM: nothing gates that CHANGELOG [Unreleased] has content, so a release closes an empty section into a bare version header (scripts/release.sh:338) — fixed in 2e39cd2, be7afe3, 19df912
- [x] FIX: MEDIUM: the generated install instructions produce a unit that cannot start — `cargo install` lands the binary in ~/.cargo/bin while the unit's ExecStart is ~/.local/bin (scripts/release.sh:361; dracon-system-guard.service:13) — fixed in 2e39cd2
- [x] FIX: MEDIUM: events.rs ROLLING_LOG is write-only dead state — emit_event pushes up to 1000 formatted strings into a process-global buffer that nothing ever reads (src/events.rs:19,217) — fixed in 9841ddd
- [ ] FIX: MEDIUM: check-unit-deployment.sh treats a dangling unit symlink as "not deployed" because -f follows symlinks, producing the silent false pass it exists to prevent (scripts/check-unit-deployment.sh:78)
- [x] FIX: MEDIUM: both failing release fixtures discard the release output's stderr via the EXIT trap, so a real tooling regression is indistinguishable from a stale fixture (scripts/test_release_pipeline.sh:148) — fixed in 24926eb
- [x] FIX: HIGH: test_release_pipeline.sh asserts the pre-nested-repo unprefixed tag in six places while release.sh derives dracon-system-v${VERSION}, so the pipeline gate has been red since the standalone-repo flip (scripts/test_release_pipeline.sh:169 vs scripts/release.sh:117) — fixed in 24926eb, c2b70b6, 68a1e81, 4c1a080, 4dd44e1
- [x] FIX: HIGH: test_release_standalone.sh asserts a `dracon-system/` subdirectory inside its clone, which a standalone clone does not have, so it dies before running any gate (scripts/test_release_standalone.sh:35, self-admitted in its own comment) — fixed in 24926eb
- [x] FIX: MEDIUM: --abort reverts only local files, but its header reads as a full undo; a real run followed by --abort leaves tag, publish and GitHub release standing and prints "no local modifications to revert" (scripts/release.sh:206) — fixed in 2e39cd2
- [x] FIX: LOW: the release commit is created with --no-verify, bypassing the warden global pre-commit hook on the one commit that must be audited (scripts/release.sh:434) — fixed in 2e39cd2
- [x] FIX: LOW: release.sh bumps the first `^version =` line in Cargo.toml, which a future `[workspace.package]` block above `[package]` would capture (scripts/release.sh:308) — fixed in 2e39cd2, 12884ea
- [ ] FIX: LOW: check-unit-deployment.sh interpolates repo/deployed paths unquoted into the printed remediation, so a space in $HOME makes the command unusable (scripts/check-unit-deployment.sh:59)
- [ ] FIX: LOW: verify-install.sh has no python3 presence check, so a missing interpreter is reported as a JSON schema failure (scripts/verify-install.sh:33)
- [ ] FIX: LOW: verify-install.sh checks the shape of --version but never that it equals the version being released, so a stale binary passes the pre-release gate (scripts/verify-install.sh:22)
- [ ] FIX: LOW: events.rs colours a "critical" severity that the EventSeverity enum does not have (src/events.rs:490 vs 29)
- [x] DECIDED: test_release_standalone.sh is folded into test_release_pipeline.sh and deleted — the monorepo-era `dracon-system/` subdir assertion cannot pass in either layout since the parent gitignored the crate on 2026-09-11, so the suite's unique deny-gate cases move to the pipeline fixture instead (2026-10-01)
- [x] DECIDED: release.sh enforces VERSION > the current Cargo.toml version and dies with a clear message otherwise, instead of leaving monotonicity to operator discipline (2026-10-01)
- [x] DISMISSED: events.rs colours a "critical" severity that the enum lacks — NOT a defect: that match runs on a `severity` STRING read back from the persisted JSONL (src/events.rs:524), not on `EventSeverity`, so it is forward-compatible rendering, and a future version that writes "critical" must colour it red. No change made (2026-10-01).
