# Code Context

## Verdict

**Safe to build upon under the explicit cooperative contract, with two bounded durability/retry fixes below before treating bootstrap guarantees as complete.** No new admission/proof soundness blocker found. This is not completed runtime steering/recovery, and does not cover existing unregistered jobs.

## Files Retrieved

1. `/home/dracon/Dev/dracon-utilities/dracon-system/src/oom_job.rs` (lines 1–449) — complete current admission, persistence, sentinel, exit mapping and unit tests.
2. `/home/dracon/Dev/dracon-utilities/dracon-system/src/main.rs` (lines 172–184, 605–615, 1430–1453, 7694–7700, 9389–9398, 9459–9482) — CLI, identity type, exit handling and synchronous startup.
3. `/home/dracon/Dev/dracon-utilities/dracon-system/docs/design/managed-job-admission-2026-10-19.md` (lines 1–82) — current explicit contract, development evidence and unfinished integration.
4. `/tmp/dracon-managed-launcher-review.md` (complete prior review) — previously accepted cooperative single-task proof and prior issues.

## Concrete findings and minimal fixes

### 1. Medium — first-use registry directory creation is not durably linked

`src/oom_job.rs:84–92` creates `.dracon/oom-jobs`, but `save()` only fsyncs the JSON file and **oom-jobs itself** (`117–129`). That persists the record within the directory, not necessarily the newly created directory's entry in `.dracon`; if `.dracon` is also new, its entry in the account home likewise lacks a parent-directory fsync. A crash can therefore lose the newly initialized registry pathname despite admission returning success and allowing spawn (`296–313`). This falls short of the unconditional durable-admission claim, particularly for later runtime write-ahead obligations.

**Minimal fix:** durably initialize the directory chain before admission: create/validate `.dracon` and `oom-jobs`, sync each newly created directory and its parent (or unconditionally sync this short chain through the existing account home), propagating failure before spawn. Preserve the existing file-fsync → atomic publication → registry-directory-fsync sequence. Add an initialization ordering/failure-injection test; ordinary save/load tests do not establish crash durability.

### 2. Medium — terminal retry diagnostics can terminate the proof writer

`src/oom_job.rs:352–355` uses `eprintln!` on the first terminal-read/lock/persistence error. Rust's `eprintln!` panics when writing stderr fails. For example, a full filesystem can make both terminal persistence and redirected stderr fail; a closed stderr pipe is another case. Instead of retaining the sentinel and retrying, this path can unwind/exit and permanently leave unresolved provenance. It remains conservative, but contradicts the explicit retry-on-terminal-failure guarantee (`docs/design/managed-job-admission-2026-10-19.md:22–25`).

**Minimal fix:** make this diagnostic best-effort/non-panicking, e.g. discard the `Result` of `writeln!` to a stderr lock. Do not propagate a logging error out of the retry loop. Test a failing diagnostic writer with an injected terminal failure, without launching a workload.

## Key Code / checked guarantees

- `src/oom_job.rs:224–278`: admission checks one sentinel thread, scope suffix, Delegate=no, nonempty InvocationID/active scope, no child groups, ordered only-self membership/task-count proof, manager ControlGroup equality and supported baseline. Helper processes complete before admission/spawn. The fresh-boundary argument remains conditional on no independent joins/migrations.
- `src/oom_job.rs:296–321`: locked no-clobber admission precedes workload spawn; baseline rechecked; failed spawn still enters terminal certification, mapping missing executable to 127 and permission denial to 126.
- `src/oom_job.rs:323–361`: terminal errors retry indefinitely; missing/replaced scope, unknown membership and immutable mismatch never become proof. Membership precedes `pids.current`; the sentinel neither forks nor starts threads after launching the workload. The accepted cooperative single-task argument still holds.
- `src/oom_job.rs:214–219, 341–346`: immutable comparison covers every Job field except phase/proof, under the same per-generation lock used at admission. Atomic replacement and directory fsync are sound for an already durably initialized registry. A rename-success/fsync-failure retry can reload its own terminal record and retry persistence safely.
- `src/main.rs:9459–9482`: both launch and enter bypass Tokio construction. CLI parsing precedes entry, but there is no explicit application thread/runtime creation on this path; enter also checks its task count.
- `src/oom_job.rs:142–193, 311–321, 361`; `src/main.rs:7694–7700, 9477–9482`: both wrapper layers preserve shell-style numeric status. Command completion intentionally waits for descendants and durable terminal proof; stderr messages for nonzero command status do not change that status. Admission refusal remains infrastructure status 1, not the requested command's status.

