# Independent v11 canonical-contract audit

Status: design-only. No authority, source, release, verifier, dispatch, merge, or credential mutation occurred.

Result: 59/59 checks passed; failures=0; authority_claim=false.

## Checks

- `pass` `v10-preserved` — v10 artifacts remain separate; v11 does not overwrite them
- `pass` `model-version` — canonical model is v11
- `pass` `model-hash` — ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930
- `pass` `root-no-self-digest` — root manifest has no root_digest field
- `pass` `root-digest-plan` — deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017
- `pass` `root-bound-normalized-hashes` — root-bound bytes and normalized fixture preimages match
- `pass` `single-model-output` — canonical DAG, schemas, fixtures and plan reference one model source
- `pass` `identity-dag` — DAG identity equals model identity
- `pass` `release-identity` — {"architecture": "x86_64", "platform": "Linux-X64", "product_name": "velnor-workflow-policy-validator", "product_namespace": "velnor.workflow-policy-validator", "schema": "velnor.workflow-policy-validator.release-manifest.v1"}
- `pass` `release-leaf-count` — exactly 14 release leaves
- `pass` `release-positive-fixture` — positive fixture covers every release leaf; status is external sidecar
- `pass` `release-positive-identity` — positive fixture uses canonical identity
- `pass` `strict-json-schema-positive-fixtures` — all six positive instances validate with Draft 2020-12
- `pass` `fixture-status-sidecar` — non-live status is outside strict instances
- `pass` `fixture-root-schema-equality` — root and release-schema references equal measured values
- `pass` `joined-positive-lifecycle` — release, pre-record, raw/canonical artifact transport, provider, verify-B, and adoption fixtures share one lifecycle
- `pass` `release-attestation-binding` — release subject, predicate, OIDC issuer, and certificate state are bound in strict pre-record
- `pass` `unique-field-producers` — 303 fields have one producer
- `pass` `stage-field-coverage` — every canonical field belongs to a declared stage
- `pass` `lineage-field-refs` — all edge field IDs resolve
- `pass` `lineage-edge-uniqueness` — no duplicate producer/consumer/field edge
- `pass` `transport-sinks` — all staged fields have a downstream transport edge or persistent adoption sink
- `pass` `release-leaf-lineage` — release leaves are in the canonical field registry
- `pass` `workflow-call-output-transport` — verify-B outputs and caller Policy consumption are explicit DAG edges
- `pass` `S4-no-future` — S4 preimage contains only S0-S3 fields
- `pass` `S6-no-future` — S6 preimage contains only S0-S5 fields
- `pass` `S7-provider-no-self` — provider result ID/digest and verify-B fields are excluded
- `pass` `S7-terminal-no-self` — terminal digest uses upstream rows only
- `pass` `preimage-exclusions-realized` — ordered/excluded field partitions are generated and disjoint
- `pass` `provider-no-census-cycle` — provider result schema has no terminal census
- `pass` `typed-output-full` — 303 typed fields
- `pass` `typed-output-not-live` — fixture explicitly non-live
- `pass` `typed-output-types` — full fixture values match canonical model types
- `pass` `pr-main-separation` — candidate PR head and resulting main are separate; result is null
- `pass` `provider-head-binding` — provider contract requires Checks head_sha/resulting-main equality
- `pass` `caller-readback` — run/workflow/Contents API equality rules are explicit
- `pass` `caller-permission-output-secret-contract` — caller upper-bound permissions, workflow-call outputs, and secret mapping are explicit
- `pass` `authority-graph` — Tree-A/Main-B/Tree-B contexts, pin state, and consumer closure are machine-bound
- `pass` `documented-api-fields` — workflow/check/artifact API fields and OIDC/blob distinctions are explicit
- `pass` `artifact-digest-and-step-provenance` — raw digest prefix, canonical digest, and immutable source/job step mapping are explicit
- `pass` `external-unresolved` — provider/freeze/live implementation blockers remain unresolved
- `pass` `negative-fixture-set` — 16 hostile cases expected reject
- `pass` `negative-repro:ambiguous_provider_producer` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:caller_pr_head_used_as_resulting_main` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:future_stage_field_in_preimage` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:joined_lifecycle_mismatch` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:own_digest_field_in_preimage` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:provider_check_head_not_resulting_main` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:provider_result_contains_terminal_census` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:raw_artifact_digest_prefix_mismatch` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:release_attestation_subject_mismatch` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:release_identity_mismatch` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:release_leaf_omitted` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:source_step_mapping_missing` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:synthetic_fixture_claims_live` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:unresolved_provider_claimed_success` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:unsupported_api_identity_field` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:workflow_call_output_untransported` — hostile mutation rejected by independent structural predicate
- `pass` `forbidden-runner-preserved` — observed macos-26 remains rejected in current evidence

## Strict validator

Every positive instance was validated with `jsonschema.Draft202012Validator` plus `FormatChecker`; the machine result records each fixture/schema/error list. Synthetic fixtures are explicitly non-live.

## Boundary

Provider identities, live verifier execution, target generator revision, native fleet closure, and provider-enforced freeze/CAS remain external blockers. V9 is preserved and must not be overwritten.
