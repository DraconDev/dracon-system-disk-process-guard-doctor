# OOM descendant provenance: architecture recommendation

Research only: no builds, tests, workload launches, service operations or repository edits. The reported 502+4+12 tests/clippy are inherited evidence, not rerun. Finding 5 remains an architecture blocker.

## Recommended architecture

Use **launch-time admitted, isolated job cgroups and a durable job-lifetime OOM restoration obligation**, not sampled ancestry. Preserve `bias_oom_on_pressure` and the main guard's sandbox/CAP_SYS_NICE-only boundary. Introduce an OOM job-lease module separate from CPU leases.

Two distinct admission contracts are possible:

- **Cooperative user-manager jobs:** a registered launcher creates a fresh boundary before workload execution, records its generation/baseline, and participating tools agree not to move tasks across job boundaries or independently change OOM policy. No privileged broker is intrinsically needed. Guarantees are conditional on that explicitly supported-workload contract.
- **Enforced containment:** for a guarantee against arbitrary same-user migration, use a narrowly authorized system-manager/broker-created, nondelegated, root-controlled job boundary outside the user's delegated subtree. Workloads still run as the user. This requires separately authorized launch integration, not more privileges in the guard.

Recommend cooperative admission if that coverage contract is acceptable; require enforced containment for a hard no-escape claim. `Delegate=no`, a scope name and a sampled member list do not establish either launch provenance or enforced containment. Existing running arbitrary jobs cannot be made provably admitted by moving a sampled root after it may already have forked.

**Coverage versus completion:** stable membership solves durable descendant coverage across fork/reparent/root exit/restart/fork-during-restore. It does not make multi-process restoration atomic or bounded-time under arbitrary endless forks. Retain the job debt until verified terminal emptiness. A stronger finite-time guarantee would require synchronous kernel enforcement or explicitly authorized quiescence; neither is supplied here. Do not label repeated enumeration a guarantee against adversarial infinite process creation.

## Precise invariants / implementation

1. **Admission before execution.** Create an empty boundary; durably register `{boot_id, job_uuid, manager identity, InvocationID, cgroup identity/path, baseline B, admission contract}`; launch into it before the first workload instruction. Manager/launcher handshake must prevent exec/fork before placement. No unrelated joins or escape while debt is possible. Initially reject nested/delegated cgroups; a snapshot cannot prohibit their later creation.
2. **Known baseline by construction.** Initially admit jobs with one baseline `-500 < B < 250`, established before execution. Protected/higher scores, heterogeneous existing jobs, overlapping leases and independent OOM writers are not silently normalized. Per-lineage heterogeneous baselines need a substantially stronger provenance design; sampling cannot recover an unknown child's original value.
3. **Explicit policy ownership.** Restore only guard-owned 250 to B, preserving values changed away from 250. Another writer choosing exactly 250 is indistinguishable: all writers must participate in a common protocol or be outside the guarantee. Exec into an exempt role also needs a launch contract routing exempt roles into separate unbiased jobs, or that job is ineligible; checking `comm` once cannot guarantee future exemptions. Define shared-mm/thread behavior explicitly.
4. **Write-ahead job debt before bias.** Persist `intent -> active -> draining -> retired` with generation/baseline, singleton ownership and fsync/rename durability. Process receipts supplement the job debt; they do not replace it. Initially allow one OOM lease per job. A clean pass does not retire it.
5. **Preserve process-targeted steering.** Continue biasing eligible offender processes rather than silently biasing every existing scope member. A uniform job baseline and lifetime debt cover their inherited children. Job-wide application is a separate policy decision. `OOMScoreAdjust=` configures executed processes; it is not a mutable cgroup-wide runtime score. CPUQuota, OOMPolicy and memory.oom.group are not substitutes.
6. **Restore by stable membership, not ancestry.** In draining state repeatedly enumerate all supported job members; open each proc descriptor, validate incarnation/membership, compare/read/write/verify owned 250 to B. A child born after enumeration from a still-biased parent stays covered by the next pass, regardless of PPID or root survival. Never retire on root exit, successful root restore, exhausted retries, disappearance of remembered PIDs, or a pass with no observed 250.
7. **Terminal emptiness closes the race.** Retire only with same-generation authoritative manager/cgroup evidence that the job is terminal, `cgroup.events populated=0`, and further admission closed. Persist a tombstone before name/path reuse. A missing path alone is insufficient when a unit has been garbage-collected; retain authoritative termination evidence under the no-escape contract. Do not stop/kill/freeze/restart/migrate workloads to create emptiness.
8. **Restart-independent recovery.** A separately supervised user worker can drain cooperative jobs; an enforced broker can own draining for enforced jobs. Share durable records under exclusive ownership, with retries/backoff. Guard death/lease expiry transitions to draining without a surviving root or guard memory. Startup reconciles job debts before new bias. Recovery service lifetime must not control workload lifetime.
9. **Honest progress bound.** Per-pass budgets bound recovery work, not obligation lifetime. With intact containment, usable proc permissions and fair scheduling, surviving restorable members are retried. With finite residual process creation after drain starts, residual inherited biases can eventually be restored. An infinite fast fork/exit chain can perpetually carry 250: keep the debt visible until empty rather than declaring complete recovery. Long-lived jobs intentionally retain lifetime debts even after apparently clean restoration.

