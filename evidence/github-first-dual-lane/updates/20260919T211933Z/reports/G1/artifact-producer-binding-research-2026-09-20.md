# Artifact -> producer job/attempt binding research

Date: 2026-09-20. Read-only research; no source edits, dispatches, releases, artifact downloads, or execution.

## Bottom line

The current GitHub artifact REST object cannot prove its producing job or attempt. It exposes a service `id`, `digest`, name, and `workflow_run` with run/head metadata, but no job ID or attempt. A durable binding is possible with existing GitHub mechanisms, but it requires a trusted fixed uploader step to capture the service outputs and sign a second attestation whose subject digest is the service artifact digest.

Use two attestations when useful:

1. Standard SLSA provenance for the actual binary/package subject.
2. A custom, signed `actions/attest` predicate for the **uploaded GitHub artifact**. Its subject digest is `sha256:<steps.upload.outputs.artifact-digest>`, and its predicate records the service `artifact-id`, expected name, `github.run_id`, `github.run_attempt`, `job.check_run_id`, `github.job`, `job.workflow_ref`, `job.workflow_sha`, and matrix key. Verify the signature/signer workflow, then cross-check every value against REST.

This is not a candidate manifest and not a `needs` string. The signing job is protected source, the artifact ID/digest come from the pinned upload action, and the verifier checks the GitHub APIs.

## Official mechanism facts

### Attestation provenance claims

`@actions/attest`'s current toolkit source requires OIDC claims `run_id`, `run_attempt`, `job_workflow_ref`, `workflow_ref`, `repository`, `event_name`, `sha`, and runner/repository IDs. Its generated SLSA predicate contains:

- subject name/digest;
- `buildDefinition.externalParameters.workflow` (repository, ref, path);
- `buildDefinition.internalParameters.github` (event, repository IDs, runner environment);
- resolved source commit;
- `runDetails.builder.id` from `job_workflow_ref`;
- `runDetails.metadata.invocationId` as `/actions/runs/<run_id>/attempts/<run_attempt>`.

Standard provenance therefore proves workflow/source/run/attempt, but not numeric job ID or artifact service ID. `job_workflow_ref` is workflow identity, not a job instance.

GitHub's `job` context supplies the missing runtime fields: `job.check_run_id` (number), `job.workflow_ref`, `job.workflow_sha`, `job.workflow_repository`, and `job.workflow_file_path`. `github.job` is the static job key. These values are available inside job steps. The REST workflow-jobs object supplies numeric `id`, `run_id`, `run_attempt`, `head_sha`, `name`, `steps`, and `check_run_url`; query the exact run attempt's jobs endpoint or `GET /actions/jobs/{job_id}` to validate the signed claims.

### Artifact service outputs

Pinned `actions/upload-artifact` exposes `artifact-id` and `artifact-digest` outputs. The digest is the uploaded artifact/archive digest; current Velnor uploads omit `archive`, so the action default is ZIP. Capture outputs using a step ID. Do not resolve by name. `overwrite` must remain false (default); an overwrite creates a new ID and removes the old artifact. v4 artifacts are immutable within a run, but same names can occur across runs and reruns.

After upload, the trusted job should query the artifact ID and assert the returned REST `digest`, `name`, `expired`, and `workflow_run.id` before attesting. Canonicalize the action output to `sha256:<hex>` and require equality with REST. No payload archive download is needed for this identity check.

### Runner artifact-subject file

`$GITHUB_ARTIFACTS_LIST` is a new, feature-flagged, job-scoped runner subject list. It contains names/digests/kinds of declared local/OCI subjects, not GitHub `upload-artifact` service IDs or attempts. It can feed standard attestation for a local binary, but does not bind the Actions artifact archive. It is not a substitute for the upload output + signed binding predicate.

### Linked-artifact storage records

The `artifact-metadata: write` storage-record mechanism is for registry OCI subjects when `push-to-registry: true`; it does not attach a job ID to `actions/upload-artifact` objects. Do not treat it as an Actions artifact binding.

## Velnor source contract observed

At runtime producer `e713841bdb9c33d853b7a9af88ceac924af1b3b6`, `ci-runtime-products.yml` has a fixed three-cell build matrix. Each matrix job runs `Attest runtime asset` **before** `Upload runtime asset`; upload uses pinned `actions/upload-artifact@043fb46d...` with names `runtime-${{ matrix.os }}-${{ matrix.arch }}`, binary plus `.sha256`, and retention 7. There is one uploader per matrix cell, not one singular uploader. The publish job only downloads and assembles/releases. The existing binary attestation thus does not cover the GitHub artifact ID/archive digest.

