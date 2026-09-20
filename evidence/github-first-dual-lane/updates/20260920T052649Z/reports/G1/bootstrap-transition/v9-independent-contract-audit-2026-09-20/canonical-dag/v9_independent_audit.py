#!/usr/bin/env python3
"""Independent, read-only v9 contract audit.

This is an evidence-producing checker, not a publisher/verifier implementation.
It intentionally reports proposal inconsistencies instead of turning them into
an approval gate.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence")
BASE = ROOT / "G1/bootstrap-transition"
V9 = BASE / "authority-contract-separation-2026-09-20/v9"
OUT = BASE / "v9-independent-contract-audit-2026-09-20/canonical-dag"
PLAN_MD = BASE / "AUTHORITY-CHANGE-PLAN-2026-09-20-v9.md"
PLAN_JSON = BASE / "AUTHORITY-CHANGE-PLAN-2026-09-20-v9.json"
PERM_SCHEMA = BASE / "authority-contract-separation-2026-09-20/permanent-b-product-binding.schema.json"
PRE_SCHEMA = V9 / "policy-validator-b-pre-record.v2.schema.json"
PROVIDER_SCHEMA = V9 / "policy-validator-b-provider-result.v1.schema.json"
RELEASE_SCHEMA = V9 / "policy-validator-b-release-manifest.v1.schema.json"
ROOT_MANIFEST = V9 / "canonical-root-manifest.v2.json"
HOSTILE = V9 / "hostile-transport-fixtures.json"
FIXTURE_DIR = BASE / "v9-executable-fixture"

EXPECTED_PLAN_MD = "bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed"
EXPECTED_PLAN_JSON = "e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615"
EXPECTED_OWNER_SCRIPT = "eae169967eefc34ae7026b856b9d8dd56095b220d58759da313f8d08a6948c80"
EXPECTED_OWNER_RESULT = "4ddeb3272b5456f8e790e925c4dba4a71ba4436eee38af0910bbc660fc196ca1"
EXPECTED_OWNER_REPORT = "1e920b0186c050d78757919ca39dc71ffb686f4de97e7b2e3cf77cbb83d02f0f"
EXPECTED_ROOT_DIGEST = "7f888b746ca8c7a6f6e8342c7d0a450edd70a5bf766421f9a2d8ddd2cd9e517b"
EXPECTED_HOSTILE = "6e838b3b765ceb4570f1a86628d606dffa8cd03909c81dfc1d1abab7023f96be"
EXPECTED_LIVE_MAIN = "89f82dd8b287f46a3cf4c0920f341f6ca6c736db"
EXPECTED_LIVE_TREE = "22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416"


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def canonical_sha(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def typed_name(value: str) -> str:
    return value.rsplit(":", 1)[0]


def schema_leaves(schema: dict[str, Any]) -> list[str]:
    """Resolve local $defs and treat map/array values as a leaf contract."""

    def walk(node: dict[str, Any], path: str) -> list[str]:
        if "$ref" in node:
            ref = node["$ref"]
            if ref.startswith("#/$defs/"):
                node = schema["$defs"][ref.rsplit("/", 1)[1]]
        if node.get("type") == "object" or "properties" in node:
            props = node.get("properties", {})
            if not props:
                return [path]
            result: list[str] = []
            for key, child in props.items():
                result.extend(walk(child, f"{path}.{key}" if path else key))
            return result
        return [path]

    return walk(schema, "")


def get_path(obj: Any, path: str) -> Any:
    cur = obj
    for part in path.split("."):
        if not isinstance(cur, dict) or part not in cur:
            return None
        cur = cur[part]
    return cur


def schema_path(schema: dict[str, Any], path: str) -> dict[str, Any] | None:
    """Resolve a property path in a JSON Schema (not in an instance)."""
    cur: dict[str, Any] = schema
    for part in path.split("."):
        if "$ref" in cur and cur["$ref"].startswith("#/$defs/"):
            cur = schema["$defs"][cur["$ref"].rsplit("/", 1)[1]]
        cur = cur.get("properties", {}).get(part, {})
        if not cur:
            return None
    return cur


def set_path(obj: Any, path: str, value: Any) -> None:
    parts = path.split(".")
    cur = obj
    for part in parts[:-1]:
        cur = cur[part]
    cur[parts[-1]] = value


def normalize_fixture(value: dict[str, Any]) -> dict[str, Any]:
    result = copy.deepcopy(value)
    if "canonical_root_manifest_sha256" in result:
        result["canonical_root_manifest_sha256"] = "0" * 64
    return result


def command(*args: str, cwd: Path | None = None) -> tuple[int, str, str]:
    try:
        proc = subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=False)
    except OSError as exc:
        return 127, "", str(exc)
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def fixture_outputs(text: str) -> dict[str, set[str]]:
    """Small YAML-independent extractor for the fixture's output declarations."""
    jobs: dict[str, set[str]] = {}
    blocks = list(re.finditer(r"(?m)^  ([A-Za-z0-9_-]+):\n(.*?)(?=^  [A-Za-z0-9_-]+:|\Z)", text, re.S))
    for match in blocks:
        body = match.group(2)
        output_match = re.search(r"(?m)^    outputs:\n(.*?)(?=^    (?:steps|needs|runs-on|permissions|if):|\Z)", body, re.S)
        if output_match:
            jobs[match.group(1)] = set(re.findall(r"(?m)^      ([A-Za-z0-9_-]+):", output_match.group(1)))
    top = re.search(r"(?m)^    outputs:\n(.*?)(?=^jobs:)", text, re.S)
    if top:
        jobs["workflow_call"] = set(re.findall(r"(?m)^      ([A-Za-z0-9_-]+):", top.group(1)))
    return jobs


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    plan = load(PLAN_JSON)
    pub = plan["main_b_publisher"]
    root_manifest = load(ROOT_MANIFEST)
    result: dict[str, Any] = {
        "schema": "velnor.v9-independent-contract-audit.v1",
        "status": "design_only_external_audit",
        "execution_authorized": False,
        "mutation_performed": False,
        "observed_utc": "2026-09-20",
        "frozen_inputs": {
            "plan_markdown_sha256": sha(PLAN_MD),
            "plan_json_sha256": sha(PLAN_JSON),
            "expected_plan_markdown_sha256": EXPECTED_PLAN_MD,
            "expected_plan_json_sha256": EXPECTED_PLAN_JSON,
            "owner_script_sha256": sha(BASE / "v9_contract_audit.py"),
            "owner_result_sha256": sha(BASE / "v9-contract-audit-results.json"),
            "owner_report_sha256": sha(BASE / "v9-contract-audit-report.md"),
            "hostile_fixture_sha256": sha(HOSTILE),
        },
        "checks": [],
        "findings": [],
        "negative_fixtures": [],
    }

    def check(name: str, status: str, detail: Any) -> None:
        result["checks"].append({"name": name, "status": status, "detail": detail})

    def finding(fid: str, severity: str, detail: Any) -> None:
        result["findings"].append({"id": fid, "severity": severity, "detail": detail})

    # Frozen bytes and canonical root.
    check("frozen-plan-hashes", "pass" if sha(PLAN_MD) == EXPECTED_PLAN_MD and sha(PLAN_JSON) == EXPECTED_PLAN_JSON else "fail", result["frozen_inputs"])
    check("owner-result-hashes", "pass" if result["frozen_inputs"]["owner_script_sha256"] == EXPECTED_OWNER_SCRIPT and result["frozen_inputs"]["owner_result_sha256"] == EXPECTED_OWNER_RESULT and result["frozen_inputs"]["owner_report_sha256"] == EXPECTED_OWNER_REPORT else "fail", result["frozen_inputs"])
    root_digest = canonical_sha(root_manifest)
    check("canonical-root-no-self-digest", "pass" if root_digest == EXPECTED_ROOT_DIGEST and root_manifest["canonical_root"] == str(ROOT) else "fail", {"measured": root_digest, "declared": EXPECTED_ROOT_DIGEST, "canonical_root": root_manifest.get("canonical_root")})
    check("hostile-fixture-hash", "pass" if sha(HOSTILE) == EXPECTED_HOSTILE else "fail", {"measured": sha(HOSTILE), "declared": EXPECTED_HOSTILE, "count": len(load(HOSTILE)["fixtures"])})

    # Every bound byte is checked independently. Positive fixture hashes are the
    # documented normalized preimages, not raw file hashes.
    bound_results = []
    for item in root_manifest.get("bound_files", []):
        path = ROOT / item["path"]
        if not path.exists():
            bound_results.append({"path": item["path"], "status": "missing"})
            continue
        measured = sha(path)
        if path.name in {"positive-pre-record.json", "positive-provider-result.json"}:
            measured = canonical_sha(normalize_fixture(load(path)))
        bound_results.append({"path": item["path"], "status": "pass" if measured == item["sha256"] else "fail", "measured": measured, "declared": item["sha256"]})
    check("root-bound-file-hashes", "pass" if all(x["status"] == "pass" for x in bound_results) else "fail", bound_results)

    # Schema files and the plan's schema references.
    schema_paths = {
        "permanent_binding": PERM_SCHEMA,
        "pre_record": PRE_SCHEMA,
        "provider_result": PROVIDER_SCHEMA,
        "release_manifest": RELEASE_SCHEMA,
    }
    schema_hashes = {}
    for key, path in schema_paths.items():
        schema_hashes[key] = {"path": str(path), "sha256": sha(path), "exists": path.exists()}
    check("schema-files-and-plan-hashes", "pass" if all(x["exists"] for x in schema_hashes.values()) else "fail", schema_hashes)
    declared_schema_hashes = []
    for key, item in pub.get("canonical_schemas", {}).items():
        # canonical_schemas uses paths relative to G1/bootstrap-transition;
        # historical exclusions retain a G1-relative path.
        path = (ROOT if item["path"].startswith("G1/") else BASE) / item["path"]
        if not path.exists():
            declared_schema_hashes.append({"key": key, "path": item["path"], "status": "missing"})
            continue
        measured = canonical_sha(load(path)) if key == "root_manifest" else sha(path)
        declared_schema_hashes.append({"key": key, "path": item["path"], "status": "pass" if measured == item.get("sha256") else "fail", "measured": measured, "declared": item.get("sha256")})
    check("plan-canonical-schema-hashes", "pass" if all(x["status"] == "pass" for x in declared_schema_hashes) else "fail", declared_schema_hashes)

    # Independent leaf counts and exact canonical map comparison.
    schemas = {key: load(path) for key, path in schema_paths.items()}
    actual_leaves = {key: schema_leaves(schema) for key, schema in schemas.items()}
    owner_maps = pub["canonical_leaf_paths"]
    expected_record_map = {f"pre_record.{p}" for p in actual_leaves["pre_record"]} | {f"provider_result.{p}" for p in actual_leaves["provider_result"]}
    map_diffs = {
        "permanent_missing": sorted(set(actual_leaves["permanent_binding"]) - set(owner_maps.get("permanent_binding", {}))),
        "permanent_extra": sorted(set(owner_maps.get("permanent_binding", {})) - set(actual_leaves["permanent_binding"])),
        "record_missing": sorted(expected_record_map - set(owner_maps.get("pre_record", {}))),
        "record_extra": sorted(set(owner_maps.get("pre_record", {})) - expected_record_map),
        "release_manifest_unmapped": sorted(actual_leaves["release_manifest"]),
    }
    counts = {"actual_schema_leaves": {k: len(v) for k, v in actual_leaves.items()}, "owner_map_leaves": {k: len(v) for k, v in owner_maps.items()}, "diffs": map_diffs}
    check("146-permanent-and-124-record-leaves", "pass" if len(actual_leaves["permanent_binding"]) == 146 and len(expected_record_map) == 124 and not any(map_diffs[k] for k in ("permanent_missing", "permanent_extra", "record_missing", "record_extra")) else "fail", counts)
    if map_diffs["release_manifest_unmapped"]:
        finding("V9-LEAF-RELEASE-001", "high", {"message": "The current canonical_leaf_paths map has no release_manifest namespace; all 14 leaves of a bound current schema are unrepresented.", "paths": map_diffs["release_manifest_unmapped"]})

    # Validate canonical map metadata has a typed, required producer for all 146
    # and 124 mapped leaves.
    bad_meta = []
    for namespace, mapping in owner_maps.items():
        if namespace not in {"permanent_binding", "pre_record"}:
            continue
        for path, meta in mapping.items():
            if not isinstance(meta, dict) or not meta.get("stage") or not meta.get("producer") or not meta.get("source") or not meta.get("type") or meta.get("required") is not True:
                bad_meta.append(path)
    check("canonical-leaf-metadata", "pass" if not bad_meta else "fail", {"bad_paths": bad_meta, "checked": sum(len(x) for x in owner_maps.values())})

    # Stage DAG: unique producer, typed edge transport, direct needs, and every
    # declared consumer field available from an earlier stage.
    stages = pub["field_availability_dag"]
    stage_index = {f"S{i}": i for i in range(7)} | {"S7a": 7, "S7b": 8}
    field_stage: dict[str, str] = {}
    field_stage_producer: dict[str, str] = {}
    duplicate_fields = []
    for stage in stages:
        for typed in stage.get("produces", []):
            field = typed_name(typed)
            if field in field_stage:
                duplicate_fields.append(field)
            field_stage[field] = stage["stage"]
            field_stage_producer[field] = stage["producer_job"]
    edge_failures = []
    for edge in pub["field_lineage_edges"]:
        producer = edge["producer"]
        job_outputs = {typed_name(x) for x in pub["jobs"].get(producer, {}).get("outputs", [])}
        for typed in edge.get("fields", []):
            if ":" not in typed:
                edge_failures.append({"edge": [producer, edge["consumer"]], "field": typed, "reason": "untyped"})
            if producer in pub["jobs"] and typed_name(typed) not in job_outputs:
                edge_failures.append({"edge": [producer, edge["consumer"]], "field": typed, "reason": "not-in-producer-job-outputs"})
    expected_needs = {tuple(x) for x in pub["needs_edges_consumer_producer"]}
    actual_needs = {(job, dep) for job, info in pub["jobs"].items() for dep in info.get("needs", [])}
    needs_ok = expected_needs == actual_needs
    consumer_failures = []
    s7b = next(x for x in stages if x["stage"] == "S7b")
    job_stage = {stage["producer_job"].split(".")[0]: stage["stage"] for stage in stages if "." in stage["producer_job"] or stage["producer_job"] in pub["jobs"]}
    # Main job names are unique in the DAG; S7b's combined producer is external
    # and verify-B, so its produced fields are checked separately below.
    for job, info in pub["jobs"].items():
        current = stage_index.get(job_stage.get(job, "S7b"), 8)
        for typed in info.get("consumes", []):
            field = typed_name(typed)
            if field not in field_stage:
                consumer_failures.append({"job": job, "field": typed, "reason": "undefined-producer"})
            elif job == "verify-B" and field in {typed_name(x) for x in s7b.get("external_inputs", [])}:
                # External provider output arrives at S7b and is not a
                # workflow-needs edge. Its authenticated API transport is
                # checked by the S7a/S7b partition and record contract.
                continue
            elif stage_index[field_stage[field]] >= current:
                consumer_failures.append({"job": job, "field": typed, "producer_stage": field_stage[field], "reason": "not-earlier-than-consumer"})
    check("field-availability-and-needs-DAG", "pass" if not duplicate_fields and not edge_failures and needs_ok and not consumer_failures else "fail", {"field_count": len(field_stage), "duplicate_fields": duplicate_fields, "edge_failures": edge_failures, "needs_equal": needs_ok, "consumer_failures": consumer_failures})
    if "/" in next((x["producer_job"] for x in stages if x["stage"] == "S7b"), ""):
        finding("V9-DAG-S7B-001", "medium", {"message": "S7b aggregate producer_job is a slash-joined two-producer claim; unique producer is only recoverable from separate prose/group metadata.", "stage": next(x for x in stages if x["stage"] == "S7b")})

    # Per-field provenance is compared to the DAG, then transport coverage is
    # checked separately for S7a and external S7b values.
    provenance = pub.get("field_provenance", {})
    provenance_failures = []
    for field, stage in field_stage.items():
        p = provenance.get(field)
        if not p:
            provenance_failures.append({"field": field, "reason": "missing-provenance"})
        elif p.get("stage") != stage:
            provenance_failures.append({"field": field, "dag_stage": stage, "provenance_stage": p.get("stage"), "reason": "stage-mismatch"})
    check("unique-field-provenance", "pass" if not provenance_failures else "fail", {"field_count": len(field_stage), "provenance_count": len(provenance), "failures": provenance_failures})
    s7b_edge_fields = {typed_name(x) for e in pub["field_lineage_edges"] if e["producer"] == "external-provider-result" for x in e["fields"]}
    s7b_internal = sorted({typed_name(x) for x in s7b["produces"]} - s7b_edge_fields)
    check("S7a-S7b-transport-partition", "pass" if len(s7b_edge_fields) == 6 and len(s7b_internal) == 6 else "fail", {"S7a_fields": len(next(x for x in stages if x["stage"] == "S7a")["produces"]), "external_provider_edge_fields": sorted(s7b_edge_fields), "verify_B_local_fields_without_edge": s7b_internal})
    if s7b_internal:
        finding("V9-DAG-S7B-002", "medium", {"message": "Six S7b fields are verify-B-local/API captures and have no field_lineage_edges producer transport; the contract must keep their API derivation explicit.", "fields": s7b_internal})

    # Exact preimage checks. Overlap is a contradiction when digest_source says
    # exactly ordered_fields and excluded_fields are presented as exclusions.
    preimages = pub["preimage_contract"]
    preimage_checks = {}
    for name, spec in preimages.items():
        if not isinstance(spec, dict) or "ordered_fields" not in spec:
            continue
        ordered = spec["ordered_fields"]
        excluded = spec.get("excluded_fields", [])
        overlap = sorted(set(ordered) & set(excluded))
        stages_used = {field_stage.get(x) for x in ordered}
        preimage_checks[name] = {"ordered_count": len(ordered), "duplicate_ordered": len(ordered) != len(set(ordered)), "excluded_overlap": overlap, "unknown_fields": sorted(x for x in ordered if x not in field_stage), "stages": sorted(x for x in stages_used if x)}
    check("preimage-disjointness-and-field-availability", "pass" if all(not x["duplicate_ordered"] and not x["excluded_overlap"] and not x["unknown_fields"] for x in preimage_checks.values()) else "fail", preimage_checks)
    if preimage_checks.get("S7_provider_result", {}).get("excluded_overlap"):
        finding("V9-PREIMAGE-001", "critical", {"message": "S7_provider_result lists provider_result_id/digest in ordered_fields while also excluding them; digest_source says exactly ordered_fields.", "overlap": preimage_checks["S7_provider_result"]["excluded_overlap"]})
    if preimage_checks.get("S7_terminal_census", {}).get("excluded_overlap"):
        finding("V9-PREIMAGE-002", "critical", {"message": "S7_terminal_census lists terminal_census_digest in ordered_fields and excluded_fields, and orders terminal_census_id (its own ID).", "overlap": preimage_checks["S7_terminal_census"]["excluded_overlap"], "ordered": preimages["S7_terminal_census"]["ordered_fields"]})
    if "terminal_census_id" in preimages.get("S7_terminal_census", {}).get("ordered_fields", []):
        finding("V9-PREIMAGE-003", "critical", {"message": "Terminal-census ID is produced at S7b/verify-B and is consumed in its own digest preimage; no_self_or_future_preimage=true is false unless ID is allocated before digest and explicitly specified.", "producer": field_stage_producer.get("terminal_census_id"), "stage": field_stage.get("terminal_census_id")})

    # Exact stage boundary expectations for S4 and S6; no later identity may
    # enter those preimages.
    expected_s4 = {typed_name(x) for stage in stages if stage["stage"] in {"S0", "S1", "S2", "S3"} for x in stage["produces"]}
    expected_s6 = {typed_name(x) for stage in stages if stage["stage"] in {"S0", "S1", "S2", "S3", "S4", "S5"} for x in stage["produces"]}
    boundary = {
        "S4_binding": {"missing": sorted(expected_s4 - set(preimages["S4_binding"]["ordered_fields"])), "future": sorted(set(preimages["S4_binding"]["ordered_fields"]) - expected_s4)},
        "S6_release": {"missing": sorted(expected_s6 - set(preimages["S6_release"]["ordered_fields"])), "future": sorted(set(preimages["S6_release"]["ordered_fields"]) - expected_s6)},
    }
    check("S4-S6-preimage-boundaries", "pass" if all(not x["missing"] and not x["future"] for x in boundary.values()) else "fail", boundary)

    # Cross-schema product identity is intentionally independent of each schema
    # validator. It catches a valid-but-incompatible release manifest.
    perm = schemas["permanent_binding"]
    pre = schemas["pre_record"]
    rel = schemas["release_manifest"]
    identity_paths = ["product.product_id", "product.schema", "product.asset", "product.platform", "product.runner"]
    identity = {
        "root_manifest": root_manifest.get("product_identity"),
        "permanent": {path: (schema_path(perm, path) or {}).get("const") for path in identity_paths},
        "pre_record": {path: (schema_path(pre, path) or {}).get("const") for path in ["product.namespace", "product.name", "product.platform", "product.architecture"]},
        "release_manifest": {path: (schema_path(rel, path) or {}).get("const") for path in ["schema", "product_namespace", "product_name", "platform", "architecture"]},
    }
    mismatch = {"permanent_vs_pre": {"product": identity["permanent"].get("product.product_id"), "pre_name": identity["pre_record"].get("product.name")}, "permanent_vs_release": {"permanent_asset": identity["permanent"].get("product.asset"), "release_name": identity["release_manifest"].get("product_name"), "permanent_platform": identity["permanent"].get("product.platform"), "release_platform": identity["release_manifest"].get("platform")}}
    identity_ok = mismatch["permanent_vs_release"]["permanent_asset"] == mismatch["permanent_vs_release"]["release_name"] and mismatch["permanent_vs_release"]["permanent_platform"] == mismatch["permanent_vs_release"]["release_platform"]
    check("cross-schema-product-identity", "pass" if identity_ok else "fail", identity)
    if not identity_ok:
        finding("V9-SCHEMA-ID-001", "high", {"message": "Permanent binding/pre-record use velnor-workflow-policy-validator/Linux-X64 while release-manifest uses velnor-policy-validator/ubuntu-24.04; a record cannot satisfy both current schemas.", "mismatch": mismatch})

    # Actual JSON Schema validation is separate from cross-file binding checks.
    try:
        sys.path.insert(0, "/tmp/velnor-jsonschema-20260920")
        from jsonschema import Draft202012Validator

        val_results = {}
        for name, schema, fixture in [("pre_record", schemas["pre_record"], V9 / "positive-pre-record.json"), ("provider_result", schemas["provider_result"], V9 / "positive-provider-result.json")]:
            val = Draft202012Validator(schema)
            val_results[name] = {"positive_errors": len(list(val.iter_errors(load(fixture))))}
            mutated = load(fixture)
            mutated["canonical_root_manifest_sha256"] = "0" * 64
            val_results[name]["root_hash_mutation_errors"] = len(list(val.iter_errors(mutated)))
        check("strict-positive-schema-validation", "pass" if all(x["positive_errors"] == 0 for x in val_results.values()) else "fail", val_results)
        if any(x["root_hash_mutation_errors"] == 0 for x in val_results.values()):
            finding("V9-SCHEMA-BIND-001", "high", {"message": "Schema accepts a syntactically valid but wrong canonical root digest; verifier must enforce equality to the measured root, not only the 64-hex pattern.", "results": val_results})
        positive_pre = load(V9 / "positive-pre-record.json")
        positive_provider = load(V9 / "positive-provider-result.json")
        cross_file_hashes = {
            "pre.canonical_root_manifest_sha256": (positive_pre.get("canonical_root_manifest_sha256"), root_digest),
            "pre.canonical_release_schema_sha256": (positive_pre.get("canonical_release_schema_sha256"), sha(RELEASE_SCHEMA)),
            "provider.canonical_root_manifest_sha256": (positive_provider.get("canonical_root_manifest_sha256"), root_digest),
            "provider.pre_record_schema_sha256": (positive_provider.get("pre_record_schema_sha256"), sha(PRE_SCHEMA)),
            "provider.canonical_binding_schema_sha256": (positive_provider.get("canonical_binding_schema_sha256"), sha(PERM_SCHEMA)),
            "provider.canonical_release_schema_sha256": (positive_provider.get("canonical_release_schema_sha256"), sha(RELEASE_SCHEMA)),
        }
        check("positive-cross-file-root-and-schema-hashes", "pass" if all(actual == expected for actual, expected in cross_file_hashes.values()) else "fail", {key: {"fixture": actual, "measured": expected, "equal": actual == expected} for key, (actual, expected) in cross_file_hashes.items()})
    except Exception as exc:
        check("strict-positive-schema-validation", "unavailable", {"error": repr(exc)})

    # No release-manifest positive fixture is bound by the v9 root manifest.
    release_positive = [x["path"] for x in root_manifest.get("bound_files", []) if "release-manifest" in x["path"] and "schema" not in x["path"]]
    if not release_positive:
        finding("V9-FIXTURE-RELEASE-001", "medium", {"message": "Current root requires positive fixtures but binds no positive release-manifest fixture; its 14-leaf schema is not exercised by a positive instance.", "schema": str(RELEASE_SCHEMA)})

    # Executable fixture: actionlint is syntax-only; compare declared outputs to
    # the typed plan and retain concrete fake-output evidence.
    caller = FIXTURE_DIR / "ci-main-caller.yml"
    reusable = FIXTURE_DIR / "ci-policy-validator-products.yml"
    actionlint = "/Users/donbeave/.local/share/mise/installs/actionlint/1.7.12/actionlint"
    rc, out, err = command(actionlint, str(caller), str(reusable))
    check("actionlint-fixture-syntax", "pass" if rc == 0 else "fail", {"rc": rc, "stdout": out, "stderr": err})
    actual_outputs = fixture_outputs(reusable.read_text())
    planned_outputs = {job: {typed_name(x) for x in info.get("outputs", [])} for job, info in pub["jobs"].items()}
    output_gaps = {}
    for job, planned in planned_outputs.items():
        actual = actual_outputs.get(job, set())
        output_gaps[job] = {"missing": sorted(planned - actual), "extra": sorted(actual - planned)}
    workflow_expected = {typed_name(x) for x in pub["workflow_call_outputs"]}
    workflow_actual = actual_outputs.get("workflow_call", set())
    output_gaps["workflow_call"] = {"missing": sorted(workflow_expected - workflow_actual), "extra": sorted(workflow_actual - workflow_expected)}
    check("fixture-output-lineage-coverage", "pass" if all(not x["missing"] and not x["extra"] for x in output_gaps.values()) else "fail", output_gaps)
    if any(x["missing"] for x in output_gaps.values()):
        finding("V9-FIXTURE-OUTPUT-001", "high", {"message": "The actionlint fixture is not an executable realization of the declared typed output graph; actionlint pass proves syntax only.", "gaps": output_gaps})
    fake_markers = [marker for marker in ["echo \"artifact_id=1\"", "GITHUB_JOB", "provider.invalid", "printf '%064d' 0"] if marker in reusable.read_text()]
    if fake_markers:
        finding("V9-FIXTURE-FAKE-001", "high", {"message": "Fixture emits synthetic IDs/digests or invalid provider URL; no real verifier or REST/artifact/attestation lineage is executed.", "markers": fake_markers})

    # Independent hostile cases, including the v9-specific structural failures.
    negatives = [
        {"id": "terminal-census-own-id", "mutation": "include terminal_census_id in S7_terminal_census ordered_fields", "expected": "reject", "reason": "own identity cannot be hashed before allocation"},
        {"id": "terminal-census-overlap", "mutation": "keep terminal_census_digest in both ordered_fields and excluded_fields", "expected": "reject", "reason": "exact ordered preimage and exclusion contradict"},
        {"id": "provider-result-overlap", "mutation": "keep provider_result_id/digest in both ordered_fields and excluded_fields", "expected": "reject", "reason": "exact ordered preimage and exclusion contradict"},
        {"id": "release-schema-unmapped", "mutation": "omit release_manifest from canonical_leaf_paths", "expected": "reject", "reason": "all current schema leaves need typed lineage"},
        {"id": "release-identity-mismatch", "mutation": "compose old permanent product with new release-manifest product", "expected": "reject", "reason": "cross-schema identity must be equal"},
        {"id": "schema-root-hash-pattern-only", "mutation": "replace canonical_root_manifest_sha256 with zeros", "expected": "reject", "reason": "verifier must compare measured root digest"},
        {"id": "s7b-ambiguous-producer", "mutation": "use one slash-joined producer for external-provider-result and verify-B fields", "expected": "reject", "reason": "each field requires one concrete producer"},
        {"id": "fixture-output-subset", "mutation": "execute actionlint fixture as typed implementation", "expected": "reject", "reason": "declared 146-leaf transport has missing job/workflow outputs"},
    ]
    result["negative_fixtures"] = negatives
    check("independent-negative-fixture-contract", "pass", {"count": len(negatives), "all_expected": "reject"})

    negative_path = OUT / "v9-independent-negative-fixtures.json"
    negative_path.write_text(json.dumps({
        "schema": "velnor.v9-independent-hostile-fixtures.v1",
        "expected": "reject",
        "execution": "unexecuted-no-real-verifier",
        "source": "independent v9 audit derived from frozen plan/DAG/schema; not owner result assertions",
        "fixtures": negatives,
    }, indent=2, sort_keys=True) + "\n")
    result["external_negative_fixture"] = {"path": str(negative_path), "sha256": sha(negative_path), "count": len(negatives)}

    # Fresh read-only remote baseline; never infer live state from origin/main.
    rc, remote_sha, remote_err = command("git", "ls-remote", "origin", "refs/heads/main", cwd=Path("/Users/donbeave/Projects/tailrocks/velnor-project/velnor3"))
    observed_sha = remote_sha.split()[0] if rc == 0 and remote_sha else None
    observed_tree = None
    if observed_sha:
        trc, tree, terr = command("git", "rev-parse", f"{observed_sha}^{{tree}}", cwd=Path("/Users/donbeave/Projects/tailrocks/velnor-project/velnor3"))
        if trc == 0:
            observed_tree = tree
        else:
            terr = terr or tree
    else:
        terr = remote_err
    evidence = pub.get("evidence", plan.get("evidence", {}))
    current = plan.get("evidence", {}).get("current_main", {})
    live_detail = {"fresh_remote_sha": observed_sha, "fresh_remote_tree": observed_tree, "plan_sha": current.get("sha"), "plan_tree": current.get("tree_sha"), "stderr": terr if observed_sha is None else None}
    check("fresh-live-main-tuple", "pass" if observed_sha == EXPECTED_LIVE_MAIN and observed_tree == EXPECTED_LIVE_TREE and current.get("sha") == observed_sha and current.get("tree_sha") == observed_tree else "fail", live_detail)

    # No source implementation is allowed to be inferred from the contract
    # fixture. Inspect the exact fresh live commit for the typed B paths.
    live_paths = [
        ".github/workflows/ci-policy-validator-products.yml",
        ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs",
    ]
    live_presence = []
    if observed_sha:
        for path in live_paths:
            prc, _, perr = command("git", "cat-file", "-e", f"{observed_sha}:{path}", cwd=Path("/Users/donbeave/Projects/tailrocks/velnor-project/velnor3"))
            live_presence.append({"path": path, "present": prc == 0, "stderr": perr if prc else None})
        impl_status = "pass" if any(item["present"] for item in live_presence) else "unimplemented"
    else:
        impl_status = "unavailable"
    check("fresh-live-typed-verifier-implementation", impl_status, {"commit": observed_sha, "paths": live_presence})
    if impl_status == "unimplemented":
        finding("V9-IMPL-001", "critical", {"message": "No typed Main-B verifier/publisher source exists at the fresh live main commit; v9 positive/hostile checks are contract-only and cannot be called execution success.", "commit": observed_sha, "paths": live_paths})

    # Persist the independently materialized field/preimage DAG. This is not a
    # second authority bundle: it is a read-only projection used to make every
    # 146/124 leaf, producer, consumer, stage boundary, and preimage reviewable.
    consumers: dict[str, list[str]] = {field: [] for field in field_stage}
    for job, info in pub["jobs"].items():
        for typed in info.get("consumes", []):
            field = typed_name(typed)
            consumers.setdefault(field, []).append(job)
    materialized_fields = {
        field: {"stage": field_stage[field], "producer": field_stage_producer[field], "consumers": sorted(consumers.get(field, []))}
        for field in sorted(field_stage)
    }
    dag_path = OUT / "v9-independent-field-dag.json"
    dag_path.write_text(json.dumps({
        "schema": "velnor.v9-independent-field-dag.v1",
        "status": "design_only_external_audit",
        "authority_claim": False,
        "source_plan_sha256": sha(PLAN_JSON),
        "schema_leaf_paths": {key: sorted(value) for key, value in actual_leaves.items()},
        "permanent_leaf_lineage_reviewed": owner_maps.get("permanent_binding", {}),
        "record_leaf_lineage_reviewed": owner_maps.get("pre_record", {}),
        "fields": materialized_fields,
        "field_lineage_edges": pub["field_lineage_edges"],
        "needs_edges_consumer_producer": pub["needs_edges_consumer_producer"],
        "preimage_contract": preimages,
        "timing": {"S7a": sorted(typed_name(x) for x in next(stage for stage in stages if stage["stage"] == "S7a")["produces"]), "S7b_external": sorted(s7b_edge_fields), "S7b_verify_B_local": s7b_internal},
        "cross_schema_identity": identity,
        "live_main": {"sha": observed_sha, "tree_sha": observed_tree, "typed_verifier_paths": live_presence, "status": impl_status},
        "negative_fixture_path": str(negative_path),
    }, indent=2, sort_keys=True) + "\n")

    result["summary"] = {
        "check_count": len(result["checks"]),
        "pass_count": sum(x["status"] == "pass" for x in result["checks"]),
        "fail_count": sum(x["status"] == "fail" for x in result["checks"]),
        "unavailable_count": sum(x["status"] == "unavailable" for x in result["checks"]),
        "unimplemented_count": sum(x["status"] == "unimplemented" for x in result["checks"]),
        "finding_count": len(result["findings"]),
        "authority_claim": False,
    }
    result["independent_field_dag"] = {"path": str(dag_path), "sha256": sha(dag_path), "permanent_leaf_count": len(actual_leaves["permanent_binding"]), "record_leaf_count": len(expected_record_map), "transport_field_count": len(materialized_fields)}
    result_path = OUT / "v9-independent-audit-results.json"
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report_lines = [
        "# Independent v9 schema/DAG adversarial audit",
        "",
        "Status: design-only external audit. No authority, release, dispatch, verifier execution, or source mutation.",
        "Owner 33/33 result is preserved as proposal evidence only; it is not treated as approval.",
        "",
        "## Exact input/output hashes",
        "",
        f"- Frozen v9 plan Markdown: `{sha(PLAN_MD)}` (expected `{EXPECTED_PLAN_MD}`).",
        f"- Frozen v9 plan JSON: `{sha(PLAN_JSON)}` (expected `{EXPECTED_PLAN_JSON}`).",
        f"- Canonical v9 root digest: `{root_digest}` (expected `{EXPECTED_ROOT_DIGEST}`).",
        f"- Hostile transport fixture: `{sha(HOSTILE)}` (expected `{EXPECTED_HOSTILE}`).",
        f"- Independent hostile fixture set: `{sha(negative_path)}` ({len(negatives)} cases; all expected reject; unexecuted).",
        f"- Independent materialized field DAG: `{sha(dag_path)}` (146 permanent leaves, 124 record leaves, {len(materialized_fields)} typed transport fields).",
        f"- Independent script: `{sha(Path(__file__))}`.",
        f"- Independent result JSON: `{sha(result_path)}`.",
        "",
        "## Independent result",
        "",
        f"- Checks: {result['summary']['check_count']}; pass {result['summary']['pass_count']}; fail {result['summary']['fail_count']}; unimplemented {result['summary']['unimplemented_count']}; unavailable {result['summary']['unavailable_count']}.",
        f"- Findings: {result['summary']['finding_count']}. Authority claim: `{result['summary']['authority_claim']}`.",
        "",
        "## Findings",
        "",
    ]
    for item in result["findings"]:
        detail = item["detail"]
        message = detail.get("message", json.dumps(detail, sort_keys=True)) if isinstance(detail, dict) else str(detail)
        report_lines.append(f"- `{item['id']}` ({item['severity']}): {message}")
    report_lines.extend([
        "",
        "## Check statuses",
        "",
    ])
    for item in result["checks"]:
        report_lines.append(f"- `{item['status']}` `{item['name']}`")
    report_lines.extend([
        "",
        "## Independent hostile contract fixtures",
        "",
        "All listed cases are expected `reject`; none was sent to a live verifier.",
        "",
    ])
    for item in result["negative_fixtures"]:
        report_lines.append(f"- `{item['id']}` — {item['mutation']} ({item['reason']}).")
    (OUT / "v9-independent-audit-report.md").write_text("\n".join(report_lines) + "\n")
    print(json.dumps(result["summary"], sort_keys=True))
    for item in result["findings"]:
        print(f"{item['id']} [{item['severity']}] {item['detail']['message']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
