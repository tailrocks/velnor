# Production-readiness unblock analysis

Captured 2026-08-27 from live GitHub API and read-only Sentry inspection.

## Finding: two separate conditions

### A. Obsolete zero-job queue objects

ChainArgos runs `32985134450`, `32984965998`, and `32984867843` target the
obsolete SHA `48f687259bed568409ac4a6308a2fc5f2d970b82`. All remain `queued`
with zero jobs and zero check runs; their check suites are also queued with
zero check runs. Normal cancel and force-cancel return HTTP 409 because the
workflow runs were never admitted to a cancellable job state.

GitHub's Aug 26 Actions incident report says 3.7% of larger-runner jobs were
stuck waiting for runner assignment and would be cancelled server-side. This
is a strong matching hypothesis, not proof of the causal backend component.
The GitHub community incident thread documents
the same zero-job/queued/contradictory-cancellation behavior and states that
the CLI/UI cancellation fix only works after GitHub's backend mitigation.

Classification: observed GitHub Actions workflow admission/scheduling anomaly;
external backend state is the leading hypothesis. The evidence does not
exclude Velnor lifecycle/routing contribution, but zero jobs/check-runs means
there is no Velnor step, Docker, workflow expression, or Results Service
failure to debug in those three runs.

### B. Current CI lane queue

Replacement runs on the current PR head `7489b6b07edfa75e589a2a35f108ffe3bd24e7f9`
show different behavior:

- Rust Docker run `33012335308` is executing successfully. It completed
  `Detect changes (Velnor)` and is running the Docker build.
- CI run `33012336003` admitted its validation and skipped jobs, but
  `ChainArgos / velnor lane` remains queued.
- Sentry currently has one online/busy runner,
  `velnor-sentry-slot-4-next-384305-71` (runner 14725); four older slot
  registrations are offline.
- Sentry logs show slot 4 repeatedly receives broker messages, creates JIT
  runners, completes jobs, renews the active Docker job, and prewarms the
  successor. This proves the broker/JIT path is live.
- The same Sentry baseline remains degraded: `github_reachable=false`,
  `routing_valid=false`, `runner_group_valid=false`, zero desired/actual/
  registered slots, failed doctor units, defunct runner processes, and a
  registry/process disagreement. One working slot proves only one admission
  path, not fleet health.

Classification: Velnor capacity/readmission degradation. The CI lane is
waiting for an available matching runner while slot 4 is occupied; offline
slots 1/2/3/5 need a separate lifecycle investigation. Do not label this a
workflow failure until the active Docker job releases capacity or a free slot
becomes available.

## Ranked unblock options

1. **Let the active Rust Docker job finish; monitor both replacement runs.**
   This is the safest immediate path. If the CI lane starts after slot 4 is
   released, current Velnor admission works for that path and the live queue
   was capacity pressure. Monitor Rust Docker run `33012335308` for lease,
   failure, cleanup, and capacity release; monitor CI run `33012336003` for
   transition. If CI remains queued for two minutes after a free matching
   runner is online, inspect slot lifecycle and runner-group admission next.

2. **GitHub backend cleanup for the three obsolete runs.** After GitHub-side
   remediation, retry normal cancel, then force-cancel, and verify each run and
   check suite is terminal. Provide GitHub Support the three run IDs, check
   suite IDs, obsolete SHA, HTTP 409 bodies, zero-job responses, and the Aug 26
   incident correlation. No repository-side change can safely manufacture a
   terminal result for these objects.

3. **Velnor slot readmission investigation now; mutation after drain.** Capture
   per-slot lifecycle state, registration IDs, broker renewal timestamps, and
   daemon errors for slots 1/2/3/5. Then implement the narrow structural fix
   that restores independently supervised slots and registration cleanup. Do
   not restart or drain Sentry while an accepted job is active unless the
   recovery procedure proves active-job safety.

4. **Re-run the current PR after cleanup.** Only after all prior non-completed
   runs are terminal, stale validation-owned registrations are removed, and
   runner capacity is healthy. Dispatch exactly once and monitor only its new
   run ID. A new commit, workflow rerun, or check-suite rerequest now would
   add queue state and violate the campaign gate.

## Do not use

- Do not rerequest the three obsolete check suites; it creates more queued
  work and does not clear the original objects.
- Do not delete workflow runs or check suites; the supported API does not
  authorize deletion of these active queue objects.
- Do not disable workflows, alter concurrency, weaken fixture/workflow content,
  delete active runner registrations, or cancel the current Rust Docker run.
- Do not treat the queued CI lane as a Velnor protocol defect while the only
  online matching runner is busy.

## Resume gate

Resume campaign verification only when each obsolete run and check suite is
`status=completed` with a non-null conclusion, unless GitHub Support confirms
server-side disappearance; no prior verification run remains non-completed;
the targeted stale-registration query is empty; Sentry health reports
`github_reachable=true`, `routing_valid=true`, `runner_group_valid=true`, and
positive desired/actual/registered/executor-ready capacity; and at least one
matching runner is online and idle. Then run the required cancel/runner-clean
proof, dispatch exactly once, and monitor only the new run ID.
