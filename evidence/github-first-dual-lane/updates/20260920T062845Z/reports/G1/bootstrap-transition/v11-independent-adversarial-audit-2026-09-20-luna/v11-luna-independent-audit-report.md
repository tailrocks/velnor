# Independent v11 Luna adversarial contract audit

Status: design-only external audit. No source, authority, release, dispatch, merge, runner, or credential mutation occurred.
Owner 59/59 and 19/19 labels are preserved as non-authoritative inputs; this audit did not import owner code.

Result: 24/38 checks passed; failures=9; unimplemented=5; findings=13; authority_claim=false.

## Exact frozen hashes

- Freeze manifest: `df14a907b29cf5839551dc0b4a32e77af5c61d8e927433530f9a061d75997c16`.
- Canonical model: `ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930`.
- Plan JSON: `4c50c8e591924cbdbccb7ec489e8a8bbb20576d5f5fbb207c42e52837f6d42c6`.
- Plan Markdown: `2fe0a7e534299e0b7dc83102706d29d91fd07e422a795ef726c1dccd5d119dde`.
- Canonical DAG: `37e3d0cb65b5b793219d94894a672eae50e8181378481dfb846afff1c49e56e7`.
- Root manifest raw: `35f8ae8a20cfe3ce774ed0d63ece8ed06d3e66d4e66693933ea5e5825e32a96d`; canonical root digest: `deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017`.
- Hostile fixture set: `e8ea509799cfee7fa428dcd599c51aa43abf63c6c2b7a1f545784fac6d4df91d`.
- Independent projection: `7d55a6eb7f5ab50f8bb4792251f9454dab9515a8b11bbf4d530a8131ccc8c3ae`.
- Independent script: `a10137b3b54d4d5f041ec2e2a899ea11733c6acb0a8904f96446f17ebfb4675c`.
- Independent result: `50ab287a460cb7d240af78743fd46ee2ddce2965215829a71120f3daf24b1dd2`.

## Findings

- `V11-FIXTURE-001` (high): The emitted 303-field typed fixture only checks coarse JSON types; strict schema projections fail canonical consts, digest prefixes, nested terminal-row shape, and typed object values.
  - Detail: `{"adoption": {"error_count": 1, "errors": ["mode: 'permanent_pin' was expected"], "field_count": 303, "prefix": "adoption"}, "permanent_binding": {"error_count": 9, "errors": ["product.application_namespace_reuse: False was expected", "product.asset: 'velnor-workflow-policy-validator-Linux-X64' was expected", "product.features: '' was expected", "product.mutable_latest: False was expected", "product.overwrite: False was expected", "product.platform: 'Linux-X64' was expected", "product.product_id: 'velnor-workflow-policy-validator' was expected", "product.runner: 'ubuntu-24.04' was expected", "product.schema: 'velnor.workflow-policy-validator.v1' was expected"], "field_count": 303, "prefix": "permanent"}, "pre_record": {"error_count": 25, "errors": ["artifact.binary_architecture: 'x86_64' was expected", "binding.final_manifest_artifact_digest: 'ad7ed67f395f2f1ac7e67564b9bbeff892206d11dcfad3261381ff6083e00905' does not match '^sha256:[0-9a-f]{64}$'", "binding.predicate_path: 'attestations/velnor-policy-validator-binding.v1.json' was expected", "binding.predicate_type: 'https://velnor.dev/attestations/policy-validator-binding/v1' was expected", "binding.subject_digest: 'b763390d31c0606b93d3bf3ded0f7f75ffbd86460668fd3017037cfadeb7e4d0' does not match '^sha256:[0-9a-f]{64}$'", "product.architecture: 'x86_64' was expected", "product.name: 'velnor-workflow-policy-validator' was expected", "product.namespace: 'velnor.workflow-policy-validator' was expected", "product.platform: 'Linux`
