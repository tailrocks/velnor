# Velnor authority-transition plan v10 — proposal only

Status: `successor_draft_external_blocked`. V9 remains immutable and preserved. This bundle is a design correction, not authorization: no source, ruleset, App, release, merge, dispatch, runner, or credential mutation occurred.

## Canonical source and revision binding

One model source generates the schemas, typed field registry, producer/consumer graph, preimage sets, fixtures, plan JSON, and this Markdown. It is `v10-canonical-model-source.json` with SHA-256 `09d586416a86fae321e35fc24fbecac870a1faec93cd6d17392169ff803e36f1`. The generated DAG is `v10-contract-bundle-2026-09-20/v10-canonical-field-dag.json` with SHA-256 `38a00228cd28207f3b257a53ba9683cdaf697c5899c897b0c2f53fdf80d364db`. The root manifest has no self-digest; its canonical digest is `44f144bee0d2437fedf3d72c951d5df248033f7b78100004d81e30980e046136`.

Current observed `main` is `89f82dd8b287f46a3cf4c0920f341f6ca6c736db` (parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`). PR 969 head `2445c647dafa43acbc5285c873d3b04174ff75c6` is candidate evidence only. Resulting-main SHA/tree are deliberately null until the protected merge response, `refs/heads/main`, run API, and workflow source readback all agree. No PR-head-as-main substitution is permitted.

## Corrected contract

Product identity is exactly `velnor-workflow-policy-validator` / `velnor.workflow-policy-validator.v1` / `velnor-workflow-policy-validator-Linux-X64` / `Linux-X64` / `ubuntu-24.04`. The generated canonical DAG contains 146 permanent leaves and exactly 14 release-manifest leaves. The release positive fixture is bound and exercises all 14 required fields.

S7 is acyclic. The provider result is emitted by `external-provider-result` and contains no terminal census. `verify-B` reads the signed result, reads the called-workflow identity, then computes a separate terminal census over typed upstream run/job/check rows. Provider-result ID/digest and census ID/digest are excluded from their own preimages; no own or future field is ordered into a digest. Transport artifact IDs are separate fields and never self-hashed.

The provider contract has concrete raw-REST transport and Checks API head binding: the provider downloads the exact pre-record artifact, signs canonical bytes, creates `Policy-bootstrap-B` with `head_sha == resulting_main_sha`, and `verify-B` reads back `{context, integration_id, app, head_sha, conclusion}`. Provider App, installation, integration, verifier revision, signature key/trust root, result endpoint, and live verifier remain unresolved external blockers; the plan claims no success.

Caller workflow-call values are selectors, not evidence. The verifier reads the run, workflow, and Contents APIs and requires event/ref/head/workflow/blob/tree equalities. Called identity uses `job_workflow_ref/job_workflow_sha`; caller identity uses `workflow_ref/workflow_sha`.

The full typed-output fixture is explicitly `deterministic_contract_fixture`, `live_binding=false`, `live_proof_status=not_executed`. Synthetic fixture values are not live proof. Current main has no typed publisher/verifier implementation; this bundle does not pretend otherwise.

## Transition and blockers

Tree A admits the source/generator change with one externally approved `Policy-bootstrap-A` audit and no permanent B pin. A normal protected merge creates the actual resulting main. That exact main push invokes a `workflow_call`-only publisher from the guarded `ci-main` caller. Main B publishes the immutable typed product, provider verifies it, and `verify-B` completes the post-provider census. Only then does a separate Tree-B PR add the permanent pin. Temporary authorities are removed only after permanent Policy verification.

Unresolved blockers: target generator revision; typed publisher source/output and fixed-point generation; provider identities/credential/trust root/live verifier; full xcode-27/Linux-X64/Linux-ARM64 closure; enforceable freeze/CAS/recovery excluding actor 5; and user threat-model choice. Observed `macos-26` remains forbidden. No authority operation is proposed as executed.

## Generated evidence

Schema hashes, fixture hashes, root-manifest digest, and machine graph are in `AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json`. Automated negative reproduction must reject own/future preimages, omitted release leaves, identity mismatches, synthetic-as-live claims, PR-head-as-main claims, provider-head mismatches, ambiguous producers, terminal-census cycles, and unresolved-provider success before independent review is requested.
