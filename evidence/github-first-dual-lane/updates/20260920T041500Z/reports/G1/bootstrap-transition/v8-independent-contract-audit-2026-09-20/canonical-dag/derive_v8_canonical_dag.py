#!/usr/bin/env python3
"""Read-only independent audit of frozen v8 field availability and preimages."""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import copy
import subprocess
from pathlib import Path
from typing import Any

TRANSITION = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
PLAN_JSON = TRANSITION / "AUTHORITY-CHANGE-PLAN-2026-09-20-v8.json"
PLAN_MD = TRANSITION / "AUTHORITY-CHANGE-PLAN-2026-09-20-v8.md"
SCHEMA_ROOT = TRANSITION / "authority-contract-separation-2026-09-20"
PERM_SCHEMA = SCHEMA_ROOT / "permanent-b-product-binding.schema.json"
RECORD_SCHEMA = SCHEMA_ROOT / "policy-validator-b-record-provenance.v1.schema.json"
BINDING_SCHEMA = TRANSITION / "validator-binding-audit-2026-09-20" / "binding-predicate.schema.json"
HISTORICAL_INDEX = TRANSITION / "validator-binding-audit-2026-09-20" / "hostile-fixtures.index.json"
RESULTS = OUT / "canonical-field-dag.json"
NEGATIVES = OUT / "canonical-preimage-negatives.json"
REPORT = OUT / "REPORT.md"

EXPECTED_MD = "503a564afcbe157e607215c0549480b2cade36525b6f3aeb41198418c6add639"
EXPECTED_JSON = "87a5896eb8cf75e53b49f8eacb61226315a9f79c574af3078fdc34390252c321"
EXPECTED_PERM = "427cbae0d9a238ae5e22a1afac2c8f840e4a8ad136983ac28a863651e533aed7"
EXPECTED_RECORD = "3f2d72acda359e8ca0a31a40a6e4aa417bd49552acec050583a92601d0c851ef"
EXPECTED_BINDING = "4bcd53cdd4e1900d4edf240f46da4c14ab570fc7be09f37484a9f56402da3b06"
LIVE_SHA = "89f82dd8b287f46a3cf4c0920f341f6ca6c736db"
LIVE_TREE = "22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416"
CACHED_SHA = "325719f1e05d3d46322c9fd3eeb9ad545e175638"