- `V11-SCHEMA-001` (high): permanent_binding.schema.json has no direct strict positive instance; the only permanent projection is the invalid coarse typed wrapper.
  - Detail: `{"schema_sha256": "3e0651ebb8cfe6d055fdf6cfe733901da92decdc9766d73f28a817f53dd04194", "typed_projection_errors": {"error_count": 9, "errors": ["product.application_namespace_reuse: False was expected", "product.asset: 'velnor-workflow-policy-validator-Linux-X64' was expected", "product.features: '' was expected", "product.mutable_latest: False was expected", "product.overwrite: False was expected", "product.platform: 'Linux-X64' was expected", "product.product_id: 'velnor-workflow-policy-validator' was expected", "product.runner: 'ubuntu-24.04' was expected", "product.schema: 'velnor.workflow-policy-validator.v1' was expected"], "field_count": 303, "prefix": "permanent"}}`
- `V11-DAG-001` (high): Strict schema leaves and canonical field registry are not bijective: required_step_conclusions child keys are schema leaves but only their object container is in fields.
  - Detail: `{"registry_entries_not_schema_leaves": ["pre_record.verification.required_step_conclusions"], "schema_leaves_missing_from_registry": ["pre_record.verification.required_step_conclusions.artifact-verify", "pre_record.verification.required_step_conclusions.attest-binding", "pre_record.verification.required_step_conclusions.attest-release", "pre_record.verification.required_step_conclusions.build-linux-x64", "pre_record.verification.required_step_conclusions.pre-record-upload", "pre_record.verification.required_step_conclusions.publish", "pre_record.verification.required_step_conclusions.reserve-release"]}`
- `V11-TRANSPORT-001` (critical): The field registry declares downstream consumers, but no carry-forward/output/storage edge transports most S0/S1/S2 fields to those jobs; only one adjacent stage edge exists and ordinary GitHub needs edges do not carry arbitrary fields.
  - Detail: `{"affected_fields": 206, "edges_without_explicit_transport": [{"consumer": "build-linux-x64", "field_ids": ["permanent.actions.artifact_metadata_write", "permanent.actions.attest_sha", "permanent.actions.create_storage_record", "permanent.actions.push_to_registry", "permanent.actions.upload_artifact_sha", "permanent.product.application_namespace_reuse", "permanent.product.asset", "permanent.product.features", "permanent.product.mutable_latest", "permanent.product.overwrite", "permanent.product.platform", "permanent.product.product_id", "permanent.product.profile", "permanent.product.purpose", "permanent.product.runner", "permanent.product.schema", "permanent.product.tag", "permanent.publisher.attest_permissions", "permanent.publisher.build_permissions", "permanent.publisher.event", "permanent.publisher.head_branch", "permanent.publisher.head_sha", "permanent.publisher.path", "permanent.publisher.ref", "permanent.publisher.repository", "permanent.publisher.repository_id", "permanent.publisher.reserve_publish_permissions", "permanent.publisher.sha", "permanent.source.closure", "permanent.source.ref", "permanent.source.repository", "permanent.source.repository_id", "permanent.source.sha", "permanent.source.tree_sha", "pre_record.caller_workflow.capture_step_id", "pre_record.caller_workflow.file_blob_sha", "pre_record.caller_workflow.path", "pre_record.caller_workflow.ref", "pre_record.caller_workflow.run_attempt", "pre_record.caller_workflow.run_id", "pre_record.caller_workflow.`
