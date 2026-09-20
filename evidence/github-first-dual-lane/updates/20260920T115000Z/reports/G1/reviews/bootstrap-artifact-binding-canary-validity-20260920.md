# G1 bootstrap cross-run canary validity contract

Observed 2026-09-20 Asia/Ho_Chi_Minh. New bounded design artifact. Read-only:
no live API calls, workflow dispatch, artifact upload/delete, token capture,
source/generated edits, permission change, authority change, or hosted run.

Reviewed frozen run-isolation plan
`bootstrap-artifact-binding-run-isolation-followup-20260920.md`, SHA-256
`56bf72bca3ddf60503f567326c5a90399b562510130b784e75e0805e533e647f`.

## Decision

A foreign-token denial is evidence only when all of these are true in the same
fresh canary:

1. the target producer run and target producer job are currently active;
2. the target job's own runtime token has successfully completed an equivalent
   fresh-name request with the exact target backend-ID pair;
3. the probe job's own runtime token has successfully completed an equivalent
   fresh-name request with the exact probe backend-ID pair;
4. the foreign request changes only the backend-ID pair from probe IDs to the
   captured target IDs; all protocol fields remain valid and fresh;
5. the target artifact remains present, unexpired, byte-identical, and owned by
   the target run after the probe; and
6. run/job/workflow metadata binds those backend IDs to the reviewed generated
   Velnor workflow, not to a stale fixture or candidate claim.

Anything else is `INVALID_FIXTURE` or `INDETERMINATE`, never a passing
authorization denial. In particular, a rejection caused by nonexistent,
expired, completed, mismatched, malformed, duplicate, or unsupported inputs is
not evidence that a valid foreign token cannot mutate a valid active target.

## Protocol identity: REST IDs are not backend IDs

The canary must preserve two separate identity namespaces:

```text
REST run ID                 decimal Actions workflow-run API `id`
REST job ID                 decimal workflow-jobs API `id`
REST artifact ID            decimal artifacts API `id`
workflowRunBackendId        opaque Actions artifact-service ID from token scope
workflowJobRunBackendId     opaque Actions artifact-service ID from token scope
```

The REST job `id` must never be placed in `CreateArtifactRequest` or
`FinalizeArtifactRequest`. The pinned toolkit extracts the two artifact-service
IDs from the runtime token's `Actions.Results:<run-backend>:<job-backend>` scope.
The diagnostic may record those two opaque IDs as canary metadata, but must
never log, upload, or persist the runtime JWT.

Relevant pinned-source behavior:

* `upload-artifact.ts` obtains `getBackendIdsFromToken()`, places both IDs in
  `CreateArtifactRequest`, then places both IDs in `FinalizeArtifactRequest`:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/upload/upload-artifact.ts>
* `util.ts` parses the two IDs from the `Actions.Results` scope:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/shared/util.ts>
* `delete-artifact.ts` places the artifact's two backend IDs and name in
  `DeleteArtifactRequest`:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/delete/delete-artifact.ts>
* the Twirp client authenticates with the runtime token while sending the
  request body to the artifact service:
  <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/shared/artifact-twirp-client.ts>

The exact toolkit/action revisions must be pinned in generated Velnor output
and recorded in evidence. Do not infer request behavior from a REST artifact
object; the REST schema exposes decimal run/job/artifact facts, not the
artifact-service backend-ID pair.

## Fresh-run handshake

The canary needs a base-owned producer workflow, a separate base-owned probe
workflow, and a read-only verifier/controller. These are generated Velnor
diagnostic workflows, not test fixtures and not candidate workflows. They use
fixed harmless bytes, no checkout, no PR input, no candidate code, no Docker,
no host/socket access, no repository write token, no secrets, and no signing or
permission authority.

### 1. Producer starts and proves its own target

The producer workflow runs a fixed job `scope_canary_target`.

At the beginning of the job, a fixed base-pinned diagnostic action obtains its
own backend pair through the pinned toolkit helper and writes only this
nonsecret handshake record to a short-retention diagnostic artifact:

```json
{
  "schema": "velnor.artifact-scope-canary.v2",
  "role": "target",
  "workflow_path": ".github/workflows/<generated-canary-target>.yml",
  "workflow_id": 123456,
  "workflow_ref": "owner/repo/.github/workflows/<file>.yml@refs/heads/main",
  "workflow_sha": "<40-hex>",
  "event": "workflow_dispatch",
  "run_id": 123456789,
  "run_attempt": 1,
  "rest_job_id": 987654321,
  "rest_job_name": "scope_canary_target",
  "workflow_run_backend_id": "<opaque-id>",
  "workflow_job_backend_id": "<opaque-id>",
  "target_repository": "owner/repo",
  "target_repository_id": 123,
  "toolkit_revision": "<40-hex>",
  "upload_action_revision": "<40-hex>"
}
```

