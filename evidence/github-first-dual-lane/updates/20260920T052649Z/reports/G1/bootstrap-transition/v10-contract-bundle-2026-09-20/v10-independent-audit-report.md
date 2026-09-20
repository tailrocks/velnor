# Independent v10 canonical-contract audit

Status: design-only. No authority, source, release, verifier, dispatch, merge, or credential mutation occurred.

Result: 36/36 structural checks passed; failures=0; authority_claim=false.

## Checks

- `pass` `v9-preserved` — v9 artifacts remain separate; v10 does not overwrite them
- `pass` `model-hash` — 09d586416a86fae321e35fc24fbecac870a1faec93cd6d17392169ff803e36f1
- `pass` `root-no-self-digest` — root manifest has no root_digest field
- `pass` `root-digest-plan` — 44f144bee0d2437fedf3d72c951d5df248033f7b78100004d81e30980e046136
- `pass` `single-model-output` — canonical DAG, schemas, fixtures and plan reference one model source
- `pass` `identity-dag` — DAG identity equals model identity
- `pass` `release-identity` — {"architecture": "x86_64", "platform": "Linux-X64", "product_name": "velnor-workflow-policy-validator", "product_namespace": "velnor.workflow-policy-validator", "schema": "velnor.workflow-policy-validator.release-manifest.v1"}
- `pass` `release-leaf-count` — exactly 14 release leaves
- `pass` `release-positive-fixture` — positive fixture covers every release leaf
- `pass` `release-positive-identity` — positive fixture uses canonical identity
- `pass` `unique-field-producers` — 299 fields have one producer
- `pass` `lineage-field-refs` — all edge field IDs resolve
- `pass` `release-leaf-lineage` — release leaves are in the canonical field registry
- `pass` `S4-no-future` — S4 preimage contains only S0-S3 fields
- `pass` `S6-no-future` — S6 preimage contains only S0-S5 fields
- `pass` `S7-provider-no-self` — provider result ID/digest and verify-B fields are excluded
- `pass` `S7-terminal-no-self` — terminal digest uses upstream rows only
- `pass` `provider-no-census-cycle` — provider result schema has no terminal census
- `pass` `typed-output-full` — 299 typed fields
- `pass` `typed-output-not-live` — fixture explicitly non-live
- `pass` `pr-main-separation` — candidate PR head and resulting main are separate; result is null
- `pass` `provider-head-binding` — provider contract requires Checks head_sha/resulting-main equality
- `pass` `caller-readback` — run/workflow/Contents API equality rules are explicit
- `pass` `external-unresolved` — provider/freeze/live implementation blockers remain unresolved
- `pass` `negative-fixture-set` — 10 hostile cases expected reject
- `pass` `negative-repro:ambiguous_provider_producer` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:caller_pr_head_used_as_resulting_main` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:future_stage_field_in_preimage` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:own_digest_field_in_preimage` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:provider_check_head_not_resulting_main` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:provider_result_contains_terminal_census` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:release_identity_mismatch` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:release_leaf_omitted` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:synthetic_fixture_claims_live` — hostile mutation rejected by independent structural predicate
- `pass` `negative-repro:unresolved_provider_claimed_success` — hostile mutation rejected by independent structural predicate
- `pass` `forbidden-runner-preserved` — observed macos-26 remains rejected in current evidence

## Boundary

Synthetic fixtures are explicitly non-live. Provider identities, live verifier execution, target generator revision, native fleet closure, and provider-enforced freeze/CAS remain external blockers. V9 is preserved and must not be overwritten.
