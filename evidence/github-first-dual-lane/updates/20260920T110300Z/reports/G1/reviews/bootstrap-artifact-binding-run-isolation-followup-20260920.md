# G1 bootstrap artifact binding: run-isolation follow-up

Observed 2026-09-20 Asia/Ho_Chi_Minh. Read-only source/design review. No
source or generated files, PRs, workflow runs, artifact uploads, token probes,
secrets, permissions, signing authority, or hosted state were changed.

## Review basis and disposition

The requested independent review input is
`8fd6a3d0da3e9715cc31d4b43a915ba7ef900c56a42dc64c66780935fe824d07`. That
report is not present in this shared checkout, so this follow-up verifies the
selected design against the exact source checkpoint
`3ed0023b038335d7b22dfa2758457e3808f777ee` in
`/private/tmp/velnor-g1-bootstrap`, the prior final repair note, and the
official Actions protocol/documentation.

Disposition: the selected `static-single-uploader-run-isolated-v2` direction
is architecturally coherent under the current no-new-authority constraint,
but is not approved. The split workflow graph, identity tuple, complete
normalized scanner, and a trusted hosted cross-run backend-ID canary are
prerequisites. The missing renderer/generated snapshot is implementation work,
not an architectural contradiction. Public token descriptions or a source
claim that “other runs cannot write this artifact” are not proof.

## Selected architecture

Render three explicit roles:

```text
ci-pr.yml (pull_request; candidate-only run)
  candidate_producer
    fixed base-owned control + explicit PR-head checkout + fixed upload
    no ordinary `needs`; no plan, unit, Velnor, provider, runtime, or
    other artifact-producing job

ci-pr-checks.yml (pull_request + existing workflow_dispatch)
  the complete existing plan/unit/provider/Velnor graph
    -> existing aggregate/check contexts

ci-policy.yml (pull_request_target; base-owned)
  acquire exactly one candidate ci-pr run/job/artifact
    -> validate source, workflow identity, digest, closure, and normal checks
    -> emit the sole policy result
```

This is a migration of the existing generated graph, not permission to remove
normal checks. `ci-pr-checks.yml` must preserve every current job ID, display
name/check context, reusable-workflow call, matrix, `needs` edge, artifact,
provider selection, and `workflow_dispatch` input (`providers`, `scope`, and
`base_sha`). Its ordinary plan must retain its current event-specific context
semantics; in particular, ordinary `github.sha` (the PR merge SHA) and the
candidate source head SHA must not be silently swapped.

The candidate file must not retain `workflow_dispatch`. A manual dispatch is an
ordinary-check operation and belongs to `ci-pr-checks.yml`; a manually started
candidate producer is not eligible. If a diagnostic producer is later needed,
it must be a separately named, base-generated canary with fixed bytes, not a
new admission path for the real candidate artifact.

## Distinct concurrency groups

The workflows need distinct, explicit groups. A suitable shape is:

```yaml
# candidate-only pull request run
group: velnor-${{ github.repository }}-candidate-${{ github.event.pull_request.number || github.ref }}

# ordinary pull request/manual check run
group: velnor-${{ github.repository }}-checks-${{ github.event.pull_request.number || github.ref }}

# trusted policy run
group: velnor-${{ github.repository }}-policy-${{ github.event.pull_request.number || github.ref }}
```

The exact prefix is implementation choice; the role suffix is not. Candidate,
checks, and policy must never share a `cancel-in-progress` group. Otherwise a
new ordinary run can cancel the only producer, or a policy rerun can cancel a
producer that a policy lookup is about to admit. Cancellation may still be
enabled within each role, but policy must bind the current PR event to the
current candidate run/attempt and reject every stale/cancelled run. Do not
fall back to a previous successful artifact after cancellation.

The scanner must compare concurrency expressions as normalized data, not merely
look for the word `candidate`. It must reject a candidate group that can collide
with the ordinary or policy group, a dynamic group that omits the PR/ref
identity, and an alternate trigger that can publish the candidate namespace.

## Full graph and context preservation

The current generated workflow proves why this mapping matters:

* `.github/workflows/ci-pr.yml:5-35` has both `pull_request` and
  `workflow_dispatch`, one shared `-pr-` group, and workflow-level
  `actions: read`/`contents: read`.
* `.github/workflows/ci-pr.yml:37-107` contains ordinary planning and runtime
  artifact publication. Those jobs cannot be silently discarded during the
  split.
* `.github/workflows/ci-pr.yml:108-128` places `candidate_producer` beside the
  ordinary graph, which is the current cross-job artifact race.
* `ir.rs:2521-2533` renders the PR trigger and dispatch inputs, while
  `ir.rs:2479-2511` renders aggregate concurrency. Both need an explicit role
  parameter after the split.
* `ir.rs:3026-3040` renders the candidate job and currently labels the binding
  `static-single-uploader-v1`; the isolated design must use a new binding label
  and emit its provenance fields from a fixed host step.

The generated normal workflow must retain provider and plan context, not just
the visible job names. Preserve `VELNOR_PROVIDERS`, `CI_SCOPE_OVERRIDE`,
`BASE_SHA`, `HEAD_SHA`, `VELNOR_EVENT_TRUSTED`, dispatch inputs, and all
reusable-workflow `with`/`secrets`/`needs` mappings. The candidate workflow
must have no path into those ordinary jobs. The policy must query the candidate
workflow ID/path explicitly and separately query the ordinary workflow/check
contexts; a run with the same commit is not interchangeable across paths.

The current source scanner does not yet establish this whole-graph invariant.
Its namespace census at `mod.rs:5349-5370` finds a fixed candidate uploader and
checks some reachable action references, but a literal uploader count is not a
complete trigger, job, reusable-workflow, permission, or cancellation proof.

## Exact executed workflow identity

Keep these values as separate fields. Never call all of them `head_sha`:

```text
pr_head_sha             = github.event.pull_request.head.sha       (H)
pr_base_sha             = github.event.pull_request.base.sha       (B)
pr_merge_sha            = pull-request API merge_commit_sha        (M; nullable)
event_sha               = github.sha                                (normally M for pull_request)
workflow_ref            = github.workflow_ref
workflow_sha            = github.workflow_sha                       (W)
workflow_file_path      = github.workflow_file_path
workflow_repository     = github.workflow_repository
run_id + run_attempt    = github.run_id + github.run_attempt
run_api_head_sha        = Actions workflow-run REST `head_sha`       (R)
producer_job_id/name    = workflow-jobs REST `id` + fixed name
artifact workflow_run   = artifact REST `workflow_run.id`
```

GitHub documents that `GITHUB_SHA` for `pull_request` is the last merge commit
and that `github.event.pull_request.head.sha` is the head commit. It also
documents `github.workflow_ref` and `github.workflow_sha` as the ref/path and
commit SHA of the workflow file. The workflow-runs API exposes a `head_sha`
filter/field, but that field must not be assumed to mean the PR head without a
hosted observation of this exact event. The workflow-run example also formats
`path` with a ref suffix, so path normalization must be explicit rather than a
blind string equality.

Sources:

* <https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows>
* <https://docs.github.com/en/actions/reference/workflows-and-actions/contexts>
* <https://docs.github.com/en/actions/concepts/workflows-and-actions/workflows>
* <https://docs.github.com/en/rest/actions/workflow-runs>
* <https://docs.github.com/en/rest/actions/workflow-jobs>

The current acquisition code conflates two fields: policy sets
`HEAD_SHA=${{ github.event.pull_request.head.sha }}` at `mod.rs:5462-5472`,
then filters both workflow runs (`mod.rs:5585-5594`) and producer jobs
(`mod.rs:5604-5608`) on `.head_sha == $HEAD_SHA`. That may be correct only if a
hosted observation proves `run_api_head_sha == H`; it is wrong if the API run
field follows the event merge SHA. The repair must first collect `R`, `M`, and
`H` and then compare the value according to the observed API contract. It must
not make a green result depend on a guessed SHA convention.

The fixed producer contract should write the workflow identity from Actions
contexts after the source-dependent build, into a base-owned provenance member
or equivalent measured handoff:

```json
{
  "workflow_file_path": ".github/workflows/ci-pr.yml",
  "workflow_repository": "target/repository",
  "workflow_ref": "target/repository/.github/workflows/ci-pr.yml@<ref>",
  "workflow_sha": "<W>",
  "event": "pull_request",
  "event_sha": "<M-or-event-value>",
  "pr_head_sha": "<H>",
  "pr_base_sha": "<B>",
  "pr_merge_sha": "<M-or-null>",
  "run_id": 123,
  "run_attempt": 1
}
```

This is not trusted merely because it is JSON. A fixed base-owned step must
write it after the build, the normalized scanner must prove that the PR build
cannot write the provenance path, and policy must compare it to API/event
observations. `run_api_head_sha` belongs in the trusted handoff as an API value,
not as a candidate assertion. `workflow_sha` must be used to fetch and scan the
exact workflow blob that actually ran; scanning only base `ci-pr.yml`, PR-head
`ci-pr.yml`, or the merge SHA by assumption is insufficient.

## Binding tuple and policy comparisons

The minimum trusted tuple is:

```text
target repository full name + numeric ID
head repository full name + numeric ID
PR number and current event association
candidate workflow file path + numeric workflow ID
workflow_ref + workflow_sha + workflow_repository
run ID + run attempt + event + status + conclusion
event_sha + run_api_head_sha + pr_head_sha + pr_base_sha + pr_merge_sha
candidate producer REST job ID + exact name + run ID + attempt
artifact ID + exact static name + artifact.workflow_run.id + service digest
raw downloaded ZIP SHA-256
base/head tree and closure identities
effective permissions + normalized reachable graph digest
```

Admission sequence:

1. Resolve the workflow ID and exact executed workflow path. Fetch/scan the
   workflow bytes at `workflow_sha`; reject an unavailable or mismatched
   workflow identity.
2. Enumerate candidate runs for the event and PR association. Require exactly
   one current completed-success run, then compare all four SHA fields rather
   than relying on a single `head_sha` query.
3. Resolve exactly one completed-success `candidate_producer` REST job for
   that run and attempt. Record numeric job `id`; job outputs are not a
   cross-workflow transport.
4. Enumerate all artifact pages for that run. Require exactly one unexpired
   fixed-name artifact with `workflow_run.id == run_id`, a valid service digest,
   and a raw ZIP digest equal to the service digest.
5. Validate the fixed provenance member and candidate manifest against the
   trusted tuple. Candidate assertions never select a run, job, artifact,
   source, or policy result.

`mod.rs:5462-5472` already distinguishes PR head/base repository IDs;
`mod.rs:5585-5608` selects a workflow/job; `mod.rs:5620-5644` selects the
artifact and compares service/raw digests; and `mod.rs:5712-5765` records the
handoff. The repair extends those checks; it must not replace them with a
candidate-provided ID or a job-output guess.

## Full normalized scanner

The scanner is an implementation prerequisite for the architecture. It must
normalize the entire reachable workflow graph at each relevant source object:

* all event triggers and activity types, including `workflow_dispatch`,
  `workflow_run`, `push`, `schedule`, `repository_dispatch`, and aliases;
* workflow/job `permissions` inheritance and job overrides, with explicit
  `{}` semantics and event/fork restrictions; reject every effective
  `actions: write`, `artifact-metadata: write`, `id-token: write`,
  `attestations: write`, contents/package/deployment write path, and any
  equivalent artifact-delete capability unless a separately approved authority
  exists;
* every job ID/name, `if`, `needs`, matrix, reusable-workflow input/secret,
  caller/callee edge, and concurrency group/cancellation expression;
* every reachable local/external action and reusable workflow, requiring the
  approved full-SHA action closure and rejecting dynamic or PR-resolved
  references;
* shell/API commands that expose runtime tokens, command files, artifact
  upload/delete calls, dynamic artifact names, or candidate namespace strings;
* all workflow files that can run in the repository, not only the two obvious
  paths. A workflow omitted from the scan can still use a writable repository
  token or a second publisher.