The fixed action must obtain `workflow_id`, `rest_job_id`, and repository facts
from trusted controller/API observations, not from a self-reported JSON field.
The producer may write the backend pair locally and upload it only through the
fixed diagnostic handoff action; no token material is included.

Before waiting, the same target job performs a valid positive artifact-service
transaction with its own runtime token and its own backend pair:

```text
target-positive-name = velnor-scope-target-positive-<run_id>-<attempt>
fixed bytes           = one known nonsecret byte sequence
mime type/version     = exact values from the pinned toolkit request
expires_at            = sufficiently later than the bounded canary window
```

The transaction must complete `CreateArtifact`, blob upload, and
`FinalizeArtifact`, returning success and a service digest. It must use a
fresh name never used by any prior run. The verifier records the resulting
artifact ID, name, size, digest, raw ZIP digest, and timestamps.

This is the target validity proof. If it fails, the canary is not a negative
test; classify `TARGET_POSITIVE_FAILED` and stop.

After that positive transaction, the target job enters a bounded hold while
the probe executes. The hold must keep both REST run and REST job status
`in_progress`; it must not finish before the probe's post-check. If the hold
expires or the target job becomes completed/cancelled, classify
`TARGET_NOT_ACTIVE` and discard the run.

### 2. Controller binds the handshake to live metadata

Before starting the probe, a base-owned controller reads the handshake and
queries the live Actions APIs. It must prove:

```text
run.id == handshake.run_id
run.run_attempt == handshake.run_attempt
run.path/workflow_id == handshake path/workflow_id
run.event == workflow_dispatch (or the generated fixed event)
run.status == in_progress
job.id == handshake.rest_job_id
job.name == handshake.rest_job_name
job.run_id == run.id
job.status == in_progress
workflow_sha/ref/path == generated contract observation
target repository name/id == reviewed repository
target-positive artifact.workflow_run.id == run.id
target-positive artifact.expired == false
target-positive artifact.expires_at > now + safety margin
```

The controller also verifies that the handshake's backend pair came from the
same job that performed the successful target positive transaction. The pair
is opaque, so the proof is behavioral plus source-bound: valid target IDs
accepted by that active target token, exact job/run/workflow metadata, and no
cross-run substitution before the probe. A REST job ID equal to a backend ID
by string coincidence is not accepted as a mapping.

Only after this validation does the controller pass the nonsecret target
metadata to the separate probe. A manual dispatch, if used for orchestration,
must be made by the base-owned controller with these validated values; arbitrary
user-supplied run/job IDs are invalid input, not a test result.

### 3. Probe proves its own valid identity

The probe workflow runs a separate fixed job `scope_canary_probe` and receives
only the validated target metadata. It obtains its own fresh runtime token and
decodes its own backend pair locally. The probe first performs the exact same
valid transaction with:

```text
probe token + probe workflowRunBackendId + probe workflowJobRunBackendId
probe-positive-name = velnor-scope-probe-positive-<probe_run_id>-<attempt>
```

This must complete create, blob upload, and finalize successfully while the
probe job is active. If it fails, classify `PROBE_POSITIVE_FAILED`; do not
interpret a later 401/403 as foreign authorization evidence. This control also
guards against an expired runtime token, broken Twirp URL, wrong artifact
version, bad MIME/size/hash fields, unsupported protocol revision, or a broken
diagnostic client.

## Valid foreign-token tests

All foreign tests reuse the exact request template that succeeded in the target
and probe positive controls. The only intended authorization variable is the
runtime token/backend-ID relationship. Every create request uses a new name;
no duplicate-name conflict is allowed.

### Primary upload/finalize denial

The probe token submits:

```text
CreateArtifactRequest:
  workflowRunBackendId:  target workflow-run backend ID
  workflowJobRunBackendId: target workflow-job backend ID
  name: fresh foreign-create-<probe_run_id>-<nonce>
  mimeType: exact positive-control value
  version: exact positive-control value
  expiresAt: valid future timestamp
```

If create is accepted, the probe uploads the same fixed bytes to the returned
signed URL and submits `FinalizeArtifactRequest` with the exact positive-control
size/hash and the same target IDs/name. Any successful create or finalize is a
security failure: the foreign probe has minted an artifact under the target
backend identity. A denial at either stage is usable only if both positive
controls succeeded and the target remained active.

### Primary delete denial