At PR head `85edab7bbbc72c8c9b81c25cc0f742a81dbf004a`, generated `ci-pr.yml` sets `candidate_publish: true` only for the github-hosted `rust-velnor-workflow` unit. `ci-unit-rust.yml` uploads the candidate directory with pinned upload-artifact, but has no upload step ID, service-output capture, or attestation. The PR workflow/config is candidate-controlled and cannot be the trust anchor for admission.

## Six acceptance dimensions

| Dimension | Current REST/source state | Enforceable mechanism | Pass? |
| --- | --- | --- | --- |
| Exact artifact object | REST `id` + `digest` available | Capture upload outputs; REST get-by-ID equality | Yes, after source change |
| Source/workflow identity | Standard attestation has workflow/ref/commit/event; current runtime attests binary | Verify signed attestation signer workflow/source SHA | Yes for trusted producer; no for candidate PR |
| Run + attempt | Standard attestation has run ID/attempt; artifact object omits attempt | Signed predicate + enclosing run API + exact-attempt jobs API | Yes |
| Exact producer job | REST artifact has no job field; standard provenance has no numeric job | Signed custom predicate with `job.check_run_id`/`github.job`, REST job equality | Yes only from protected fixed uploader |
| Payload/transport semantics | Current upload digest is archive/service digest; binary attestation differs | Attest service digest separately; retain binary attestation separately | Yes if explicit; current state no |
| Replay/collision resistance | Names collide across runs/reruns; sibling uploads possible | Artifact ID + digest + run/attempt + job + workflow SHA; one uploader; no overwrite/name lookup | Yes only after contract |

## Threat model

| Threat | Weak check defeated | Required rejection/check |
| --- | --- | --- |
| Sibling job uploads another artifact | Name, run ID, or overall success | Exact signed `job.check_run_id`/job key; REST job `run_id`, attempt, SHA, name, steps; only fixed uploader has binding permission |
| Sibling uses same name with overwrite | Name-only lookup | `overwrite:false`; exact artifact ID + REST digest; reject duplicate IDs/names; no name resolution |
| Prior run or prior attempt replay | Branch/SHA/name; run ID alone (run ID survives rerun) | Signed and API-checked `run_id` + `run_attempt`; exact-attempt jobs endpoint; workflow SHA/source-ref |
| Same digest appears in another run | Digest-only lookup | Subject digest **and** artifact ID/run/attempt/job; require expected signer workflow and event/branch |
| PR modifies candidate manifest or workflow mapping | Manifest fields, `needs` output, candidate name | Candidate artifacts never trust anchor; signer workflow must be protected fixed main/reusable workflow at allowlisted SHA; verify REST service outputs |
| Unrelated platform/matrix cell | Artifact prefix or display name | Fixed matrix contract plus per-cell job ID in signed predicate; exact expected artifact ID/digest |
| Future workflow drift | Moving `main`/workflow display name | Verify attestation certificate `buildConfigDigest`/`job.workflow_sha`, workflow path, signer workflow, event/ref |

## Recommendation

Bootstrap owner should define one admitted artifact namespace and one protected uploader contract. Prefer a final trusted publisher job that uploads each fully verified product once, with an immutable static name per platform, `overwrite:false`, `if-no-files-found:error`, and a pinned action. If intermediate matrix artifacts remain, treat them as staging and bind each separately; never let a consumer select them by name.

The uploader step must have an ID, capture `artifact-id`/`artifact-digest`, query the artifact by ID, validate the enclosing run and expected name/digest, and create a custom signed binding attestation after upload. Include `run_id`, `run_attempt`, `job.check_run_id`, `github.job`, `job.workflow_*`, expected source SHA/event/ref, matrix key, artifact ID/name/digest. Consumer validation must verify both the standard/custom attestation signer and REST run/job/artifact records. Keep current raw binary attestation for payload provenance; it is not a replacement for the service binding.

Do not accept any of: candidate-manifest claims, `needs` output without trusted-source and REST checks, workflow/job display name alone, run ID without attempt, digest without artifact ID, or `artifact-metadata` storage records for an Actions artifact.

Strict interpretation: standard GitHub provenance plus a protected one-uploader workflow proves the uploader **role** (fixed workflow path/SHA and fixed job key), but standard provenance alone cannot prove the numeric job instance. If the acceptance gate requires that number, use the signed custom binding predicate and REST job cross-check. If custom predicates are disallowed, state the weaker role-level guarantee explicitly; do not relabel it as exact artifact-to-job proof.

Official references:

- https://github.com/actions/toolkit/blob/main/packages/attest/src/provenance.ts
- https://github.com/actions/toolkit/blob/main/packages/attest/src/oidc.ts
- https://docs.github.com/en/actions/reference/workflows-and-actions/contexts
- https://github.com/actions/upload-artifact/blob/main/action.yml
- https://github.com/actions/upload-artifact#outputs
- https://docs.github.com/en/rest/actions/artifacts
- https://docs.github.com/en/rest/actions/workflow-jobs
- https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations
