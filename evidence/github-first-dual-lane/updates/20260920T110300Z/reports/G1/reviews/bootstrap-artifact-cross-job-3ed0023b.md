# Bootstrap artifact cross-job capability review

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

Scope: read-only review of bootstrap source commit
`3ed0023b038335d7b22dfa2758457e3808f777ee` and the pinned
`actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` / its
`@actions/artifact@6.2.0` dependency. No artifact upload, token probe, workflow
dispatch, hosted execution, or repository mutation was performed.

## Disposition

**CHANGES REQUIRED.** `static-single-uploader-v1` proves only a source-level
singular YAML uploader. It does not bind the resulting artifact to the
`candidate_producer` job when other PR-controlled jobs execute in the same
workflow run.

The producer build container is correctly denied the runtime token, but that
does not constrain a different ordinary `ci-pr` job. A PR-controlled Node
action in that job receives that job's Actions runtime token and can use the
artifact results protocol. The official upload action documents a separate
`upload-again` job deleting and replacing an artifact made by `upload`, using
the same name and `overwrite: true`. Therefore this is a real cross-job
create/delete/overwrite capability, not an artifact-REST speculation.

## Exact repository evidence

At the reviewed generated checkpoint:

* `.github/workflows/ci-pr.yml:108-113` defines `candidate_producer` with
  `permissions: {}` but leaves it in the same `ci-pr` run as ordinary jobs.
* `.github/workflows/ci-pr.yml:122-139` runs the build from a shell and checks
  that `GITHUB_TOKEN`, `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_RUNTIME_URL`, and
  `ACTIONS_ID_TOKEN_REQUEST_TOKEN` are empty. This protects that shell/build
  boundary only; it is not a workflow-wide capability restriction.
* `.github/workflows/ci-pr.yml:226-233` invokes one fixed
  `actions/upload-artifact` step for
  `velnor-workflow-candidate-linux-x64`, with no `overwrite` input (default
  false).
* The authoritative renderer has the same boundary at
  `crates/velnor-workflow/src/s2/primitives/ir.rs:3026-3040,3062-3083,3181-3190`.
  It exports `artifact_id`/`artifact_digest` as producer job outputs, but the
  policy consumer is a separate `workflow_run`-style acquisition path and
  does not consume those `needs` outputs.
* `crates/velnor-workflow/src/s2/mod.rs:5527-5546` selects one successful
  `candidate_producer` job and then one artifact by static name, run ID, size,
  expiry, and timestamps. The job response is checked for a successful
  `candidate_upload` step (`:5539-5540`), but no artifact/job binding is
  available in that response.
* The timestamp predicate at `:5542-5546` rejects an overwrite after the
  producer job completed, but it permits a replacement finalized after the
  producer upload and before producer job completion. Concurrent ordinary
  jobs make that a race, not a proof.
* The namespace scanner at `:5027-5042` records literal
  `actions/upload-artifact` YAML steps and at `:5277-5298` requires one named
  uploader. It cannot prove that arbitrary PR-controlled JS, a child process,
  or a daemon will not call the results protocol directly. A direct toolkit
  client does not contain the literal YAML action reference the census scans.

The earlier artifact-feasibility record remains applicable:
`G1/bootstrap-artifact-feasibility-2026-09-20.md` records that public REST
artifact metadata contains `workflow_run`, digest, name, and ID, but no
uploader job ID or upload-step ID.

## Pinned protocol proof

The pinned upload action source is available at:

* `src/upload/upload-artifact.ts` at
  <https://raw.githubusercontent.com/actions/upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/src/upload/upload-artifact.ts>.
  It delegates `overwrite` to `artifact.deleteArtifact(name)` before upload.
* The pinned action manifest at
  <https://raw.githubusercontent.com/actions/upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml>
  declares `overwrite` default `false`, and says `true` deletes a matching
  artifact before creating a new one.
* The pinned package lock resolves `@actions/artifact` to 6.2.0. Its upload
  implementation (package source, same public client) constructs:

  ```text
  CreateArtifact {
    workflowRunBackendId,
    workflowJobRunBackendId,
    name,
    version: 7
  }
  upload bytes to signedUploadUrl
  FinalizeArtifact {
    workflowRunBackendId,
    workflowJobRunBackendId,
    name,
    size,
    sha256 hash
  }
  ```

  The client obtains both backend IDs by decoding the Actions JWT `scp`
  claim; it does not use `GITHUB_TOKEN` or a public artifact-upload REST
  endpoint. The source is visible in the toolkit package and its generated
  client under
  <https://github.com/actions/toolkit/tree/main/packages/artifact/src/internal>.
* The toolkit client’s internal delete path first lists the current workflow
  run by name, selects the newest matching artifact, then sends
  `DeleteArtifact` with the selected artifact’s run/job backend IDs. This is
  the path used by `overwrite: true`; it is not the public REST delete API.

The official action README gives an executable protocol contract for the
cross-job case: one `upload` job creates `my-artifact`, then a separate
`upload-again` job (`needs: upload`) uses the same name and `overwrite: true`.
The README states that the old artifact is deleted and a new ID is created:
<https://github.com/actions/upload-artifact#overwriting-an-artifact>.
The migration document repeats that v4 artifacts are immutable **unless
deleted**, and that overwrite creates a new artifact ID:
<https://github.com/actions/upload-artifact/blob/main/docs/MIGRATION.md#overwriting-an-artifact>.