- `V11-DAG-002` (critical): The strict pre-record uploaded at S7a requires 38 fields whose sole declared producer is S7c verify-B, which runs after the provider consumes that artifact.
  - Detail: `{"count": 38, "fields": ["pre_record.binding.attestation_digest", "pre_record.binding.attestation_id", "pre_record.binding.core_digest", "pre_record.binding.final_manifest_artifact_digest", "pre_record.binding.final_manifest_artifact_id", "pre_record.binding.final_manifest_digest", "pre_record.binding.predicate_path", "pre_record.binding.predicate_type", "pre_record.binding.subject_digest", "pre_record.binding.subject_name", "pre_record.canonical_release_schema_sha256", "pre_record.canonical_root_manifest_sha256", "pre_record.producer.check_run_id", "pre_record.producer.job_id", "pre_record.producer.job_workflow_sha", "pre_record.producer.run_attempt", "pre_record.producer.run_id", "pre_record.producer.workflow_file_blob_sha", "pre_record.producer.workflow_path", "pre_record.producer.workflow_ref", "pre_record.record_kind", "pre_record.release_attestation.attestation_digest", "pre_record.release_attestation.attestation_id", "pre_record.release_attestation.certificate_identity", "pre_record.release_attestation.certificate_verified", "pre_record.release_attestation.manifest_subject_sha256", "pre_record.release_attestation.oidc_issuer", "pre_record.release_attestation.oidc_policy_revision_sha256", "pre_record.release_attestation.predicate_path", "pre_record.release_attestation.predicate_type", "pre_record.release_attestation.signer_repository", "pre_record.release_attestation.signer_source_ref", "pre_record.release_attestation.signer_workflow", "pre_record.schema_version", "pre_`
- `V11-DAG-003` (high): Artifact-verify S2 is declared as producer for reserve-release, attest-binding, publish, and attest-release step IDs/statuses that do not exist until those later jobs run.
  - Detail: `{"count": 16, "fields": [{"declared_stage": "S2_artifact_verify", "expected_stage": "S4_binding_attest", "field": "permanent.steps.attest_binding.conclusion", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S4_binding_attest", "field": "permanent.steps.attest_binding.id", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S4_binding_attest", "field": "permanent.steps.attest_binding.name", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S4_binding_attest", "field": "permanent.steps.attest_binding.status", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S6_release_attest", "field": "permanent.steps.attest_release.conclusion", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S6_release_attest", "field": "permanent.steps.attest_release.id", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S6_release_attest", "field": "permanent.steps.attest_release.name", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S6_release_attest", "field": "permanent.steps.attest_release.status", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact_verify", "expected_stage": "S5_publish", "field": "permanent.steps.publish.conclusion", "producer": "artifact-verify"}, {"declared_stage": "S2_artifact`
- `V11-DAG-004` (high): pre_record release asset/manifest identities are assigned to reserve-release before their real publish/binding producers; the S3→S4 lineage therefore consumes future values.
  - Detail: `[{"declared_stage": "S3_release_reserve", "expected_stage": "S4_binding_attest", "field": "pre_record.release.manifest_digest"}, {"declared_stage": "S3_release_reserve", "expected_stage": "S5_publish", "field": "pre_record.release.asset_id"}, {"declared_stage": "S3_release_reserve", "expected_stage": "S5_publish", "field": "pre_record.release.asset_digest"}]`
- `V11-PREIMAGE-001` (critical): S7b provider preimage excludes result ID/digest but still orders provider_check_run_id and provider_attestation_digest, both provider-owned result identities with no specified pre-sign allocation/acyclic sequence.
  - Detail: `{"ordered": ["provider_result.provider_attestation_digest", "provider_result.provider_check_run_id"], "signature_contract": "canonical provider-result payload excluding provider_result_id and provider_result_digest"}`
- `V11-PREIMAGE-002` (high): Binding/release/provider/terminal digest values are synthetic and no complete preimage bytes or real signing verifier is available for recomputation.
  - Detail: `{"preimage_files": [], "typed_fixture_live": false}`
- `V11-DATA-001` (high): Schemas carry service-ZIP, inner-payload, binary, and release-asset digests/sizes but the frozen model has no executable equality/byte fixture proving those joins.
  - Detail: `{"build_to_rest_service_zip": "permanent.artifact.service_zip_sha256 == permanent.artifact.rest_service_zip_sha256", "inner_payload_to_binary": "inner payload binary digest/size == binary fields", "raw_service_zip_to_rest_digest": "permanent.artifact.rest_service_zip_sha256 == bytes(S2 REST ZIP)", "raw_zip_to_pre_record": "pre_record.artifact.service_zip_digest == verified raw service ZIP digest", "release_asset_raw_bytes": "release asset REST bytes digest == release_manifest.asset_digest", "service_zip_to_inner_payload": "service ZIP extracted payload digest/size == inner_payload fields"}`