Candidate role requirements:

* exact `pull_request` trigger only; no manual, scheduled, push,
  `workflow_run`, or repository dispatch route;
* one job, fixed ID/name, no `needs`, matrix, service, local action, reusable
  call, second upload, dynamic name, or post-job publisher;
* `permissions: {}` and fixed checkout/build/upload actions; no candidate code
  in a host-side trusted step; exact action archive digests;
* source-dependent build only in the already reviewed boundary; fixed
  provenance writer after the build; no candidate write to provenance or
  command-file paths.

Ordinary role requirements:

* the complete original graph remains reachable with the same provider and
  dispatch semantics;
* no ordinary path executes in the candidate workflow run;
* no effective artifact-delete/write permission and no repository token path to
  delete the candidate artifact; ordinary runtime artifact tokens remain a
  separate capability that the hosted canary must test;
* ordinary artifacts use disjoint names where practical, but name disjointness
  is not the security boundary; `workflow_run.id` and full tuple binding are.

Policy role requirements:

* base-owned `pull_request_target`; no PR checkout or candidate execution;
* exact candidate and ordinary workflow IDs/paths, current PR association,
  run/attempt, source SHA tuple, and normalized contract digests;
* zero, duplicate, stale, failed, cancelled, fork, manual, or workflow-run
  producer => red; no stale-artifact fallback.

## Trusted hosted cross-run backend-ID canary (plan only)

The cross-run claim remains **UNPROVEN**. The toolkit source shows artifact
requests carrying workflow-run and workflow-job backend IDs, but source/docs do
not establish that an ordinary job's runtime token cannot submit a request for
another run. This needs a real hosted negative canary. Do not call it proven
from JWT/public-token claims, an artifact REST object, or a fixture boolean.

The canary must be generated by Velnor's approved workflow generator and run
only after source/preflight review. It is not an out-of-scope fixture, does not
use PR code, and does not alter repository permissions, branch protection,
secrets, signing roots, or any other authority. No canary is executed in this
review.

### A. Generated source and preflight (before any PR or dispatch)

1. Generate a uniquely named, base-owned diagnostic producer/probe pair from
   the Velnor source. Do not hand-edit YAML. The pair must use fixed harmless
   bytes, fixed full-SHA action pins, hosted Linux only, no checkout, no Docker,
   no candidate source, no secrets, and no repository write token.
2. Review the rendered graph and normalized permissions. Confirm distinct
   canary concurrency groups, no ordinary `needs`, no `GITHUB_TOKEN` write,
   no `id-token`/attestation authority, no PR-controlled `uses`/`run`, and no
   host/socket/command-file access outside the artifact action's own runtime.
3. Review the pinned `actions/upload-artifact`/toolkit implementation at its
   exact source revision. The concrete diagnostic mechanism is a tiny,
   base-owned, pinned toolkit client: in each run it calls the toolkit's
   `getBackendIdsFromToken()` and records only the two opaque
   `workflowRunBackendId`/`workflowJobRunBackendId` strings, never the JWT.
   The producer's two IDs are passed to the probe as reviewed nonsecret
   workflow-dispatch data; the probe also records its own IDs locally. The
   probe then constructs the exact `CreateArtifactRequest` and
   `FinalizeArtifactRequest` fields from that pinned toolkit source, replacing
   only the target IDs with producer IDs while authenticating with its own
   runtime token. This tests the actual backend boundary rather than a public
   REST claim. The `DeleteArtifactRequest` path is tested only if the pinned
   protocol exposes the same target-ID fields; do not invent an endpoint.
   Source review must confirm the client and request schema before dispatch.

### B. Producer run

1. Start a base-owned Velnor-generated producer run with a unique canary
   namespace and fixed bytes such as
   `velnor-artifact-scope-canary-v1`. The only upload is through the pinned
   artifact action; no candidate build or source checkout runs.
2. Record from trusted run/artifact API responses: producer run ID/attempt,
   workflow path/ID, `workflow_sha`, job ID, artifact ID/name/service digest,
   and raw ZIP digest. The fixed toolkit diagnostic emits only the producer's
   two opaque backend-ID strings for the reviewed probe input; it never emits
   or stores the runtime token. These IDs are canary metadata, not candidate
   provenance and not a new authority.

