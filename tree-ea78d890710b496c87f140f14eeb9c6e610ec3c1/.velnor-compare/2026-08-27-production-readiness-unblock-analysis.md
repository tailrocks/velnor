# Production-readiness unblock analysis

Captured on `2026-08-27` from live GitHub API and read-only Sentry inspection.
The exact UTC capture time is unavailable for this artifact. The exact UTC
time known for the indexed run-state snapshot is recorded in the
reconciliation index; the admission refresh has no recorded exact UTC time.

Run state is indexed in
.velnor-compare/2026-08-27-production-readiness-reconciliation.md. That file is
the authoritative timestamped index for run IDs and unresolved objects; this
document records the analysis and the single canonical admission finding.

Older P0 inventory and cleanup artifacts referenced by this campaign are
historical snapshots, not current host or run state.

## Dated read-only recheck — 2026-08-27

Exact capture time unavailable. Runs `33010150644`, `32987670118`,
`32962658148`, and `32940470688` are completed with cancelled conclusions and
are no longer unresolved queue objects.

Run `33012336003` and check suite `89435047597` returned API HTTP 404. Both
remain unresolved absent GitHub Support confirmation of authoritative removal
or terminal state. ChainArgos runs `32985134450`, `32984965998`, and
`32984867843`, with suites `89353010038`, `89352428140`, and `89352110318`,
remain queued with null/zero check-run state unchanged since
`2026-08-26T15:22–15:26Z`.

## Exact-target remediation attempt — 2026-08-27

Capture date: `2026-08-27`; exact time unavailable. Only these ChainArgos
run/suite pairs were targeted. Each initial run and suite read returned HTTP
200 with `queued` status, null conclusion, and zero jobs/check runs. Normal
cancellation returned HTTP 409 with the exact message `Cannot cancel a
workflow run that has not been queued yet.` Force-cancellation returned HTTP
409 with the same message. Each final read remained queued/null/zero, with no
timestamp change.

| run | suite | created | initial HTTP 200 | normal cancel | force-cancel | final state |
|---:|---:|---|---|---|---|---|
| `32985134450` | `89353010038` | `2026-08-26T15:26:20Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |
| `32984965998` | `89352428140` | `2026-08-26T15:23:45Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |
| `32984867843` | `89352110318` | `2026-08-26T15:22:25Z` | queued / null / zero | HTTP 409, exact message above | HTTP 409, same message | queued / null / zero; timestamp unchanged |

No terminality or GitHub Support confirmation is claimed. No other runs,
suites, runners, files, workflows, or policies changed. The cleanup gate
remains open.

No fresh Sentry probe was performed in this recheck. Prior Sentry runner
registration observations remain historical and cannot establish current
registration, readiness, or capacity.

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
external backend state is a plausible hypothesis. The evidence does not
exclude Velnor lifecycle/routing contribution, but zero jobs/check-runs means
there is no evidence of step/Docker/expression/Results Service execution
failure in those three runs.

### Ownership/state audit pointer

