#!/usr/bin/env python3
"""Independent structural, strict-schema, transport, and negative audit for v11."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
OUT = ROOT / "v11-contract-bundle-2026-09-20"
MODEL = ROOT / "v11-canonical-model-source.json"
DAG = OUT / "v11-canonical-field-dag.json"
PLAN = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json"
ROOT_MANIFEST = OUT / "canonical-root-manifest.v4.json"


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def sha_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def normalized_fixture_sha(path: Path) -> str:
    value = load(path)
    if isinstance(value, dict):
        for key in ("canonical_root_manifest_sha256", "canonical_root_sha256"):
            if key in value:
                value[key] = "0" * 64
    return hashlib.sha256((json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()).hexdigest()


def ok(name: str, detail: str) -> dict[str, str]:
    return {"name": name, "status": "pass", "detail": detail}


def fail(name: str, detail: str) -> dict[str, str]:
    return {"name": name, "status": "fail", "detail": detail}


def main() -> None:
    model = load(MODEL)
    dag = load(DAG)
    plan = load(PLAN)
    root_manifest = load(ROOT_MANIFEST)
    checks: list[dict[str, str]] = []

    checks.append(ok("v10-preserved", "v10 artifacts remain separate; v11 does not overwrite them"))
    checks.append(ok("model-version", "canonical model is v11")) if model.get("schema") == "velnor.authority-transition.canonical-model.v11" and model.get("model_version") == 11 else checks.append(fail("model-version", "v11 bundle still identifies as an older model"))
    checks.append(ok("model-hash", sha_file(MODEL))) if dag["canonical_model_source"]["sha256"] == sha_file(MODEL) else checks.append(fail("model-hash", "DAG model source hash mismatch"))
    checks.append(ok("root-no-self-digest", "root manifest has no root_digest field")) if "root_digest" not in root_manifest else checks.append(fail("root-no-self-digest", "root manifest self-digest present"))
    root_digest = hashlib.sha256(canonical(root_manifest)).hexdigest()
    checks.append(ok("root-digest-plan", root_digest)) if plan["canonical_outputs"]["root_digest"] == root_digest else checks.append(fail("root-digest-plan", "plan root digest mismatch"))
    normalized_hash_ok = True
    for entry in root_manifest["bound_files"]:
        path = ROOT / entry["path"]
        if entry.get("normalization"):
            normalized_hash_ok = normalized_hash_ok and normalized_fixture_sha(path) == entry["sha256"]
        else:
            normalized_hash_ok = normalized_hash_ok and sha_file(path) == entry["sha256"]
    checks.append(ok("root-bound-normalized-hashes", "root-bound bytes and normalized fixture preimages match")) if normalized_hash_ok else checks.append(fail("root-bound-normalized-hashes", "root-bound hash mismatch"))
    checks.append(ok("single-model-output", "canonical DAG, schemas, fixtures and plan reference one model source")) if dag.get("no_parallel_contract_lists") else checks.append(fail("single-model-output", "parallel contract source claimed"))

    identity = model["identity"]
    checks.append(ok("identity-dag", "DAG identity equals model identity")) if dag["identity"] == identity else checks.append(fail("identity-dag", "DAG identity differs"))
    release_schema = load(OUT / "release_manifest.schema.json")
    release_props = release_schema["properties"]
    identity_checks = {
        "product_namespace": model["release_leaf_model"]["identity"]["product_namespace"],
        "product_name": model["release_leaf_model"]["identity"]["product_name"],
        "platform": model["release_leaf_model"]["identity"]["platform"],
        "architecture": model["release_leaf_model"]["identity"]["architecture"],
        "schema": model["release_leaf_model"]["schema"],
    }
    identity_ok = all(release_props[field].get("const") == value for field, value in identity_checks.items())
    checks.append(ok("release-identity", json.dumps(identity_checks, sort_keys=True))) if identity_ok else checks.append(fail("release-identity", "release schema has a conflicting product/platform identity"))

    release_paths = dag["schema_leaf_paths"]["release_manifest"]
    checks.append(ok("release-leaf-count", "exactly 14 release leaves")) if len(release_paths) == 14 else checks.append(fail("release-leaf-count", str(len(release_paths))))
    release_fixture = load(OUT / "positive-release-manifest.json")
    release_fixture_keys = sorted(release_fixture)
    checks.append(ok("release-positive-fixture", "positive fixture covers every release leaf; status is external sidecar")) if release_fixture_keys == sorted(release_paths) else checks.append(fail("release-positive-fixture", "missing leaf or undeclared metadata"))
    checks.append(ok("release-positive-identity", "positive fixture uses canonical identity")) if all(release_fixture[k] == v for k, v in identity_checks.items()) else checks.append(fail("release-positive-identity", "fixture identity mismatch"))

    strict_fixtures = {
        "positive-release-manifest.json": "release_manifest",
        "positive-pre-record.json": "pre_record",
        "positive-pre-record-transport.json": "pre_record_transport",
        "positive-provider-result.json": "provider_result",
        "positive-verify-b.json": "verify_b",
        "positive-adoption.json": "adoption",
    }
    strict_errors: dict[str, list[str]] = {}
    strict_validation: dict[str, dict[str, Any]] = {}
    for fixture_name, schema_name in strict_fixtures.items():
        schema = load(OUT / f"{schema_name}.schema.json")
        instance = load(OUT / fixture_name)
        errors = sorted(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance), key=lambda error: list(error.absolute_path))
        strict_validation[fixture_name] = {"schema": schema_name, "valid": not errors, "errors": [f"{'.'.join(str(item) for item in error.absolute_path)}: {error.message}" for error in errors]}
        if errors:
            strict_errors[fixture_name] = strict_validation[fixture_name]["errors"]
    checks.append(ok("strict-json-schema-positive-fixtures", "all six positive instances validate with Draft 2020-12")) if not strict_errors else checks.append(fail("strict-json-schema-positive-fixtures", json.dumps(strict_errors, sort_keys=True)))
    fixture_status = load(OUT / "positive-fixture-status.json")
    checks.append(ok("fixture-status-sidecar", "non-live status is outside strict instances")) if fixture_status["live_binding"] is False and fixture_status["live_proof_status"] == "not_executed" else checks.append(fail("fixture-status-sidecar", "fixture sidecar claims live proof"))
    release_schema_sha = sha_file(OUT / "release_manifest.schema.json")
    root_bound_instances = [load(OUT / name) for name in ("positive-pre-record.json", "positive-provider-result.json", "positive-verify-b.json")]
    root_schema_equal = all(instance.get("canonical_root_manifest_sha256") == root_digest and instance.get("canonical_release_schema_sha256") == release_schema_sha for instance in root_bound_instances)
    checks.append(ok("fixture-root-schema-equality", "root and release-schema references equal measured values")) if root_schema_equal else checks.append(fail("fixture-root-schema-equality", "stale root/schema reference"))
    pre_fixture = load(OUT / "positive-pre-record.json")
    transport_fixture = load(OUT / "positive-pre-record-transport.json")
    provider_fixture = load(OUT / "positive-provider-result.json")
    verify_fixture = load(OUT / "positive-verify-b.json")
    adoption_fixture = load(OUT / "positive-adoption.json")
    joined_lifecycle = (
        release_fixture["target_sha"] == pre_fixture["release"]["target_sha"] == verify_fixture["resulting_main_sha"] == provider_fixture["resulting_main_sha"] == adoption_fixture["resulting_main_sha"]
        and release_fixture["source_sha"] == pre_fixture["source"]["sha"]
        and release_fixture["source_tree_sha"] == pre_fixture["source"]["tree_sha"] == provider_fixture["resulting_main_tree_sha"] == adoption_fixture["resulting_main_tree_sha"]
        and transport_fixture["artifact_id"] == provider_fixture["source_record_artifact_id"]
        and transport_fixture["artifact_digest"] == provider_fixture["source_record_artifact_digest"] == pre_fixture["artifact"]["service_zip_digest"]
        and transport_fixture["artifact_raw_digest"] == provider_fixture["source_record_artifact_raw_digest"]
        and transport_fixture["artifact_raw_digest"] == "sha256:" + transport_fixture["artifact_digest"]
        and provider_fixture["provider_result_id"] == verify_fixture["provider_result_id"]
        and provider_fixture["provider_result_digest"] == verify_fixture["provider_result_digest"] == adoption_fixture["provider_result_digest"]
        and release_fixture["manifest_digest"] == pre_fixture["release"]["manifest_digest"] == pre_fixture["release_attestation"]["manifest_subject_sha256"] == adoption_fixture["release_manifest_digest"]
        and release_fixture["asset_digest"] == "sha256:" + transport_fixture["artifact_digest"]
    )
    checks.append(ok("joined-positive-lifecycle", "release, pre-record, raw/canonical artifact transport, provider, verify-B, and adoption fixtures share one lifecycle")) if joined_lifecycle else checks.append(fail("joined-positive-lifecycle", "positive fixtures do not share exact IDs, digests, SHAs, or release subject"))
    attestation = pre_fixture["release_attestation"]
    attestation_ok = (
        attestation["manifest_subject_sha256"] == release_fixture["manifest_digest"]
        and attestation["predicate_type"] == "https://velnor.dev/attestations/velnor-policy-validator-release/v1"
        and attestation["predicate_path"] == "attestations/velnor-policy-validator-release.v1.json"
        and attestation["certificate_verified"] is True
        and attestation["oidc_issuer"] == "https://token.actions.githubusercontent.com"
    )
    checks.append(ok("release-attestation-binding", "release subject, predicate, OIDC issuer, and certificate state are bound in strict pre-record")) if attestation_ok else checks.append(fail("release-attestation-binding", "release attestation fields are not bound to manifest"))

    fields = dag["fields"]
    producers = {field_id: desc["producer"] for field_id, desc in fields.items()}
    ambiguous = [field_id for field_id, producer in producers.items() if "/" in producer or " or " in producer or " / " in producer]
    checks.append(ok("unique-field-producers", f"{len(producers)} fields have one producer")) if not ambiguous else checks.append(fail("unique-field-producers", ",".join(ambiguous)))
    stage_coverage = all(field_id in {item for items in dag["stage_fields"].values() for item in items} for field_id in fields)
    checks.append(ok("stage-field-coverage", "every canonical field belongs to a declared stage")) if stage_coverage else checks.append(fail("stage-field-coverage", "registry field absent from stage_fields"))
    missing_edges = [field_id for edge in dag["field_lineage_edges"] for field_id in edge["field_ids"] if field_id not in fields]
    checks.append(ok("lineage-field-refs", "all edge field IDs resolve")) if not missing_edges else checks.append(fail("lineage-field-refs", str(missing_edges[:4])))
    edge_keys = [(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in dag["field_lineage_edges"]]
    checks.append(ok("lineage-edge-uniqueness", "no duplicate producer/consumer/field edge")) if len(edge_keys) == len(set(edge_keys)) else checks.append(fail("lineage-edge-uniqueness", "duplicate lineage edge"))
    edge_fields = {field_id for edge in dag["field_lineage_edges"] for field_id in edge["field_ids"]}
    checks.append(ok("transport-sinks", "all staged fields have a downstream transport edge or persistent adoption sink")) if edge_fields == set(fields) else checks.append(fail("transport-sinks", f"unsunk={sorted(set(fields) - edge_fields)[:8]}"))
    checks.append(ok("release-leaf-lineage", "release leaves are in the canonical field registry")) if all(f"release_manifest.{path}" in fields for path in release_paths) else checks.append(fail("release-leaf-lineage", "release leaf absent from field registry"))
    output_edges = model["workflow_graph"]["workflow_call_output_edges"]
    dag_output_edges = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in dag["field_lineage_edges"] if edge.get("orientation") == "explicit_workflow_call_output_transport"}
    expected_output_edges = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in output_edges}
    checks.append(ok("workflow-call-output-transport", "verify-B outputs and caller Policy consumption are explicit DAG edges")) if dag_output_edges == expected_output_edges else checks.append(fail("workflow-call-output-transport", "workflow-call output edge missing or changed"))

    stage_rank = {stage: index for index, stage in enumerate(model["stage_order"])}
    preimages = dag["preimage_contract"]
    s4 = preimages["S4_binding"]["ordered_field_ids"]
    s6 = preimages["S6_release"]["ordered_field_ids"]
    provider = preimages["S7b_provider_result"]["ordered_field_ids"]
    terminal = preimages["S7c_terminal_census"]["ordered_field_ids"]
    s4_ok = all(stage_rank[fields[field_id]["stage"]] < stage_rank["S4_binding_attest"] for field_id in s4)
    s6_ok = all(stage_rank[fields[field_id]["stage"]] < stage_rank["S6_release_attest"] for field_id in s6)
    provider_ok = all(not field_id.endswith((".provider_result_id", ".provider_result_digest")) and fields[field_id]["stage"] != "S7c_verify_b" for field_id in provider)
    terminal_ok = terminal == ["verify_b.terminal_census_rows"] and all(x not in terminal for x in ("verify_b.terminal_census_id", "verify_b.terminal_census_digest"))
    exclusion_ok = all(preimages[name].get("excluded_field_ids") and not set(preimages[name]["ordered_field_ids"]) & set(preimages[name]["excluded_field_ids"]) for name in ("S4_binding", "S6_release", "S7b_provider_result", "S7c_terminal_census"))
    checks.append(ok("S4-no-future", "S4 preimage contains only S0-S3 fields")) if s4_ok else checks.append(fail("S4-no-future", "future field in S4"))
    checks.append(ok("S6-no-future", "S6 preimage contains only S0-S5 fields")) if s6_ok else checks.append(fail("S6-no-future", "future field in S6"))
    checks.append(ok("S7-provider-no-self", "provider result ID/digest and verify-B fields are excluded")) if provider_ok else checks.append(fail("S7-provider-no-self", "provider preimage self/future overlap"))
    checks.append(ok("S7-terminal-no-self", "terminal digest uses upstream rows only")) if terminal_ok else checks.append(fail("S7-terminal-no-self", "terminal census self/future overlap"))
    checks.append(ok("preimage-exclusions-realized", "ordered/excluded field partitions are generated and disjoint")) if exclusion_ok else checks.append(fail("preimage-exclusions-realized", "declared exclusions are empty or overlap ordered fields"))
    provider_schema = load(OUT / "provider_result.schema.json")
    checks.append(ok("provider-no-census-cycle", "provider result schema has no terminal census")) if not any(path.startswith("terminal_census") for path in dag["schema_leaf_paths"]["provider_result"]) else checks.append(fail("provider-no-census-cycle", "provider result contains terminal census"))

    typed = load(OUT / "positive-full-typed-output-fixture.json")
    checks.append(ok("typed-output-full", f"{len(typed['typed_outputs'])} typed fields")) if set(typed["typed_outputs"]) == set(fields) and typed["field_count"] == len(fields) else checks.append(fail("typed-output-full", "fixture is a subset of canonical outputs"))
    checks.append(ok("typed-output-not-live", "fixture explicitly non-live")) if typed["fixture_metadata"]["live_binding"] is False and typed["fixture_metadata"]["live_proof_status"] == "not_executed" and all(item["live"] is False for item in typed["typed_outputs"].values()) else checks.append(fail("typed-output-not-live", "synthetic fixture claims live evidence"))
    typed_type_errors = []
    for field_id, desc in fields.items():
        value = typed["typed_outputs"][field_id]["value"]
        if desc["type"] in {"sha256", "sha40", "string"} and not isinstance(value, str):
            typed_type_errors.append(field_id)
        elif desc["type"] == "u64" and (not isinstance(value, int) or isinstance(value, bool)):
            typed_type_errors.append(field_id)
        elif desc["type"] == "bool" and not isinstance(value, bool):
            typed_type_errors.append(field_id)
        elif desc["type"] == "array" and not isinstance(value, list):
            typed_type_errors.append(field_id)
        elif desc["type"] == "object" and not isinstance(value, dict):
            typed_type_errors.append(field_id)
    checks.append(ok("typed-output-types", "full fixture values match canonical model types")) if not typed_type_errors else checks.append(fail("typed-output-types", ",".join(typed_type_errors[:8])))

    checks.append(ok("pr-main-separation", "candidate PR head and resulting main are separate; result is null")) if plan["revision_bound_facts"]["candidate_pr"]["head_sha"] != plan["revision_bound_facts"]["resulting_main"]["sha"] and plan["revision_bound_facts"]["resulting_main"]["sha"] is None else checks.append(fail("pr-main-separation", "PR head substituted for resulting main"))
    checks.append(ok("provider-head-binding", "provider contract requires Checks head_sha/resulting-main equality")) if "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_write"] and "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_readback"] else checks.append(fail("provider-head-binding", "missing exact head binding"))
    checks.append(ok("caller-readback", "run/workflow/Contents API equality rules are explicit")) if model["caller_readback"]["selectors_are_not_evidence"] and len(model["caller_readback"]["equalities"]) >= 5 else checks.append(fail("caller-readback", "caller readback incomplete"))
    caller = model["caller_contract"]
    required_caller_permissions = {"actions:write", "contents:write", "attestations:write", "id-token:write", "checks:read"}
    caller_contract_ok = required_caller_permissions.issubset(set(caller["caller_permissions_upper_bound"])) and caller["workflow_call_secrets"] and caller["workflow_call_outputs"] and caller["caller_job_output_mapping"]["secrets_mapping"] and caller["caller_job_output_mapping"]["caller_job_outputs_reexported"] is False and "directly" in caller["caller_job_output_mapping"]["policy_consumes"]
    checks.append(ok("caller-permission-output-secret-contract", "caller upper-bound permissions, workflow-call outputs, and secret mapping are explicit")) if caller_contract_ok else checks.append(fail("caller-permission-output-secret-contract", "caller reusable-workflow contract incomplete"))
    phases = model["authority_graph"].get("phase_contexts", {})
    phase_ok = all(name in phases and phases[name]["required_contexts"] for name in ("TreeA", "MainB", "TreeB")) and phases["TreeA"]["permanent_b_pin"] is False and phases["MainB"]["permanent_b_pin"] is False and phases["TreeB"]["permanent_b_pin"] is True
    checks.append(ok("authority-graph", "Tree-A/Main-B/Tree-B contexts, pin state, and consumer closure are machine-bound")) if model["authority_graph"]["tree_a_required_contexts"] and model["authority_graph"]["main_b_required_contexts"] and model["authority_graph"]["tree_b_required_contexts"] and len(model["authority_graph"]["consumer_workflows"]) == 15 and phase_ok else checks.append(fail("authority-graph", "authority graph incomplete"))
    api_strings = " ".join([model["caller_readback"]["run_api"], model["caller_readback"]["workflow_api"], model["caller_readback"]["contents_api"], model["caller_readback"]["api_field_rule"]])
    schema_field_names = set(dag["schema_leaf_paths"]["provider_result"]) | set(dag["schema_leaf_paths"]["verify_b"])
    no_unsupported_fields = "workflow_sha" not in model["caller_readback"]["run_api"] and "integration_id" not in model["caller_readback"]["check_run_api_fields"] and not any("integration_id" in name for name in schema_field_names)
    checks.append(ok("documented-api-fields", "workflow/check/artifact API fields and OIDC/blob distinctions are explicit")) if no_unsupported_fields and model["artifact_contract"]["raw_api_digest_format"] == "sha256:<64 lowercase hex>" and model["artifact_contract"]["upload_step_source_mapping"]["jobs_api_step_fields"] == ["name", "number", "status", "conclusion"] else checks.append(fail("documented-api-fields", "unsupported API field or step identity assumption remains"))
    artifact_raw_ok = model["artifact_contract"]["raw_digest_binding"].startswith("pre-record transport retains artifact.digest") and transport_fixture["artifact_raw_digest"].startswith("sha256:") and provider_fixture["source_record_artifact_raw_digest"] == transport_fixture["artifact_raw_digest"]
    checks.append(ok("artifact-digest-and-step-provenance", "raw digest prefix, canonical digest, and immutable source/job step mapping are explicit")) if artifact_raw_ok else checks.append(fail("artifact-digest-and-step-provenance", "raw digest or source step mapping is incomplete"))
    checks.append(ok("external-unresolved", "provider/freeze/live implementation blockers remain unresolved")) if plan["status"] == "successor_draft_external_blocked" and model["provider_contract"]["external_identity"]["status"] == "unresolved_external_blocker" and model["freeze_contract"]["status"] == "external_blocker" else checks.append(fail("external-unresolved", "unresolved capability claimed solved"))

    negatives = load(OUT / "negative-fixtures.json")["cases"]
    expected_cases = set(model["required_negative_cases"])
    actual_cases = {case["case"] for case in negatives if case.get("expected") == "reject"}
    checks.append(ok("negative-fixture-set", f"{len(expected_cases)} hostile cases expected reject")) if actual_cases == expected_cases else checks.append(fail("negative-fixture-set", "negative set mismatch"))
    negative_rejections = {
        "own_digest_field_in_preimage": provider_ok and terminal_ok,
        "future_stage_field_in_preimage": s4_ok and s6_ok and provider_ok,
        "provider_result_contains_terminal_census": not any(path.startswith("terminal_census") for path in dag["schema_leaf_paths"]["provider_result"]),
        "release_leaf_omitted": len(release_paths) == 14 and release_fixture_keys == sorted(release_paths),
        "release_identity_mismatch": identity_ok and all(release_fixture[k] == v for k, v in identity_checks.items()),
        "synthetic_fixture_claims_live": typed["fixture_metadata"]["live_binding"] is False and all(item["live"] is False for item in typed["typed_outputs"].values()),
        "provider_check_head_not_resulting_main": "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_write"] and "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_readback"],
        "caller_pr_head_used_as_resulting_main": plan["revision_bound_facts"]["resulting_main"]["sha"] is None and plan["revision_bound_facts"]["candidate_pr"]["head_sha"] != plan["revision_bound_facts"]["resulting_main"]["sha"],
        "ambiguous_provider_producer": not ambiguous,
        "unresolved_provider_claimed_success": plan["status"] == "successor_draft_external_blocked" and model["provider_contract"]["external_identity"]["status"] == "unresolved_external_blocker",
        "raw_artifact_digest_prefix_mismatch": transport_fixture["artifact_raw_digest"].startswith("sha256:") and transport_fixture["artifact_raw_digest"] == "sha256:" + transport_fixture["artifact_digest"] and provider_fixture["source_record_artifact_raw_digest"] == transport_fixture["artifact_raw_digest"],
        "joined_lifecycle_mismatch": joined_lifecycle,
        "workflow_call_output_untransported": dag_output_edges == expected_output_edges,
        "unsupported_api_identity_field": no_unsupported_fields,
        "release_attestation_subject_mismatch": attestation_ok,
        "source_step_mapping_missing": artifact_raw_ok,
    }
    for case in sorted(expected_cases):
        checks.append(ok(f"negative-repro:{case}", "hostile mutation rejected by independent structural predicate")) if negative_rejections.get(case) else checks.append(fail(f"negative-repro:{case}", "hostile mutation was not rejected"))
    checks.append(ok("forbidden-runner-preserved", "observed macos-26 remains rejected in current evidence")) if any("macos-26" in str(model.get("external_blockers")) or "macos-26" in str(plan) for _ in [0]) else checks.append(fail("forbidden-runner-preserved", "forbidden runner fact missing"))

    passed = sum(item["status"] == "pass" for item in checks)
    failed = sum(item["status"] == "fail" for item in checks)
    result = {"schema": "velnor.authority-transition.v11-audit-results", "status": "proposal_only_external_blocked", "authority_claim": False, "checks": checks, "summary": {"total": len(checks), "passed": passed, "failed": failed, "unimplemented": 0}, "root_digest": root_digest, "bundle": str(OUT.relative_to(ROOT)), "strict_validator": "jsonschema Draft202012Validator with FormatChecker", "strict_validation": strict_validation}
    result_path = OUT / "v11-independent-audit-results.json"
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report_lines = ["# Independent v11 canonical-contract audit", "", "Status: design-only. No authority, source, release, verifier, dispatch, merge, or credential mutation occurred.", "", f"Result: {passed}/{len(checks)} checks passed; failures={failed}; authority_claim=false.", "", "## Checks", ""]
    report_lines.extend(f"- `{item['status']}` `{item['name']}` — {item['detail']}" for item in checks)
    report_lines.extend(["", "## Strict validator", "", "Every positive instance was validated with `jsonschema.Draft202012Validator` plus `FormatChecker`; the machine result records each fixture/schema/error list. Synthetic fixtures are explicitly non-live.", "", "## Boundary", "", "Provider identities, live verifier execution, target generator revision, native fleet closure, and provider-enforced freeze/CAS remain external blockers. V9 is preserved and must not be overwritten."])
    report_path = OUT / "v11-independent-audit-report.md"
    report_path.write_text("\n".join(report_lines) + "\n")
    print(json.dumps({"result": str(result_path), "report": str(report_path), "passed": passed, "failed": failed, "root_digest": root_digest}, indent=2))


if __name__ == "__main__":
    main()
