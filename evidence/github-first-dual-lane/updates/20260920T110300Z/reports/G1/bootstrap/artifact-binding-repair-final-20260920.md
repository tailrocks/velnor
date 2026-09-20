# Bootstrap artifact binding: bounded repair decision

Observed 2026-09-20. Read-only design review. No source, generated workflow,
hosted run, artifact upload, token probe, signing authority, or remote state was
changed.

Reviewed source: `3ed0023b038335d7b22dfa2758457e3808f777ee` in
`/private/tmp/velnor-g1-bootstrap`.

This report supersedes the mechanism-selection portion of
`G1/bootstrap/artifact-binding-repair-20260920.md`. That earlier report
documents GitHub artifact attestations as a possible future authority. The
decision below is the only repair selected under the accepted constraints:
preserve the direct `ci-pr` producer, keep it independent of ordinary jobs,
and make no new signing/identity authority change.

## Decision

Select `static-single-uploader-run-isolated-v2`:

```text
ci-pr.yml (pull_request, base-contracted candidate run)
  candidate_producer only
    fixed checkout + isolated PR-head build + fixed upload
      -> candidate artifact

ci-pr-checks.yml (pull_request, separate ordinary run)
  plan/unit/provider jobs

ci-policy.yml (pull_request_target, trusted status writer)
  select exact candidate ci-pr run/job/artifact
  download exact artifact ID and verify service/raw digest
  hand off only after source/manifest checks
```

The candidate producer remains a direct job in the `ci-pr` path and has no
ordinary `needs`. The `ci-pr` candidate workflow run contains no plan, unit,
provider, runtime, reusable-workflow, or other artifact-producing job. Those
jobs move to a distinct normal-check workflow/run. Candidate artifact
provenance is therefore bound by the complete trusted run contract, not by a
name/timestamp correlation inside a mixed run.

Approval is held for the generator migration that splits the workflow and for
the full normalized workflow/permission scanner. Do not implement, regenerate,
publish, or call this a gate pass from this report.

## Why exact outputs do not repair the current graph

The producer template currently emits:

`crates/velnor-workflow/src/s2/primitives/ir.rs:3026-3040`

```yaml
outputs:
  artifact_id: ${{ steps.candidate_upload.outputs.artifact-id }}
  artifact_digest: ${{ steps.candidate_upload.outputs.artifact-digest }}
  upload_step_id: candidate_upload
  artifact_binding_method: static-single-uploader-v1
```

GitHub job outputs are valid only to downstream jobs in the same workflow via
`needs`. The current trusted consumer is a separate `pull_request_target`
workflow (`mod.rs:5744-5754`), so it cannot consume these values. The official
workflow-jobs REST response contains job ID/run ID/status/conclusion/name/steps
and timestamps, but no evaluated `jobs.<job_id>.outputs` map:

* <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idoutputs>
* <https://docs.github.com/en/rest/actions/workflow-jobs>

`workflow_run` supplies an upstream run identity and artifact access, not
arbitrary upstream job outputs:
<https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run>.

Copying the ID through an artifact, log, summary, command file, or candidate
manifest is ordinary mutable transport. A same-workflow base-pinned reusable
verifier could use `needs`, but that changes the current trusted status graph;
it is not selected here.

## Why static-single-uploader is valid only after run isolation

The current mixed `ci-pr` run has ordinary PR jobs with their own artifact
runtime tokens. An ordinary job can upload the static name before the producer,
causing fixed `overwrite=false` producer failure, or delete/recreate the name
after producer upload, yielding a new wrong-job artifact. Official
`upload-artifact` documentation explicitly shows a later separate job deleting
and recreating an earlier job's same-named artifact with `overwrite: true`:
<https://github.com/actions/upload-artifact#overwriting-an-artifact>.

After the split, the candidate run has exactly one job and one upload path. A
normal-check job belongs to another workflow run and has a different Actions
runtime run/backend identity. Its same-named upload cannot become an artifact
of the candidate run. This assertion must be validated against the pinned
artifact protocol; the toolkit request carries workflow-run and workflow-job
backend IDs:
<https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/upload/upload-artifact.ts>.

The ordinary workflow must also be forbidden from using a repository
`GITHUB_TOKEN` with `actions: write` to delete the candidate artifact through
the REST API. Require `actions: read`/none for ordinary jobs and reject every
effective `actions: write`, `artifact-metadata: write`, or equivalent delete
capability in the normalized workflow contract. GitHub creates a unique
`GITHUB_TOKEN` per job and permits job-level scopes:
<https://docs.github.com/en/enterprise-cloud@latest/actions/concepts/security/github_token>.