The target positive artifact remains present. The probe token submits the exact
`DeleteArtifactRequest` shape with the target backend pair and the target
positive artifact name. If the request succeeds or the target artifact
disappears/changes, classify `FOREIGN_MUTATION_SUCCEEDED`. If it is denied and
the post-check shows unchanged target ID/name/size/service digest/raw bytes,
classify `FOREIGN_DELETE_DENIED_AUTH`.

If a future pinned toolkit removes or changes this internal delete operation,
classify `UNSUPPORTED_DELETE_PROTOCOL`; do not substitute an invented REST
operation or call a denied test a pass.

### Secondary pair-mismatch controls

After the primary exact target-pair case, run these controls using valid IDs
from both active jobs:

```text
target run + probe job
probe run  + target job
```

They should fail, but they are diagnostic only. The primary case is the one
that isolates foreign-token authorization: the request carries the complete,
valid target pair while the bearer token belongs to the probe. Pair-mismatch
failures cannot substitute for it.

## Required post-check

The base-owned verifier executes before the target hold ends, then rechecks
after the hold releases if possible. It re-queries every artifact page for the
target run and records:

```text
target-positive artifact ID/name/workflow_run.id
target-positive size/service digest/raw ZIP digest
target-positive created/updated/expires timestamps
target run/job status and attempt
probe-positive artifact workflow_run.id == probe run ID
foreign-create artifact absent from target run
target artifact still downloadable and byte-identical
```

The target artifact's `updated_at` may change only according to a documented
read-only observation; any delete, replacement, duplicate target artifact, new
target artifact, digest, size, or byte change is a security failure. The probe's
own artifact is expected to belong to the probe run and must not be mistaken
for a target mutation.

No token, signed URL, Authorization header, JWT claim set, or secret may enter
logs or evidence. Record operation, target IDs (opaque IDs may be retained as
canary metadata if policy allows), redacted HTTP/Twirp status, request ID, and
pre/post artifact facts.

## Failure classification

| Observation | Classification | Counts as authorization denial? |
| --- | --- | --- |
| Target own-token fresh-name create/upload/finalize fails | `TARGET_POSITIVE_FAILED` | No; discard run |
| Probe own-token fresh-name create/upload/finalize fails | `PROBE_POSITIVE_FAILED` | No; discard run |
| Target run/job not `in_progress` during foreign request | `TARGET_NOT_ACTIVE` | No |
| Target artifact expired or expiry margin too small | `TARGET_EXPIRED` | No |
| Target/producer IDs absent, stale, wrong workflow, wrong repo, or wrong job | `TARGET_METADATA_MISMATCH` | No |
| REST job ID supplied where backend UUID is required | `INVALID_ID_NAMESPACE` | No |
| Malformed/unknown protocol version, MIME, size, hash, timestamp, or request shape | `INVALID_PROTOCOL_FIELDS` | No |
| Duplicate artifact name causes rejection | `DUPLICATE_NAME_CONFLICT` | No |
| Probe token expired/invalid and own positive did not succeed | `PROBE_TOKEN_INVALID` | No |
| 429/5xx/network timeout or service ambiguity | `INDETERMINATE_SERVICE_ERROR` | No; rerun fresh |
| Exact target pair + valid fields + probe token denied; target unchanged | `FOREIGN_DENIED_AUTH` | Yes |
| Target artifact delete request denied; target unchanged | `FOREIGN_DELETE_DENIED_AUTH` | Yes |
| Foreign request creates/finalizes/deletes/changes target artifact | `FOREIGN_MUTATION_SUCCEEDED` | No; architecture fails |
| Probe response denied but target changed or post-check incomplete | `POSTCHECK_FAILED` | No; architecture fails/indeterminate |

Only `FOREIGN_DENIED_AUTH` and, when supported, `FOREIGN_DELETE_DENIED_AUTH`
are valid negative evidence, and only when paired with both positive controls,
active target metadata, exact valid fields, and an unchanged post-check.

## Acceptance gate

The future hosted run must publish one immutable, sanitized record containing:

```text
generator/source/contract digest
workflow path/id/ref/SHA for target and probe
target/probe run IDs and attempts
target/probe REST job IDs and names
target/probe backend-ID pair fingerprints or approved opaque IDs
target/probe positive artifact IDs/names/digests
exact toolkit/upload-action revisions
request operation and target backend-ID pair (no token)
redacted response status/request ID
target pre/post artifact identity, digest, bytes, and expiry
failure classification from the table above
```

No live canary is authorized by this artifact. Before dispatch, review the
generated Velnor source, action/toolkit pins, normalized effective permissions,
concurrency isolation, protocol request schema, and redaction behavior. A
negative result without the fresh active positive controls is rejected as a
false proof. A canary that cannot keep the target job active while the probe
runs is not a valid canary; change the orchestration or hold the gate.