- `V11-IMPL-001` (critical): No typed publisher/verifier implementation exists at fresh origin/main; schema and hostile checks are contract-only.
  - Detail: `{"commit": "89f82dd8b287f46a3cf4c0920f341f6ca6c736db", "paths": [".github/workflows/ci-policy-validator-products.yml", ".github-gen/sources/workflows/ci-policy-validator-products.yml", "crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs"]}`
- `V11-TRUST-001` (critical): Exact provider signer/app/repository/workflow/ref/digest trust values and immutable action source SHA are unresolved; schemas accept arbitrary valid SHA strings and URI endpoints.
  - Detail: `{"provider_identity": {"installation_id": null, "provider_app_id": null, "result_endpoint": null, "status": "unresolved_external_blocker", "verifier_revision": null}, "signature": {"algorithm": null, "key_id": null, "signed_bytes": "canonical provider-result payload excluding provider_result_id and provider_result_digest", "status": "unresolved_external_blocker", "trust_root": null}, "source_commit_sha": null}`
- `V11-TERM-001` (high): The positive terminal census has one row while the authority graph declares 15 consumer workflows; verify-B has no required coverage set or completeness predicate.
  - Detail: `{"required_workflows": ["ci-main.yml", "ci-policy.yml", "ci-pr.yml", "ci-unit-rust.yml", "ci-unit-bun.yml", "ci-unit-docker.yml", "ci-unit-docs.yml", "ci-unit-opentofu.yml", "maintenance.yml", "preview.yml", "release.yml", "ci-runtime-products.yml", "ci-policy-validator-products.yml", "nightly.yml", "ci-release-package-signer.yml"], "rows": [{"attempt": 1, "check_run_id": 7404, "conclusion": "success", "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "job_id": 7403, "run_id": 7401, "status": "completed", "workflow_path": ".github/workflows/ci-main.yml", "workflow_sha": "5555555555555555555555555555555555555555"}]}`

## Check statuses

- `pass` `frozen-v11-input-hashes`
- `pass` `canonical-root-digest`
- `pass` `root-bound-bytes`
- `pass` `bundle-index-references`
- `pass` `v10-preserved`
- `pass` `schema-hash-projection`
- `pass` `single-canonical-model`
- `pass` `strict-positive-instances`
- `fail` `typed-fixture-strict-projections`
- `fail` `permanent-strict-positive-coverage`
- `pass` `schema-leaf-path-derivation`
- `fail` `schema-leaf-registry-bijection`
- `pass` `field-registry-unique-producers`
- `pass` `stage-field-coverage`
- `pass` `field-producer-stage-consistency`
- `pass` `lineage-edge-integrity`
- `fail` `declared-consumer-transport-coverage`
- `pass` `workflow-call-output-transport`
- `fail` `pre-record-fields-available-before-upload`
- `fail` `step-producer-temporal-availability`
- `fail` `release-field-producer-timing`
- `pass` `preimage-partition-integrity`
- `pass` `declared-preimage-stage-boundaries`
- `pass` `terminal-census-own-exclusion`
- `fail` `provider-result-identity-exclusion`
- `pass` `model-suffix-exclusions-realized`
- `unimplemented` `preimage-digest-recomputation`
- `pass` `joined-positive-lifecycle`
- `pass` `release-attestation-binding`
- `unimplemented` `artifact-inner-binary-digest-contract`
- `pass` `v10-hostile-classes-replayed`
- `pass` `hostile-fixture-set`
- `pass` `fresh-live-main-reconciliation`
- `unimplemented` `fresh-live-typed-verifier`
- `unimplemented` `exact-action-source-and-oidc-trust`
- `fail` `terminal-census-completeness-contract`
- `pass` `external-blockers-honest`
- `unimplemented` `real-hostile-execution`

Strict positives were run with jsonschema 4.26.0 Draft202012Validator plus FormatChecker. Hostile cases are isolated JSON contract fixtures only; no real verifier was present at the fresh origin/main revision, so hostile rejection execution is unimplemented and no gate/authority claim is made.