The artifact REST response still does not identify an uploader job. The new
proof is structural: the trusted candidate run contains exactly one eligible
job and one fixed uploader, while every other workflow run is excluded by
`workflow_run.id == candidate_run_id`. No REST `job_id` field is invented.

## Required normalized contract

The base-owned policy checker must fetch and normalize the exact workflow
objects; a source string test limited to the producer block is insufficient.

### Candidate workflow (`ci-pr.yml`)

Require all of the following in both base contract and PR-head candidate file:

* trigger is the intended `pull_request` event; manual dispatch, workflow-run
  chaining, and alternate candidate triggers are absent or ineligible;
* exactly one job, fixed ID/name `candidate_producer`, fixed hosted runner,
  fixed same-repository PR condition, timeout, and `permissions: {}`;
* no `needs` entry and no matrix; no local action, reusable workflow, service,
  post-job uploader, or dynamic artifact name;
* only the base-pinned checkout, fixed isolated build wrapper, fixed
  `actions/upload-artifact`, and cleanup steps are reachable;
* upload action ref and raw archive SHA are fixed; name/path/retention and
  `if-no-files-found` are exact; `overwrite` is absent/default false;
* the build input is PR head data only inside the reviewed container boundary;
  candidate source cannot see runtime tokens, command files, workspace mounts,
  sockets, or host process state; and
* output fields may record action ID/digest for diagnostics but are not trusted
  by the separate policy workflow.

The contract hash must cover the complete candidate workflow file and its
reachable action manifests. It must reject any added job even if that job has a
different name or does not contain a literal `upload-artifact` step.

### Ordinary workflow (`ci-pr-checks.yml` and reachable calls)

Require:

* no path can run in the candidate `ci-pr` workflow run;
* effective permissions never include `actions: write`,
  `artifact-metadata: write`, or another artifact-delete capability;
* no mutable action/workflow refs, and no PR-local path can call a repository
  artifact-delete API with a writable token; and
* normal artifact names are disjoint from the candidate name where practical.

The cross-workflow boundary is a provenance boundary, not permission magic.
Ordinary jobs may still obtain their own artifact runtime token and attempt a
same-name upload in their own run. They must not obtain the candidate run's
token or a repository write scope capable of deleting candidate artifacts.

### Trusted policy workflow

`ci-policy.yml` remains base-owned `pull_request_target`; it never executes PR
bytes. It must require one exact candidate run/attempt and one exact producer
job before artifact selection. It must fail on zero, duplicate, stale, failed,
cancelled, or fork runs.

## Concrete acquisition tuple

The handoff must contain live API values, not candidate assertions:

```text
target_repository + target_repository_id
head_repository + head_repository_id
pull_request_number
workflow_path = .github/workflows/ci-pr.yml
workflow_id
run_id + run_attempt + event + conclusion
head_sha + base_sha
producer REST job_id + job_name = candidate_producer
artifact_id + artifact_name
artifact service digest = sha256:<64 hex>
raw downloaded ZIP SHA-256 = <64 hex>
artifact expiry/created/updated fields
source tree/object/closure identity
```

Required comparisons:

1. Resolve exactly one target workflow path and current same-repository
   pull-request run with exact head/base/PR/repository IDs and event.
2. Resolve exactly one completed-success `candidate_producer` job with exact
   run ID, attempt, head SHA, and fixed name; retain numeric REST `job_id` as
   evidence of the sole eligible uploader.
3. Enumerate every artifact page for that run. Require exactly one unexpired
   candidate-name artifact with `workflow_run.id == run_id`, valid ID, and
   nonblank service digest.
4. Download only that numeric artifact ID. Require the downloaded raw ZIP hash
   to equal the REST service digest. Reject missing/mismatched digest, expiry,
   duplicates, stale attempts, and unsafe archive members.
5. Record `artifact_binding_method=static-single-uploader-run-isolated-v2` and
   the complete workflow contract digest. Do not claim the REST object itself
   supplies uploader ownership.

The REST artifact schema documents the available ID/name/digest/run fields and
the absence of an uploader-job field:
<https://docs.github.com/en/rest/actions/artifacts>.

## Threat outcomes

