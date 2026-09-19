# G1 bootstrap artifact feasibility probe

Date: 2026-09-20 (API records created 2026-09-19)
Scope: read-only GitHub REST metadata and pinned workflow/action source. No artifact archive was downloaded, opened, hashed locally, or executed. No dispatch, release, or repository write.

## Result

GitHub's artifact service exposes a SHA-256 `digest`, but the artifact object binds only to a `workflow_run`. The object has no `job_id`, numeric check-run ID, producer step ID, run attempt, uploader identity, or attestation ID. The run and jobs APIs are separately queryable: a run has `artifacts_url`, `jobs_url`, and `run_attempt`; a job has `run_id`, `run_attempt`, `head_sha`, `id`, and `check_run_url`; neither side carries the other object's artifact/job ID.

Therefore API metadata proves `artifact -> repository/run/head branch/head SHA`; it does **not** prove `artifact -> exact job/step`. Correlating an artifact name with the only source job whose generated config enables candidate publication is useful evidence, not a service-level binding. Do not invent an exact producer job from the artifact object.

## Known mainline runtime producer

Run `35463512640` (`Runtime products · push · main`, run `68`, workflow `359790572`, event `push`, status/conclusion `completed/success`, attempt `1`, head `e713841bdb9c33d853b7a9af88ceac924af1b3b6`, branch `main`, check suite `96034876638`). Jobs API returned these successful jobs, all attempt `1` and the same head SHA:

| Job | Numeric job/check-run ID |
| --- | --- |
| Resolve runtime closure | `105951479673` |
| Build runtime (Linux-ARM64) | `105951498865` |
| Build runtime (Linux-X64) | `105951498907` |
| Build runtime (macOS-ARM64) | `105951498910` |
| Publish runtime products | `105951823951` |

Artifact list (`total_count=3`, all `expired=false`, each `workflow_run.id=35463512640`, `head_sha=e713841bdb9c33d853b7a9af88ceac924af1b3b6`):

| Artifact ID | Name | Size | Service digest | Expires |
| --- | --- | ---: | --- | --- |
| `10590941303` | `runtime-Linux-ARM64` | 3,800,831 | `sha256:4ec595125cbd1424ff27100cc9b3d70aee0d1778aedb9b7dc0bcae5d56558836` | 2026-09-26T19:12:18Z |
| `10590263020` | `runtime-macOS-ARM64` | 3,770,255 | `sha256:bc56189ee2408f99eb44606f4fa052f03004a01ad74d70175e2c5805fe5396c0` | 2026-09-26T19:12:53Z |
| `10590262997` | `runtime-Linux-X64` | 4,149,523 | `sha256:41afc23407f89cf2db7f826d0454cea2e0c7d5865b251776d60d437ccf644f37` | 2026-09-26T19:12:30Z |

The per-artifact endpoint exposes `archive_download_url`, but it was not fetched. Artifact `workflow_run` contained only `id`, `repository_id=1255367013`, `head_repository_id=1255367013`, `head_branch`, and `head_sha`; no attempt/job field.

The producer source at the recorded head (`.github/workflows/ci-runtime-products.yml`) uses pinned `actions/upload-artifact` v7.0.1, omits `archive`, uploads each binary plus its `.sha256` file, and retains 7 days. The publish job downloads the artifacts and verifies the shipped binary against the `.sha256` files before release assembly. Those inner binary digests are different evidence from the GitHub service artifact digest.

## Recent PR producer artifact

Run `35467040858` (`CI / PR · pull_request · 954/merge`, run `1060`, workflow `347892207`, event `pull_request`, status/conclusion `completed/success`, attempt `1`, head `85edab7bbbc72c8c9b81c25cc0f742a81dbf004a`, branch `codex/package-release-contract`, check suite `96043626948`). Artifact list (`total_count=2`, both `expired=false`):

| Artifact ID | Name | Size | Service digest | Expires |
| --- | --- | ---: | --- | --- |
| `10591573147` | `velnor-workflow-candidate-0c304c147d66fcdf-Linux-X64` | 41,455,421 | `sha256:0c23c251f98bd22720cba6fa03e1addc2f623ba59b7a733854e036fed0772c8e` | 2026-09-20T20:26:36Z |
| `10591273097` | `velnor-workflow-runtime-fdeed261bd2247a38db6922a7726cd45d3d6f31e-Linux-X64` | 8,235,045 | `sha256:e1571870a127d5ca6aa703e236e17a2dc7d9d4b244d0f0e7ef78b24fd9a715c7` | 2026-09-26T20:22:16Z |

The candidate-producing job observed in the jobs API is `105961562345`, `Rust · velnor-workflow · github-hosted — rust-velnor-workflow / GitHub · hosted`, successful, same run/attempt/head SHA; its `Prepare candidate generator product` and `Publish candidate generator product` steps both succeeded. The generated PR workflow has `candidate_publish: true` only for the github-hosted `rust-velnor-workflow` unit, and `ci-unit-rust.yml` uses the pinned upload action. This is source/step correlation only: the artifact response itself still has no field proving job `105961562345` produced ID `10591573147`.

## Digest semantics

Official `actions/upload-artifact` documentation says `artifact-digest` is the SHA-256 digest for the uploaded artifact, and that the displayed size is the ZIP created during upload. Both producer workflows omit `archive`, whose action default is `true`; current uploads are therefore archived artifacts. Treat REST `digest` as the service/archive transport digest, not the SHA-256 of an inner binary, manifest, or extracted member. No local content digest was computed because the archive was intentionally not downloaded. The runtime workflow's `.sha256`/release manifest and the candidate's `candidate-manifest.json` carry inner binary digests; they are separate fields and are not present in REST artifact metadata.

Official references:

- https://docs.github.com/en/rest/actions/artifacts
- https://docs.github.com/en/rest/actions/workflow-runs
- https://docs.github.com/en/rest/actions/jobs
- https://github.com/actions/upload-artifact#outputs
- https://github.com/actions/upload-artifact/blob/main/action.yml

## Bootstrap implication

An artifact consumer can safely require: exact run ID, head SHA/branch, attempt from the enclosing run, artifact ID/name, `expired=false`, service digest, retention timestamp, and a separately authenticated producer manifest/attestation for the payload. It cannot safely require an exact producing job using current artifact REST metadata alone. Future producer design should record the upload step's `artifact-id` and `artifact-digest` together with run ID, run attempt, stable job key, and a producer-signed/attested manifest (or otherwise expose a verifiable job-to-artifact record). A name/digest match or same-run correlation alone is not that proof.