The sole canonical ownership/state audit—including stale run/suite IDs,
ownership-unresolved classification, protected runs, current runner IDs,
Jackin omission, and the empty deletion set—is the reconciliation section
[`Ownership/state audit`](2026-08-27-production-readiness-reconciliation.md#ownershipstate-audit--2026-08-26t233806z).

## Canonical admission finding

The exact 2026-08-27 local read-only refresh is:

- Current PR head for `tailrocks/velnor` is
  `7489b6b07edfa75e589a2a35f108ffe3bd24e7f9`.
- Prior/indexed state recorded run `33012336003` as `queued`; the current
  recheck returned API HTTP 404 for that run and check suite `89435047597`.
  Current status is unknown and remains unresolved absent GitHub Support
  confirmation of authoritative removal or terminal state.
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

The successful validation job `98321562308` proves validation success only; it
does not prove runner admission or execution. The remaining blocker is group
membership/admission/readiness. Do not add a label or introduce a fallback.
Keep run `33012336003` unresolved as evidence of the unresolved admission
state; its prior/indexed state was `queued`, while the current recheck is API
HTTP 404 and remains unresolved absent GitHub Support confirmation.
The next safe step is read-only capture of group policy and membership, JIT
request/group assignment, and per-slot registration, broker-renewal,
watchdog, and daemon lifecycle state. Perform policy repair and re-admission
only after drain safety for accepted jobs is proven.

The read-only org group API capture used for this finding was:

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

## Provenance and API scope

- The reconciliation index snapshot has exact UTC provenance
  `2026-08-26T22:51:21Z`.
- The 2026-08-27 admission refresh and Sentry observations have only the date
  `2026-08-27`; their exact UTC capture times are unavailable and are not
  inferred here.
- GitHub evidence was read-only and scoped to the attributed repositories,
  workflow runs, jobs, check suites, and the ChainArgos organization runner
  group policy, repository-selection, and runner-list endpoints. The group
  endpoint scope was organization `ChainArgos`, runner group ID `4`.
- During the 2026-08-27 read-only recheck, no GitHub, Sentry, or SSH mutation
  occurred; no dispatch, rerun, or rerequest occurred either. Prior
  cleanup-attempt evidence records earlier cancellation requests and the
  removal of eight stale validation-owned registrations; those historical
  actions are not actions of this recheck and do not establish current state.

## Safe next action

Keep the no-dispatch and no-mutation gate in force. First obtain GitHub Support
confirmation for the 404 Velnor run/suite and the three unchanged ChainArgos
queue objects, while separately restoring Sentry access through the external
provider, console, or network owner. After access is restored, perform only
the read-only admission/lifecycle capture described below. Do not infer
resolution from HTTP 404, historical Sentry registration, or the successful
validation job.

## Ranked unblock options

1. **Restore Sentry access through the external provider, console, or network
   owner.** This is the sole prerequisite before read-only lifecycle/admission
   capture. It advances the cleanup gate by restoring the only missing path for
   authoritative per-slot, registration, health, and capacity evidence; it does
   not authorize mutation.

2. **Perform the read-only admission/lifecycle capture after access is restored.**
   Capture group policy and membership, JIT request/group assignment, and
   daemon/per-slot lifecycle state. Do not infer idle matching capacity from
   historical Sentry evidence or the successful validation request. If policy
   repair or re-admission is needed, defer it until drain safety for accepted
   jobs is proven.

3. **GitHub backend ownership resolution for the three obsolete runs.** Do
   not cancel or recommend cancelling these unowned objects. Provide GitHub
   Support the three run IDs, check suite IDs, obsolete SHA, HTTP 409 bodies,
   and zero-job responses; only explicit ownership resolution may authorize a
   later targeted cancellation attempt. No repository-side change can safely
   manufacture a terminal result for these objects.

4. **Policy repair/re-admission after drain safety.** Only after the read-only
   capture and proof that accepted jobs are safe to drain may policy be repaired
   or capacity re-admitted. Do not restart, drain, delete registrations, or
   otherwise mutate Sentry before that proof.

5. **Re-run the current PR only after the campaign gate clears.** No dispatch,
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
  delete active runner registrations, cancel unowned runs, or cancel the
  current Rust Docker run. Protect run `33019314096` and active Velnor runs
  `33023384527` and `33023384501`.
- Do not treat the queued CI lane as a Velnor protocol defect or a workflow
  defect until group admission, readiness, and Velnor queue matching are
  investigated.
- Do not dispatch, rerun, or rerequest checks while the no-dispatch gate is in
  force.

## Resume gate

After Sentry access is restored, execute this sequence exactly; there is no
circular requirement:

1. Individually re-query every prior unresolved/non-completed run and check
   suite. Request normal cancellation and force-cancellation only for an
   object with explicit current campaign ownership and where applicable.
   Do not cancel the three unowned/ownership-unresolved stale candidates.
   Record each object as terminal with its conclusion, or record GitHub
   Support-confirmed disappearance. Unresolved IDs remain a hard gate.
2. Remove only stale registrations owned by this validation. Do not delete
   active or unowned registrations.
3. Prove clean state, health, and capacity: no prior non-completed runs;
   targeted validation-owned registrations are clean; Sentry reports
   `github_reachable=true`, `routing_valid=true`, `runner_group_valid=true`,
   positive desired/actual/registered/executor-ready capacity; and one matching
   runner is online and idle.
4. Dispatch exactly once and monitor only that new run ID.

No dispatch, rerun, or rerequest is authorized before the sequence passes.

## Current access — 2026-08-27

The following are historical observations only:

- `ssh -G sentry` resolved the `sentry` alias to port `22` with no proxy.
- DNS resolved `sentry` to one record.
- An SSH attempt remained running for approximately 28 seconds with no
  diagnostics before it was interrupted.

Local state cannot distinguish a route, TCP, authentication, or remote-shell
failure. No machine classification is claimed from these observations. No
further SSH probe is authorized from this campaign: remote shell startup,
`~/.ssh/rc`, `ForceCommand`, and hooks may have side effects, and raw debug
output may leak sensitive data.

The next action is an external provider, console, or network-owner decision to
restore or inspect Sentry access. After that decision, an operator-authorized,
separately captured, sanitized diagnostic may be performed. No dispatch,
restart, drain, policy mutation, or further mutation is authorized.

Behavioral verifier artifact is static-only; no live probe was executed.

## Root-cause proof — 2026-08-27T00:16:00Z

Durable evidence for `tailrocks/velnor` PR run `33024674982`, SHA
`e79ed1969e9b05081c4af4e5f0f7c2d295590883`: the GitHub lane failed test
`controller_observes_live_session_and_executor_before_ready_proof` with
`session_live=false` and `executor_proven=true`. The contract and `ci-required`
checks were downstream of that failure; the Velnor lane was skipped. This is a
test/readiness failure record, not live production proof.

Root cause: direct test slot launches omitted `--generation`, while strict
procfs identity validation ignored the heartbeat. The architecture therefore
allowed a slot to appear executor-proven without proving a live session for the
same generation and heartbeat identity.

Structural fix: make `SlotArgs` generation required and use explicit test and
systemd invocations, so every slot launch carries generation identity and the
readiness proof observes the strict procfs/heartbeat contract.

Local regression proof: focused `cargo nextest` passed 3/3, and formatting
passed. No GitHub Actions rerun was performed because cleanup and external
access blockers remain. Residual gate: the Actions result and live
admission/readiness proof remain unresolved; do not claim live production
proof or production readiness from this evidence.

## Current read-only refresh — 2026-08-26T23:30:38Z

Successful read-only facts captured in this refresh:

- ChainArgos runs `32985134450`, `32984965998`, and `32984867843`, with check
  suites `89353010038`, `89352428140`, and `89352110318` respectively, remain
  queued with null conclusions and zero checks; their timestamps remain
  unchanged.
- Velnor run `33012336003` and check suite `89435047597` return HTTP 404.
- Runner group `4` (`velnor-trusted`) policy is unchanged, and its
  `/runners` endpoint reports `total_count=0`.

A repo-wide runner/noncompleted-run listing was not captured because local
`rtk` proxy/shell argument parsing failed. Prior runner-registration evidence
therefore remains historical; no current-state inference is made from it.

No mutation occurred in this refresh. The production-readiness gates remain
blocked.

## Current partial recheck — 2026-08-26T23:49:51Z

At capture, noncompleted runs were observed as follows:

| repository | noncompleted runs at capture |
|---|---|
| `tailrocks/velnor` | `33024640131` in progress; `33024640078` queued; `33024639794` in progress |
| `ChainArgos/java-monorepo` | `33019314096`, `32985134450`, `32984965998`, and `32984867843` queued |
| `jackin-project/jackin` | none |

Historical Velnor runs `33023384527` and `33023384501` are now completed with
failure conclusions. The newly observed runs are not claimed to be
campaign-owned.

Runner registrations, validation labels, and runner-group `4` membership were
not captured in this partial recheck. No inference is made from their absence.
No cleanup action is safe on this evidence. No mutation or dispatch occurred;
the no-dispatch gate and production-readiness gate remain open.
