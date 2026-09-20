# Bootstrap artifact-binding repair proposal

Observed 2026-09-20. Read-only design review. No source, generated workflow,
hosted run, artifact upload, token probe, or remote state was changed.

Reviewed source checkpoint: `3ed0023b038335d7b22dfa2758457e3808f777ee` in
`/private/tmp/velnor-g1-bootstrap`.

Disposition: **select `github-artifact-attestation-v1`; replace
`static-single-uploader-v1`.** This is a design amendment only. It is not
implementation approval, hosted approval, or a merge recommendation.

## Result

Keep the direct `ci-pr.yml` `candidate_producer` job with no ordinary CI
dependencies. Keep the candidate build untrusted and token-free inside the
existing isolated boundary. After the fixed upload action returns its
`artifact-id` and `artifact-digest`, a second fixed, base-owned step creates a
GitHub artifact attestation whose signed subject and custom predicate bind:

```text
target repository + numeric repository IDs
ci-pr workflow path + normalized contract digest
pull_request event + head/base SHA
workflow run ID + run attempt
producer REST job ID + fixed logical job name
static artifact name + artifact ID
artifact service digest + raw ZIP SHA-256
fixed upload/attest action revisions and archive digests
```

The `pull_request_target` `policy_acquire` job remains the trusted status
boundary. It selects the current run/job/artifact through the API, downloads
the selected artifact ID, hashes the raw ZIP, fetches/verifies the attestation
for that digest, and compares every signed predicate field. No artifact name,
timestamp, candidate manifest, job output copied through an artifact, or REST
artifact-to-job inference is a fallback.

If artifact attestations, their API, the trusted root, or the fixed verifier is
unavailable, acquisition fails closed. It does not fall back to
`static-single-uploader-v1`.

## Exact current gap

The producer template already declares outputs at
`crates/velnor-workflow/src/s2/primitives/ir.rs:3026-3040`:

```yaml
outputs:
  artifact_id: ${{ steps.candidate_upload.outputs.artifact-id }}
  artifact_digest: ${{ steps.candidate_upload.outputs.artifact-digest }}
  upload_step_id: candidate_upload
  artifact_binding_method: static-single-uploader-v1
```

The generated direct job is `.github/workflows/ci-pr.yml:108-113`; its
producer has no ordinary `needs` and uses `permissions: {}`. The trusted
consumer is rendered separately as `ci-policy.yml` at
`crates/velnor-workflow/src/s2/mod.rs:5744-5754`, triggered by
`pull_request_target`. Its current acquisition selects a run, producer job,
and static-name artifact, then downloads the REST artifact ID
(`mod.rs:5527-5565`). The artifact REST object has no uploader job ID.

Thus the existing outputs are useful inside the producer workflow but are not
transported to the separate policy workflow. The current timestamp/static
uploader proof permits this attack:

```text
ordinary PR job uploads candidate name first
  -> fixed producer's overwrite=false upload fails (denial only)

fixed producer uploads ID A / digest A
ordinary PR job overwrite=true deletes A, creates ID B / digest B
  -> policy sees one static-name artifact B
  -> B's service digest equals its own ZIP hash
  -> current policy can accept wrong-job bytes (false positive)
```

`actions/upload-artifact` documents cross-job same-name overwrite as delete
then recreate with a new ID. The official action's default is no overwrite,
which explains the first branch but does not prevent the second:
<https://github.com/actions/upload-artifact#overwriting-an-artifact>.

## Why exact job outputs are not the current repair

GitHub job outputs are a supported same-workflow channel. A downstream job
with `needs: candidate_producer` can consume
`needs.candidate_producer.outputs.artifact_id`; job-call `with` inputs to a
pinned reusable workflow may also use the `needs` context:

* <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idoutputs>
* <https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations>

The current trusted consumer is not such a downstream job. The documented
workflow-jobs REST response exposes job ID, run ID, status, conclusion, name,
steps, and timestamps, but no evaluated job-output map:
<https://docs.github.com/en/rest/actions/workflow-jobs>.

`workflow_run` supplies the upstream run identity and permits artifact
download, but does not add arbitrary upstream `needs` outputs. Copying the
artifact ID through another artifact, log, summary, command file, or
candidate-written manifest returns to the same ordinary-job overwrite threat.

