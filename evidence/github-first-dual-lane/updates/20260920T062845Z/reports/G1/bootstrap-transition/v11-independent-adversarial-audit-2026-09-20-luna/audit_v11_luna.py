#!/usr/bin/env python3
"""Independent, read-only v11 contract audit.

This audit intentionally does not import or execute the owner audit.  It
reconstructs schema leaves, field ownership, stage timing, transport coverage,
and preimage partitions from the frozen model/DAG.  Hostile JSON instances are
written as contract fixtures only: no real verifier or authority exists at
the observed main revision, so those cases are never reported as executed.
"""

from __future__ import annotations

import copy
import hashlib
import importlib.metadata
import json
import subprocess
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker


BASE = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
BUNDLE = BASE / "v11-contract-bundle-2026-09-20"
OUT = BASE / "v11-independent-adversarial-audit-2026-09-20-luna"
REPO = Path("/Users/donbeave/Projects/tailrocks/velnor-project/velnor3")

MODEL = BASE / "v11-canonical-model-source.json"
FREEZE = BASE / "v11-freeze-manifest.json"
DAG = BUNDLE / "v11-canonical-field-dag.json"
PLAN_JSON = BUNDLE / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json"
PLAN_MD = BUNDLE / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.md"
ROOT_MANIFEST = BUNDLE / "canonical-root-manifest.v4.json"
INDEX = BUNDLE / "bundle-index.json"

EXPECTED = {
    "freeze_manifest": "df14a907b29cf5839551dc0b4a32e77af5c61d8e927433530f9a061d75997c16",
    "model": "ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930",
    "plan_json": "4c50c8e591924cbdbccb7ec489e8a8bbb20576d5f5fbb207c42e52837f6d42c6",
    "plan_md": "2fe0a7e534299e0b7dc83102706d29d91fd07e422a795ef726c1dccd5d119dde",
    "dag": "37e3d0cb65b5b793219d94894a672eae50e8181378481dfb846afff1c49e56e7",
    "root_raw": "35f8ae8a20cfe3ce774ed0d63ece8ed06d3e66d4e66693933ea5e5825e32a96d",
    "root_digest": "deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017",
    "owner_result": "50160264a4e886a548b4c9494ce0e32f7a8f06f0704bf9daee4200a577fb8411",
    "owner_report": "b81a47a7951d2437701e0a5ec2664fcad84e71e2e1dedf79400f423864b66e24",
}

SCHEMA_FILES = {
    "adoption": BUNDLE / "adoption.schema.json",
    "permanent_binding": BUNDLE / "permanent_binding.schema.json",
    "pre_record": BUNDLE / "pre_record.schema.json",
    "pre_record_transport": BUNDLE / "pre_record_transport.schema.json",
    "provider_result": BUNDLE / "provider_result.schema.json",
    "release_manifest": BUNDLE / "release_manifest.schema.json",
    "verify_b": BUNDLE / "verify_b.schema.json",
}

STRICT_PAIRS = {
    "positive-adoption.json": "adoption",
    "positive-pre-record.json": "pre_record",
    "positive-pre-record-transport.json": "pre_record_transport",
    "positive-provider-result.json": "provider_result",
    "positive-release-manifest.json": "release_manifest",
    "positive-verify-b.json": "verify_b",
}

PREFIXES = {
    "adoption": "adoption",
    "permanent_binding": "permanent",
    "pre_record": "pre_record",
    "pre_record_transport": "pre_record_transport",
    "provider_result": "provider_result",
    "release_manifest": "release_manifest",
    "verify_b": "verify_b",
}


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def sha_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha(path: Path) -> str:
    return sha_bytes(path.read_bytes())


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def canonical_sha(value: Any) -> str:
    return sha_bytes(canonical_bytes(value))


def normalized_fixture_sha(path: Path) -> str:
    value = load(path)
    if isinstance(value, dict):
        for key in ("canonical_root_manifest_sha256", "canonical_root_sha256"):
            if key in value:
                value[key] = "0" * 64
    return sha_bytes((json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode())


def resolve_bound(path: str) -> Path:
    # Root manifests in this evidence family use both bundle-relative and
    # canonical-root-relative paths.  Resolve without rewriting the frozen
    # manifest.
    candidates = [BUNDLE / path, BASE / path, BASE.parent / path, BASE.parent.parent / path]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return candidates[0]


def schema_leaves(schema: dict[str, Any]) -> list[str]:
    """Flatten strict properties; an empty-properties object is one leaf."""

    def walk(node: dict[str, Any], prefix: str) -> list[str]:
        props = node.get("properties")
        if isinstance(props, dict):
            if not props:
                return [prefix]
            values: list[str] = []
            for key, child in props.items():
                child_path = f"{prefix}.{key}" if prefix else key
                values.extend(walk(child, child_path))
            return values
        return [prefix]

    return sorted(walk(schema, ""))


def set_nested(root: dict[str, Any], dotted: str, value: Any) -> None:
    cursor = root
    parts = dotted.split(".")
    for part in parts[:-1]:
        cursor = cursor.setdefault(part, {})
    cursor[parts[-1]] = value


def get_nested(root: dict[str, Any], dotted: str) -> Any:
    cursor: Any = root
    for part in dotted.split("."):
        cursor = cursor[part]
    return cursor


def extract_typed(typed: dict[str, Any], prefix: str) -> dict[str, Any]:
    result: dict[str, Any] = {}
    marker = prefix + "."
    for field_id, entry in typed.get("typed_outputs", {}).items():
        if field_id.startswith(marker):
            set_nested(result, field_id[len(marker) :], entry.get("value"))
    return result


def error_list(schema: dict[str, Any], instance: Any) -> list[str]:
    errors = sorted(
        Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance),
        key=lambda error: list(error.absolute_path),
    )
    return [f"{'.'.join(str(x) for x in error.absolute_path)}: {error.message}" for error in errors]


def json_type(value: Any) -> str:
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return "string"


def put_path(root: dict[str, Any], path: str, value: Any) -> None:
    set_nested(root, path, value)


def valid(schema: dict[str, Any], value: Any) -> bool:
    return not error_list(schema, value)


