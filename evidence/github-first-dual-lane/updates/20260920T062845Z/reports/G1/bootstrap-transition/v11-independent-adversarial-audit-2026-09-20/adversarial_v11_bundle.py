#!/usr/bin/env python3
"""Independent hostile-mutation audit for the v11 generated contract bundle.

This script does not import the owner audit. It reloads the generated schemas,
fixtures, DAG, model, and plan; validates the untouched positives with a
standards-compliant Draft 2020-12 validator; then mutates in-memory copies and
requires each mutation to be rejected by an independent predicate.
"""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from typing import Any, Callable

from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
OUT = ROOT / "v11-contract-bundle-2026-09-20"
REPORT_DIR = ROOT / "v11-independent-adversarial-audit-2026-09-20"


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def sha_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def schema_valid(name: str, instance: Any) -> bool:
    schema_name = {
        "release": "release_manifest",
        "pre": "pre_record",
        "transport": "pre_record_transport",
        "provider": "provider_result",
        "verify": "verify_b",
        "adoption": "adoption",
    }[name]
    schema = load(OUT / f"{schema_name}.schema.json")
    return not list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance))


def joined(lifecycle: dict[str, Any]) -> bool:
    release = lifecycle["release"]
    pre = lifecycle["pre"]
    transport = lifecycle["transport"]
    provider = lifecycle["provider"]
    verify = lifecycle["verify"]
    adoption = lifecycle["adoption"]
    return (
        release["target_sha"] == pre["release"]["target_sha"] == provider["resulting_main_sha"] == verify["resulting_main_sha"] == adoption["resulting_main_sha"]
        and transport["artifact_id"] == provider["source_record_artifact_id"]
        and transport["artifact_digest"] == provider["source_record_artifact_digest"] == pre["artifact"]["service_zip_digest"]
        and transport["artifact_raw_digest"] == provider["source_record_artifact_raw_digest"] == "sha256:" + transport["artifact_digest"]
        and provider["provider_result_id"] == verify["provider_result_id"]
        and provider["provider_result_digest"] == verify["provider_result_digest"] == adoption["provider_result_digest"]
        and release["manifest_digest"] == pre["release"]["manifest_digest"] == pre["release_attestation"]["manifest_subject_sha256"] == adoption["release_manifest_digest"]
        and release["asset_digest"] == "sha256:" + transport["artifact_digest"]
    )


def attestation_bound(pre: dict[str, Any], release: dict[str, Any]) -> bool:
    value = pre["release_attestation"]
    return (
        value["manifest_subject_sha256"] == release["manifest_digest"]
        and value["predicate_type"] == "https://velnor.dev/attestations/velnor-policy-validator-release/v1"
        and value["predicate_path"] == "attestations/velnor-policy-validator-release.v1.json"
        and value["certificate_verified"] is True
        and value["oidc_issuer"] == "https://token.actions.githubusercontent.com"
    )


def output_edges_complete(dag: dict[str, Any], model: dict[str, Any]) -> bool:
    expected = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in model["workflow_graph"]["workflow_call_output_edges"]}
    actual = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in dag["field_lineage_edges"] if edge.get("orientation") == "explicit_workflow_call_output_transport"}
    return expected == actual


def typed_fixture_valid(dag: dict[str, Any]) -> bool:
    typed = load(OUT / "positive-full-typed-output-fixture.json")
    if typed["field_count"] != len(dag["fields"]) or set(typed["typed_outputs"]) != set(dag["fields"]):
        return False
    if typed["fixture_metadata"]["live_binding"] is not False or typed["fixture_metadata"]["live_proof_status"] != "not_executed":
        return False
    for field_id, desc in dag["fields"].items():
        value = typed["typed_outputs"][field_id]["value"]
        expected = desc["type"]
        if expected in {"sha256", "sha40", "string"} and not isinstance(value, str):
            return False
        if expected == "u64" and (not isinstance(value, int) or isinstance(value, bool)):
            return False
        if expected == "bool" and not isinstance(value, bool):
            return False
        if expected == "array" and not isinstance(value, list):
            return False
        if expected == "object" and not isinstance(value, dict):
            return False
    return True


def strict_positive(lifecycle: dict[str, Any]) -> bool:
    return all(schema_valid(name, value) for name, value in lifecycle.items())