A same-workflow trusted reusable verifier could be a different architecture:
the direct `ci-pr` workflow would call a base-pinned verifier job with
`needs: [candidate_producer]`, and a separate policy workflow would have to
verify that exact call and its check conclusion. That changes the current
trusted status topology. It is not the selected repair. The selected repair
keeps the existing separate policy status writer while adding a signed
cross-workflow binding.

## Selected binding protocol

### Producer fixed steps

1. The base-owned candidate block remains the only direct producer path. Its
   PR-head build runs in the existing explicit container/env isolation and
   never receives `GITHUB_TOKEN`, `ACTIONS_RUNTIME_TOKEN`, OIDC, command-file
   paths, or artifact service access.
2. The fixed SHA-pinned `actions/upload-artifact` step uploads the exact static
   namespace and must return numeric `artifact_id` plus nonblank
   `artifact_digest`.
3. A base-pinned fixed attestor runs immediately after upload. It obtains the
   producer REST job ID through the authenticated Actions jobs API, queries the
   exact artifact ID, downloads its ZIP through the artifact API, and computes
   `raw_zip_sha256`. It requires:

   ```text
   artifact REST run ID       == GITHUB_RUN_ID
   artifact name              == fixed candidate name
   artifact digest            == upload action digest
   SHA-256(downloaded ZIP)    == artifact service digest
   producer job                == candidate_producer, current run/head
   ```

   Missing job identity, artifact metadata, service digest, download, or raw
   hash is a hard failure before signing.
4. The attestor writes a canonical custom predicate to a fresh fixed scratch
   path. It invokes `actions/attest` (or the exact reviewed replacement) with:

   ```yaml
   subject-name: velnor-workflow-candidate-linux-x64#<artifact_id>
   subject-digest: sha256:<artifact_service_digest_hex>
   predicate-type: https://tailrocks.dev/velnor/candidate-artifact-binding/v1
   predicate-path: <fixed-scratch-predicate>
   ```

   The subject name includes the server artifact ID so a replay of identical
   bytes under a new artifact ID cannot reuse the old attestation. The custom
   predicate includes the complete tuple above, including both service and raw
   ZIP digests.

GitHub documents `subject-digest`, `subject-name`, and custom predicates for
`actions/attest`; it also documents that the action uses a short-lived
Sigstore signature and stores the bundle in the GitHub attestations service:
<https://github.com/actions/attest>.

The candidate producer's effective job permissions must be explicit and only
the fixed post-build path may use them:

```yaml
permissions:
  actions: read
  contents: read
  artifact-metadata: write
  attestations: write
  id-token: write
```

`id-token`, `attestations`, and `artifact-metadata` are intentionally not
available to the candidate container or any ordinary job. The build step is
still `env -i`/container-isolated; no token is passed into it. The action
archives, predicate generator, and API client must be base-owned, immutable
SHA references with independently checked archive hashes. No PR-local action,
PR workspace file, candidate manifest, or shell interpolation chooses the
subject/predicate schema.

The official attestation documentation lists `id-token: write`,
`attestations: write`, and artifact metadata permission for generation:
<https://github.com/actions/attest#usage>.

### Trusted acquire/verify

`policy_acquire` remains a fresh `pull_request_target` job with read-only
repository/Actions/attestation access. It must:

1. Resolve exactly one target `ci-pr.yml` workflow and current same-repository
   pull-request run/attempt. Verify target/head numeric IDs, event, base/head
   SHA, workflow path, status, conclusion, and pagination.
2. Resolve exactly one completed-success REST job named
   `candidate_producer` for that run/attempt/head. Record its numeric REST
   `job_id`; this is the job identity in the trusted tuple. A display name,
   candidate JSON field, or job/output proximity is not enough.
3. Resolve exactly one unexpired candidate artifact with the exact static name
   and `workflow_run.id == run_id`. Reject duplicates, stale attempts, missing
   service digest, and malformed IDs.
4. Download only that numeric artifact ID. Require the API service digest and
   downloaded raw ZIP SHA-256 to be present and equal. Validate the exact
   archive surface and candidate manifest independently as today.