## Architecture / integration constraints

Fresh user scope → synchronous sentinel validates boundary/baseline → locked durable registration → command spawn → wait root and all descendants → ordered single-task proof → locked immutable validation and durable terminal publication → propagate command status.

When adding runtime writers, keep sentinel exclusion by full incarnation effective immediately when a record becomes visible; never bias it during the admission-to-spawn interval. All writers must use the same persistent lock inode, preserve immutable admission fields, and never regress terminal state/proof. Do not unlink lock files while any writer may hold them. `save()` itself does not acquire the lock, so future callers must honor that protocol. These are upcoming integration requirements, not undisclosed defects in a nonexistent runtime integration.

Registry hardening is cooperative, not hostile-same-UID isolation: the final directory is owner-checked/private, but parent symlinks and record/lock symlinks are not comprehensively rejected. Do not advertise this as an adversarial boundary. No additional privilege or retroactive adoption is warranted.

The documentation accurately distinguishes the baseline-200 `true` success from the refused inherited-250 exit-42 attempt. The latter did not execute the command and is not live validation of status 42. Foreground descriptor retention, cancellation differences, whole-job completion and lack of true signal/core semantics are explicitly disclosed; they are not blockers for this opt-in API.

## Start Here

`src/oom_job.rs:84–92` and `352–355`: close initialization durability and fallible logging, then build runtime integration on the existing proof protocol.

## Validation / residual risks

Read-only inspection only; no builds, tests, workloads, service actions, source edits or fanout. Only this requested `/tmp` report was written. `git status --short` showed existing source/docs/evidence changes and no staged files. Crash durability, injected stderr failure, live nonzero propagation, kernel interleavings and later recovery behavior were not exercised in this review. Earlier test/live results are documented observations, not independently rerun evidence.

```acceptance-report
{
  "criteriaSatisfied": [
    {"id":"criterion-1","status":"satisfied","evidence":"Reviewed current bootstrap and startup wiring; identified two concrete bounded fixes, checked prior admission/proof issues, and recorded integration constraints and residual risks."}
  ],
  "changedFiles": ["/tmp/dracon-managed-job-bootstrap-review.md"],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {"command":"nl -ba src/oom_job.rs; nl -ba docs/design/managed-job-admission-2026-10-19.md; git status --short (selective inspection)","result":"passed","summary":"Verified exact finding line numbers and existing unstaged work; no execution of project code."}
  ],
  "validationOutput": ["Current sentinel proof and pre-Tokio dispatch inspected; no new cooperative proof soundness blocker found."],
  "residualRisks": ["Directory-chain crash durability and fallible stderr retry path require fixes/tests.","No tests, builds, workload launches or service changes performed; runtime steering/recovery remains intentionally unfinished.","Actual live exit-42 propagation is still unqualified because admission was refused."],
  "noStagedFiles": true,
  "diffSummary": "Review artifact only; no repository edits.",
  "reviewFindings": ["medium: src/oom_job.rs:84-92 — newly created registry parents not fsynced.","medium: src/oom_job.rs:352-355 — eprintln failure can panic out of terminal retry."],
  "manualNotes": "Bootstrap is suitable for continued cooperative integration; fix the two bounded durability/retry issues before claiming its guarantees complete. No privileged or unregistered-job coverage requested or inferred."
}
```