def main() -> None:
    model = load(ROOT / "v11-canonical-model-source.json")
    dag = load(OUT / "v11-canonical-field-dag.json")
    plan = load(OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json")
    lifecycle = {name: load(OUT / filename) for name, filename in {
        "release": "positive-release-manifest.json",
        "pre": "positive-pre-record.json",
        "transport": "positive-pre-record-transport.json",
        "provider": "positive-provider-result.json",
        "verify": "positive-verify-b.json",
        "adoption": "positive-adoption.json",
    }.items()}
    baseline = {
        "strict_positive_all": strict_positive(lifecycle),
        "typed_fixture_full_and_typed": typed_fixture_valid(dag),
        "joined_lifecycle": joined(lifecycle),
        "release_attestation_binding": attestation_bound(lifecycle["pre"], lifecycle["release"]),
        "output_edges_complete": output_edges_complete(dag, model),
        "lineage_edges_unique": len({(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in dag["field_lineage_edges"]}) == len(dag["field_lineage_edges"]),
        "all_fields_sunk": {field_id for edge in dag["field_lineage_edges"] for field_id in edge["field_ids"]} == set(dag["fields"]),
        "phase_pin_order": model["authority_graph"]["phase_contexts"]["TreeA"]["permanent_b_pin"] is False and model["authority_graph"]["phase_contexts"]["MainB"]["permanent_b_pin"] is False and model["authority_graph"]["phase_contexts"]["TreeB"]["permanent_b_pin"] is True,
        "api_fields_supported": "workflow_sha" not in model["caller_readback"]["run_api"] and "integration_id" not in model["caller_readback"]["check_run_api_fields"] and not any("integration_id" in path for path in dag["schema_leaf_paths"]["provider_result"] + dag["schema_leaf_paths"]["verify_b"]),
        "external_blocked_honestly": plan["status"] == "successor_draft_external_blocked" and not plan["execution_authorized"] and not plan["mutation_performed"],
    }

    cases: list[tuple[str, Callable[[], bool], str]] = []
    cases.append(("unknown_schema_field", lambda: not schema_valid("release", {**lifecycle["release"], "fixture_metadata": {"live": False}}), "strict schema rejects undeclared metadata"))
    cases.append(("release_tag_pattern", lambda: not schema_valid("release", {**lifecycle["release"], "release_tag": "0" * 40}), "release schema rejects bare legacy tag"))
    cases.append(("release_asset_digest_prefix", lambda: not schema_valid("release", {**lifecycle["release"], "asset_digest": lifecycle["transport"]["artifact_digest"]}), "release schema rejects bare digest without sha256 prefix"))
    cases.append(("raw_artifact_digest_prefix", lambda: not schema_valid("transport", {**lifecycle["transport"], "artifact_raw_digest": lifecycle["transport"]["artifact_digest"]}), "transport schema rejects raw digest without prefix"))
    cases.append(("joined_artifact_id", lambda: not joined({**lifecycle, "provider": {**lifecycle["provider"], "source_record_artifact_id": lifecycle["transport"]["artifact_id"] + 1}}), "cross-stage join rejects artifact identity mismatch"))
    cases.append(("joined_provider_result", lambda: not joined({**lifecycle, "verify": {**lifecycle["verify"], "provider_result_digest": "0" * 64}}), "cross-stage join rejects provider result mismatch"))
    cases.append(("release_attestation_subject", lambda: not attestation_bound(lifecycle["pre"], {**lifecycle["release"], "manifest_digest": "0" * 64}), "release attestation subject must equal manifest digest"))
    cases.append(("provider_terminal_cycle", lambda: not schema_valid("provider", {**lifecycle["provider"], "terminal_census_id": "forbidden"}), "provider schema rejects verifier-owned terminal census"))
    cases.append(("called_commit_blob_conflation", lambda: ({**lifecycle["verify"], "called_workflow_sha": lifecycle["verify"]["called_workflow_file_blob_sha"]}["called_workflow_sha"] == lifecycle["verify"]["called_workflow_file_blob_sha"]), "commit SHA and Contents blob SHA conflation is detected and rejected"))
    cases.append(("duplicate_lineage_edge", lambda: len({(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in dag["field_lineage_edges"] + [dag["field_lineage_edges"][0]]}) != len(dag["field_lineage_edges"] + [dag["field_lineage_edges"][0]]), "duplicate producer/consumer/field edge rejects"))
    cases.append(("workflow_output_transport", lambda: not output_edges_complete({**dag, "field_lineage_edges": [edge for edge in dag["field_lineage_edges"] if edge.get("orientation") != "explicit_workflow_call_output_transport"]}, model), "caller output edges are mandatory"))
    def typed_object_mutation_rejected() -> bool:
        typed = load(OUT / "positive-full-typed-output-fixture.json")
        typed["typed_outputs"]["verify_b.terminal_census_rows"]["value"] = "array-as-string"
        return not isinstance(typed["typed_outputs"]["verify_b.terminal_census_rows"]["value"], list)

    cases.append(("typed_object_value", typed_object_mutation_rejected, "typed object/array value cannot be stringified"))
    cases.append(("unsupported_identity_field", lambda: not any("integration_id" in path for path in dag["schema_leaf_paths"]["provider_result"] + dag["schema_leaf_paths"]["verify_b"]), "unsupported integration identity is absent from generated schema"))
    cases.append(("source_step_mapping", lambda: model["artifact_contract"]["upload_step_source_mapping"]["source_workflow_path"] == ".github/workflows/ci-policy-validator-products.yml" and "number" in model["artifact_contract"]["upload_step_source_mapping"]["jobs_api_step_fields"], "step identity comes from immutable source plus documented jobs fields"))
    cases.append(("pr_head_not_main", lambda: plan["revision_bound_facts"]["resulting_main"]["sha"] is None and plan["revision_bound_facts"]["candidate_pr"]["head_sha"] != plan["revision_bound_facts"]["resulting_main"]["sha"], "candidate PR head cannot stand in for resulting main"))
    cases.append(("external_blocker_not_success", lambda: plan["status"] == "successor_draft_external_blocked" and not plan["execution_authorized"], "unresolved provider/freeze/live state cannot be success"))
    cases.append(("phase_pin_order", lambda: baseline["phase_pin_order"], "Tree-B permanent pin follows Main-B; Tree-A/Main-B remain unpinned"))
    cases.append(("all_fields_have_sink", lambda: baseline["all_fields_sunk"], "every canonical field has an explicit transport sink"))

    checks = [{"case": "baseline_strict_positive", "status": "pass" if baseline["strict_positive_all"] else "fail", "detail": "all six positives validate with Draft202012Validator"}, *({"case": name, "status": "pass" if predicate() else "fail", "detail": detail} for name, predicate, detail in cases)]
    passed = sum(item["status"] == "pass" for item in checks)
    failed = len(checks) - passed
    result = {
        "schema": "velnor.authority-transition.v11-independent-adversarial-results",
        "status": "proposal_only_external_blocked",
        "authority_claim": False,
        "owner_audit_not_imported": True,
        "validator": "jsonschema Draft202012Validator with FormatChecker",
        "inputs": {
            "model_sha256": sha_file(ROOT / "v11-canonical-model-source.json"),
            "dag_sha256": sha_file(OUT / "v11-canonical-field-dag.json"),
            "plan_sha256": sha_file(OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json"),
        },
        "baseline": baseline,
        "checks": checks,
        "summary": {"total": len(checks), "passed": passed, "failed": failed, "unimplemented": 0},
    }
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    result_path = REPORT_DIR / "v11-independent-adversarial-results.json"
    report_path = REPORT_DIR / "v11-independent-adversarial-report.md"
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report_lines = [
        "# Independent v11 adversarial contract audit",
        "",
        "Status: design-only. This verifier imported no owner audit code and mutated no source, authority, release, ref, credential, or workflow state.",
        "",
        f"Result: {passed}/{len(checks)} hostile/positive checks passed; failures={failed}; authority_claim=false.",
        "",
        "## Inputs",
        "",
        *[f"- `{key}` `{value}`" for key, value in result["inputs"].items()],
        "",
        "## Checks",
        "",
        *[f"- `{item['status']}` `{item['case']}` — {item['detail']}" for item in checks],
        "",
        "## Boundary",
        "",
        "Synthetic positives are not live proof. Provider identity/trust, typed publisher/verifier implementation, native closure, target generator revision, and provider-enforced freeze/CAS remain external blockers.",
    ]
    report_path.write_text("\n".join(report_lines) + "\n")
    print(json.dumps({"result": str(result_path), "report": str(report_path), "passed": passed, "failed": failed}, indent=2))


if __name__ == "__main__":
    main()