5. Fetch all attestations for the selected service digest through the GitHub
   artifact-attestation API (or an equivalently pinned verifier). Verify the
   Sigstore bundle, timestamp, issuer/trusted root, repository identity, and
   signer workflow. Require exactly one current qualifying binding with:

   ```text
   subject name  == fixed-name#selected-artifact-id
   subject digest == selected-service-digest
   predicate run_id/attempt        == selected run/attempt
   predicate producer_job_id/name == selected REST job and candidate_producer
   predicate artifact_id/name     == selected REST artifact
   predicate service/raw digest   == independently measured values
   predicate head/base/repository == selected PR identity
   predicate workflow/contract    == base-owned normalized contract
   predicate upload/attest pins   == trusted action/archive pins
   ```

   Invalid signature, wrong signer, stale run, wrong artifact ID, duplicate
   qualifying bundle, absent bundle, or any predicate mismatch fails closed.

The artifact-attestation API supports lookup by subject digest and warns that
signature, timestamp, and signer identity must be cryptographically verified:
<https://docs.github.com/en/rest/orgs/attestations>.
GitHub's artifact-attestation overview confirms that provenance carries the
workflow, repository, commit, triggering event, and OIDC-derived identity:
<https://docs.github.com/en/actions/concepts/security/artifact-attestations>.

The resulting handoff records the attestation ID/bundle digest alongside the
selected run ID, attempt, REST producer job ID, artifact ID, service digest,
raw ZIP digest, and source identity. Candidate execution still consumes only
the trusted handoff; it never sees attestation credentials.

## Effective ordinary-job threat model

The normalized base-owned workflow contract must inspect the complete reachable
`ci-pr` workflow/action/reusable-workflow graph. It must require:

* exactly one candidate uploader and one candidate attestor, both in the fixed
  `candidate_producer` block;
* no ordinary job or reachable reusable workflow with
  `id-token: write`, `attestations: write`, or `artifact-metadata: write`;
* no mutable action/workflow ref and no PR-local attestation action, Sigstore
  client, or direct attestations API call in an ordinary job;
* explicit effective permissions for every job; top-level permissions cannot
  silently elevate an ordinary caller;
* fixed candidate artifact name, upload path, attestation subject-name pattern,
  predicate type, and action archive hashes;
* no extra producer job, matrix duplicate, alias, post-job uploader, or
  dynamic candidate namespace; and
* candidate build steps are limited to the base-owned checkout/upload/attest
  allowlist and cannot run PR-local actions outside the isolated build input.

GitHub creates a unique `GITHUB_TOKEN` at the start of each job and permits
job-specific permission scopes. This supports separating attestation identity
from ordinary jobs, but only if the normalized contract actually enforces the
effective scopes:
<https://docs.github.com/en/enterprise-cloud@latest/actions/concepts/security/github_token>
and <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idpermissions>.

Ordinary jobs retain the ability to attack the artifact results service with
their own runtime tokens. Under this binding:

| Attack | Required result |
| --- | --- |
| Upload static name before producer | Producer `overwrite=false` fails; no attestation; policy red. |
| Delete/recreate producer artifact after upload | New ID has no matching producer attestation; policy red. |
| Recreate identical bytes under new ID | Subject name/predicate artifact ID mismatch; policy red. |
| Upload wrong bytes and forge service/raw digest fields | API/raw hash and signed subject mismatch; policy red. |
| Replay old valid attestation | Run/attempt/artifact/job/head tuple mismatch; policy red. |
| Ordinary job creates a valid attestation | Contract forbids its identity permissions; any signer workflow/job mismatch is red. |
| Candidate build tries runtime token/OIDC/command files | Existing isolation must deny; attestor privileges are not in the build boundary. |

The repair does not prevent denial: an ordinary job can delete the producer
artifact, exhaust artifact quota, or race the upload. Those cases must fail
the gate. The repair removes the false-success path.

## Why hand-rolled signatures and check-run output are not selected

A repository secret or private key in the producer job would be exposed if the
candidate escaped its boundary. A key baked into a fixed action is recoverable
by anyone who can inspect the action. A custom external signer could be safe
only if it authenticates the producer's exact Actions identity/backend job and
rejects every ordinary job; that introduces an unproven external authority and
recovery dependency.

