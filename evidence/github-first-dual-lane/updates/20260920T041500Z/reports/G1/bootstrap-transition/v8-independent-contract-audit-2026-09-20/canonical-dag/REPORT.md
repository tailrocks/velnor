# Independent v8 canonical field/DAG audit

Status: **independent_structural_findings_external_blocked**. Gate claim: **false**. Real verifier: **unimplemented**.
Read-only external evidence; no workflow, provider, release, merge, dispatch, or authority operation.

## Inputs and baseline

- v8 MD SHA-256: `503a564afcbe157e607215c0549480b2cade36525b6f3aeb41198418c6add639` (expected `503a564afcbe157e607215c0549480b2cade36525b6f3aeb41198418c6add639`)
- v8 JSON SHA-256: `87a5896eb8cf75e53b49f8eacb61226315a9f79c574af3078fdc34390252c321` (expected `87a5896eb8cf75e53b49f8eacb61226315a9f79c574af3078fdc34390252c321`)
- permanent B schema: `427cbae0d9a238ae5e22a1afac2c8f840e4a8ad136983ac28a863651e533aed7`; record schema: `3f2d72acda359e8ca0a31a40a6e4aa417bd49552acec050583a92601d0c851ef`; historical predicate schema: `4bcd53cdd4e1900d4edf240f46da4c14ab570fc7be09f37484a9f56402da3b06`
- live remote observed `2026-09-20T03:58:55Z`: main `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`
- cached local origin/main: `325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree `e9019f00578d34c3f339c7e8d55b66f7e53f8567`
- revision-bound v8 facts match live 89f82dd8.../22ccc1d.... The plan's evidence.current_main/current_tree still carry cached 325719f1.../e9019f0... and must not be consumed as live authority.

- strict permanent-B positive: 0 schema errors; five typed/additional-property/terminal/action/event mutations each reject. No strict record positive document is supplied.

## Findings

1. S7 has **16** non-unique producer fields. record-upload declares provider/called identities despite actions:write-only permissions; verify-B declares no new output.
2. Field lineage omits **16** S7 fields. Its seven from_job/to_job labels reverse the needs consumer/producer pairs.
3. Permanent B has **146** concrete leaves; owner map has 128 paths, missing **24** step leaves. Record schema has **68** leaves, with **50** unmapped by that map.
4. Timing mismatches (11): `{"artifact.expired": {"declared": "S1", "derived": "S2"}, "artifact.rest_service_zip_sha256": {"declared": "S1", "derived": "S2"}, "artifact.rest_service_zip_size": {"declared": "S1", "derived": "S2"}, "artifact.workflow_run_id": {"declared": "S1", "derived": "S2"}, "attestation.binding_digest": {"declared": "S6", "derived": "S4"}, "attestation.binding_predicate_path": {"declared": "S6", "derived": "S4"}, "attestation.binding_predicate_type": {"declared": "S6", "derived": "S4"}, "attestation.binding_subject_name": {"declared": "S6", "derived": "S4"}, "attestation.binding_subject_sha256": {"declared": "S6", "derived": "S4"}, "release.manifest_signed_after_draft_id": {"declared": "S3", "derived": "S4"}, "release.tag_immutable": {"declared": "S3", "derived": "S5"}}`. Manifest-signed-after-draft is S3 in owner mapping but S4; tag immutability S3 but S5; raw REST artifact fields S1 but S2; binding attestation paths disagree S4/S6.
5. Preimage partitions have stage lists but no explicit field sets. Missing exact S4 exclusions: `binding_record_artifact_digest, binding_record_artifact_id, called_workflow_file_blob_sha, called_workflow_path, called_workflow_ref, called_workflow_sha, policy_bootstrap_b_check_id, policy_bootstrap_b_integration_id, policy_bootstrap_b_provider_app_id, policy_bootstrap_b_verifier_revision`. Missing exact record self-exclusions: `record_artifact_name, record_upload_run_attempt, record_upload_step_id`.
6. Product identity differs: plan `velnor-policy-validator`/`velnor.policy-validator`, strict `velnor-workflow-policy-validator`, historical predicate `velnor-workflow-policy-validator`.

## Hostile fixtures and execution boundary

Independent negatives: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition/v8-independent-contract-audit-2026-09-20/canonical-dag/canonical-preimage-negatives.json` SHA-256 `cf180c341af91980662ef560500c18798b60f284eb22b10f1438e46dbd828ebf`, 13 derived rejects, all unexecuted.
Prior 34-fixture bundle preserved: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition/validator-binding-audit-2026-09-20/hostile-fixtures.index.json` SHA-256 `1652ebf312aa4ccf047b4cf93c77f2fe8e3feff72a490ccf022c528226fb5912`; unexecuted.
No typed B publisher/verifier paths were present at live main, so no hostile case demonstrates real verifier rejection. No gate claim.

Metrics: `{"ambiguous_s7_fields": 16, "duplicate_stage_fields": 0, "historical_negative_fixtures": 34, "independent_negative_fixtures": 13, "lineage_edges": 7, "lineage_gap_fields": 16, "owner_canonical_paths": 128, "permanent_missing_leaves": 24, "permanent_schema_leaves": 146, "record_schema_leaves": 68, "record_unmapped_leaves": 50, "stage_field_count": 82, "timing_mismatches": 11}`.
Machine output: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition/v8-independent-contract-audit-2026-09-20/canonical-dag/canonical-field-dag.json` SHA-256 `aa2f86a91cb0b8458ec8e101c3ad99859514c146bcaead655db9b041bddbbf5d`.
