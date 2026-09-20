# Velnor authority-transition plan v8 — proposal only

Status: `successor_draft_external_blocked`. This is a new immutable successor to v7. It is a design and acceptance contract, not authorization. No source, GitHub ruleset, App, release, merge, or runner mutation was performed. The freeze choice remains unselected.

The plan has one transition: Tree A admits and merges the generator/source change without a permanent B pin; the resulting `main` publishes the typed Linux validator through the real GitHub workflow; only after live publication and binding verification does Tree B add the permanent literal pin. The temporary admission authority and permanent B verifier are distinct typed identities.

## Revision-bound facts

All facts below bind to current `main` `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`, observed `2026-09-20T03:12:50Z`. The source change is PR 969, head `2445c647dafa43acbc5285c873d3b04174ff75c6`. Generator revision is `54`; target generator revision is still null. The old policy/runtime pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` is not a target and cannot bootstrap this transition.

Current evidence is outside the source revision:

- `G1/bootstrap-transition/current-main-checkpoint-89f82dd8-2026-09-20.json`, SHA-256 `77e1ecc848e0a3ad73cdfd853b788054e8cda02e0aae38d076b7c59b0da130bd`.
- `G1/bootstrap-transition/current-main-closure-inputs-89f82dd8-2026-09-20.json`, SHA-256 `1b890ef899c8a02146e7d6ab1d664fcb92213924e8c7a722f8f57a2bfb55c072`.
- Runtime run `35484968618`: macOS job `106009574341` used forbidden `macos-26`; not accepted. Linux jobs were `106009574363` and `106009574375`; publish job `106009876473`.
- Preview run `35484968732`, first job `106009550596`: generated-tree drift; not accepted.
- Main CI run `35484968706`: Policy job `106009552370` failed; required child `106009339325` and Control/Required `106011381740` did not establish a pass.

The current closure snapshot records 15 workflow blobs, config/state, and renderer inputs. It does not prove a semantic B publisher source/output, fixed-point generation, or an accepted native matrix. The strict D19 feasibility report found no clean published runtime candidate; computed tag `2246fdb3f05f3b21` is unpublished and invalid for admission.

V7 is preserved unchanged. Its corrected independent report SHA is `26df89a7e4cbcd1b01f38432163e1b449f17bc5121d3c59132475a2a9fa32810`, results SHA `449730ce54a42dd355bb3c4c8f076a9153b2de0a3d384eac5b63fa60ae4119d5`. The independent canonical DAG report that drives this revision is `G1/bootstrap-transition/v7-independent-contract-audit-2026-09-20/canonical-dag/REPORT.md`, SHA `fd30b668c2b756287045f885288afd45c530a6d448875731b042fc4fdb2f7d16`; JSON SHA `dacdd33b97beecb67ba49d812d65e14a00ba0aa7b6d659069f61156e8ed0ba38`.

## Deterministic transition

### Tree A

Tree A contains no `[policy.validator]`, permanent literal pin, candidate manifest, `--pin-build`, old setup/runtime alias, old macOS fallback, or compatibility route. It retains every ordinary workload and the full native `Linux-X64`, `Linux-ARM64`, and `xcode-27` obligations. The generator must emit all required jobs from one reviewed immutable target revision.

The only temporary required policy context is `Policy-bootstrap-A`, emitted by one external owner-controlled verifier. It receives the exact PR number, head, locked base, synthetic merge/tree, full pre-merge check census, and full source/config/generated closure. It may read and report; it may not merge, mutate refs/rulesets, publish, create releases, write packages, use secrets, or bypass a check. This is not the permanent Policy check and does not claim a GitHub Actions run, artifact, release, or validator product.

Tree A's ruleset, if and only if an independently capable provider can enforce it, is exactly:

```text
DCO                + exact DCO integration_id
ci-required        + exact ci-required integration_id
Policy-bootstrap-A + exact bootstrap-A integration_id
```

The old `Policy` context is not simultaneously required here. Requiring it would make Tree A impossible because the old `0dc` parser rejects the new transition before generation. Removing it is a temporary explicit trust amendment, not a skipped workload; it is blocked until provider freeze, source admission, and all ordinary checks are proven.

### Main B

After a normal protected PR merge, the exact resulting `main` SHA is reread. The generated caller `.github/workflows/ci-main.yml` invokes exactly one reusable workflow:

```text
ci-main push refs/heads/main
  -> policy-validator-B (uses ./.github/workflows/ci-policy-validator-products.yml)
  -> Policy consumes needs.policy-validator-B.outputs directly