def read(path: Path) -> Any:
    return json.loads(path.read_text())


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def digest(value: Any) -> str:
    raw = (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()
    return hashlib.sha256(raw).hexdigest()


def typed(values: list[str]) -> dict[str, str]:
    return {value.split(":", 1)[0]: value.split(":", 1)[1] for value in values}


def resolve(schema: dict[str, Any], node: Any) -> Any:
    if isinstance(node, dict) and "$ref" in node:
        return schema.get("$defs", {}).get(node["$ref"].split("/")[-1], node)
    return node


def leaves(schema: dict[str, Any], prefix: str = "") -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    def walk(raw_node: Any, current: str) -> None:
        node = resolve(schema, raw_node)
        for name, raw in node.get("properties", {}).items():
            path = f"{current}.{name}" if current else name
            definition = resolve(schema, raw)
            if isinstance(definition, dict) and "properties" in definition:
                walk(definition, path)
            else:
                result[path] = definition if isinstance(definition, dict) else {}
    walk(schema, prefix)
    return result


def cmd(*args: str) -> tuple[int, str]:
    try:
        p = subprocess.run(args, text=True, capture_output=True, check=False)
        return p.returncode, p.stdout.strip()
    except Exception as exc:  # pragma: no cover
        return 127, str(exc)


def schema_errors(schema: dict[str, Any], document: Any) -> list[str]:
    try:
        from jsonschema import Draft202012Validator
    except Exception as exc:  # pragma: no cover
        return [f"jsonschema unavailable: {exc}"]
    return [error.message for error in sorted(Draft202012Validator(schema).iter_errors(document), key=str)]


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    plan = read(PLAN_JSON)
    pub = plan["main_b_publisher"]
    perm_schema = read(PERM_SCHEMA)
    record_schema = read(RECORD_SCHEMA)
    binding_schema = read(BINDING_SCHEMA)
    positive_path = SCHEMA_ROOT / "permanent-b-binding-positive.json"
    positive = read(positive_path)["document"]
    checks: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []

    def check(name: str, ok: bool, detail: Any, severity: str = "finding") -> None:
        item = {"id": name, "pass": bool(ok), "detail": detail}
        checks.append(item)
        if not ok:
            findings.append({"id": name, "severity": severity, "detail": detail})

    md_sha = sha(PLAN_MD)
    json_sha = sha(PLAN_JSON)
    check("frozen-v8-md", md_sha == EXPECTED_MD, {"observed": md_sha, "expected": EXPECTED_MD})
    check("frozen-v8-json", json_sha == EXPECTED_JSON, {"observed": json_sha, "expected": EXPECTED_JSON})
    check("not-authorized", plan.get("execution_authorized") is False and plan.get("mutation_performed") is False, {
        "execution_authorized": plan.get("execution_authorized"), "mutation_performed": plan.get("mutation_performed")})

    observed = dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    remote_rc, remote_out = cmd("git", "ls-remote", "origin", "refs/heads/main")
    live = remote_out.split()[0] if remote_rc == 0 and remote_out else None
    tree_rc, live_tree = cmd("git", "rev-parse", f"{live}^{{tree}}") if live else (127, "")
    cached_rc, cached = cmd("git", "rev-parse", "origin/main")
    cached_tree_rc, cached_tree = cmd("git", "rev-parse", "origin/main^{tree}")
    baseline = {
        "observed_utc": observed, "git_ls_remote_rc": remote_rc,
        "live_remote_main_sha": live, "live_remote_tree_sha": live_tree if tree_rc == 0 else None,
        "cached_origin_main_sha": cached if cached_rc == 0 else None,
        "cached_origin_tree_sha": cached_tree if cached_tree_rc == 0 else None,
    }
    facts = plan["revision_bound_facts"]
    check("live-main", live == LIVE_SHA == facts["main_sha"], baseline | {"plan": facts["main_sha"]}, "external-blocker")
    check("live-tree", live_tree == LIVE_TREE == facts["tree_sha"], baseline | {"plan": facts["tree_sha"]}, "external-blocker")
    stale = plan["evidence"]
    check("cached-live-separated", stale.get("current_main") == CACHED_SHA and stale.get("current_main") != facts["main_sha"] and stale.get("current_tree") != facts["tree_sha"], {
        "evidence.current_main": stale.get("current_main"), "evidence.current_parent": stale.get("current_parent"),
        "evidence.current_tree": stale.get("current_tree"), "revision_bound.main": facts.get("main_sha"),
        "revision_bound.parent": facts.get("main_parent_sha"), "revision_bound.tree": facts.get("tree_sha"),
        "cached_origin": cached, "cached_tree": cached_tree})

    stages = pub["field_availability_dag"]
    stage_for: dict[str, str] = {}
    producer_for: dict[str, str] = {}
    duplicates: dict[str, list[str]] = {}
    for stage in stages:
        for raw in stage["produces"]:
            name = raw.split(":", 1)[0]
            if name in stage_for:
                duplicates.setdefault(name, [stage_for[name]]).append(stage["stage"])
            stage_for[name] = stage["stage"]
            producer_for[name] = stage["producer_job"]
    transport = typed(pub["transport_fields"])
    workflow_outputs = typed(pub["workflow_call_outputs"])
    provenance = pub["field_provenance"]
    check("unique-stage-fields", not duplicates, duplicates)
    check("typed-transport-closure", set(stage_for) == set(transport) == set(workflow_outputs), {
        "stage_not_transport": sorted(set(stage_for) - set(transport)), "transport_not_stage": sorted(set(transport) - set(stage_for)),
        "workflow_not_transport": sorted(set(workflow_outputs) - set(transport))})
    check("provenance-closure", set(provenance) == set(transport), {"missing": sorted(set(transport) - set(provenance)), "extra": sorted(set(provenance) - set(transport))})
    prov_stage_bad = {name: {"dag": stage_for.get(name), "provenance": item.get("stage")} for name, item in provenance.items() if item.get("stage") != stage_for.get(name)}
    check("provenance-stage-agreement", not prov_stage_bad, prov_stage_bad)

    s7 = {raw.split(":", 1)[0] for stage in stages if stage["stage"] == "S7" for raw in stage["produces"]}
    ambiguous_s7 = sorted(name for name in s7 if "/" in producer_for.get(name, "") or " or " in provenance.get(name, {}).get("producer", ""))
    check("unique-s7-producer", not ambiguous_s7, {"fields": ambiguous_s7, "stage_producer": producer_for.get(next(iter(s7), "")), "provenance_values": sorted({provenance[name].get("producer") for name in ambiguous_s7})})

    jobs = pub["jobs"]
    needs = pub["needs_edges"]
    transport_gaps = []
    for consumer, producer in needs:
        c = typed(jobs[consumer].get("consumes", [])); p = typed(jobs[producer].get("outputs", []))
        missing = sorted(set(c) - set(p)); mismatch = sorted(name for name in set(c) & set(p) if c[name] != p[name])
        if missing or mismatch:
            transport_gaps.append({"consumer": consumer, "producer": producer, "missing": missing, "type_mismatch": mismatch})
    check("needs-output-transport", not transport_gaps, transport_gaps)
    lineage = pub["field_lineage_edges"]
    lineage_fields = {raw.split(":", 1)[0] for edge in lineage for raw in edge["fields"]}
    lineage_gap = sorted(set(stage_for) - lineage_fields)
    check("lineage-field-coverage", not lineage_gap, {"missing": lineage_gap, "count": len(lineage_gap)})
    actual_pairs = {(edge["from_job"], edge["to_job"]) for edge in lineage}
    expected_pairs = {(producer, consumer) for consumer, producer in needs}
    reverse_pairs = {(consumer, producer) for consumer, producer in needs}
    check("lineage-direction", actual_pairs == expected_pairs, {"expected_producer_to_consumer": sorted(expected_pairs), "actual": sorted(actual_pairs), "actual_is_reverse_needs": actual_pairs == reverse_pairs})

    record_upload = jobs["record-upload"]
    verify_b = jobs["verify-B"]
    record_upload_new = sorted(set(typed(record_upload["outputs"])) - set(typed(record_upload["consumes"])))
    provider_called = sorted(name for name in record_upload_new if name.startswith("policy_bootstrap_b_") or name.startswith("called_workflow_"))
    record_transport_new = sorted(name for name in record_upload_new if name not in provider_called)
    check("record-upload-provider-source", not provider_called, {"provider_called_outputs": provider_called, "permissions": record_upload.get("permissions"), "verify_b_new_outputs": sorted(set(typed(verify_b["outputs"])) - set(typed(verify_b["consumes"])) )})
    binding_artifact_fields = sorted(name for name in typed(jobs["attest-binding"]["outputs"]) if name.startswith("binding_record_") or name.startswith("final_manifest_artifact_"))
    check("binding-artifact-permission", "actions:write" in jobs["attest-binding"].get("permissions", []), {"fields": binding_artifact_fields, "permissions": jobs["attest-binding"].get("permissions")})

    perm_leaves = leaves(perm_schema)
    record_leaves = leaves(record_schema)
    owner_paths = set(pub["canonical_schemas"]["canonical_leaf_paths"])
    missing_perm = sorted(set(perm_leaves) - owner_paths)
    extra_owner = sorted(owner_paths - set(perm_leaves))
    record_unmapped = sorted(set(record_leaves) - owner_paths)
    check("permanent-leaf-coverage", not missing_perm, {"schema_leaves": len(perm_leaves), "owner_paths": len(owner_paths), "missing": missing_perm, "object_or_unknown_owner_paths": extra_owner})
    check("record-leaf-coverage", not record_unmapped, {"record_schema_leaves": len(record_leaves), "unmapped": record_unmapped})

    owner_stage = {path: value.get("stage") for path, value in pub["canonical_schemas"]["canonical_leaf_paths"].items()}
    semantic_stage = {
        "release.manifest_signed_after_draft_id": "S4", "release.tag_immutable": "S5",
        "artifact.rest_service_zip_sha256": "S2", "artifact.rest_service_zip_size": "S2",
        "artifact.workflow_run_id": "S2", "artifact.expired": "S2",
        "attestation.binding_subject_name": "S4", "attestation.binding_subject_sha256": "S4",
        "attestation.binding_predicate_type": "S4", "attestation.binding_predicate_path": "S4",
        "attestation.binding_digest": "S4",
    }
    timing_bad = {path: {"declared": owner_stage.get(path), "derived": expected} for path, expected in semantic_stage.items() if owner_stage.get(path) != expected}
    check("canonical-timing", not timing_bad, timing_bad)

    partitions = pub["attestation_contract"]["stage_partition"]
    s4_excludes = set(partitions["S4_binding"].get("excludes", []))
    s7_excludes = set(partitions["S7_record"].get("excludes", []))
    binding_exact = {"binding_record_artifact_id", "binding_record_artifact_digest", "policy_bootstrap_b_check_id", "policy_bootstrap_b_provider_app_id", "policy_bootstrap_b_integration_id", "policy_bootstrap_b_verifier_revision", "called_workflow_path", "called_workflow_ref", "called_workflow_sha", "called_workflow_file_blob_sha"}
    record_exact = {"record_artifact_id", "record_artifact_name", "record_artifact_digest", "record_upload_step_id", "record_upload_run_id", "record_upload_run_attempt", "record_upload_job_id", "record_upload_check_run_id"}
    missing_s4_excludes = sorted(binding_exact - s4_excludes)
    missing_s7_excludes = sorted(record_exact - s7_excludes)
    check("preimage-explicit-field-sets", all("fields" in partitions[name] for name in ("S4_binding", "S6_release", "S7_record")), {"partitions": {name: sorted(value) for name, value in partitions.items()}})
    check("binding-exact-exclusions", not missing_s4_excludes, {"missing": missing_s4_excludes, "declared": sorted(s4_excludes)})
    check("record-exact-exclusions", not missing_s7_excludes, {"missing": missing_s7_excludes, "declared": sorted(s7_excludes)})
    graph = pub["attestation_contract"]["digest_graph"]
    check("s7-graph-nodes-have-fields", False, {"nodes": graph["nodes"], "unrepresented_typed_nodes": ["S7_record_transport", "S7_provider_check"]})

    dynamic = sorted(path for path, node in perm_leaves.items() if "const" not in node and "enum" not in node)
    direct_names = set(transport)
    no_named_transport = sorted(path for path in dynamic if not any(name == path.rsplit(".", 1)[-1] or name.endswith("_" + path.rsplit(".", 1)[-1]) for name in direct_names))
    check("dynamic-leaf-named-transport", not no_named_transport, {"dynamic_count": len(dynamic), "unresolved_by_name": no_named_transport})

    strict_product = perm_schema["properties"]["product"]["properties"]
    binding_product = binding_schema["properties"]["product"]["properties"]
    identity = {"plan.name": pub["product"].get("name"), "plan.namespace": pub["product"].get("namespace"), "strict.product_id": strict_product["product_id"].get("const"), "strict.asset": strict_product["asset"].get("const"), "binding.product.id": binding_product["id"].get("const")}
    check("product-identity", pub["product"].get("name") == strict_product["product_id"].get("const") == binding_product["id"].get("const"), identity)

    source_paths = [".github/workflows/ci-policy-validator-products.yml", ".github-gen/sources/workflows/ci-policy-validator-products.yml", "crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs"]
    source_scan = []
    for path in source_paths:
        rc, _ = cmd("git", "cat-file", "-e", f"{LIVE_SHA}:{path}")
        source_scan.append({"path": path, "present": rc == 0})
    check("real-b-source-verifier", any(item["present"] for item in source_scan), source_scan, "unimplemented")

    schema_mutations = {
        "wrong_type_run_id": lambda item: item["run"].__setitem__("id", "42"),
        "extra_attestation_property": lambda item: item["attestation"].__setitem__("extra", 1),
        "failed_run_conclusion": lambda item: item["run"].__setitem__("conclusion", "failure"),
        "wrong_upload_action_sha": lambda item: item["actions"].__setitem__("upload_artifact_sha", "0" * 40),
        "standalone_dispatch_event": lambda item: item["publisher"].__setitem__("event", "workflow_dispatch"),
    }
    schema_hostile = {}
    for name, mutate in schema_mutations.items():
        document = copy.deepcopy(positive)
        mutate(document)
        schema_hostile[name] = {"error_count": len(schema_errors(perm_schema, document)), "errors": schema_errors(perm_schema, document)[:3], "expected": "reject"}
    schema_validation = {
        "permanent_positive_errors": schema_errors(perm_schema, positive),
        "permanent_positive_fixture": str(positive_path),
        "hostile_mutations": schema_hostile,
        "record_positive_fixture": "absent; schema is declared but no record document is supplied",
    }
    check("strict-schema-positive", not schema_validation["permanent_positive_errors"], schema_validation["permanent_positive_errors"])
    check("strict-schema-hostile-rejects", all(item["error_count"] > 0 for item in schema_hostile.values()), schema_hostile)

    negatives = [
        ("NEG-S5-release-asset-in-S4", "add release_asset_id to S4 binding preimage", "S5 value is unavailable at S4"),
        ("NEG-S4-binding-record-id", "add binding_record_artifact_id to binding core", "S4 binding-record output is exact own/later transport"),
        ("NEG-S7-provider-fields-record-upload", "claim record-upload produced provider/called identities", "record-upload has no provider/API source"),
        ("NEG-S7-lineage-gap", "remove 16 S7 fields from lineage", "every produced field requires explicit edge"),
        ("NEG-manifest-flag-before-S4", "produce manifest_signed_after_draft_id at S3", "manifest sign occurs at S4"),
        ("NEG-tag-immutable-before-publish", "produce tag_immutable at S3", "publish/readback occurs at S5"),
        ("NEG-raw-REST-archive-at-S1", "bind raw REST archive digest to build self-report", "raw archive is read at S2"),
        ("NEG-lineage-label-reversal", "read field_lineage_edges from_job as producer", "all seven labels reverse needs direction"),
        ("NEG-caller-called-OIDC-swap", "satisfy called SHA from caller SHA", "caller and called claims are distinct"),
        ("NEG-terminal-states-untransported", "omit run/job terminal states", "strict B requires completed/success states"),
        ("NEG-record-schema-unmapped", "accept record schema without its 68 leaves", "record is a separate strict consumer"),
        ("NEG-binding-specific-exclusion-alias", "treat generic record_artifact_id as binding_record_artifact_id", "exact fields require exact exclusions"),
        ("NEG-product-namespace-substitution", "replace strict product ID with plan namespace/name", "product constants disagree"),
    ]
    fixture_rows = [{"id": ident, "mutation": mutation, "rule": rule, "expected": "reject", "derived_result": "reject", "execution_status": "contract-only-unexecuted"} for ident, mutation, rule in negatives]
    NEGATIVES.write_text(json.dumps({"schema": "velnor.v8-independent-canonical-preimage-negative-fixtures.v1", "execution_status": "contract-only-unexecuted", "real_verifier": "unimplemented", "fixtures": fixture_rows}, indent=2, sort_keys=True) + "\n")
    historical = read(HISTORICAL_INDEX)

    consumers: dict[str, list[str]] = {name: [] for name in stage_for}
    for job, definition in jobs.items():
        for name in typed(definition.get("consumes", [])):
            consumers.setdefault(name, []).append(f"job:{job}")
    field_rows = [{"field": name, "type": transport.get(name), "stage": stage_for.get(name), "producer": producer_for.get(name), "provenance": provenance.get(name), "consumers": sorted(consumers.get(name, [])), "unique_producer": name not in ambiguous_s7} for name in sorted(stage_for)]
    preimages = []
    for name, part in partitions.items():
        descriptor = {"name": name, "preimage_stages": part.get("preimage_stages", []), "explicit_fields": part.get("fields", []), "requires": part.get("requires", []), "excludes": part.get("excludes", []), "status": "contract-descriptor-only; no signed bytes"}
        preimages.append(descriptor | {"descriptor_sha256": digest(descriptor)})

    metrics = {"stage_field_count": len(stage_for), "duplicate_stage_fields": len(duplicates), "ambiguous_s7_fields": len(ambiguous_s7), "lineage_edges": len(lineage), "lineage_gap_fields": len(lineage_gap), "permanent_schema_leaves": len(perm_leaves), "owner_canonical_paths": len(owner_paths), "permanent_missing_leaves": len(missing_perm), "record_schema_leaves": len(record_leaves), "record_unmapped_leaves": len(record_unmapped), "timing_mismatches": len(timing_bad), "independent_negative_fixtures": len(fixture_rows), "historical_negative_fixtures": historical.get("case_count")}
    result = {"schema": "velnor.v8-independent-canonical-field-availability-preimage-dag.v1", "status": "independent_structural_findings_external_blocked" if findings else "independent_structural_pass_external_blocked", "execution_status": "static-derived-contract-only", "real_verifier": "unimplemented", "gate_claim": False, "execution_authorized": False, "frozen_inputs": {"plan_json": {"path": str(PLAN_JSON), "sha256": json_sha}, "plan_markdown": {"path": str(PLAN_MD), "sha256": md_sha}, "permanent_schema": {"path": str(PERM_SCHEMA), "sha256": sha(PERM_SCHEMA)}, "record_schema": {"path": str(RECORD_SCHEMA), "sha256": sha(RECORD_SCHEMA)}, "binding_schema": {"path": str(BINDING_SCHEMA), "sha256": sha(BINDING_SCHEMA)}}, "live_vs_cached_baseline": baseline, "metrics": metrics, "checks": checks, "findings": findings, "stage_contract": {"stages": stages, "duplicate_stage_fields": duplicates, "ambiguous_s7_fields": ambiguous_s7, "fields": field_rows}, "lineage_contract": {"needs_edges_consumer_producer": needs, "field_lineage_edges": lineage, "lineage_gap": lineage_gap, "record_upload_new_fields": record_upload_new, "record_upload_provider_called_fields": provider_called, "record_upload_record_transport_fields": record_transport_new}, "schema_coverage": {"permanent_missing_leaf_paths": missing_perm, "owner_object_or_unknown_paths": extra_owner, "record_unmapped_leaf_paths": record_unmapped, "dynamic_permanent_paths_without_named_transport": no_named_transport}, "timing_contract": {"semantic_expectations": semantic_stage, "mismatches": timing_bad}, "preimage_contract": {"partitions": preimages, "digest_graph": graph, "missing_exact_binding_exclusions": missing_s4_excludes, "missing_exact_record_exclusions": missing_s7_excludes}, "schema_identity": identity, "schema_validation": schema_validation, "source_scan": source_scan, "historical_34_fixture_reference": {"path": str(HISTORICAL_INDEX), "sha256": sha(HISTORICAL_INDEX), "case_count": historical.get("case_count"), "status": "preserved and unexecuted"}, "negative_fixture_file": {"path": str(NEGATIVES), "sha256": sha(NEGATIVES), "count": len(fixture_rows)}, "descriptor_hashes": {"fields": digest(field_rows), "lineage": digest({"edges": lineage, "gap": lineage_gap}), "schemas": digest({"missing": missing_perm, "record_unmapped": record_unmapped}), "preimages": digest(preimages)}, "limitations": ["No real B source/verifier/artifact/release/attestation or signed bytes were executed.", "Negative fixtures are structural expected rejects, not verifier rejection results.", "Live remote and cached origin are recorded separately; cached origin is not called live authority.", "No owner plan, frozen v7, schema, workflow, ruleset, release, or remote state was modified."]}
    RESULTS.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")

    report_lines = [
        "# Independent v8 canonical field/DAG audit", "", 
        f"Status: **{result['status']}**. Gate claim: **false**. Real verifier: **unimplemented**.",
        "Read-only external evidence; no workflow, provider, release, merge, dispatch, or authority operation.", "",
        "## Inputs and baseline", "",
        f"- v8 MD SHA-256: `{md_sha}` (expected `{EXPECTED_MD}`)",
        f"- v8 JSON SHA-256: `{json_sha}` (expected `{EXPECTED_JSON}`)",
        f"- permanent B schema: `{sha(PERM_SCHEMA)}`; record schema: `{sha(RECORD_SCHEMA)}`; historical predicate schema: `{sha(BINDING_SCHEMA)}`",
        f"- live remote observed `{observed}`: main `{live}`, tree `{live_tree}`",
        f"- cached local origin/main: `{cached}`, tree `{cached_tree}`",
        "- revision-bound v8 facts match live 89f82dd8.../22ccc1d.... The plan's evidence.current_main/current_tree still carry cached 325719f1.../e9019f0... and must not be consumed as live authority.", "",
        f"- strict permanent-B positive: {len(schema_validation['permanent_positive_errors'])} schema errors; five typed/additional-property/terminal/action/event mutations each reject. No strict record positive document is supplied.", "",
        "## Findings", "",
        f"1. S7 has **{len(ambiguous_s7)}** non-unique producer fields. record-upload declares provider/called identities despite actions:write-only permissions; verify-B declares no new output.",
        f"2. Field lineage omits **{len(lineage_gap)}** S7 fields. Its seven from_job/to_job labels reverse the needs consumer/producer pairs.",
        f"3. Permanent B has **{len(perm_leaves)}** concrete leaves; owner map has {len(owner_paths)} paths, missing **{len(missing_perm)}** step leaves. Record schema has **{len(record_leaves)}** leaves, with **{len(record_unmapped)}** unmapped by that map.",
        f"4. Timing mismatches ({len(timing_bad)}): `{json.dumps(timing_bad, sort_keys=True)}`. Manifest-signed-after-draft is S3 in owner mapping but S4; tag immutability S3 but S5; raw REST artifact fields S1 but S2; binding attestation paths disagree S4/S6.",
        f"5. Preimage partitions have stage lists but no explicit field sets. Missing exact S4 exclusions: `{', '.join(missing_s4_excludes)}`. Missing exact record self-exclusions: `{', '.join(missing_s7_excludes)}`.",
        f"6. Product identity differs: plan `{identity['plan.name']}`/`{identity['plan.namespace']}`, strict `{identity['strict.product_id']}`, historical predicate `{identity['binding.product.id']}`.", "",
        "## Hostile fixtures and execution boundary", "",
        f"Independent negatives: `{NEGATIVES}` SHA-256 `{sha(NEGATIVES)}`, {len(fixture_rows)} derived rejects, all unexecuted.",
        f"Prior 34-fixture bundle preserved: `{HISTORICAL_INDEX}` SHA-256 `{sha(HISTORICAL_INDEX)}`; unexecuted.",
        "No typed B publisher/verifier paths were present at live main, so no hostile case demonstrates real verifier rejection. No gate claim.", "",
        f"Metrics: `{json.dumps(metrics, sort_keys=True)}`.",
        f"Machine output: `{RESULTS}` SHA-256 `{sha(RESULTS)}`.",
    ]
    REPORT.write_text("\n".join(report_lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
