# Code Context
## Files Retrieved
- `docs/evidence/performance-2026-11-01/raw/guard-user-incident-journal.stdout` lines 1-26, 180-201, 383, 591-600; targeted searches across the file.
- `src/main.rs` lines 1378-1447, 2290-2345, 2444-2541, 6320-6656: pressure classification, reversible mitigation, safety gates.
- `src/policy.rs` lines 163-208, 567-575, 1144-1162; `src/cpu_limit.rs` lines 198-210, 334 (targeted matches).
- `docs/evidence/performance-2026-11-01/raw/current-policy-selection.json` lines 1-22; `raw/export-observations.json` lines 69-94 (same evidence directory).
## Key Code / Findings
**Guard DID detect pressure. The earlier theory that it did not detect pressure contradicts direct historical journal evidence.**
All timestamps below are 2026-11-01 UTC; J = `docs/evidence/performance-2026-11-01/raw/guard-user-incident-journal.stdout`.
- J:190, 13:55:31.554065: `Error: memory/pressure - critical: observed=critical, mem available 4%, swap used 56%, PSI full Some(56.31), top rss: .xdg-desktop-po=666MiB,rustc=590MiB,bun=381MiB,MainThread=349MiB,chromium=361MiB`.
- Detection/action existed earlier: J:12-13, 13:49:53.433832/.436681: `oom-bias pid=3748254 cmd=MainThread adj 200 -> 250 (critical pressure)` and the same adjustment for bun PID 3751733.
**Actual logged actions versus refused actions:**
- OOM adjustment writes were logged as successful, not merely proposed: J:184, 13:55:31.357841: `oom-bias pid=3841881 cmd=rustc adj 200 -> 250 (critical pressure)`.
- Restoration also happened: J:187, 13:55:31.542861: `oom-descendant-restore pid=3847355 parent=3808141 adj -> 0`; other descendants were restored to 200 (J:2).
- Renice was refused for namespace restoration safety: J:183, 13:55:31.340816: `renice mitigation skipped for pid=3841881 cmd=rustc — the guard cannot restore its priority in that process's user namespace, so it will not lower it`.
- CPU capping was refused: J:185, 13:55:31.358314: `cpu-cap failed for pid=3841881 cmd=rustc: scope contains exempt process pi`.
- Other CPU safety refusals: J:25, 13:50:24.127468: `CPU cap requires an isolated per-job workload scope; shared/unknown units are untouched`; J:73, 13:51:38.153424: `process is outside the supported user workload slice`.
- Browser termination protection: J:186, 13:55:31.533586: `heavy process tree vetoed: chrome pid=4119877 32 processes 3820MiB — no --user-data-dir: a normal browser session`.
- Identity-race protection: J:383, 14:04:09.256122: `OOM bias not applied pid=4067533: process changed before OOM bias`.
- Exact searches found no `mem-renice`, successful `cpu-cap pid=`, `pressure-reap`, or PID `1642075` in this captured journal. This bounds the evidence, not all possible historical activity.
**Portal PID 1642075:**
- The critical report names `.xdg-desktop-po`, but omits its PID; identifying that report entry as 1642075 is plausible, not proven by J:190 alone.
- `docs/evidence/performance-2026-11-01/raw/export-observations.json`:73-76 records 14:00:32.814Z inspection of PID 1642075, xdg-desktop-portal 1.20.4, RSS 685 MB, parent systemd PID 1224.
- Same file:85-88 records a separate historical tool result at 14:02:22.441Z: `terminating leaked xdg-desktop-portal 1642075 ... SIGTERM sent ... exited after 8s`, alongside resuming twelve stopped tmux daemons.
- That is evidence of separate intervention, NOT evidence that guard killed the portal. “Leaked” is the export's label, not an independently established leak diagnosis. No portal-specific guard refusal or OOM write is present here.
## Architecture — CURRENT source, not proven historical loaded runtime
- `src/main.rs`:6320-6362 samples available RAM, swap occupancy, PSI full and swap-in rate, then applies sustained-state hysteresis (:2314-2345).
- Classifier (:2298-2310): low RAM plus high swap OR active stalls => critical; low RAM OR active stalls => warn; swap occupancy alone => ok.
- Current defaults (`src/policy.rs`:1144-1162): available <=10%, swap >=50%, PSI >=10, swap-in >=1000 pages/s, sustain 120s. Monitoring, memory-renice and OOM bias default on; CPU cap defaults off (:567-575).
- Later policy snapshot at 15:16:27 (`raw/current-policy-selection.json`:1-22) instead explicitly selects 15% available, CPU cap 50%, and exemptions including pi/chrome. It explicitly says `historical_policy_proven: false`; do not backdate it.
- Current top-five selection excludes kernel/exempt names; RSS candidates get swap read and re-ranking (:6374-6406). This is NOT exhaustive global RSS+swap ranking: a mostly swapped process can miss the initial RSS shortlist.
- Renice requires available ledger, pressure, policy opt-in, CAP_SYS_NICE and per-target namespace equality/readability (:1378-1447, :6420-6487). Namespace mismatch is a fail-closed reversibility check, not proof of absent pressure or globally absent capability.
- Current OOM apply checks identity and durable recovery intent, verifies the write, and skips already >=250 or protected <=-500 (:2444-2531). Managed-job routing now also exists (:6488-6518); this source does not establish what binary was loaded historically.
- CPU cap requires critical pressure, policy >0 and available ledger (:6519-6581); isolation/exemption checks match historical refusal strings (`src/cpu_limit.rs`:198-210,334).
- Pressure orphan reap is separately opt-in (:6585-6623); heavy-tree scan/veto is separate (:6625-6656). Lack of killing is not lack of pressure detection.
- Renice changes scheduling priority; OOM steering changes potential kernel victim selection. Neither directly frees memory, and an OOM adjustment does not itself kill a process (`src/policy.rs`:185-206).
## Start Here / Limitations
Start at J:183-190: one timestamp cluster proves detection, successful OOM steering, and simultaneous renice/CPU/browser safety refusals.
Captured journal spans at least 13:48:40–14:09:48 for guard PID 2798562; no loaded binary hash/version or historical policy provenance was established. Do not equate current source/defaults, later unit properties, or later policy with that runtime.
No causal efficacy, memory reclaimed by guard, portal skip reason, or kernel OOM kill attributable to guard is proven. Read-only inspection only; no commands, workloads, services, signals, configuration changes, or child agents were run.
```acceptance-report
{"criteriaSatisfied":[{"id":"criterion-1","status":"satisfied","evidence":"Timestamped journal distinguishes detection, OOM writes/restoration, and safety refusals; current source separated from historical runtime."}],"changedFiles":[],"testsAddedOrUpdated":[],"commandsRun":[],"validationOutput":["Exact journal search found no PID 1642075, mem-renice success, CPU-cap success, or pressure-reap marker."],"residualRisks":["Historical loaded binary and policy unproven; captured logs cannot prove unlogged actions or portal-specific skip reason."],"diffSummary":"Research artifact only; no repository edits.","reviewFindings":["Earlier no-pressure-detection theory contradicted by critical-pressure journal report and successful OOM bias logs."],"manualNotes":"Only bounded file reads/find/grep and requested artifact write; no runtime intervention."}
```
