# Independent review: cross-job candidate-artifact overwrite race

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

Disposition: **CHANGES REQUIRED.** The cited finding is a real provenance
failure, not a source-scanner false positive. No upload, token probe, workflow
dispatch, hosted execution, or repository/trust mutation was performed.

## Review tuple and scope

* Finding under review:
  `bootstrap-artifact-cross-job-3ed0023b.md`, SHA-256
  `fe4368a2394e696481a125e73d3b933ae29600b7412628ddbc37ede3c06abbc0`.
* Source revision:
  `3ed0023b038335d7b22dfa2758457e3808f777ee`, tree
  `45be601efb57e8d9da424a07e9115beee93a1564`.
* Related API feasibility record:
  `bootstrap-artifact-feasibility-2026-09-20.md`, SHA-256
  `3316446c8ca5715c1d75b198a6884a26f8fdd769fbcd0a036db9a8a55685070e`.
* `dual-lane-source-integration` currently points at `f6cb27c4606103d0c879bbc60a6a060911ce7b91`, a divergent source snapshot. The line citations below are deliberately from `git show 3ed0023b...`, not from the current worktree.

## Independent verdict

The `static-single-uploader-v1` contract proves only that the reviewed YAML
contains one literal candidate `upload-artifact` step. It does not prove that
the artifact object selected later was created by `candidate_producer`.

At `3ed0023b`:

* `.github/workflows/ci-pr.yml:108-113` puts `candidate_producer` in the same
  `pull_request` run as ordinary PR jobs. It has no `needs` edge to isolate the
  run and only sets `permissions: {}` for the ordinary GitHub permission set.
* The fixed producer uploads the static name at lines `226-233`, with the
  pinned action's default `overwrite: false`; its build shell's empty-token
  checks at lines `135-138` do not constrain a different job.
* `ir.rs:3036-3040` exports `artifact_id` and `artifact_digest` as **job
  outputs**, but `ci-policy.yml:4-27` is a separate `pull_request_target`
  workflow. The policy acquisition code has no `needs` path to those outputs.
* `s2/mod.rs:5527-5546` selects a successful job by name and then selects an
  artifact by static name, run, size, expiry, and timestamps. It checks the
  upload step name/status but never compares the REST artifact ID to a trusted
  producer output. The artifact REST object has no creator-job field.
* The YAML/source census at `s2/mod.rs:5027-5042` and `5277-5298` is not a
  semantic deny rule for a PR-controlled Node action, child process, daemon,
  or direct Actions results-protocol client.