### C. Probe run with a different runtime identity

1. Start a separate Velnor-generated probe workflow run. It receives only the
   producer's nonsecret run/artifact/backend IDs and has its own artifact
   runtime token. It does not receive the producer token, `GITHUB_TOKEN` write,
   repository secrets, candidate bytes, Docker socket, or host process access.
   The fixed probe client decodes its own token for its own IDs, then sends
   Twirp requests through the pinned artifact service client with the producer
   IDs substituted in the request body. No token claim is edited and no token
   is logged.
2. Positive control: upload a harmless same-name diagnostic artifact in the
   probe's own run using its own backend IDs. This proves the probe client can
   make a normal request.
3. Negative cases: using the probe token, submit the reviewed artifact-service
   request with (a) producer run + probe job IDs, (b) producer run + producer
   job IDs, and (c) probe run + producer job ID. Where the pinned protocol has
   target artifact delete/overwrite operations, attempt them only against the
   unique canary artifact. Do not use the real candidate namespace.
4. Treat every successful target upload/finalize/overwrite/delete as a hard
   architecture failure. A 403/404/authorization rejection is expected but
   must be recorded with the token redacted and request IDs/status retained.

### D. Trusted post-check

The base-owned read-only verifier re-queries the producer run and all artifact
pages. It requires the original artifact ID, name, `workflow_run.id`, size,
service digest, raw ZIP digest, timestamps, and bytes to be unchanged. It also
confirms any probe artifact belongs only to the probe run. A probe failure to
reach the target is not enough if the target changed; the post-check is the
decision path.

The evidence record must include both runs' exact workflow path/ID,
`workflow_sha`, event, run/attempt, job IDs, action/toolkit SHAs, request
operation/target IDs, redacted response status, and pre/post artifact facts.
It must not include runtime-token material. Use fresh unique runs and canary
artifact names; do not rely on a prior artifact or shared concurrency group.

### E. Acceptance rule

Until the generated source passes preflight and the hosted canary proves every
cross-run mutation attempt is denied while the target artifact remains byte
identical, `static-single-uploader-run-isolated-v2` is a design candidate only.
If any cross-run operation succeeds, static run isolation is insufficient under
the current authority constraint; stop and escalate to an explicitly approved
new binding authority or a stronger repository/workflow isolation boundary.

## Remaining blockers

1. Implement the generator split and regenerate all snapshots. Do not reject
   the architecture merely because the renderer is not yet changed.
2. Fix run identity acquisition to distinguish H/B/M/event SHA/R/W and
   normalize workflow API paths; the current H-only `.head_sha` filters are not
   accepted without hosted evidence.
3. Implement the full normalized graph/effective-permission scanner and bind
   the exact executed workflow blob at `github.workflow_sha`.
4. Add fixed provenance context emission and trusted comparisons; candidate
   manifest/job outputs remain descriptive.
5. Run the generated Velnor-only hosted backend-ID negative canary after
   preflight. No public-doc claim substitutes for it.

## References

* Source checkpoint: `3ed0023b038335d7b22dfa2758457e3808f777ee`.
* Candidate renderer: `crates/velnor-workflow/src/s2/primitives/ir.rs:3026-3040`.
* Current generated PR graph: `.github/workflows/ci-pr.yml:5-128`.
* Current policy acquisition: `crates/velnor-workflow/src/s2/mod.rs:5459-5644`.
* Current handoff fields: `crates/velnor-workflow/src/s2/mod.rs:5712-5765`.
* Action artifact protocol implementation:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/upload/upload-artifact.ts>
* Toolkit backend-ID extraction:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/shared/util.ts>
* Toolkit artifact service client:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/shared/artifact-twirp-client.ts>
* Toolkit internal delete request:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/delete/delete-artifact.ts>
* Artifact REST schema:
  <https://docs.github.com/en/rest/actions/artifacts>
* Job outputs (same-workflow `needs` only):
  <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idoutputs>