```

The publisher has `workflow_call` only. It has no standalone push, tag, pull-request, or publishing dispatch trigger. The caller job has no shell steps and declares no outputs. The called workflow's `verify-B` outputs are the only transport into `Policy`; no caller-job output re-export is invented.

Main B builds and publishes a separately typed product: namespace `velnor.policy-validator`, product `velnor-policy-validator`, platform `ubuntu-24.04`, architecture `x86_64`. It records source SHA/tree, closure digest, immutable release tag, raw service ZIP/inner payload/binary sizes and digests, release asset, both attestation predicates, and all live producer IDs. The first Main B run must produce the real GitHub workflow/run/job/check/artifact/release/asset records required by the permanent schema. No pre-main artifact or external admission record can substitute for those IDs.

The Main B required policy context is `Policy-bootstrap-B`, one downstream verifier/provider bound to the exact resulting SHA and live B record. It is not the Tree A App and it is not the old Policy. After the live B proof, the ruleset can move to `DCO`, `ci-required`, and `Policy-bootstrap-B`; this handoff is a separate full ruleset operation and snapshot.

### Tree B and permanent state

Only after Main B's live API proof does a new normal PR add `.github/ci/validator-pin-adoption.json` and `.github/ci/validator-pin-adoption.sha256`. The JSON binds the exact base Policy workflow blob, verifier key/revision/integration, target config/source/template/static-file mapping, Main B source/release/actions artifact/release asset/record/binding/release-attestation digests, and provider identity. The sidecar hashes canonical UTF-8 LF JSON bytes and has no self-digest field.

After normal Tree B merge and resulting-main verification, permanent state is `DCO`, `ci-required`, `Policy`; the permanent B publisher, trust record, and exact base Policy verifier remain. Both temporary A and transition-B authorities, coordinator permissions, handoff state, and watchdog permissions are removed and deletion is verified by recursive closure scan. No old checker, candidate, alias, or fallback is retained.

## Publisher job graph and least privilege

The graph is a single acyclic chain. Every edge transports named typed outputs or immutable artifact/API IDs; no workspace crosses a job.

```text
build-linux-x64
  -> artifact-verify
  -> reserve-release
  -> attest-binding
  -> publish
  -> attest-release
  -> record-upload
  -> verify-B
