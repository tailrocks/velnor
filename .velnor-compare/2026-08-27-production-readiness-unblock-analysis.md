# Production-readiness unblock analysis

Captured 2026-08-27 from live GitHub API and read-only Sentry inspection.

## Finding: two separate conditions

### A. Obsolete zero-job queue objects

ChainArgos runs `32985134450`, `32984965998`, and `32984867843` target the
obsolete SHA `48f687259bed568409ac4a6308a2fc5f2d970b82`. All remain `queued`
with zero jobs and zero check runs; their check suites are also queued with
zero check runs. Normal cancel and force-cancel returned HTTP 409 responses.
That response is consistent with the objects remaining non-cancellable at the
time of capture, but does not establish the cause or a permanent state. The
deeper cause remains unproven.

Classification: observed GitHub Actions workflow admission/scheduling anomaly;
external backend state is the leading hypothesis. The evidence does not
exclude Velnor lifecycle/routing contribution, but zero jobs/check-runs means
there is no Velnor step, Docker, workflow expression, or Results Service
failure to debug in those three runs.

### B. Current CI lane queue

The exact 2026-08-27 local read-only refresh is:

- Current PR head is `7489b6b07edfa75e589a2a35f108ffe3bd24e7f9`.
- Run `33012336003` remains `queued`; check suite `89435047597`.
- Job `98321609613` remains `queued`, has labels `[velnor-trusted]`,
  `runner_group_id=0`, an empty `runner_group_name`, and `runner_id=0`.
  This is a label/group-admission mismatch or unresolved interpretation, not a
  proven request for the `velnor-trusted` runner group.
- The validation job `98321562308` succeeded.
- Runner group ID `4` is `velnor-trusted`; its policy is
  `restricted_to_workflows=false` and `allows_public_repositories=true`.
  Its selected repositories are exactly `[ChainArgos/blockchain-nodes,
  cloudflare-tofu, github-terraform, jackin-agent-brown, java-monorepo,
  velnor-actions]`.
- The group’s /runners endpoint returned total_count=0 and runners=[]. Therefore this refresh does not
  establish matching capacity, online capacity, or an idle matching runner.

Sentry runner and JIT observations from earlier read-only inspection are
time-bounded historical evidence only: slot 4 (`runner 14725`) was once
online/busy, older slot registrations were offline, and logs showed broker/JIT
activity. They are not current state. The current refresh has no active JIT
registration to report. Do not infer present runner health, daemon health, or
JIT availability from those historical observations.

Classification: runner-group admission/readiness remains unproven. Do not label
this a workflow failure or claim idle matching capacity until group membership,
JIT group assignment, runner readiness, and Velnor queue matching are captured.

### Read-only org group API capture

Exact commands used for org group `4` at capture time:

```sh
gh api orgs/ChainArgos/actions/runner-groups/4
gh api orgs/ChainArgos/actions/runner-groups/4/repositories --paginate
gh api orgs/ChainArgos/actions/runner-groups/4/runners
```

The repository response contained exactly `[ChainArgos/blockchain-nodes,
ChainArgos/cloudflare-tofu, ChainArgos/github-terraform,
ChainArgos/jackin-agent-brown, ChainArgos/java-monorepo,
ChainArgos/velnor-actions]`; the runners response reported
the group’s /runners endpoint returned total_count=0 and runners=[].

The watchdog label-only concern is source evidence requiring a focused test; it
is not proven live behavior unless separately documented by that test.

### C. Runner-group admission remains unproven

The active ChainArgos queued workflow run `33012336003`, job `98321609613`,
remains queued with labels `[velnor-trusted]`, `runner_group_id=0`, an empty
`runner_group_name`, and `runner_id=0`. This is a label/group-admission
mismatch or unresolved interpretation, not a proven request for the
`velnor-trusted` runner group. The group policy snapshot above is exact,
including group ID 4, six selected repositories, and the group’s /runners endpoint returned total_count=0 and runners=[].
The successful validation job `98321562308` proves validation success only; it
does not prove runner admission or execution.

The remaining blocker is group membership/admission/readiness. Do not add a
label or introduce a fallback. Keep run `33012336003` pending as evidence of
the unresolved admission state. The next safe step is read-only capture of
group policy and membership, JIT request/group assignment, and per-slot
registration, broker-renewal, watchdog, and daemon lifecycle state. Perform
policy repair and re-admission only after drain safety for accepted jobs is
proven.

## Ranked unblock options

1. **Perform the read-only admission/lifecycle capture.** Capture group policy
   and membership, JIT request/group assignment, and daemon/per-slot lifecycle
   state. Do not infer idle matching capacity from historical Sentry evidence or
   the successful validation request. If policy repair or re-admission is needed,
   defer it until drain safety for accepted jobs is proven.

2. **GitHub backend cleanup for the three obsolete runs.** After GitHub-side
   remediation, retry normal cancel, then force-cancel, and verify each run and
   check suite is terminal. Provide GitHub Support the three run IDs, check
   suite IDs, obsolete SHA, HTTP 409 bodies, and zero-job responses. No
   repository-side change can safely manufacture a
   terminal result for these objects.

3. **Policy repair/re-admission after drain safety.** Only after the read-only
   capture and proof that accepted jobs are safe to drain may policy be repaired
   or capacity re-admitted. Do not restart, drain, delete registrations, or
   otherwise mutate Sentry before that proof.

4. **Re-run the current PR only after the campaign gate clears.** No dispatch,
   workflow rerun, or check-suite rerequest is permitted now. Only after all
   prior non-completed runs are terminal, stale validation-owned registrations
   are removed, and healthy matching capacity is proven may verification dispatch
   exactly once and monitor only its new run ID.

## Do not use

- Do not rerequest the three obsolete check suites; it creates more queued
  work and does not clear the original objects.
- Do not delete workflow runs or check suites; the supported API does not
  authorize deletion of these active queue objects.
- Do not disable workflows, alter concurrency, weaken fixture/workflow content,
  delete active runner registrations, or cancel the current Rust Docker run.
- Do not treat the queued CI lane as a Velnor protocol defect or a workflow
  defect until group admission, readiness, and Velnor queue matching are
  investigated.
- Do not dispatch, rerun, or rerequest checks while the no-dispatch gate is in
  force.

## Resume gate

Resume campaign verification only when each obsolete run and check suite is
`status=completed` with a non-null conclusion, unless GitHub Support confirms
server-side disappearance; no prior verification run remains non-completed;
the targeted stale-registration query is empty; Sentry health reports
`github_reachable=true`, `routing_valid=true`, `runner_group_valid=true`, and
positive desired/actual/registered/executor-ready capacity; and at least one
matching runner is online and idle. Then run the required cancel/runner-clean
proof, dispatch exactly once, and monitor only the new run ID.
