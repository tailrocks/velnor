# Independent v11 adversarial contract audit

Status: design-only. This verifier imported no owner audit code and mutated no source, authority, release, ref, credential, or workflow state.

Result: 19/19 hostile/positive checks passed; failures=0; authority_claim=false.

## Inputs

- `model_sha256` `ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930`
- `dag_sha256` `37e3d0cb65b5b793219d94894a672eae50e8181378481dfb846afff1c49e56e7`
- `plan_sha256` `4c50c8e591924cbdbccb7ec489e8a8bbb20576d5f5fbb207c42e52837f6d42c6`

## Checks

- `pass` `baseline_strict_positive` — all six positives validate with Draft202012Validator
- `pass` `unknown_schema_field` — strict schema rejects undeclared metadata
- `pass` `release_tag_pattern` — release schema rejects bare legacy tag
- `pass` `release_asset_digest_prefix` — release schema rejects bare digest without sha256 prefix
- `pass` `raw_artifact_digest_prefix` — transport schema rejects raw digest without prefix
- `pass` `joined_artifact_id` — cross-stage join rejects artifact identity mismatch
- `pass` `joined_provider_result` — cross-stage join rejects provider result mismatch
- `pass` `release_attestation_subject` — release attestation subject must equal manifest digest
- `pass` `provider_terminal_cycle` — provider schema rejects verifier-owned terminal census
- `pass` `called_commit_blob_conflation` — commit SHA and Contents blob SHA conflation is detected and rejected
- `pass` `duplicate_lineage_edge` — duplicate producer/consumer/field edge rejects
- `pass` `workflow_output_transport` — caller output edges are mandatory
- `pass` `typed_object_value` — typed object/array value cannot be stringified
- `pass` `unsupported_identity_field` — unsupported integration identity is absent from generated schema
- `pass` `source_step_mapping` — step identity comes from immutable source plus documented jobs fields
- `pass` `pr_head_not_main` — candidate PR head cannot stand in for resulting main
- `pass` `external_blocker_not_success` — unresolved provider/freeze/live state cannot be success
- `pass` `phase_pin_order` — Tree-B permanent pin follows Main-B; Tree-A/Main-B remain unpinned
- `pass` `all_fields_have_sink` — every canonical field has an explicit transport sink

## Boundary

Synthetic positives are not live proof. Provider identity/trust, typed publisher/verifier implementation, native closure, target generator revision, and provider-enforced freeze/CAS remain external blockers.