## Failure behavior and autonomy

- Permission loss, credential/dumpability transitions, manager outage, enumeration failure, corrupt records, generation ambiguity or admission-contract breach retain an **unresolved job debt**, visible diagnostics and retries. Monitoring and unrelated safe jobs continue. No workload suspension/termination or guessed global sweep of all same-user 250 processes.
- Do not mutate when durable provenance/restoration authority cannot be established. Surface a specific admission error and eligible/unsupported coverage; preserve the configured OOM opt-in. Deployment must wait for an explicit admission/coverage decision, not quietly ship a permanent no-op for expected workloads.
- No new application in a draining job until terminal retirement. Existing ancestry-only debts retain their uncertainty; a new cgroup lookup cannot retroactively prove complete historical provenance. Resolve them through explicit operator-assisted reconciliation or verified natural termination, not fabricated attribution.
- A successful root round trip does not establish future children's proc ownership, hidden kernel minimum, credentials or LSM permissions. Constrain transitions in the admission contract or authorize a fallback.

## Kernel events versus stable job provenance

Proc connector/netlink, perf traces and ring-buffer fork events can drop records. Restart gaps, overflow, missing parent births or PID reuse break lineage; a rescan cannot identify already-reparented unknown children. Pidfds identify known incarnations, not unknown births. Process groups/sessions can change; subreaper adoption changes workload semantics and is not general preexisting provenance.

Privileged eBPF could maintain kernel-resident lineage/baseline tags rather than trusting userspace event delivery. It needs race-proven seeding/attachment before bias, pinned program/link/map lifetime beyond guard restart, reuse-safe identities, fork/exit/exec semantics, bounded-map exhaustion behavior, map-loss handling and restoration coordination. Ring-buffer-only tracking is insufficient. Attaching later cannot repair an already-lost history. Tracing is not synchronous restoration: arbitrary task-structure writes are not a supported generic BPF API for setting oom_score_adj. Lossless tags still do not make userspace restoration atomic under endless forks.

Host metadata: kernel **7.1.2**, systemd **258.7**, cgroup v2 and BTF available; `unprivileged_bpf_disabled=2`, `perf_event_paranoid=2`, `ptrace_scope=1`. A kernel tracker therefore needs separately authorized loader/attachment privileges and exact-kernel qualification. Modern tracing commonly needs CAP_BPF + CAP_PERFMON; exact attach/pinning rights and any older CAP_SYS_ADMIN fallback depend on the program. Do not grant these speculatively to the guard. Stable admitted membership is the smaller implementable design.

## Exact missing authority / decision

**Cooperative mode:** authorize launcher integration, durable admission/baseline registration, no-migration/no-independent-writer contract, and an independently supervised recoverer with proc access. No direct cgroup writes or new capability in the main guard. Finding 5 alone does not justify a privileged broker.

**Enforced mode:** authorize a separate trusted launcher/broker to create/manage allowlisted root-owned system-manager job boundaries, launch the authenticated user's requested work at its normal UID before execution, and disallow cross-boundary attachment/delegation. Access only to the user's delegated app.slice is insufficient. On this host user@1000.service and app.slice cgroup.procs are user-writable; system.slice is root-owned. User scopes therefore are not enforcement boundaries against same-UID movement.

**Restoration authority:** CAP_SYS_NICE does not permit lowering below the kernel's stored OOM minimum. **CAP_SYS_RESOURCE in the applicable host authority context** is needed if that minimum would otherwise prohibit restoration. It is not universally necessary: the repair document records a successful same-user 200 -> 250 -> 200 round trip; EACCES alone is not proof of a minimum failure. Across unrestricted credential/dumpability transitions the broker also needs proc write access, potentially a narrowly confined CAP_DAC_OVERRIDE-bearing restore worker. A root process with all capabilities removed does not automatically bypass user-owned proc permissions. LSM restrictions require explicit applicable policy, not speculative CAP_SYS_PTRACE.

