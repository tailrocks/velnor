# Independent bounded decision: run-isolated follow-up

Observed 2026-09-20 Asia/Ho_Chi_Minh. Read-only review. No source,
generated workflow, PR, ruleset, check, App, permission, dispatch, artifact,
token, signing authority, or hosted state was changed.

Reviewed exact follow-up:

* `G1/reviews/bootstrap-artifact-binding-run-isolation-followup-20260920.md`
* SHA-256 `56bf72bca3ddf60503f567326c5a90399b562510130b784e75e0805e533e647f`

Underlying source checkpoint: commit
`3ed0023b038335d7b22dfa2758457e3808f777ee`, tree
`45be601efb57e8d9da424a07e9115beee93a1564`, at
`/private/tmp/velnor-g1-bootstrap`.

## Bounded decision

**Yes: local generator-only structure and negative fixtures may be implemented
before hosted provider proof**, provided live acquisition acceptance is hard
disabled and remains unadopted. The follow-up removes the prior architecture
contradictions by specifying distinct candidate/checks/policy groups, old-graph
preservation, separate workflow/SHA identity, full-graph scanning, and a
provider canary.

This is permission for reversible source/test work only. It is not permission
to publish workflows, dispatch a canary, change App/OIDC/signing authority,
change rulesets/checks, merge, or make any candidate artifact authoritative.

Hosted cross-run backend-ID binding remains unproven. Until its canary passes,
the live acquisition path must return an explicit blocked/unauthorized result,
never green, never fall back to the mixed workflow, and never treat a local
fixture as provider evidence.

## Why no new architecture contradiction is present

The follow-up now gives the necessary separation:

* `ci-pr.yml` is candidate-only: one `candidate_producer`, no ordinary
  `needs`, no plan/unit/provider/Velnor/runtime path, and no `workflow_dispatch`;
* `ci-pr-checks.yml` owns the complete old ordinary graph, including dispatch
  inputs and required contexts; and
* `ci-policy.yml` remains base-owned `pull_request_target`, selecting the
  candidate run/job/artifact by trusted API data.

It also specifies noncolliding candidate/checks/policy concurrency groups and
separate `pr_head_sha`, base/merge/event/API SHA, workflow path/ref/SHA, run
attempt, job ID, and artifact ID/digest fields. Those are implementable
contracts; they do not depend on an impossible atomic merge-and-release
operation.

The remaining provider uncertainty is deliberately isolated in a future
hosted canary. That is a proof prerequisite, not a reason to block local
rendering and hostile-fixture work, so long as no local result can satisfy the
live admission predicate.

## Exact owned contract for local implementation

### 1. Generator roles are distinct types, not a filename alias

Implement explicit candidate and ordinary PR roles (or equivalent typed role
parameter). A second filename mapped to the existing `PullRequest` aggregate
is unsafe: it would render another candidate uploader and recreate the race.

The generator owns the complete output. Generated candidate/checks files,
reachable action manifests, workflow config/state, policy constants, and
fixtures must be regenerated from one source revision. No hand-edited YAML or
path-only string replacement.

### 2. Candidate role contract

The normalized candidate contract must require:

* `pull_request` only; no manual, push, schedule, `workflow_run`, or dispatch
  route;
* exactly one fixed job/name, no matrix, `needs`, service, reusable/local
  action, second upload, post publisher, or dynamic artifact name;
* `permissions: {}` and exact full-SHA checkout/build/upload/cleanup closure;
* fixed candidate namespace, action archive digests, runner, timeout, and
  noncolliding candidate concurrency group;
* PR-head checkout only inside the existing isolated build boundary; host
  steps cannot execute PR source or write trusted provenance; and
* fixed base-owned provenance writer after build. Candidate JSON, job outputs,
  logs, or summaries remain descriptive, never trusted transport.

`base-owned` here means the executed workflow blob is proven equal to the
reviewed base contract. A PR-controlled workflow may still execute before
policy rejects it; the safety claim is no authority exposure plus fail-closed
admission, not pre-dispatch prevention.

### 3. Ordinary role contract

The rendered `ci-pr-checks.yml` must machine-preserve the old ordinary graph:
plan outputs/digest, every job ID/name/`needs`/`if`, provider selection,
reusable-workflow inputs/secrets, runtime artifact publication, terminal
semantics, action pins, and `workflow_dispatch` inputs
(`providers`, `scope`, `base_sha`). Preserve ordinary PR merge-SHA context;
do not substitute candidate PR-head SHA.

The ordinary role must have a distinct fixed concurrency group and no path into
the candidate run. Effective permissions through reusable callees must exclude
artifact delete/write capability and any PAT/App/secret path that can reach it.
Same-name ordinary artifacts are still an allowed hostile case; name
disjointness is not the security boundary.

### 4. Policy and identity contract

Policy must bind all of these independently:

```text
repository names/IDs, PR number, head/base repositories
pr_head_sha, pr_base_sha, pr_merge_sha, event_sha, run_api_head_sha
workflow path, workflow ID, workflow_ref, workflow_sha, workflow repository
run ID, run attempt, event, status, conclusion
candidate job ID/name/run/attempt
artifact ID/name/workflow_run.id/service digest/raw ZIP digest
base/head tree and closure digests
effective permission and normalized reachable-graph digests
```

The exact workflow blob identified by `workflow_sha` must be fetched/scanned.
Do not assume the Actions run API `head_sha` means the PR head; record and
compare all distinct values until a hosted observation establishes its event
semantics. Duplicate, stale, cancelled, failed, fork, manual, or alternate
producer runs must be red with no stale-artifact fallback.

Official semantics requiring this separation:

* <https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows>
* <https://docs.github.com/en/actions/reference/workflows-and-actions/contexts>
* <https://docs.github.com/en/rest/actions/workflow-runs>
* <https://docs.github.com/en/rest/actions/workflow-jobs>

### 5. Scanner contract

Local scanner work is safe only if the scanner is semantic and closed-graph,
not the current line/indent normalization. It must reject duplicate YAML keys,
aliases/merges, unknown protected fields, added jobs, hidden local/reusable
actions, mutable refs, dynamic names, changed triggers/concurrency/env,
runtime upload/delete calls, and every effective write/delete permission through
reachable callers/callees. Scan all repository workflows capable of emitting
the candidate namespace or required context, not just the two expected paths.

Hostile fixtures must exercise the complete admission path and fail on the
specific changed field. A fixture boolean such as `provider_binding: true`,
`sole_uploader: true`, or `workflow_sha_verified: true` is forbidden.

## Mandatory disabled-acquisition gate

The implementation must make the provider dependency machine-visible and
unbypassable:

```text
provider_cross_run_binding_proof = Unknown
admission_mode = Disabled
=> acquire_candidate(...) = ProviderProofPending / NotAuthorized
=> no artifact is admitted, no policy success is emitted, no fallback runs
```

Required properties:

1. `Unknown` is the default and cannot be set to `Passed` by generated output,
   a candidate manifest, a check-run, a job output, a fixture, or a local
   environment variable.
2. Offline fixture tests may exercise the decision function, but their result
   is namespaced as synthetic test data and is not readable by production
   admission. No test artifact, cached JSON, or checked-in boolean may satisfy
   the live predicate.
3. The disabled path must fail before candidate artifact selection/publication
   and must not emit a green required context. Existing remote workflow/check
   behavior remains untouched until separately approved cutover.
4. Enabling acceptance requires an externally supplied, hash-bound proof record
   naming generator/source revision, exact toolkit/action SHAs, both run IDs and
   attempts, request operation/target IDs, redacted response statuses/request
   IDs, and target artifact pre/post ID/name/run/digest/raw-byte facts. No token
   or secret material may appear.
5. A proof record from the wrong source revision, action/toolkit revision,
   namespace, repository, or workflow role remains `Unknown`; there is no
   compatibility fallback.

This gate is the critical distinction between useful local implementation and
fabricated trust.

## Safe local work before hosted proof

The following can be implemented and tested locally without authority change:

1. Typed generator roles and distinct rendered candidate/checks/policy group
   expressions.
2. Generated positive snapshots and machine graph-equivalence comparison for
   the old ordinary graph.
3. Semantic reachable-workflow/action/permission scanner and hostile fixtures.
4. SHA/context tuple parsing, workflow-path normalization, duplicate-run and
   stale-attempt decision tests.
5. Disabled-gate behavior tests proving all provider-unknown paths return
   `ProviderProofPending`/`NotAuthorized` and never success or fallback.
6. A non-live, base-owned canary renderer and request-schema fixture based on
   the exact pinned toolkit source. It may validate request construction and
   redaction locally; it cannot claim provider enforcement.

The cross-run protocol source shows the request fields being tested:

* <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/upload/upload-artifact.ts>
* <https://raw.githubusercontent.com/actions/toolkit/main/packages/artifact/src/internal/shared/util.ts>
* <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/shared/artifact-twirp-client.ts>

No local test may call the real artifact service or dispatch a probe in this
phase.

## Canary-specific proof-quality correction

The follow-up canary is directionally sound but its test implementation must
avoid an ambiguous duplicate-name negative. If the probe submits producer IDs
and the producer's existing artifact name, a rejection could be ordinary
same-name conflict rather than authorization. Use a fresh unique target name
under producer IDs for create/finalize authorization testing, and separately
test target artifact mutation only where the pinned protocol exposes an exact
target-ID operation. The pinned internal delete path first lists an artifact
and then uses returned backend IDs; do not invent a delete request shape:

* <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/delete/delete-artifact.ts>

Every successful target create/finalize/overwrite/delete is a hard failure.
Every expected denial must be followed by trusted re-read of the producer
artifact and bytes. A denial without unchanged-target proof is incomplete.

The canary source must remain base-generated, fixed harmless bytes, no checkout,
no Docker/socket, no PR code, no repository write scope, no OIDC/attestation
scope, and no secrets. Its workflow files may be rendered locally, but not
published or dispatched under this decision.

## Classification of remaining blockers

Implementation/test prerequisites, not architecture contradictions:

* generator role split and generated snapshots;
* full semantic scanner/effective permission closure;
* workflow SHA/context capture and API path normalization;
* ordinary graph equivalence;
* disabled admission gate and synthetic-fixture isolation;
* canary request-shape/redaction/post-check harness; and
* real builder digest and hosted isolation proof.

External blockers that remain hard gates:

* approved hosted provider canary proving ordinary runtime tokens cannot
  create/finalize/overwrite/delete against producer backend identity; and
* separate approval for publishing/dispatching the canary, enabling admission,
  changing required contexts/rulesets, changing App/OIDC/signing trust, or
  cutting over generated workflows.

No exact contradiction requires abandoning run isolation. If the hosted canary
ever succeeds, run isolation is insufficient under the current authority
constraint; stop and require an explicitly approved stronger binding authority
or isolation boundary.

## Final disposition

Proceed with local generator and negative-fixture implementation only under the
owned contracts and disabled gate above. Keep live acquisition acceptance
explicitly disabled/unadopted until external provider evidence is hash-bound
and separately approved. This review is not a trust, check, dispatch, merge,
or execution approval.