This establishes the relevant server scope: a job-scoped runtime token can
perform same-run artifact replacement through the supported client. The
backend job ID in the protocol request identifies the caller/target artifact
record; it is not exposed as a creator field in the REST artifact object, and
the current policy code never compares it.

## Why `permissions: {}` is insufficient

The Actions runner injects the system Actions runtime URL/token into each
Node action subprocess. Current runner source sets
`ACTIONS_RUNTIME_URL` and `ACTIONS_RUNTIME_TOKEN` in
`NodeScriptActionHandler`:
<https://github.com/actions/runner/blob/main/src/Runner.Worker/Handlers/NodeScriptActionHandler.cs#L677-L683>.
The runner's authentication design distinguishes this per-job Actions
service token from `GITHUB_TOKEN`:
<https://github.com/actions/runner/blob/main/docs/design/auth.md#accessing-github-resources>.

Thus `permissions: {}` and the build shell's empty-variable assertions do not
establish that no other job can call the artifact service. A PR-controlled
Node action can read the runtime token and results URL directly. A malicious
action can also retain a child daemon with that environment for later steps in
the same job; a shell/test process need not itself receive the variable. The
artifact client source reads `ACTIONS_RUNTIME_TOKEN` and the results service
URL from the process environment, then uses the JWT's run/job backend IDs.

The current `ci-pr` ordinary-job graph includes PR-reachable local actions
(for example the Rust reusable workflow's report action). More generally, the
source census is not a semantic proof against a PR-controlled action or test
process that implements the same protocol with HTTP/protobuf.

## Attack outcomes: denial versus false positive

Let `C` be the fixed candidate name, `P` the candidate producer, and `O` an
ordinary PR-controlled job in the same `ci-pr` run.

| Attack schedule | Producer outcome | Acquisition outcome | Security meaning |
| --- | --- | --- | --- |
| `O` creates `C` before `P` upload; `P` uses default overwrite=false | `P` upload conflicts/fails; no successful producer artifact | policy rejects because producer/job or artifact is absent | denial-only; fail-closed but does not prove provenance |
| `P` finalizes, then `O` overwrites before `P` completes | `P` remains successful | old artifact is deleted; one new `C` remains; new timestamps can satisfy `updated_at <= P.completed_at`; policy downloads `O`'s bytes | **false-positive wrong-job artifact** |
| `O` overwrites after `P` completes | `P` successful | timestamp predicate rejects the replacement (`updated_at > P.completed_at`) | denial-only if the timestamp check is reliable |

The second row is the gap. `O` can poll the same-run artifact list and race the
short post-upload cleanup/completion interval. It can upload a small crafted
archive quickly. The archive can carry the expected two members and a
head/revision-consistent manifest; service digest and raw ZIP digest then
match the wrong artifact perfectly. Those digests prove transport integrity,
not producer-job provenance.

No hosted attack was run. The official cross-job overwrite contract plus the
pinned client request sequence is sufficient source proof; a live token probe
would be unsafe and unnecessary.

## Required disposition/options

1. **Preferred: whole transitive job isolation.** Put the producer artifact in
   a base-owned workflow/run containing only the fixed candidate producer (and
   no PR-controlled ordinary jobs). Keep policy acquisition separate and
   acquire only that isolated run. The PR source remains data in the fixed
   builder; the only process with the runtime artifact token is the fixed
   uploader action. A source census of one YAML uploader inside a mixed run is
   not equivalent.

2. **Alternative: trusted artifact-ID binding in one workflow.** Keep producer
   and consumer in a single workflow graph and pass
   `${{ needs.candidate_producer.outputs.artifact_id }}` from the pinned upload
   action to the consumer. Download by that exact ID, require the expected
   producer job's output/digest, and fail if the ID was deleted/replaced. Do
   not infer this from artifact name, `databaseId`, timestamps, or the public
   REST object. This requires an actual trusted cross-workflow handoff if
   policy remains a separate `workflow_run`; jobs REST does not expose job
   outputs, so a candidate-written manifest is not enough.

3. **Narrow temporal mitigation only:** make every ordinary PR job depend on
   producer completion and retain the `updated_at <= producer.completed_at`
   check. This turns later overwrite into denial, but it changes the intended
   independent producer/test graph and must be treated as a design decision,
   not provenance proof. It is weaker than isolated run or exact artifact-ID
   binding.

Do not “fix” this by adding `overwrite: true` to the trusted producer or by
accepting the newest static artifact. Neither addresses the cross-job token
capability.

## Regression requirements for owner

Use a local protocol fixture or a trusted mock results service, never live
credentials, with two distinct JWT `scp` job IDs and one run ID. It must model
the pinned client sequence and assert:

* ordinary job A creates `C`; producer job B's default upload collides and the
  gate returns denial;
* producer B creates `C`; ordinary A deletes/overwrites it through the
  supported client; policy sees a different artifact ID and must reject unless
  exact producer artifact ID binding is present;
* overwrite after producer completion is denied by the timestamp guard, while
  overwrite before completion is not accepted as producer provenance;
* changing only service digest, raw ZIP bytes, artifact ID, backend job ID, or
  upload-step success independently fails;
* a direct Twirp client / child daemon path is rejected by whole-run isolation
  even when no `actions/upload-artifact` YAML step names the candidate.

These tests are architectural regressions, not proof that an arbitrary
workflow source keyword scan is a security boundary.