A check-run output is an authenticated GitHub record, but not a cryptographic
artifact binding. It would require a producer-only `checks: write` capability,
and policy would still need a source/job contract to identify the creating job.
It is weaker than the supported Sigstore attestation and is not selected.

The selected GitHub attestation uses a short-lived OIDC identity, a signed
subject digest, a custom predicate, and a public verification path. Its trust
boundary is explicit: fixed attestor code plus producer-only identity
permissions. It is still not a substitute for candidate content validation.

## Independent offline fixture proposal

No live upload or token probe is needed. Extend the existing bootstrap policy
fixture harness with a protocol-only scenario containing:

```json
{
  "run": {"id": 9001, "attempt": 1, "event": "pull_request",
           "head_sha": "<40hex>", "base_sha": "<40hex>"},
  "jobs": [
    {"id": 1001, "name": "candidate_producer", "run_id": 9001,
     "head_sha": "<40hex>", "conclusion": "success"},
    {"id": 1002, "name": "ordinary_pr_job", "run_id": 9001,
     "head_sha": "<40hex>", "conclusion": "success"}
  ],
  "artifacts": [
    {"id": 7001, "name": "velnor-workflow-candidate-linux-x64",
     "workflow_run": {"id": 9001}, "digest": "sha256:<A>"}
  ],
  "attestations": [
    {"subject_name": "velnor-workflow-candidate-linux-x64#7001",
     "subject_digest": "sha256:<A>", "bundle": "valid fixture",
     "predicate": {"run_id": 9001, "attempt": 1, "job_id": 1001,
       "artifact_id": 7001, "service_digest": "sha256:<A>",
       "raw_zip_sha256": "<A>"}}
  ]
}
```

The harness must model two distinct artifact-service job/backend identities
without using real credentials. A genuinely passing control is required before
each mutation. Run each mutation through the full acquisition decision path:

1. producer-first ordinary upload, producer upload failure, no attestation;
2. producer ID 7001 replaced by ordinary-job ID 7002/digest B;
3. ordinary job recreates exact bytes/digest A under ID 7002;
4. stale valid attestation from run 8999 or attempt 2;
5. wrong run/head/base/repository/workflow/job in a valid-looking predicate;
6. invalid signature with all predicate fields otherwise correct;
7. valid signature from another workflow/repository or ordinary job;
8. duplicate artifacts, duplicate qualifying attestations, missing pages;
9. service digest missing, raw ZIP mismatch, and subject-name ID mismatch;
10. producer/ordinary permission matrix where ordinary jobs gain each forbidden
    `id-token`, `attestations`, or `artifact-metadata` scope; and
11. fork producer, manual dispatch, wrong event, wrong attempt, and stale run.

Each case must fail for the concrete reason above. A fixture boolean such as
`attested: true` is not evidence; the harness must exercise signature,
identity, artifact-ID, service-digest, raw-digest, and API-selection checks.
The fixture should also assert that the REST jobs response has no job-output
field, preventing a future implementation from assuming a nonexistent API.

## Limits and implementation gates

* GitHub documents artifact attestations as available only on supported plans
  (public repositories broadly; private/internal repositories require the
  documented Enterprise Cloud availability). The repository's actual plan and
  API access must be checked before implementation. Unsupported means red, not
  static-uploader fallback.
* The attestation API returns bundles by subject digest; it does not itself
  provide artifact uploader job ownership. The custom predicate and trusted
  normalized workflow contract provide that binding; policy still compares all
  live REST identities.
* The action's artifact digest semantics must be validated against a real
  pinned canary: policy must continue requiring `sha256(downloaded raw ZIP) ==
  REST artifact.digest` and the attested subject digest. No digest equality is
  assumed from a name or manifest.
* Attestation deletion, API outage, trusted-root mismatch, action archive
  drift, stale bundle, or unknown predicate schema is a hard failure.
* The attestation proves the fixed wrapper signed the selected bytes and
  identity tuple. It does not prove the untrusted binary is semantically safe;
  source/object/closure checks and the candidate runtime boundary remain
  mandatory.

This amendment should replace the current design's static-uploader section,
change `artifact_binding_method` to `github-artifact-attestation-v1`, add the
fixed attestor/action pins and permission contract, and add the full mutation
matrix before any source implementation is approved.