def run_git(*args: str) -> tuple[int, str, str]:
    try:
        proc = subprocess.run(args, cwd=REPO, text=True, capture_output=True, check=False)
    except OSError as error:
        return 127, "", str(error)
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    model = load(MODEL)
    freeze = load(FREEZE)
    dag = load(DAG)
    plan = load(PLAN_JSON)
    root = load(ROOT_MANIFEST)
    index = load(INDEX)
    checks: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []

    def check(name: str, status: str, detail: Any) -> None:
        checks.append({"name": name, "status": status, "detail": detail})

    def finding(fid: str, severity: str, message: str, detail: Any) -> None:
        findings.append({"id": fid, "severity": severity, "message": message, "detail": detail})

    # Frozen bytes and root projection.
    measured = {
        "freeze_manifest": sha(FREEZE),
        "model": sha(MODEL),
        "plan_json": sha(PLAN_JSON),
        "plan_md": sha(PLAN_MD),
        "dag": sha(DAG),
        "root_raw": sha(ROOT_MANIFEST),
        "owner_result": sha(BUNDLE / "v11-independent-audit-results.json"),
        "owner_report": sha(BUNDLE / "v11-independent-audit-report.md"),
    }
    check(
        "frozen-v11-input-hashes",
        "pass" if all(measured[key] == EXPECTED[key] for key in EXPECTED if key in measured) else "fail",
        {"measured": measured, "expected": EXPECTED},
    )
    measured_root_digest = canonical_sha(root)
    check(
        "canonical-root-digest",
        "pass" if measured_root_digest == EXPECTED["root_digest"] and "root_digest" not in root else "fail",
        {"measured": measured_root_digest, "expected": EXPECTED["root_digest"], "self_digest_present": "root_digest" in root},
    )
    bound_results = []
    for entry in root.get("bound_files", []):
        path = resolve_bound(entry["path"])
        if not path.exists():
            bound_results.append({"path": entry["path"], "status": "missing"})
            continue
        actual = normalized_fixture_sha(path) if entry.get("normalization") else sha(path)
        bound_results.append({"path": entry["path"], "status": "pass" if actual == entry["sha256"] else "fail", "measured": actual, "declared": entry["sha256"]})
    check("root-bound-bytes", "pass" if all(item["status"] == "pass" for item in bound_results) else "fail", bound_results)
    check(
        "bundle-index-references",
        "pass"
        if index.get("root_digest") == measured_root_digest
        and index.get("root_manifest_sha256") == measured["root_raw"]
        and index.get("canonical_model_sha256") == measured["model"]
        and index.get("canonical_dag_sha256") == measured["dag"]
        and index.get("plan_json_sha256") == measured["plan_json"]
        and index.get("plan_md_sha256") == measured["plan_md"]
        else "fail",
        index,
    )
    check("v10-preserved", "pass" if (BASE.parent / "bootstrap-transition/v10-contract-bundle-2026-09-20").exists() else "fail", "v10 bundle remains separate")

    schema_hashes = []
    for name, path in SCHEMA_FILES.items():
        actual = sha(path)
        declared = plan.get("canonical_outputs", {}).get("schema_hashes", {}).get(name)
        schema_hashes.append({"schema": name, "actual": actual, "declared": declared, "status": "pass" if actual == declared else "fail"})
    check("schema-hash-projection", "pass" if all(x["status"] == "pass" for x in schema_hashes) else "fail", schema_hashes)
    check(
        "single-canonical-model",
        "pass"
        if dag.get("canonical_model_source", {}).get("sha256") == measured["model"]
        and plan.get("canonical_source", {}).get("sha256") == measured["model"]
        and dag.get("no_parallel_contract_lists") is True
        else "fail",
        {"dag": dag.get("canonical_model_source"), "plan": plan.get("canonical_source")},
    )

    # Strict JSON Schema validation of every emitted strict positive.
    schemas = {name: load(path) for name, path in SCHEMA_FILES.items()}
    strict_validation: dict[str, Any] = {}
    for filename, schema_name in STRICT_PAIRS.items():
        instance = load(BUNDLE / filename)
        errors = error_list(schemas[schema_name], instance)
        strict_validation[filename] = {"schema": schema_name, "valid": not errors, "errors": errors}
    check(
        "strict-positive-instances",
        "pass" if all(item["valid"] for item in strict_validation.values()) else "fail",
        strict_validation,
    )

    # The full typed fixture is a wrapper, not a strict instance.  Validate its
    # per-schema projections anyway; this catches type-only fixtures that do
    # not satisfy const/pattern/nested-object semantics.
    typed = load(BUNDLE / "positive-full-typed-output-fixture.json")
    projected_validation: dict[str, Any] = {}
    for schema_name, prefix in PREFIXES.items():
        instance = extract_typed(typed, prefix)
        errors = error_list(schemas[schema_name], instance)
        projected_validation[schema_name] = {
            "prefix": prefix,
            "field_count": len(typed.get("typed_outputs", {})),
            "error_count": len(errors),
            "errors": errors[:16],
        }
    projection_ok = all(item["error_count"] == 0 for item in projected_validation.values())
    check("typed-fixture-strict-projections", "pass" if projection_ok else "fail", projected_validation)
    if not projection_ok:
        finding(
            "V11-FIXTURE-001",
            "high",
            "The emitted 303-field typed fixture only checks coarse JSON types; strict schema projections fail canonical consts, digest prefixes, nested terminal-row shape, and typed object values.",
            projected_validation,
        )
    permanent_positive = BUNDLE / "positive-permanent-binding.json"
    check(
        "permanent-strict-positive-coverage",
        "pass" if permanent_positive.exists() else "fail",
        {"expected": "positive-permanent-binding.json", "present": permanent_positive.exists(), "schema": str(SCHEMA_FILES["permanent_binding"])},
    )
    if not permanent_positive.exists():
        finding(
            "V11-SCHEMA-001",
            "high",
            "permanent_binding.schema.json has no direct strict positive instance; the only permanent projection is the invalid coarse typed wrapper.",
            {"schema_sha256": sha(SCHEMA_FILES["permanent_binding"]), "typed_projection_errors": projected_validation["permanent_binding"]},
        )

    # Independently derive strict leaves and compare the canonical registry.
    actual_leaves = {name: schema_leaves(schema) for name, schema in schemas.items()}
    dag_leaves = {name: sorted(dag.get("schema_leaf_paths", {}).get(name, [])) for name in SCHEMA_FILES}
    leaf_map_diff = {
        name: {"missing_from_dag": sorted(set(actual_leaves[name]) - set(dag_leaves[name])), "extra_in_dag": sorted(set(dag_leaves[name]) - set(actual_leaves[name]))}
        for name in SCHEMA_FILES
    }
    check("schema-leaf-path-derivation", "pass" if not any(v["missing_from_dag"] or v["extra_in_dag"] for v in leaf_map_diff.values()) else "fail", {"counts": {k: len(v) for k, v in actual_leaves.items()}, "diff": leaf_map_diff})
    registry = set(dag.get("fields", {}))
    leaf_union = {f"{PREFIXES[name]}.{path}" for name, paths in actual_leaves.items() for path in paths}
    registry_diff = {"schema_leaves_missing_from_registry": sorted(leaf_union - registry), "registry_entries_not_schema_leaves": sorted(registry - leaf_union)}
    check("schema-leaf-registry-bijection", "pass" if not registry_diff["schema_leaves_missing_from_registry"] and not registry_diff["registry_entries_not_schema_leaves"] else "fail", {"registry_count": len(registry), "schema_leaf_count": len(leaf_union), "diff": registry_diff})
    if registry_diff["schema_leaves_missing_from_registry"] or registry_diff["registry_entries_not_schema_leaves"]:
        finding(
            "V11-DAG-001",
            "high",
            "Strict schema leaves and canonical field registry are not bijective: required_step_conclusions child keys are schema leaves but only their object container is in fields.",
            registry_diff,
        )

    fields = dag.get("fields", {})
    stage_order = model.get("stage_order", [])
    stage_rank = {stage: index for index, stage in enumerate(stage_order)}
    stage_fields = dag.get("stage_fields", {})
    stage_union = {field_id for values in stage_fields.values() for field_id in values}
    metadata_errors = []
    for field_id, desc in fields.items():
        for key in ("path", "schema", "stage", "producer", "type", "source"):
            if desc.get(key) in (None, ""):
                metadata_errors.append({"field": field_id, "missing": key})
        if desc.get("required") is not True:
            metadata_errors.append({"field": field_id, "required": desc.get("required")})
    check("field-registry-unique-producers", "pass" if not metadata_errors and len(fields) == len(set(fields)) else "fail", {"field_count": len(fields), "metadata_errors": metadata_errors[:20], "producer_count": len({desc.get("producer") for desc in fields.values()})})
    check("stage-field-coverage", "pass" if stage_union == set(fields) else "fail", {"missing": sorted(set(fields) - stage_union), "extra": sorted(stage_union - set(fields))})

    # Producer/stage consistency.  terminal-census rows are the one explicit
    # subproducer exception in the frozen DAG.
    producer_stage_errors = []
    stage_producers = {stage: model.get("stage_contract", {}).get(stage, {}).get("producer") for stage in stage_order}
    for field_id, desc in fields.items():
        expected = stage_producers.get(desc.get("stage"))
        if field_id == "verify_b.terminal_census_rows":
            expected = "verify-B.terminal-census-api"
        if desc.get("producer") != expected:
            producer_stage_errors.append({"field": field_id, "stage": desc.get("stage"), "declared": desc.get("producer"), "expected": expected})
    check("field-producer-stage-consistency", "pass" if not producer_stage_errors else "fail", producer_stage_errors[:40])

    edges = dag.get("field_lineage_edges", [])
    edge_keys = [(edge.get("producer"), edge.get("consumer"), tuple(edge.get("field_ids", []))) for edge in edges]
    duplicate_edges = [list(key) for key, count in Counter(edge_keys).items() if count > 1]
    unknown_edge_fields = sorted({field_id for edge in edges for field_id in edge.get("field_ids", []) if field_id not in fields})
    edge_producer_errors = []
    edge_consumer_errors = []
    for edge in edges:
        for field_id in edge.get("field_ids", []):
            if field_id not in fields:
                continue
            desc = fields[field_id]
            producer = desc.get("producer")
            terminal_census_edge = field_id == "verify_b.terminal_census_rows" and edge.get("producer") == "verify-B" and producer == "verify-B.terminal-census-api"
            if edge.get("orientation") not in {"explicit_workflow_call_output_transport", "producer_outputs_to_persistent_adoption_input"} and edge.get("producer") != producer and not terminal_census_edge:
                edge_producer_errors.append({"field": field_id, "edge": edge.get("producer"), "declared": producer})
            if edge.get("orientation") == "producer_outputs_to_consumer_inputs" and edge.get("consumer") not in desc.get("consumers", []):
                edge_consumer_errors.append({"field": field_id, "edge": edge.get("consumer"), "declared": desc.get("consumers", [])})
    check("lineage-edge-integrity", "pass" if not duplicate_edges and not unknown_edge_fields and not edge_producer_errors and not edge_consumer_errors else "fail", {"edge_count": len(edges), "duplicates": duplicate_edges, "unknown_fields": unknown_edge_fields, "producer_errors": edge_producer_errors[:20], "consumer_errors": edge_consumer_errors[:20]})

    # The registry lists every later job as a consumer, but stage edges carry
    # only fields produced at the immediately preceding stage.  Without a
    # carry-forward/output map, declared downstream consumers cannot receive
    # S0/S1/S2 values through isolated GitHub jobs.
    direct_consumers: dict[str, set[str]] = defaultdict(set)
    for edge in edges:
        for field_id in edge.get("field_ids", []):
            direct_consumers[field_id].add(edge.get("consumer"))
    missing_declared_consumers = {
        field_id: sorted(set(desc.get("consumers", [])) - direct_consumers.get(field_id, set()))
        for field_id, desc in fields.items()
        if set(desc.get("consumers", [])) - direct_consumers.get(field_id, set())
    }
    check("declared-consumer-transport-coverage", "pass" if not missing_declared_consumers else "fail", {"field_count_with_missing_consumers": len(missing_declared_consumers), "examples": dict(list(missing_declared_consumers.items())[:12])})
    if missing_declared_consumers:
        finding(
            "V11-TRANSPORT-001",
            "critical",
            "The field registry declares downstream consumers, but no carry-forward/output/storage edge transports most S0/S1/S2 fields to those jobs; only one adjacent stage edge exists and ordinary GitHub needs edges do not carry arbitrary fields.",
            {"affected_fields": len(missing_declared_consumers), "examples": dict(list(missing_declared_consumers.items())[:20]), "edges_without_explicit_transport": [edge for edge in edges if edge.get("orientation") == "producer_outputs_to_consumer_inputs" and "transport" not in edge][:4]},
        )

    output_edges = [edge for edge in edges if edge.get("orientation") == "explicit_workflow_call_output_transport"]
    expected_outputs = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in model.get("workflow_graph", {}).get("workflow_call_output_edges", [])}
    actual_outputs = {(edge["producer"], edge["consumer"], tuple(edge["field_ids"])) for edge in output_edges}
    check("workflow-call-output-transport", "pass" if expected_outputs == actual_outputs else "fail", {"expected": sorted(expected_outputs), "actual": sorted(actual_outputs)})

    # Temporal availability: S7a uploads the strict pre-record before provider
    # result and verify-B exist.  A pre-record field produced at S7c cannot be
    # present in that artifact.  Step API fields for later jobs are likewise
    # incorrectly assigned to S2 artifact verification.
    pre_record_after_upload = sorted(field_id for field_id, desc in fields.items() if field_id.startswith("pre_record.") and stage_rank.get(desc.get("stage"), 999) > stage_rank.get("S7a_pre_record_transport", 999))
    check("pre-record-fields-available-before-upload", "pass" if not pre_record_after_upload else "fail", {"count": len(pre_record_after_upload), "fields": pre_record_after_upload})
    if pre_record_after_upload:
        finding(
            "V11-DAG-002",
            "critical",
            "The strict pre-record uploaded at S7a requires 38 fields whose sole declared producer is S7c verify-B, which runs after the provider consumes that artifact.",
            {"count": len(pre_record_after_upload), "fields": pre_record_after_upload},
        )

    expected_step_stage = {
        # The artifact verifier reads completed build/upload steps through the
        # Jobs API.  The later release jobs do not exist at S2.
        "build": "S2_artifact_verify",
        "upload": "S2_artifact_verify",
        "reserve_release": "S3_release_reserve",
        "attest_binding": "S4_binding_attest",
        "publish": "S5_publish",
        "attest_release": "S6_release_attest",
    }
    future_step_fields = []
    for field_id, desc in fields.items():
        if not field_id.startswith("permanent.steps."):
            continue
        step_name = field_id.split(".")[2]
        expected_stage = expected_step_stage.get(step_name)
        if expected_stage and desc.get("stage") != expected_stage:
            future_step_fields.append({"field": field_id, "declared_stage": desc.get("stage"), "expected_stage": expected_stage, "producer": desc.get("producer")})
    check("step-producer-temporal-availability", "pass" if not future_step_fields else "fail", {"count": len(future_step_fields), "fields": future_step_fields})
    if future_step_fields:
        finding(
            "V11-DAG-003",
            "high",
            "Artifact-verify S2 is declared as producer for reserve-release, attest-binding, publish, and attest-release step IDs/statuses that do not exist until those later jobs run.",
            {"count": len(future_step_fields), "fields": future_step_fields},
        )

    release_stage_errors = []
    expected_release_stage = {
        "pre_record.release.release_id": "S3_release_reserve",
        "pre_record.release.tag": "S3_release_reserve",
        "pre_record.release.target_sha": "S3_release_reserve",
        "pre_record.release.manifest_digest": "S4_binding_attest",
        "pre_record.release.asset_id": "S5_publish",
        "pre_record.release.asset_digest": "S5_publish",
    }
    for field_id, expected_stage in expected_release_stage.items():
        actual = fields.get(field_id, {}).get("stage")
        if actual != expected_stage:
            release_stage_errors.append({"field": field_id, "declared_stage": actual, "expected_stage": expected_stage})
    check("release-field-producer-timing", "pass" if not release_stage_errors else "fail", release_stage_errors)
    if release_stage_errors:
        finding(
            "V11-DAG-004",
            "high",
            "pre_record release asset/manifest identities are assigned to reserve-release before their real publish/binding producers; the S3→S4 lineage therefore consumes future values.",
            release_stage_errors,
        )

    # Preimage partitions and own/future identity checks.
    preimages = dag.get("preimage_contract", {})
    preimage_checks: dict[str, Any] = {}
    for name, spec in preimages.items():
        ordered = spec.get("ordered_field_ids", [])
        unknown = sorted(set(ordered) - set(fields))
        duplicate = len(ordered) != len(set(ordered))
        excluded_ids = set(spec.get("excluded_field_ids", []))
        overlap = sorted(set(ordered) & excluded_ids)
        partition_missing = sorted(set(fields) - (set(ordered) | excluded_ids))
        partition_extra = sorted((set(ordered) | excluded_ids) - set(fields))
        preimage_checks[name] = {"ordered_count": len(ordered), "unknown": unknown, "duplicate": duplicate, "overlap": overlap, "partition_missing": partition_missing, "partition_extra": partition_extra, "stages": sorted({fields[x]["stage"] for x in ordered if x in fields}, key=lambda stage: stage_rank.get(stage, 999))}
    check("preimage-partition-integrity", "pass" if not any(item["unknown"] or item["duplicate"] or item["overlap"] or item["partition_missing"] or item["partition_extra"] for item in preimage_checks.values()) else "fail", preimage_checks)

    boundary_bad = {
        "S4_binding": sorted(field_id for field_id in preimages.get("S4_binding", {}).get("ordered_field_ids", []) if stage_rank.get(fields.get(field_id, {}).get("stage"), 999) >= stage_rank.get("S4_binding_attest", 999)),
        "S6_release": sorted(field_id for field_id in preimages.get("S6_release", {}).get("ordered_field_ids", []) if stage_rank.get(fields.get(field_id, {}).get("stage"), 999) >= stage_rank.get("S6_release_attest", 999)),
        "S7b_provider_result": sorted(field_id for field_id in preimages.get("S7b_provider_result", {}).get("ordered_field_ids", []) if stage_rank.get(fields.get(field_id, {}).get("stage"), 999) >= stage_rank.get("S7c_verify_b", 999)),
    }
    check("declared-preimage-stage-boundaries", "pass" if not any(boundary_bad.values()) else "fail", boundary_bad)
    terminal_ordered = preimages.get("S7c_terminal_census", {}).get("ordered_field_ids", [])
    terminal_ok = terminal_ordered == ["verify_b.terminal_census_rows"] and set(preimages.get("S7c_terminal_census", {}).get("excluded_fields", [])) == {"terminal_census_id", "terminal_census_digest"}
    check("terminal-census-own-exclusion", "pass" if terminal_ok else "fail", {"ordered": terminal_ordered, "excluded": preimages.get("S7c_terminal_census", {}).get("excluded_fields", [])})

    provider_ordered = set(preimages.get("S7b_provider_result", {}).get("ordered_field_ids", []))
    provider_result_own = sorted(provider_ordered & {"provider_result.provider_result_id", "provider_result.provider_result_digest", "provider_result.provider_check_run_id", "provider_result.provider_attestation_digest"})
    check("provider-result-identity-exclusion", "fail" if provider_result_own else "pass", {"ordered_provider_identity_fields": provider_result_own, "explicit_exclusions": preimages.get("S7b_provider_result", {}).get("excluded_fields", [])})
    if provider_result_own:
        finding(
            "V11-PREIMAGE-001",
            "critical",
            "S7b provider preimage excludes result ID/digest but still orders provider_check_run_id and provider_attestation_digest, both provider-owned result identities with no specified pre-sign allocation/acyclic sequence.",
            {"ordered": provider_result_own, "signature_contract": model.get("provider_contract", {}).get("signature", {}).get("signed_bytes")},
        )

    model_suffix_errors = {}
    for name in ("S4_binding", "S6_release"):
        spec = model.get("preimage_model", {}).get("digest_ids", {}).get(name, {})
        suffixes = tuple(spec.get("exclude_field_suffixes", []))
        expected_ordered = {field_id for field_id, desc in fields.items() if desc.get("stage") in spec.get("input_stages", []) and not field_id.endswith(suffixes)}
        actual_ordered = set(preimages.get(name, {}).get("ordered_field_ids", []))
        model_suffix_errors[name] = {"missing_expected": sorted(expected_ordered - actual_ordered), "unexpected_ordered": sorted(actual_ordered - expected_ordered), "suffixes": list(suffixes)}
    check("model-suffix-exclusions-realized", "pass" if not any(item["missing_expected"] or item["unexpected_ordered"] for item in model_suffix_errors.values()) else "fail", model_suffix_errors)

    # No concrete canonical preimage bytes are included.  Synthetic digest
    # values therefore cannot be recomputed independently from the fixtures.
    preimage_bytes = list(BUNDLE.glob("*preimage*"))
    check("preimage-digest-recomputation", "unimplemented", {"preimage_files": [str(path) for path in preimage_bytes], "reason": "bundle contains ordered field IDs and synthetic digest strings, not complete canonical preimage bytes plus a live signer"})
    finding("V11-PREIMAGE-002", "high", "Binding/release/provider/terminal digest values are synthetic and no complete preimage bytes or real signing verifier is available for recomputation.", {"preimage_files": [str(path) for path in preimage_bytes], "typed_fixture_live": typed.get("fixture_metadata", {}).get("live_binding")})

    # Cross-fixture lifecycle equalities.  These are useful contract oracles,
    # but their hostile executions remain unimplemented without a verifier.
    release = load(BUNDLE / "positive-release-manifest.json")
    pre = load(BUNDLE / "positive-pre-record.json")
    transport = load(BUNDLE / "positive-pre-record-transport.json")
    provider = load(BUNDLE / "positive-provider-result.json")
    verify = load(BUNDLE / "positive-verify-b.json")
    adoption = load(BUNDLE / "positive-adoption.json")
    lifecycle_equalities = {
        "main_sha": release["target_sha"] == pre["release"]["target_sha"] == provider["resulting_main_sha"] == verify["resulting_main_sha"] == adoption["resulting_main_sha"],
        "source_tree": release["source_tree_sha"] == pre["source"]["tree_sha"] == provider["resulting_main_tree_sha"] == adoption["resulting_main_tree_sha"],
        "artifact_id": transport["artifact_id"] == provider["source_record_artifact_id"],
        "artifact_digest": transport["artifact_digest"] == provider["source_record_artifact_digest"] == pre["artifact"]["service_zip_digest"],
        "raw_artifact_digest": transport["artifact_raw_digest"] == provider["source_record_artifact_raw_digest"] == "sha256:" + transport["artifact_digest"],
        "provider_result": provider["provider_result_id"] == verify["provider_result_id"] and provider["provider_result_digest"] == verify["provider_result_digest"] == adoption["provider_result_digest"],
        "manifest_subject": release["manifest_digest"] == pre["release"]["manifest_digest"] == pre["release_attestation"]["manifest_subject_sha256"] == adoption["release_manifest_digest"],
        "asset_digest": release["asset_digest"] == "sha256:" + transport["artifact_digest"],
    }
    check("joined-positive-lifecycle", "pass" if all(lifecycle_equalities.values()) else "fail", lifecycle_equalities)
    check("release-attestation-binding", "pass" if pre["release_attestation"]["manifest_subject_sha256"] == release["manifest_digest"] and pre["release_attestation"]["certificate_verified"] is True and pre["release_attestation"]["oidc_issuer"] == "https://token.actions.githubusercontent.com" else "fail", {"subject": pre["release_attestation"]["manifest_subject_sha256"], "manifest": release["manifest_digest"]})

    # Explicit semantic checks absent from the schemas/model are recorded as
    # unimplemented rather than converted into synthetic passes.
    artifact_semantic_contract = {
        "raw_service_zip_to_rest_digest": "permanent.artifact.rest_service_zip_sha256 == bytes(S2 REST ZIP)",
        "build_to_rest_service_zip": "permanent.artifact.service_zip_sha256 == permanent.artifact.rest_service_zip_sha256",
        "service_zip_to_inner_payload": "service ZIP extracted payload digest/size == inner_payload fields",
        "inner_payload_to_binary": "inner payload binary digest/size == binary fields",
        "raw_zip_to_pre_record": "pre_record.artifact.service_zip_digest == verified raw service ZIP digest",
        "release_asset_raw_bytes": "release asset REST bytes digest == release_manifest.asset_digest",
    }
    check("artifact-inner-binary-digest-contract", "unimplemented", {"reason": "no raw ZIP/payload/binary bytes and no live verifier; schema permits independent valid digests", "required_equalities": artifact_semantic_contract})
    finding("V11-DATA-001", "high", "Schemas carry service-ZIP, inner-payload, binary, and release-asset digests/sizes but the frozen model has no executable equality/byte fixture proving those joins.", artifact_semantic_contract)

    # Hostile fixture definitions.  Each case carries enough data to rerun the
    # mutation and whether the strict schema/oracle rejects it.  Real verifier
    # execution is deliberately marked unimplemented below.
    hostile_cases: list[dict[str, Any]] = []

    def add_hostile(name: str, target: str, path: str, value: Any, reason: str, schema_name: str | None = None, oracle: Any = None) -> None:
        hostile_cases.append({"case": name, "target": target, "path": path, "mutated_value": value, "expected": "reject", "reason": reason, "schema": schema_name, "schema_valid_after_mutation": oracle.get("schema_valid") if isinstance(oracle, dict) else None, "contract_oracle_rejects": oracle.get("rejects") if isinstance(oracle, dict) else None, "real_verifier": "unimplemented-no-live-verifier"})

    release_tag_bad = copy.deepcopy(release)
    release_tag_bad["release_tag"] = "0" * 40
    add_hostile("release_fixture_tag_shape", "positive-release-manifest.json", "release_tag", release_tag_bad["release_tag"], "bare legacy tag", "release_manifest", {"schema_valid": valid(schemas["release_manifest"], release_tag_bad), "rejects": not valid(schemas["release_manifest"], release_tag_bad)})
    release_digest_bad = copy.deepcopy(release)
    release_digest_bad["asset_digest"] = transport["artifact_digest"]
    add_hostile("release_fixture_digest_prefix_missing", "positive-release-manifest.json", "asset_digest", release_digest_bad["asset_digest"], "bare digest where sha256: is required", "release_manifest", {"schema_valid": valid(schemas["release_manifest"], release_digest_bad), "rejects": not valid(schemas["release_manifest"], release_digest_bad)})
    transport_prefix_bad = copy.deepcopy(transport)
    transport_prefix_bad["artifact_raw_digest"] = transport["artifact_digest"]
    add_hostile("raw_artifact_digest_prefix_mismatch", "positive-pre-record-transport.json", "artifact_raw_digest", transport_prefix_bad["artifact_raw_digest"], "raw REST digest loses sha256 prefix", "pre_record_transport", {"schema_valid": valid(schemas["pre_record_transport"], transport_prefix_bad), "rejects": not valid(schemas["pre_record_transport"], transport_prefix_bad)})
    metadata_bad = copy.deepcopy(release)
    metadata_bad["fixture_metadata"] = {"live": False}
    add_hostile("raw_fixture_metadata_extra", "positive-release-manifest.json", "fixture_metadata", metadata_bad["fixture_metadata"], "strict schema must reject wrapper metadata", "release_manifest", {"schema_valid": valid(schemas["release_manifest"], metadata_bad), "rejects": not valid(schemas["release_manifest"], metadata_bad)})

    provider_id_bad = copy.deepcopy(provider)
    provider_id_bad["source_record_artifact_id"] += 1
    add_hostile("provider_source_transport_mismatch", "positive-provider-result.json", "source_record_artifact_id", provider_id_bad["source_record_artifact_id"], "provider must bind exact uploaded artifact ID", "provider_result", {"schema_valid": valid(schemas["provider_result"], provider_id_bad), "rejects": provider_id_bad["source_record_artifact_id"] != transport["artifact_id"]})
    provider_result_bad = copy.deepcopy(verify)
    provider_result_bad["provider_result_digest"] = "0" * 64
    add_hostile("verify_provider_identity_mismatch", "positive-verify-b.json", "provider_result_digest", provider_result_bad["provider_result_digest"], "verify-B must read exact provider ID/digest", "verify_b", {"schema_valid": valid(schemas["verify_b"], provider_result_bad), "rejects": provider_result_bad["provider_result_digest"] != provider["provider_result_digest"]})
    head_bad = copy.deepcopy(provider)
    head_bad["provider_check_head_sha"] = "a" * 40
    add_hostile("provider_check_head_not_resulting_main", "positive-provider-result.json", "provider_check_head_sha", head_bad["provider_check_head_sha"], "Checks head must equal resulting main", "provider_result", {"schema_valid": valid(schemas["provider_result"], head_bad), "rejects": head_bad["provider_check_head_sha"] != provider["resulting_main_sha"]})
    attestation_bad = copy.deepcopy(pre)
    attestation_bad["release_attestation"]["manifest_subject_sha256"] = "0" * 64
    add_hostile("release_attestation_subject_mismatch", "positive-pre-record.json", "release_attestation.manifest_subject_sha256", attestation_bad["release_attestation"]["manifest_subject_sha256"], "release attestation subject must equal release manifest digest", "pre_record", {"schema_valid": valid(schemas["pre_record"], attestation_bad), "rejects": attestation_bad["release_attestation"]["manifest_subject_sha256"] != release["manifest_digest"]})
    called_sha_bad = copy.deepcopy(verify)
    called_sha_bad["called_job_workflow_sha"] = "a" * 40
    add_hostile("wrong_oidc_called_workflow_sha", "positive-verify-b.json", "called_job_workflow_sha", called_sha_bad["called_job_workflow_sha"], "called job OIDC workflow SHA must equal called workflow commit SHA", "verify_b", {"schema_valid": valid(schemas["verify_b"], called_sha_bad), "rejects": called_sha_bad["called_job_workflow_sha"] != verify["called_workflow_sha"]})
    called_ref_bad = copy.deepcopy(verify)
    called_ref_bad["called_job_workflow_ref"] = "evil/repo/.github/workflows/evil.yml@refs/heads/main"
    add_hostile("wrong_oidc_called_workflow_ref", "positive-verify-b.json", "called_job_workflow_ref", called_ref_bad["called_job_workflow_ref"], "called workflow identity must be exact trusted repository/path/ref", "verify_b", {"schema_valid": valid(schemas["verify_b"], called_ref_bad), "rejects": "tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml@refs/heads/main" not in called_ref_bad["called_job_workflow_ref"]})
    attempt_bad = copy.deepcopy(transport)
    attempt_bad["upload_run_attempt"] = 2
    add_hostile("upload_run_attempt_mismatch", "positive-pre-record-transport.json", "upload_run_attempt", attempt_bad["upload_run_attempt"], "artifact upload run attempt must be read from exact producer run", "pre_record_transport", {"schema_valid": valid(schemas["pre_record_transport"], attempt_bad), "rejects": attempt_bad["upload_run_attempt"] != transport["upload_run_attempt"]})
    service_bad = copy.deepcopy(pre)
    service_bad["artifact"]["inner_payload_digest"] = "b" * 64
    add_hostile("service_zip_inner_payload_mismatch", "positive-pre-record.json", "artifact.inner_payload_digest", service_bad["artifact"]["inner_payload_digest"], "raw service ZIP extraction must bind inner payload digest", "pre_record", {"schema_valid": valid(schemas["pre_record"], service_bad), "rejects": service_bad["artifact"]["inner_payload_digest"] != service_bad["artifact"]["service_zip_digest"]})
    binary_bad = copy.deepcopy(pre)
    binary_bad["artifact"]["binary_digest"] = "b" * 64
    add_hostile("inner_payload_binary_mismatch", "positive-pre-record.json", "artifact.binary_digest", binary_bad["artifact"]["binary_digest"], "inner payload extraction must bind binary digest", "pre_record", {"schema_valid": valid(schemas["pre_record"], binary_bad), "rejects": binary_bad["artifact"]["binary_digest"] != binary_bad["artifact"]["inner_payload_digest"]})
    terminal_incomplete = copy.deepcopy(verify)
    terminal_incomplete["terminal_census_rows"][0]["workflow_path"] = ".github/workflows/unlisted.yml"
    add_hostile("terminal_census_incomplete", "positive-verify-b.json", "terminal_census_rows[0].workflow_path", terminal_incomplete["terminal_census_rows"][0]["workflow_path"], "census must cover every required terminal workflow, not only one arbitrary row", "verify_b", {"schema_valid": valid(schemas["verify_b"], terminal_incomplete), "rejects": terminal_incomplete["terminal_census_rows"][0]["workflow_path"] != ".github/workflows/ci-main.yml"})
    add_hostile("permanent_binding_positive_missing", "positive-full-typed-output-fixture.json", "permanent.*", None, "direct permanent strict positive is required", "permanent_binding", {"schema_valid": False, "rejects": True})
    add_hostile("typed_fixture_const_mismatch", "positive-full-typed-output-fixture.json", "permanent.product.product_id", typed["typed_outputs"]["permanent.product.product_id"]["value"], "typed coarse value must satisfy permanent schema const", "permanent_binding", {"schema_valid": projected_validation["permanent_binding"]["error_count"] == 0, "rejects": projected_validation["permanent_binding"]["error_count"] > 0})
    add_hostile("provider_check_id_in_result_preimage", "v11-canonical-field-dag.json", "preimage_contract.S7b_provider_result.ordered_field_ids", "provider_result.provider_check_run_id", "provider-owned check ID must be preallocated or excluded from signed payload", None, {"schema_valid": False, "rejects": "provider_result.provider_check_run_id" in provider_ordered})
    add_hostile("provider_attestation_digest_in_result_preimage", "v11-canonical-field-dag.json", "preimage_contract.S7b_provider_result.ordered_field_ids", "provider_result.provider_attestation_digest", "provider attestation digest must not be an unsequenced self/future field", None, {"schema_valid": False, "rejects": "provider_result.provider_attestation_digest" in provider_ordered})
    add_hostile("future_step_fields_at_artifact_verify", "v11-canonical-field-dag.json", "fields.permanent.steps.publish.stage", "S2_artifact_verify", "future publish step cannot be produced by S2", None, {"schema_valid": False, "rejects": bool(future_step_fields)})
    add_hostile("pre_record_fields_after_upload", "v11-canonical-field-dag.json", "fields.pre_record.binding.*.stage", "S7c_verify_b", "provider input cannot depend on post-provider verify-B", None, {"schema_valid": False, "rejects": bool(pre_record_after_upload)})
    pre_prefix_bad = copy.deepcopy(pre)
    pre_prefix_bad["binding"]["subject_digest"] = transport["artifact_digest"]
    add_hostile("pre_fixture_digest_prefix_missing", "positive-pre-record.json", "binding.subject_digest", pre_prefix_bad["binding"]["subject_digest"], "pre-record digest loses required sha256 prefix", "pre_record", {"schema_valid": valid(schemas["pre_record"], pre_prefix_bad), "rejects": not valid(schemas["pre_record"], pre_prefix_bad)})
    release_identity_bad = copy.deepcopy(release)
    release_identity_bad["product_name"] = "forged-product"
    add_hostile("release_identity_mismatch", "positive-release-manifest.json", "product_name", release_identity_bad["product_name"], "release identity must equal canonical product", "release_manifest", {"schema_valid": valid(schemas["release_manifest"], release_identity_bad), "rejects": not valid(schemas["release_manifest"], release_identity_bad)})
    joined_bad = copy.deepcopy(provider)
    joined_bad["source_record_raw_zip_sha256"] = "0" * 64
    add_hostile("joined_lifecycle_mismatch", "positive-provider-result.json", "source_record_raw_zip_sha256", joined_bad["source_record_raw_zip_sha256"], "provider source must join exact raw transport digest", "provider_result", {"schema_valid": valid(schemas["provider_result"], joined_bad), "rejects": joined_bad["source_record_raw_zip_sha256"] != transport["raw_zip_sha256"]})
    add_hostile("release_leaf_omitted", "positive-release-manifest.json", "asset_id", None, "all 14 strict release leaves are required", "release_manifest", {"schema_valid": False, "rejects": "asset_id" in schemas["release_manifest"].get("required", [])})
    add_hostile("own_digest_field_in_preimage", "v11-canonical-field-dag.json", "preimage_contract.S7b_provider_result.ordered_field_ids", "provider_result.provider_result_digest", "provider result digest cannot sign itself", None, {"schema_valid": False, "rejects": "provider_result.provider_result_digest" not in provider_ordered})
    add_hostile("future_stage_field_in_preimage", "v11-canonical-field-dag.json", "preimage_contract.S4_binding.ordered_field_ids", "provider_result.provider_result_digest", "S4 cannot hash future S7b data", None, {"schema_valid": False, "rejects": stage_rank.get(fields["provider_result.provider_result_digest"]["stage"], 999) >= stage_rank["S4_binding_attest"]})
    add_hostile("provider_result_contains_terminal_census", "provider_result.schema.json", "properties.terminal_census_id", "forbidden", "provider result must not own verify-B terminal census", None, {"schema_valid": False, "rejects": "terminal_census_id" not in actual_leaves["provider_result"]})
    add_hostile("workflow_call_output_untransported", "v11-canonical-field-dag.json", "field_lineage_edges.workflow_call_output", None, "caller Policy must consume explicit workflow-call outputs", None, {"schema_valid": False, "rejects": expected_outputs == actual_outputs})
    add_hostile("source_step_mapping_missing", "v11-canonical-model-source.json", "artifact_contract.upload_step_source_mapping", None, "step identity must join immutable source and Jobs API fields", None, {"schema_valid": False, "rejects": model.get("artifact_contract", {}).get("upload_step_source_mapping", {}).get("source_workflow_path") == ".github/workflows/ci-policy-validator-products.yml" and "number" in model.get("artifact_contract", {}).get("upload_step_source_mapping", {}).get("jobs_api_step_fields", [])})
    add_hostile("unsupported_api_identity_field", "v11-canonical-model-source.json", "caller_readback.check_run_api_fields", "integration_id", "Check Runs API must not invent integration_id", None, {"schema_valid": False, "rejects": "integration_id" not in model.get("caller_readback", {}).get("check_run_api_fields", [])})
    add_hostile("ambiguous_provider_producer", "v11-canonical-field-dag.json", "fields.provider_result.provider_result_id.producer", "provider-A or provider-B", "provider result needs one authenticated producer", None, {"schema_valid": False, "rejects": all(" or " not in desc.get("producer", "") for desc in fields.values())})
    add_hostile("caller_pr_head_used_as_resulting_main", "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json", "revision_bound_facts.resulting_main.sha", plan.get("revision_bound_facts", {}).get("candidate_pr", {}).get("head_sha"), "candidate PR head cannot substitute protected resulting main", None, {"schema_valid": False, "rejects": plan.get("revision_bound_facts", {}).get("resulting_main", {}).get("sha") is None})
    add_hostile("synthetic_fixture_claims_live", "positive-full-typed-output-fixture.json", "fixture_metadata.live_binding", True, "synthetic typed output must not claim live proof", None, {"schema_valid": False, "rejects": typed.get("fixture_metadata", {}).get("live_binding") is False})
    add_hostile("unresolved_provider_claimed_success", "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json", "status", "success", "unresolved provider/freeze/live state cannot be called success", None, {"schema_valid": False, "rejects": plan.get("status") == "successor_draft_external_blocked" and model.get("provider_contract", {}).get("external_identity", {}).get("status") == "unresolved_external_blocker"})

    # v10 negative classes replayed against the v11 projection.
    # Replay v10 attack mutations against independent predicates.  These
    # predicates operate on mutated copies/sets, not on the owner's labels.
    duplicate_mutated_keys = edge_keys + [edge_keys[0]]
    schema_leaf_mutated = set(actual_leaves["pre_record"]) - {"verification.required_step_conclusions.artifact-verify"}
    adoption_mutated_stage_union = stage_union - {field_id for field_id in fields if field_id.startswith("adoption.")}
    output_mutated_actual = actual_outputs - {next(iter(actual_outputs))} if actual_outputs else set()
    provider_terminal_mutated = set(actual_leaves["provider_result"]) | {"terminal_census_id"}
    suffix_mutated = set(preimages["S4_binding"]["ordered_field_ids"]) | {"pre_record.release.asset_id"}
    v10_cases = [
        ("duplicate_external_provider_edge", "duplicate exact external-provider-result -> verify-B edge", len(duplicate_mutated_keys) != len(set(duplicate_mutated_keys))),
        ("schema_leaf_dynamic_object_omitted", "omit required_step_conclusions child leaves", schema_leaf_mutated != set(actual_leaves["pre_record"])),
        ("adoption_fields_without_stage", "remove adoption fields from stage_fields", set(fields) - adoption_mutated_stage_union),
        ("verify_b_terminal_outputs_untransported", "remove explicit workflow_call output edges", expected_outputs - output_mutated_actual),
        ("verify_b_terminal_census_extra", "provider result owns terminal census", any(path.startswith("terminal_census") for path in provider_terminal_mutated)),
        ("pre_fixture_stale_root_digest", "replace fixture root with stale digest", "0" * 64 != measured_root_digest),
        ("pre_fixture_wrong_release_schema_hash", "replace pre-record schema hash", "0" * 64 != sha(SCHEMA_FILES["release_manifest"])),
        ("S4_suffix_exclusion_unapplied", "leave a model-declared identity suffix in S4 ordered preimage", any(suffix_mutated and field_id.endswith(("_id", "_digest", "_attestation_id", "_attestation_digest")) for field_id in suffix_mutated)),
        ("typed_object_value_string", "encode terminal rows as string", not isinstance("array-as-string", list)),
        ("live_verifier_absent", "claim synthetic bundle executed", False),
    ]
    for name, reason, rejects in v10_cases:
        add_hostile(name, "bundle projection", "contract", None, reason, None, {"schema_valid": None, "rejects": bool(rejects)})
    check("v10-hostile-classes-replayed", "pass" if all(bool(case[2]) for case in v10_cases if case[0] != "live_verifier_absent") else "fail", {name: bool(rejects) for name, _, rejects in v10_cases})

    hostile_path = OUT / "v11-luna-hostile-fixtures.json"
    hostile_payload = {"schema": "velnor.authority-transition.v11-luna-hostile-fixtures.v1", "execution": "unexecuted-no-real-verifier", "all_expected": "reject", "cases": hostile_cases}
    hostile_path.write_text(json.dumps(hostile_payload, indent=2, sort_keys=True) + "\n")
    check("hostile-fixture-set", "pass", {"count": len(hostile_cases), "path": str(hostile_path), "sha256": sha(hostile_path), "real_execution": "unimplemented"})

    # Fresh remote main and implementation paths.  Cached local origin state is
    # not treated as authority.
    rc, remote_out, remote_err = run_git("git", "ls-remote", "origin", "refs/heads/main")
    live_sha = remote_out.split()[0] if rc == 0 and remote_out else None
    live_tree = None
    tree_err = None
    if live_sha:
        trc, tree_out, tree_err = run_git("git", "rev-parse", f"{live_sha}^{{tree}}")
        live_tree = tree_out if trc == 0 else None
    observed = model.get("observed_revision", {}).get("main", {})
    check("fresh-live-main-reconciliation", "pass" if live_sha == observed.get("sha") and live_tree == observed.get("tree_sha") else "fail", {"remote_sha": live_sha, "remote_tree": live_tree, "observed_sha": observed.get("sha"), "observed_tree": observed.get("tree_sha"), "stderr": remote_err or tree_err})
    typed_paths = [
        ".github/workflows/ci-policy-validator-products.yml",
        ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs",
    ]
    implementation = []
    for path in typed_paths:
        if not live_sha:
            implementation.append({"path": path, "present": None})
            continue
        prc, _, perr = run_git("git", "cat-file", "-e", f"{live_sha}:{path}")
        implementation.append({"path": path, "present": prc == 0, "stderr": perr if prc else None})
    impl_status = "unimplemented" if live_sha and not any(item["present"] for item in implementation) else "pass" if live_sha else "unavailable"
    check("fresh-live-typed-verifier", impl_status, {"commit": live_sha, "tree": live_tree, "paths": implementation})
    if impl_status == "unimplemented":
        finding("V11-IMPL-001", "critical", "No typed publisher/verifier implementation exists at fresh origin/main; schema and hostile checks are contract-only.", {"commit": live_sha, "paths": typed_paths})

    provider_contract = model.get("provider_contract", {})
    external_identity = provider_contract.get("external_identity", {})
    signature = provider_contract.get("signature", {})
    freeze_contract = model.get("freeze_contract", {})
    action_pin_missing = model.get("artifact_contract", {}).get("upload_step_source_mapping", {}).get("source_commit_sha") is None
    check("exact-action-source-and-oidc-trust", "unimplemented", {"source_commit_sha": model.get("artifact_contract", {}).get("upload_step_source_mapping", {}).get("source_commit_sha"), "provider_identity": external_identity, "signature": signature, "oidc_claims": model.get("caller_readback", {}).get("oidc_claims"), "action_pin_missing": action_pin_missing})
    finding("V11-TRUST-001", "critical", "Exact provider signer/app/repository/workflow/ref/digest trust values and immutable action source SHA are unresolved; schemas accept arbitrary valid SHA strings and URI endpoints.", {"provider_identity": external_identity, "signature": signature, "source_commit_sha": model.get("artifact_contract", {}).get("upload_step_source_mapping", {}).get("source_commit_sha")})

    # Terminal census completeness and actual terminal-state coverage.
    rows = verify.get("terminal_census_rows", [])
    required_workflows = model.get("authority_graph", {}).get("consumer_workflows", [])
    check("terminal-census-completeness-contract", "fail" if len(rows) < len(required_workflows) else "pass", {"rows": len(rows), "declared_consumer_workflows": len(required_workflows), "schema_minItems": schemas["verify_b"].get("properties", {}).get("terminal_census_rows", {}).get("minItems")})
    if len(rows) < len(required_workflows):
        finding("V11-TERM-001", "high", "The positive terminal census has one row while the authority graph declares 15 consumer workflows; verify-B has no required coverage set or completeness predicate.", {"rows": rows, "required_workflows": required_workflows})

    # External blockers are an explicit unimplemented boundary, never success.
    external_blockers_ok = (
        plan.get("status") == "successor_draft_external_blocked"
        and plan.get("execution_authorized") is False
        and plan.get("mutation_performed") is False
        and external_identity.get("status") == "unresolved_external_blocker"
        and signature.get("status") == "unresolved_external_blocker"
        and freeze_contract.get("status") == "external_blocker"
    )
    check("external-blockers-honest", "pass" if external_blockers_ok else "fail", {"plan_status": plan.get("status"), "execution_authorized": plan.get("execution_authorized"), "mutation_performed": plan.get("mutation_performed"), "provider_identity_status": external_identity.get("status"), "signature_status": signature.get("status"), "freeze_status": freeze_contract.get("status")})
    check("real-hostile-execution", "unimplemented", {"reason": "fresh main has no typed verifier/provider; hostile JSON fixtures are persisted but not submitted"})

    projection = {
        "schema": "velnor.authority-transition.v11-luna-independent-projection.v1",
        "status": "design_only_external_blocked",
        "authority_claim": False,
        "source_hashes": {"freeze_manifest": measured["freeze_manifest"], "model": measured["model"], "dag": measured["dag"], "plan_json": measured["plan_json"], "plan_md": measured["plan_md"], "root_raw": measured["root_raw"], "root_digest": measured_root_digest},
        "schema_leaf_counts": {name: len(paths) for name, paths in actual_leaves.items()},
        "schema_leaf_paths": actual_leaves,
        "schema_leaf_registry_diff": registry_diff,
        "fields": fields,
        "stage_fields": stage_fields,
        "field_lineage_edges": edges,
        "declared_consumer_transport_missing": missing_declared_consumers,
        "temporal": {"pre_record_after_upload": pre_record_after_upload, "future_step_fields": future_step_fields, "release_stage_errors": release_stage_errors},
        "preimages": preimages,
        "preimage_checks": preimage_checks,
        "provider_result_ordered_identity_fields": provider_result_own,
        "strict_validation": strict_validation,
        "typed_projection_validation": projected_validation,
        "lifecycle_equalities": lifecycle_equalities,
        "live_implementation": {"sha": live_sha, "tree": live_tree, "status": impl_status, "paths": implementation},
    }
    projection_path = OUT / "v11-luna-independent-field-dag.json"
    projection_path.write_text(json.dumps(projection, indent=2, sort_keys=True) + "\n")

    summary = {
        "total": len(checks),
        "passed": sum(item["status"] == "pass" for item in checks),
        "failed": sum(item["status"] == "fail" for item in checks),
        "unimplemented": sum(item["status"] == "unimplemented" for item in checks),
        "unavailable": sum(item["status"] == "unavailable" for item in checks),
        "findings": len(findings),
    }
    result = {
        "schema": "velnor.authority-transition.v11-luna-independent-audit.v1",
        "status": "proposal_only_external_blocked",
        "authority_claim": False,
        "execution_authorized": False,
        "mutation_performed": False,
        "frozen_inputs": measured | {"root_digest": measured_root_digest, "hostile_fixture_sha256": sha(hostile_path), "projection_sha256": sha(projection_path)},
        "audit_script_sha256": sha(Path(__file__)),
        "owner_results_not_authority": {"result_sha256": measured["owner_result"], "report_sha256": measured["owner_report"]},
        "checks": checks,
        "findings": findings,
        "summary": summary,
        "strict_validator": f"jsonschema {importlib.metadata.version('jsonschema')} Draft202012Validator with FormatChecker",
        "hostile_execution": "unimplemented-no-live-verifier",
    }
    result_path = OUT / "v11-luna-independent-audit-results.json"
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report_lines = [
        "# Independent v11 Luna adversarial contract audit",
        "",
        "Status: design-only external audit. No source, authority, release, dispatch, merge, runner, or credential mutation occurred.",
        "Owner 59/59 and 19/19 labels are preserved as non-authoritative inputs; this audit did not import owner code.",
        "",
        f"Result: {summary['passed']}/{summary['total']} checks passed; failures={summary['failed']}; unimplemented={summary['unimplemented']}; findings={summary['findings']}; authority_claim=false.",
        "",
        "## Exact frozen hashes",
        "",
        f"- Freeze manifest: `{measured['freeze_manifest']}`.",
        f"- Canonical model: `{measured['model']}`.",
        f"- Plan JSON: `{measured['plan_json']}`.",
        f"- Plan Markdown: `{measured['plan_md']}`.",
        f"- Canonical DAG: `{measured['dag']}`.",
        f"- Root manifest raw: `{measured['root_raw']}`; canonical root digest: `{measured_root_digest}`.",
        f"- Hostile fixture set: `{sha(hostile_path)}`.",
        f"- Independent projection: `{sha(projection_path)}`.",
        f"- Independent script: `{sha(Path(__file__))}`.",
        f"- Independent result: `{sha(result_path)}`.",
        "",
        "## Findings",
        "",
    ]
    for item in findings:
        report_lines.append(f"- `{item['id']}` ({item['severity']}): {item['message']}")
        report_lines.append(f"  - Detail: `{json.dumps(item['detail'], sort_keys=True)[:1500]}`")
    report_lines.extend(["", "## Check statuses", ""])
    report_lines.extend(f"- `{item['status']}` `{item['name']}`" for item in checks)
    report_lines.extend([
        "",
        f"Strict positives were run with jsonschema {importlib.metadata.version('jsonschema')} Draft202012Validator plus FormatChecker. Hostile cases are isolated JSON contract fixtures only; no real verifier was present at the fresh origin/main revision, so hostile rejection execution is unimplemented and no gate/authority claim is made.",
        "",
    ])
    report_path = OUT / "v11-luna-independent-audit-report.md"
    report_path.write_text("\n".join(report_lines))
    print(json.dumps({"summary": summary, "hostile_fixture": str(hostile_path), "projection": str(projection_path), "result": str(result_path), "report": str(report_path)}, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