GitHub's pinned action contract confirms the race: `overwrite: false` rejects a
same-name collision, while `overwrite: true` deletes the old object before
creating a new object; successful upload returns a unique `artifact-id` and
`artifact-digest` ([pinned action manifest](https://raw.githubusercontent.com/actions/upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml),
[pinned implementation](https://raw.githubusercontent.com/actions/upload-artifact/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/src/upload/upload-artifact.ts#L9-L16)).
The official example explicitly uses a second job with `overwrite: true` and
states that the replacement receives a new ID ([official overwrite example](https://github.com/actions/upload-artifact#overwriting-an-artifact)).
The runner injects `ACTIONS_RUNTIME_URL` and `ACTIONS_RUNTIME_TOKEN` into each
Node action process independently of the shell's environment assertions
([runner source](https://github.com/actions/runner/blob/main/src/Runner.Worker/Handlers/NodeScriptActionHandler.cs#L677-L706)).

## Concrete false-positive schedule

Let `P` be `candidate_producer`, `O` an ordinary PR job, and `C` the static
candidate name:

1. `P` finalizes `C` (ID `p`) successfully.
2. Before `P` reaches job completion, `O` uses its own Actions runtime token
   and the supported results client with `overwrite: true`. The service
   deletes `p` and creates `C` as ID `o`.
3. `P` completes successfully. The current policy lists one `C`, sees
   `updated_at <= P.completed_at`, downloads ID `o`, and verifies the wrong
   archive's own service/raw ZIP digest and candidate manifest.

The timestamp predicate is therefore a race heuristic, not provenance. It
allows the replacement window before `P.completed_at` (and timestamp equality
or precision cannot establish causal ordering). An attacker can make the
replacement manifest claim the expected head/closure; those claims and the
transport digest do not identify `P`. A pre-upload collision is denial-only;
the before-completion replacement is the security-relevant false-positive.

The REST contract exposes artifact ID, name, digest, expiry, and enclosing
workflow run, but not producer job, upload step, or job output ([artifact REST schema](https://docs.github.com/en/rest/actions/artifacts#list-workflow-run-artifacts)).
The jobs API therefore cannot recover the `artifact-id` output from
`candidate_upload`; listing by name or correlating timestamps cannot repair the
missing edge.

## Repair assessment

### Exact artifact-ID binding

This is sound only when the ID comes through a trusted transport:

1. A fixed consumer in the **same workflow run** has
   `needs: [candidate_producer]` and consumes both producer outputs.
2. It calls `GET /repos/{owner}/{repo}/actions/artifacts/{artifact_id}` and
   downloads by that ID, requiring exact run ID/attempt, repository/head SHA,
   name, non-expiry, service digest, and fixed producer/job identity.
3. A deleted ID, digest mismatch, wrong run, or replacement is denial; the
   consumer never falls back to static-name lookup.

GitHub documents this exact job-output transport (`job1.outputs` to
`needs.job1.outputs`) and artifact-ID download. The current `ci-policy` is a
separate `pull_request_target` run, so its shell cannot consume
`needs.candidate_producer.outputs.*`. A candidate-written manifest, static
handoff artifact, job log convention, or REST name lookup is not an equivalent
transport. Moving the trusted policy verifier into the same run (for example a
base-pinned reusable workflow with an externally enforced caller contract) is a
trust/source-contract change and must be explicit; it must not add ordinary CI
`needs` edges or let PR YAML define policy semantics.

The numeric ID alone also does not prove the producing job: the verifier must
bind it to a fixed producer output and recheck REST run/head/digest data. If
exact numeric job identity is required, the signed-binding route below or an
equivalent protected record is still needed.

### Signed producer binding

This can preserve the independent ordinary `ci-pr` graph **only** with an
explicit trusted signer path. After upload, a base-owned fixed signer should
canonicalize and sign a predicate containing at least:

`artifact_id`, service `artifact_digest`, run ID and attempt, producer job ID
and check-run ID, stable job key/name, workflow path/SHA, artifact name, PR
head SHA/tree/closure, platform/features/profile, binary digest, and upload
step ID.

The verifier must authenticate the signer workflow/ref/SHA, GET the exact
artifact ID, and compare every signed field with run/jobs/artifact REST data.
If `O` deletes/replaces the producer artifact, the exact signed ID is gone and
verification fails; a new `O` ID is never accepted. Standard binary
provenance or a candidate manifest does not bind the GitHub artifact archive
or numeric producer job.

This is **not** a local permission-free fix. Adding OIDC/attestation or an
external signing key, signer workflow, predicate schema, key rotation,
revocation, and recovery creates a new trust authority. A separate signer job in
the same PR run must also be fixed/base-owned and receive the producer output
by `needs`; a signer step inside the producer still needs the same explicit
signing authority. A signer in a separate workflow has the unsupported
cross-run output-transport problem. No signer identity or permission is
approved here.

### Whole transitive isolation

The strongest repair is a separate base-owned workflow/run containing only the
fixed candidate producer and its pinned uploader. No ordinary PR job in that
run can obtain a sibling runtime token to delete/recreate the candidate name.
The policy may then select the exact producer run and artifact, still by run
identity and numeric artifact ID, never by a floating name or fallback.

This preserves the ordinary CI graph and requires no new signing key, but it is
not the current `ci-pr` same-run contract: a reusable workflow called from
`ci-pr` does **not** create a separate artifact namespace/run. A new
base-owned trigger/workflow or an operator/API dispatch is a trust/authority
decision. If the requirement is literally to preserve the current `ci-pr`
workflow path, isolation alone cannot satisfy it; choose trusted same-run
transport or signed binding instead. Do not silently relabel a new workflow
trigger as the old `ci-pr` path.

### Temporal/overwrite/name mitigations

`overwrite: true`, newest-artifact selection, a dynamic/static name, or a
stronger `updated_at` predicate does not bind creator identity. Making every
ordinary job depend on `candidate_producer` (or serializing all PR jobs) is a
behavioral graph change and violates the stated no-ordinary-`needs` boundary;
it is at best a denial mitigation, not an accepted provenance proof. A source
scanner that rejects literal extra uploaders cannot constrain direct protocol
use.

## Safe local work versus explicit authority

Safe to implement in source/tests after a mechanism is chosen (no trust
mutation):

* retain the accepted independent candidate build and no ordinary `needs`;
* add typed fields for exact artifact ID/digest/run attempt/job identity and
  make the verifier download by exact ID;
* remove static-name/timestamp acceptance and fail closed when no trusted ID
  transport is present;
* add a local results-protocol fixture with two distinct job JWT IDs testing
  pre-upload collision, producer-then-overwrite-before-completion,
  post-completion overwrite, deleted-ID, digest, run/attempt, and job-identity
  cases; and
* keep candidate manifest/closure self-reports descriptive only and preserve
  the no-PR-semantic-authority rule.

Requires a separate user-approved trust/authority amendment before source or
workflow adoption:

* moving policy into the same trusted `ci-pr` run or adding a base-pinned
  reusable workflow/caller contract to carry `needs` outputs;
* introducing OIDC/attestation or an external signing root for the custom
  artifact-to-job predicate; or
* creating a separate base-owned producer workflow/run, changing trigger/event
  semantics, or granting an operator/App dispatch capability.

## Exact approval blockers

The next plan must choose one executable mechanism and freeze these fields:

1. **Same-run ID:** trusted verifier workflow/job path and source SHA, exact
   `needs` edge, caller-integrity proof, no ordinary CI dependency, API
   readback fields, and policy check ownership.
2. **Signed binding:** signer workflow/job/ref/SHA, OIDC/key authority,
   canonical predicate, subject digest (service ZIP versus inner binary), REST
   cross-check, key lifecycle, and recovery/revocation.
3. **Isolated run:** exact base-owned workflow path/trigger, source admission,
   run-selection filters, producer-only job graph, artifact/run binding, and
   how the accepted `ci-pr` status path remains represented without claiming
   the isolated run is the old one.

Until one is explicitly approved and implemented, the current candidate path
has a false-positive provenance race. No old App plan, candidate semantic
authority, skipped/failed result, or static artifact correlation closes it.