```

| Job | Required inputs | Produced outputs | Permission boundary |
|---|---|---|---|
| `build-linux-x64` | caller source/ref/tree/closure | Actions artifact ID/name/digest, ZIP/payload/binary bytes, sizes, architecture, upload step, caller run/attempt | `contents:read`, `actions:read`; no write, signing, release, package, or checks write |
| `artifact-verify` | all build outputs | verified raw digests plus producer run/attempt/job/check, workflow path/ref/SHA/blob | `contents:read`, `actions:read`, `attestations:read` |
| `reserve-release` | verified build and producer outputs | nonzero draft release ID, immutable tag, target SHA | `contents:write` only |
| `attest-binding` | S0–S3 values, no release asset | binding core/record/attestation and final manifest artifact IDs/digests | `contents:read`, `actions:read`, `id-token:write`, `attestations:write`; no source build or release write |
| `publish` | binding outputs and S0–S3 | published release ID/tag/target, release asset ID/raw digest | `contents:write` only; no token or attestation write |
| `attest-release` | published release/asset and binding outputs | release attestation ID/digest and release predicate/trust fields | read plus `id-token:write`, `attestations:write` |
| `record-upload` | all final values except record transport | record Actions artifact ID/name/digest and upload run/attempt/job/check | `actions:write` only; no source, release, or signing permissions |
| `verify-B` | complete record plus live APIs | strict B result, provider handoff, called identity | read-only contents/actions/attestations/checks |

The caller permission union is explicit. Build contents/actions permissions cannot mint attestations. Only the two attestation jobs receive OIDC/attestation write. Only `publish` receives contents write. The external verifier uses its own installation credential with `metadata:read`, `contents:read`, `actions:read`, `attestations:read`, and checks read/write. `GITHUB_TOKEN` cannot impersonate the external provider.

## Machine field availability and preimage contract

The machine JSON contains the executable S0–S7 DAG, field-level producer/source objects, edge field lists, and canonical leaf-path mapping. The stages are:

| Stage | Producer | Fields |
|---|---|---|
| S0 | `ci-main` caller | repository, source ref/SHA/tree, closure, caller run/attempt, caller workflow path/ref/SHA/blob |
| S1 | `build-linux-x64` | Actions artifact identity, service ZIP/payload/binary bytes, sizes, architecture, upload step |
| S2 | `artifact-verify` | producer run/attempt/job/check, workflow path/ref/SHA/blob, verified raw digests |
| S3 | `reserve-release` | release ID, immutable tag, release target SHA |
| S4 | `attest-binding` | binding core, binding record/attestation, final manifest artifact/digest |
| S5 | `publish` | release asset ID/raw digest, published release ID/tag/target |
| S6 | `attest-release` | release predicate/subject, release attestation, signer/OIDC/certificate/policy fields |
| S7 | `record-upload`, external provider, `verify-B` | record artifact/upload IDs, provider check identity, called workflow identity, post-run terminal census |

The canonical binding schema is `G1/bootstrap-transition/authority-contract-separation-2026-09-20/permanent-b-product-binding.schema.json`, SHA `427cbae0d9a238ae5e22a1afac2c8f840e4a8ad136983ac28a863651e533aed7`. The binding predicate schema is `G1/bootstrap-transition/validator-binding-audit-2026-09-20/binding-predicate.schema.json`, SHA `4bcd53cdd4e1900d4edf240f46da4c14ab570fc7be09f37484a9f56402da3b06`. The strict record/provenance schema is `G1/bootstrap-transition/authority-contract-separation-2026-09-20/policy-validator-b-record-provenance.v1.schema.json`, SHA `3f2d72acda359e8ca0a31a40a6e4aa417bd49552acec050583a92601d0c851ef`.

The provenance record is `policy-validator-b-record.zip`, uploaded by `record-upload`. It contains strict `binding.json` and `provenance.json`; the raw Actions artifact REST response supplies record artifact ID/name/digest and upload run/attempt/job/check. The record's own IDs and digests are outside its preimage. External B reads the raw ZIP, checks both schema hashes, and rejects a missing/duplicate/mismatched artifact. An artifact name or run/name correlation alone is never proof.

There are three explicit digest partitions:

1. S4 binding signs only S0–S3 and the raw service ZIP subject. It excludes release assets, release attestation, record transport, provider fields, and its own attestation/manifest IDs/digests.
2. S6 release attestation signs the final manifest plus S0–S5, after the release asset is published and read back. It includes the release manifest subject SHA-256, release predicate type/path, signer repository/workflow/source ref, OIDC issuer, certificate identity/verification, and policy hash. It excludes its own ID/digest and all record/provider fields.
3. S7 record transport is uploaded after S6. Its own artifact/upload IDs are not in any digest preimage. The post-run terminal census is a companion verifier record and excludes `verify-B`, `Policy`, the caller wrapper, and the external B check to prevent a self-cycle.

The machine digest graph is `S0 -> S1 -> S2 -> S3 -> S4 -> S5 -> S6 -> S7`. No node consumes a future node, its own ID/digest, a release asset in S4, or a record transport field in S6. OIDC roles are distinct: the caller uses `workflow_ref`/`workflow_sha`; the called and producer jobs use `job_workflow_ref`/`job_workflow_sha`. Each is separately compared with the exact Contents API workflow blob/commit relation; a SHA-shaped string is not accepted as proof.

## External check and live API binding

The B provider must be a real installed App with concrete app, installation, integration, verifier revision, and credential identities. Its check binds `{context, integration_id, provider_app_id, resulting_main_sha, record_artifact_id, record_artifact_digest, verifier_revision, transaction_id}`. It obtains the record with `GET /actions/artifacts/{record_artifact_id}/zip`, verifies raw bytes and schema, then queries exact run attempts, jobs, check-runs, artifact IDs/digests, release/asset IDs/digests, attestation predicates, OIDC claims, and workflow blobs.

The consumer uses bounded API polling: exact `head_sha`, branch, event, workflow path, run attempt, and terminal conclusion; 15-second interval; 45-minute deadline. `success` is the only terminal success. Missing, duplicate, stale-attempt, neutral, skipped, cancelled, timed-out, failed, or deadline results fail closed. `needs` cannot cross workflows, so this live API handoff is explicit and does not use `workflow_run` or a publishing dispatch.

The immutable release protocol creates a draft first and requires a nonzero release ID. It reserves one immutable tag/target, retries only by querying that exact draft identity, publishes once, reads back release and asset IDs/digests, then signs the release predicate. A release ID or asset ID is never signed before creation and is never replaced by a mutable latest tag.

## Recursive generated closure

Tree A closure must include source and generated bytes for all active consumers, not just a string scan:

```text
.github-gen/velnor-workflow.toml
.github-gen/sources/workflows/ci-main.yml
.github-gen/sources/workflows/ci-policy.yml
.github-gen/sources/workflows/ci-pr.yml
.github-gen/sources/workflows/ci-unit-rust.yml
.github-gen/sources/workflows/ci-unit-bun.yml
.github-gen/sources/workflows/ci-unit-docker.yml
.github-gen/sources/workflows/ci-unit-docs.yml
.github-gen/sources/workflows/ci-unit-opentofu.yml
.github-gen/sources/workflows/maintenance.yml
.github-gen/sources/workflows/preview.yml
.github-gen/sources/workflows/release.yml
.github-gen/sources/workflows/ci-runtime-products.yml
.github-gen/sources/workflows/ci-policy-validator-products.yml
.github-gen/sources/workflows/nightly.yml
.github-gen/sources/workflows/ci-release-package-signer.yml
.github-gen/sources/actions/setup-velnor-workflow/action.yml
.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml
.github-gen/sources/actions/setup-velnor-policy-validator/action.yml
.github/ci/.github-actions-generator-state
.github/actionlint.yaml
.github/workflows/ci-main.yml
.github/workflows/ci-policy.yml
.github/workflows/ci-pr.yml
.github/workflows/ci-unit-rust.yml
.github/workflows/ci-unit-bun.yml
.github/workflows/ci-unit-docker.yml
.github/workflows/ci-unit-docs.yml
.github/workflows/ci-unit-opentofu.yml
.github/workflows/maintenance.yml
.github/workflows/preview.yml
.github/workflows/release.yml
.github/workflows/ci-runtime-products.yml
.github/workflows/ci-policy-validator-products.yml
.github/workflows/nightly.yml
.github/workflows/ci-release-package-signer.yml
.github/actions/setup-velnor-workflow/action.yml
.github/actions/report-velnor-ci-outcomes/action.yml
.github/actions/setup-velnor-policy-validator/action.yml
```

The source B publisher and generated B output are mandatory closure members. Their source/template/static-file mapping, source blob hashes, output blob hashes, and two-render fixed-point digest must be recorded. The generator must remain neutral: S2 may define typed schema/rendering and transport, but may not hardcode `tailrocks`, Velnor product names, fixed consumers, or a literal pin. No old 0dc/candidate/setup/runtime path may remain active after Tree A. Historical fixtures are detached and explicitly labelled; they are not active closure inputs.

The current closure inventory observed 82 dynamic old-renderer hits across 12 active files and 35 reusable calls in each main/PR unit-workflow set. A zero literal hit is not a clean result because runtime identity is derived. Acceptance requires semantic graph evaluation: no old checker, candidate publish, stale `--pin-build`, old setup identity, or old product acquisition path is reachable from any active workflow/action/state/config. `ci-policy-validator-products.yml` must be in both source and generated closure and in renderer tests.

## Ruleset, freeze, and recovery boundary

The existing protect-main ruleset is ID `19573071`; observed required contexts were `DCO`, `ci-required`, `Policy`; bypass actor 5 was `always`. Full returned ruleset JSON bodies and canonical hashes are mandatory before and after every change. Context identity is `{context, integration_id}`, never a name-only match. No ETag/CAS behavior is assumed.

The required order is:

1. Read-only census of main ref, PR head/base/tree, all paginated runs/jobs/checks, applicable required children, and full ruleset.
2. Resolve an actual provider-enforced freeze and independent recovery actor. The current feasibility report says no provider CAS excluding actor 5 is proven; an operator lease or proxy assertion is insufficient. This is an external blocker, not solved by this document.
3. Reread main/ruleset and hashes immediately before the full Tree A ruleset PUT.
4. Run Tree A admission and all ordinary DCO/ci-required workloads; no skipped required cells.
5. Normal protected merge; verify exact resulting main SHA, not a ref update or guessed merge result.
6. Run Main B's real publisher and live provider check; verify every native and supporting run/job/attempt/artifact/asset/attestation identity.
7. Run the post-run terminal census, excluding only the explicit verifier/provider self nodes.
8. Full PUT to Main B contexts and reread its complete body/hash.
9. Create and merge Tree B normally; then verify final Policy and remove both temporary authorities.

Forward recovery must survive coordinator death after a mutation. Conditional restore may occur only when the exact temporary ruleset hash and expected main ref still match; no unconditional saved JSON restore is allowed. After a successful main merge, recovery is forward-only. Without an actual provider-enforced freeze, recovery actor, and disposable race/death exercise, no operation may start.

## Native and workload acceptance

The resulting main must execute all applicable jobs with terminal `success`: Linux-X64 validator publication, Linux-ARM64 product/native checks, `xcode-27` macOS checks, ordinary PR/unit Bun/Docker/docs/OpenTofu/Rust checks, maintenance, preview, release where its trigger applies, nightly dispatch child census, signer closure, actionlint, generator drift/state, DCO, ci-required, Main B provider check, and Policy. `macos-26` or macOS-15 is a hard failure. Empty matrices, missing workflows, skipped required jobs, wrapper-green/child-failed runs, stale checks, wrong runner identity, and unpublished local fixes are incomplete.

For each run, evidence binds repository, workflow path/ref/blob, source SHA/tree, event/ref, run ID/attempt, job ID, check-run ID, runner label, artifact IDs/digests, release/asset IDs/digests, attestation predicate/subject, and provider integration. Names, labels, run IDs, or artifact IDs without the raw API and digest relation do not pass.

## Current hard blockers

- No reviewed immutable target generator revision; current revision 54 and old pin 0dc are not targets.
- Current main has no proven B source/output or fixed-point generated closure.
- The canonical DAG report found 90 fields with 21 undefined producers, 20 unmapped fields, and 13 ambiguous fields. This revision supplies a typed S0–S7 proposal, not execution proof.
- Strict canonical record/provenance and live raw artifact verification are not implemented.
- B App/provider app ID, installation, integration, verifier revision, credential, and called-workflow identity are unresolved.
- Current observed runtime used forbidden `macos-26`; complete `xcode-27` native proof is absent.
- D19 found no clean published candidate.
- Actor 5 always-bypass and provider freeze/recovery are unproven. The feasibility report SHA is `9735c1b2184f32f3b6b1cd1fefe98045dbb23403f937cd65692d2bcf54b06725`; JSON SHA `29fa711c78ffd8c323acaf6e19653e4adbad87d1658b4d27c9116d669bab36b9`.
- Independent approval is absent. `g0_reviewer` and `authority_transition_review` must independently review current main, the typed DAG, hostile negatives, schemas, provider binding, ruleset snapshots, recovery, native census, and Tree B cleanup.

## Deterministic status

V8 is a successor design draft with explicit machine edges, strict record schema, canonical path mapping, predicate partitions, typed OIDC roles, live artifact handoff, native obligations, and fail-closed ruleset order. It is not approval-ready, execution-ready, or a G1 pass. The authority choice remains unselected. Any claim that Tree A, Main B, or Tree B has run is false until exact current-branch API evidence and both independent approvals exist.