| Mutation | Required disposition |
| --- | --- |
| Add ordinary job to candidate `ci-pr` | Candidate workflow contract fails before artifact admission. |
| Add hidden local/reusable action that can upload candidate name | Reachable-graph contract fails. |
| Ordinary separate workflow uploads same name | Different `workflow_run.id`; ignored by candidate selection. |
| Ordinary job tries runtime-token overwrite of candidate run | No candidate-run token; operation denied or affects only its own run. |
| Ordinary job gets `actions: write` and deletes candidate artifact | Permission contract fails; no green policy result. |
| Candidate uploads first, then artifact disappears | Exact artifact lookup/download fails; red, never fallback. |
| Static name has duplicate candidate artifacts in one run | Ambiguous selection; red. |
| Stale prior run has valid candidate artifact | Exact current run/head/attempt filter rejects it. |
| Fork/manual dispatch/workflow-run producer | Eligibility predicate rejects it. |
| Candidate build attempts token/command-file/socket access | Existing isolated build canary must fail closed; fixed uploader remains outside candidate input. |

Run isolation intentionally prevents false green; it does not prevent every
denial. An ordinary job can still waste its own run, race external APIs, or
cause the candidate producer to fail through repository-wide cancellation policy.
Those outcomes remain red and must not be converted to warning/skip success.

## Independent protocol fixture

No live artifact upload or token probe is required. Add a protocol-only
fixture to the policy harness with two workflow runs and distinct backend
identities:

```json
{
  "candidate_run": {
    "id": 9001, "attempt": 1, "path": ".github/workflows/ci-pr.yml",
    "event": "pull_request", "head_sha": "<H>", "base_sha": "<B>"
  },
  "candidate_jobs": [
    {"id": 1001, "name": "candidate_producer", "run_id": 9001,
     "head_sha": "<H>", "status": "completed", "conclusion": "success"}
  ],
  "candidate_artifacts": [
    {"id": 7001, "name": "velnor-workflow-candidate-linux-x64",
     "workflow_run": {"id": 9001}, "digest": "sha256:<A>"}
  ],
  "ordinary_run": {
    "id": 9002, "attempt": 1, "path": ".github/workflows/ci-pr-checks.yml",
    "event": "pull_request", "head_sha": "<H>"
  },
  "ordinary_artifacts": [
    {"id": 7002, "name": "velnor-workflow-candidate-linux-x64",
     "workflow_run": {"id": 9002}, "digest": "sha256:<X>"}
  ]
}
```

Positive control: exactly one candidate job/artifact, current run identity,
service digest equal to raw ZIP SHA-256, exact source tree/closure, and exact
base/head workflow contract. Then mutate one decision input at a time:

1. add a second job to candidate `ci-pr`, including one with no literal upload;
2. add a local/reusable uploader or dynamic name to candidate workflow;
3. move the ordinary artifact to candidate `workflow_run.id` while retaining
   ordinary job identity;
4. grant ordinary workflow `actions: write` or artifact metadata write;
5. duplicate candidate runs/attempts or candidate artifacts;
6. stale/wrong head/base/repository/event/PR association;
7. missing/blank service digest, raw ZIP mismatch, unsafe archive member;
8. fork/manual/workflow-run producer;
9. ordinary same-name artifact with correct or incorrect bytes; and
10. source contract digest, action pin, producer name, or job ID mismatch.

Every mutation must traverse the full acquisition decision path and fail for a
specific comparison. A fixture boolean such as `sole_uploader: true` is not
evidence. The fixture must assert that the jobs REST object has no job-output
field, preventing a future implementation from inventing a cross-workflow
output API.

## Signed-binding alternative and explicit authority hold

GitHub artifact attestations can provide a stronger cryptographic cross-workflow
binding: a fixed post-upload action could sign a custom predicate containing
run/job/artifact/service/raw-digest fields using `id-token: write`,
`attestations: write`, and `artifact-metadata: write`, then policy could verify
the bundle by subject digest. Official references:

* <https://github.com/actions/attest>
* <https://docs.github.com/en/actions/concepts/security/artifact-attestations>
* <https://docs.github.com/en/rest/orgs/attestations>

That is **not selected for this bounded repair**. It changes the accepted
producer authority from `permissions: {}` and adds a plan/API/trusted-root
dependency. It requires explicit root/user approval, a new fixed action pin,
ordinary-job denylist coverage, and hosted canary proof. Do not silently add
those permissions or treat a candidate manifest/check-run output as a
signature. If workflow splitting is rejected, no full repair exists under the
current no-new-authority constraint; the gate remains blocked rather than
falling back to the known-racy static contract.