Prefer unprivileged apply where possible: privileged OOM writes can alter the hidden minimum too. Launch baseline/restore semantics must account for this; exact kernel behavior needs controlled verification before selecting the final broker capability set. Separate launch authority from restore authority. Expose authenticated **job lease/generation operations**, never arbitrary PID/path/value writes or unrestricted manager calls. Do not grant CAP_SYS_RESOURCE, CAP_BPF, CAP_PERFMON, CAP_SYS_ADMIN, writable cgroups or relaxed sandboxing to the main guard.

The unavoidable operator choice is **cooperative supported-workload coverage versus enforced containment**, plus whether otherwise inaccessible credential transitions are in scope. Hard containment/restoration outside ordinary same-user authority genuinely needs a separate authorized component; an agent cannot assume that approval.

## Required tests (not run)

- Fake manager/proc/cgroup interleavings: fork before/after apply; double-fork/root exit before first sample; orphan survives guard restart; fork after enumeration/before parent restore; child forks during its restore. All retain job debt and permit later restoration, never premature retirement.
- Omitted newborn despite a clean pass; endless fork chain with bounded work and honestly pending debt. Admission barriers, launch failure, joins/migration/delegation, nested cgroups, unrelated same-score tasks, heterogeneous baselines and overlapping leases. Enforced mode must actually deny unauthorized migration.
- Protected scores/external edits, equal-valued independent edits explicitly outside unsupported ownership, exec exemptions, credential/dumpability changes, shared-mm/thread score propagation.
- Crash at every persistence/apply/restore/retirement point; rootless restart; boot/PID/cgroup/unit reuse; corruption, lock contention, helper outage, manager disappearance without terminal evidence; terminal empty generation with admission closed.
- Later authorized disposable integration: real membership inheritance, root exit/live child, guard-only and recoverer-only restarts, actual sandbox, proc EACCES versus minimum EPERM, and exact broker authority. No workload stop/freeze/kill as recovery.

# Code Context

## Files Retrieved

Exact paths/ranges from this research snapshot:

1. `/home/dracon/Dev/dracon-utilities/AGENTS.md` (lines 1–944): repository ownership, autonomy, hardening/test discipline.
2. `/home/dracon/Dev/dracon-utilities/dracon-system/docs/design/protection-repair-2026-10-19.md` (lines 1–119): current repair evidence and unresolved provenance gate.
3. `/tmp/dracon-protection-safety-review.md` (lines 1–111; finding 5 at 57–63): historical lost-descendant/restore-fork finding, not assumed current for all other findings.
4. `/home/dracon/Dev/dracon-utilities/dracon-system/src/main.rs` (lines 1842–1911, 2043–2112, 5360–5549; targeted symbol search at 646–797, 1473–1540, 2372–2518): apply, sampled ancestry, process ledger and root retirement.
5. `/home/dracon/Dev/dracon-utilities/dracon-system/src/oom_restore.rs` (lines 1–75): descriptor-bound restore primitive.
6. `/home/dracon/Dev/dracon-utilities/dracon-system/src/cpu_limit.rs` (lines 180–264): recognized scope/member checks, not an OOM admission certificate.
7. `/home/dracon/Dev/dracon-utilities/dracon-system/dracon-system-guard.service` (lines 1–130): ProtectControlGroups/NoNewPrivileges/CAP_SYS_NICE-only boundary.

## Key Code / Architecture

`apply_oom_bias()` persists `(original, identity)` by root PID and snapshots existing descendants. `sweep_stranded_oom_descendants()` uses `nearest_biased_ancestor()` over sampled PPIDs. The ledger persists boot identity/root records/known children/pending restorations but no independent job-lifetime provenance. `oom_restore::restore()` safely holds a proc descriptor and verifies its write, but cannot identify unknown births. Add a job-level lease/admission module above that primitive.

## Start Here

Open `src/main.rs:1851–1906` and `2048–2108`; specify `OomJobLease` **together with launcher admission** before replacing discovery. Another ancestry pass or delaying root deletion does not supply the missing proof.

## Residual risks / validation

Inspection only; no runtime permission or timing claims. Final git inspection showed concurrent changes to `src/cpu_limit.rs` and `src/protection_probe.rs`; this research made none and the index reported no staged paths. Host reads were limited to metadata/settings/permissions/manual text. Cooperative versus enforced coverage and eventual versus bounded-time recovery remain explicit product/authority decisions. Existing unproven debts cannot be retroactively certified. The only file written by this task is this requested artifact.
