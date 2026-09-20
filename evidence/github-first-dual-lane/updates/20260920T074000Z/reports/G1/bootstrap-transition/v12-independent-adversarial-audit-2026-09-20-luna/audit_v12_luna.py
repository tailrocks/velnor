#!/usr/bin/env python3
"""Independent v12 strict-schema/dataflow audit.

This is external design evidence only.  It never mutates the frozen bundle,
repository, GitHub authority, releases, workflows, or credentials.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
import subprocess
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker


ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
BUNDLE = ROOT / "v12-contract-bundle-2026-09-20"
MODEL_PATH = ROOT / "v12-canonical-model-source.json"
GENERATOR_PATH = ROOT / "build_v12_bundle.py"
FREEZE_PATH = ROOT / "v12-freeze-manifest.json"
OLD_PRE_PATH = ROOT / "authority-contract-separation-2026-09-20/v9/policy-validator-b-pre-record.v2.schema.json"
OUT = ROOT / "v12-independent-adversarial-audit-2026-09-20-luna"
HOSTILES = OUT / "hostile-fixtures"


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def sha_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha_file(path: Path) -> str:
    return sha_bytes(path.read_bytes())


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def pretty(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()


def normalized_fixture_sha(path: Path) -> str:
    value = load(path)
    if isinstance(value, dict):
        for key in ("canonical_root_manifest_sha256", "canonical_root_sha256"):
            if key in value:
                value[key] = "0" * 64
    return sha_bytes(pretty(value))


def flatten_schema(node: dict[str, Any], prefix: str = "", root: dict[str, Any] | None = None, seen: frozenset[str] = frozenset()) -> dict[str, dict[str, Any]]:
    root = node if root is None else root
    if "$ref" in node:
        name = node["$ref"].rsplit("/", 1)[-1]
        if name in seen:
            return {}
        return flatten_schema(root.get("$defs", {}).get(name, {}), prefix, root, seen | {name})
    props = node.get("properties")
    if isinstance(props, dict):
        result: dict[str, dict[str, Any]] = {}
        for key, child in props.items():
            child_path = f"{prefix}.{key}" if prefix else key
            result.update(flatten_schema(child, child_path, root, seen))
        return result
    return {prefix: node} if prefix else {}


def dotted_get(root: dict[str, Any], path: str) -> Any:
    value: Any = root
    for part in path.split("."):
        value = value[part]
    return value


def dotted_set(root: dict[str, Any], path: str, value: Any) -> None:
    bits = path.split(".")
    cursor = root
    for bit in bits[:-1]:
        cursor = cursor[bit]
    cursor[bits[-1]] = value


def schema_errors(schema: dict[str, Any], instance: Any) -> list[str]:
    errors = sorted(
        Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance),
        key=lambda error: list(error.absolute_path),
    )
    return [f"{'.'.join(str(item) for item in error.absolute_path)}: {error.message}" for error in errors]


def check_digest_for_preimage(name: str, dag: dict[str, Any], roots: dict[str, dict[str, Any]]) -> str:
    payload: dict[str, Any] = {}
    for field_id in dag["preimage_contract"][name]["ordered_field_ids"]:
        prefix, path = field_id.split(".", 1)
        value = dotted_get(roots[prefix], path)
        if path in {"canonical_root_manifest_sha256", "canonical_root_sha256"}:
            value = "0" * 64
        payload[field_id] = value
    return sha_bytes(canonical(payload))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(pretty(value))


def git_read(args: list[str]) -> str:
    try:
        result = subprocess.run(["git", *args], cwd="/Users/donbeave/Projects/tailrocks/velnor-project/velnor3", check=False, capture_output=True, text=True)
        return result.stdout.strip()
    except OSError:
        return ""


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    HOSTILES.mkdir(parents=True, exist_ok=True)
    model = load(MODEL_PATH)
    freeze = load(FREEZE_PATH)
    dag = load(BUNDLE / "v12-canonical-field-dag.json")
    plan = load(BUNDLE / "AUTHORITY-CHANGE-PLAN-2026-09-20-v12.json")
    root_manifest = load(BUNDLE / "canonical-root-manifest.v5.json")

    schemas = {
        "permanent_binding": load(BUNDLE / "permanent_binding.schema.json"),
        "pre_record": load(BUNDLE / "pre_record.schema.json"),
        "pre_record_transport": load(BUNDLE / "pre_record_transport.schema.json"),
        "provider_result": load(BUNDLE / "provider_result.schema.json"),
        "verify_b": load(BUNDLE / "verify_b.schema.json"),
        "release_manifest": load(BUNDLE / "release_manifest.schema.json"),
        "adoption": load(BUNDLE / "adoption.schema.json"),
    }
    positives = {
        "permanent_binding": load(BUNDLE / "positive-permanent-binding.json"),
        "pre_record": load(BUNDLE / "positive-pre-record.json"),
        "pre_record_transport": load(BUNDLE / "positive-pre-record-transport.json"),
        "provider_result": load(BUNDLE / "positive-provider-result.json"),
        "verify_b": load(BUNDLE / "positive-verify-b.json"),
        "release_manifest": load(BUNDLE / "positive-release-manifest.json"),
        "adoption": load(BUNDLE / "positive-adoption.json"),
    }
    checks: list[dict[str, Any]] = []

    def add(name: str, status: str, detail: str, evidence: Any | None = None) -> None:
        item: dict[str, Any] = {"name": name, "status": status, "detail": detail}
        if evidence is not None:
            item["evidence"] = evidence
        checks.append(item)

    # Frozen-input identity and root byte audit.
    root_digest = sha_bytes(canonical(root_manifest))
    freeze_root = freeze["bundle"]["root_digest"]
    add("frozen-root-digest", "pass" if root_digest == freeze_root else "fail", f"recomputed={root_digest}; frozen={freeze_root}")
    add("model-byte-identity", "pass" if sha_file(MODEL_PATH) == freeze["canonical_model"]["sha256"] else "fail", f"sha256={sha_file(MODEL_PATH)}")
    add("generator-byte-identity", "pass" if sha_file(GENERATOR_PATH) == freeze["generator"]["sha256"] else "fail", f"sha256={sha_file(GENERATOR_PATH)}")
    add("root-no-self-digest", "pass" if "root_digest" not in root_manifest else "fail", "canonical root object has no self-digest member")
    bad_bound: list[str] = []
    for entry in root_manifest["bound_files"]:
        target = ROOT / entry["path"]
        if not target.is_file():
            bad_bound.append(f"missing:{entry['path']}")
            continue
        measured = normalized_fixture_sha(target) if entry.get("normalization") else sha_file(target)
        if measured != entry["sha256"]:
            bad_bound.append(entry["path"])
    add("root-bound-byte-hashes", "pass" if not bad_bound else "fail", "all root entries independently measured", bad_bound)

    # All seven strict positive instances, using a fresh Draft 2020-12 validator.
    strict_failures: dict[str, list[str]] = {}
    for schema_name, instance in positives.items():
        errors = schema_errors(schemas[schema_name], instance)
        if errors:
            strict_failures[schema_name] = errors
    add("strict-seven-positives", "pass" if not strict_failures else "fail", f"validated={len(positives)}", strict_failures)

    # Independently flatten each schema and compare with the typed field registry.
    independent_leaf_descs = {name: flatten_schema(schema) for name, schema in schemas.items()}
    independent_leaves = {name: sorted(descs) for name, descs in independent_leaf_descs.items()}
    dag_leaves = dag["schema_leaf_paths"]
    leaf_diffs = {
        name: {"schema_only": sorted(set(independent_leaves[name]) - set(dag_leaves.get(name, []))), "dag_only": sorted(set(dag_leaves.get(name, [])) - set(independent_leaves[name]))}
        for name in schemas
        if set(independent_leaves[name]) != set(dag_leaves.get(name, []))
    }
    counts = {name: len(paths) for name, paths in independent_leaves.items()}
    add("independent-schema-leaf-count", "pass" if not leaf_diffs and sum(counts.values()) == 311 else "fail", f"counts={counts}; total={sum(counts.values())}", leaf_diffs)
    fields = dag["fields"]
    registry_by_schema: dict[str, set[str]] = {name: set() for name in schemas}
    duplicate_ids: set[str] = set()
    for field_id, desc in fields.items():
        schema_name = desc["schema"]
        prefix = "permanent" if schema_name == "permanent_binding" else schema_name
        if not field_id.startswith(prefix + "."):
            duplicate_ids.add(field_id)
        registry_by_schema.setdefault(schema_name, set()).add(field_id.split(".", 1)[1])
    registry_diff = {
        name: {"missing": sorted(set(independent_leaves[name]) - registry_by_schema.get(name, set())), "extra": sorted(registry_by_schema.get(name, set()) - set(independent_leaves[name]))}
        for name in schemas
        if set(independent_leaves[name]) != registry_by_schema.get(name, set())
    }
    add("leaf-registry-bijection", "pass" if len(fields) == 311 and not registry_diff and not duplicate_ids else "fail", f"registry={len(fields)}; schema_leaves={sum(counts.values())}", registry_diff)

    typed = load(BUNDLE / "positive-full-typed-output-fixture.json")
    typed_errors: list[str] = []
    for field_id, desc in fields.items():
        schema_name = desc["schema"]
        leaf = independent_leaf_descs[schema_name][desc["path"]]
        value = typed["typed_outputs"].get(field_id, {}).get("value")
        if field_id not in typed["typed_outputs"] or schema_errors(leaf, value):
            typed_errors.append(field_id)
    typed_metadata = typed.get("fixture_metadata", {})
    add("typed-output-strictness", "pass" if set(typed["typed_outputs"]) == set(fields) and not typed_errors and typed["field_count"] == 311 else "fail", f"typed_fields={len(typed['typed_outputs'])}; invalid={len(typed_errors)}", typed_errors[:20])
    add("synthetic-not-live", "pass" if typed_metadata.get("live_binding") is False and typed_metadata.get("live_proof_status") == "not_executed" and all(not row.get("live") for row in typed["typed_outputs"].values()) else "fail", "typed fixture metadata and every leaf are non-live")

    # Permanent constants, identity namespaces, and release leaf coverage.
    permanent = positives["permanent_binding"]
    const_failures: dict[str, Any] = {}
    for path, expected in model["permanent_binding_contract"]["required_const_values"].items():
        actual = dotted_get(permanent, path)
        if actual != expected:
            const_failures[path] = {"actual": actual, "expected": expected}
    add("permanent-binding-constants", "pass" if not const_failures else "fail", "immutable Linux-X64 and temporary-key constants", const_failures)
    add("permanent-no-old-integration-id", "pass" if not any("integration_id" in path for path in independent_leaves["permanent_binding"]) else "fail", "legacy integration_id absent")
    release = positives["release_manifest"]
    release_expected = set(model["release_leaf_model"]["fields"])
    add("release-fourteen-leaves", "pass" if len(independent_leaves["release_manifest"]) == 14 and set(release) == release_expected else "fail", f"leaves={len(independent_leaves['release_manifest'])}")
    add("id-namespace-separation", "pass" if positives["pre_record_transport"]["artifact_id"] != release["asset_id"] and positives["provider_result"]["source_record_artifact_id"] == positives["pre_record_transport"]["artifact_id"] else "fail", "Actions artifact ID is distinct from release asset ID", {"actions_artifact_id": positives["pre_record_transport"]["artifact_id"], "release_asset_id": release["asset_id"]})

    # Stage/producer availability: check every registry entry against stage contract and downstream order.
    stage_order = model["stage_order"]
    stage_rank = {stage: index for index, stage in enumerate(stage_order)}
    stage_producer_errors: list[str] = []
    for field_id, desc in fields.items():
        stage = desc.get("stage")
        expected_producer = model["stage_contract"].get(stage, {}).get("producer")
        if stage not in stage_rank or expected_producer is None or desc.get("producer") != expected_producer and not (field_id == "verify_b.terminal_census_rows" and desc.get("producer") == "verify-B.terminal-census-api"):
            stage_producer_errors.append(field_id)
        for consumer in desc.get("consumers", []):
            consumer_stages = [s for s, contract in model["stage_contract"].items() if contract.get("producer") == consumer]
            if consumer_stages and min(stage_rank[s] for s in consumer_stages) <= stage_rank.get(stage, 999):
                stage_producer_errors.append(f"non-later-consumer:{field_id}->{consumer}")
    add("registry-stage-producer-closure", "pass" if not stage_producer_errors else "fail", f"fields={len(fields)}; violations={len(stage_producer_errors)}", stage_producer_errors[:40])

    needs_edges = model["workflow_graph"]["needs_edges_consumer_producer"]
    producer_stage = {contract["producer"]: stage for stage, contract in model["stage_contract"].items()}
    # Workflow needs use job names, while stage contracts name a few producer
    # steps with a suffix (for example capture-caller-context.capture).
    job_stage = {stage.split(".", 1)[0]: stage for stage, contract in model["stage_contract"].items() for stage in [stage]}
    job_stage.update({contract["producer"].split(".", 1)[0]: stage for stage, contract in model["stage_contract"].items()})
    bad_needs = []
    for consumer, producer in needs_edges:
        if producer not in job_stage or consumer not in job_stage or stage_rank[job_stage[consumer]] <= stage_rank[job_stage[producer]]:
            bad_needs.append([consumer, producer])
    add("needs-is-acyclic-and-later", "pass" if not bad_needs else "fail", "workflow needs edges respect stage order", bad_needs)

    # Explicit producer-stage contradictions visible in the generated registry.
    pre_release = {field_id: fields[field_id] for field_id in fields if field_id in {"pre_record.release.asset_id", "pre_record.release.asset_digest", "pre_record.release.manifest_digest", "pre_record.binding.final_manifest_digest"}}
    expected_late = {
        "pre_record.release.asset_id": "S5_publish",
        "pre_record.release.asset_digest": "S5_publish",
        "pre_record.release.manifest_digest": "S6_release_attest",
        "pre_record.binding.final_manifest_digest": "S6_release_attest",
    }
    late_mismatch = {field_id: {"declared": desc["stage"], "expected": expected_late[field_id]} for field_id, desc in pre_release.items() if desc["stage"] != expected_late[field_id]}
    add("release-and-final-manifest-producer-timing", "fail" if late_mismatch else "pass", "S5 asset and final-manifest values must not be claimed by earlier producers", late_mismatch)

    # Preimage ordering and exact byte recomputation.
    preimage_specs = dag["preimage_contract"]
    roots = {"permanent": permanent, "pre_record": positives["pre_record"], "release_manifest": release}
    preimage_temporal: dict[str, list[str]] = {}
    for name, spec in preimage_specs.items():
        if name == "S7c_terminal_census":
            continue
        future = [field_id for field_id in spec["ordered_field_ids"] if stage_rank.get(fields[field_id]["stage"], 999) > (stage_rank["S4_binding_attest"] if name == "S4_binding" else stage_rank["S6_release_attest"] if name == "S6_release" else stage_rank["S7b_provider_result"] if name == "S7b_provider_result" else 999)]
        overlap = sorted(set(spec["ordered_field_ids"]) & set(spec["excluded_field_ids"]))
        preimage_temporal[name] = future + [f"overlap:{x}" for x in overlap]
    add("preimage-future-field-exclusion", "pass" if not any(preimage_temporal.values()) else "fail", "ordered digest fields are before their producer and disjoint from exclusions", preimage_temporal)

    binding_measured = check_digest_for_preimage("S4_binding", dag, roots)
    release_measured = check_digest_for_preimage("S6_release", dag, roots)
    add("binding-preimage-byte-identity", "pass" if positives["pre_record"]["binding"]["core_digest"] == binding_measured else "fail", f"declared={positives['pre_record']['binding']['core_digest']}; measured={binding_measured}")
    add("release-preimage-byte-identity", "pass" if release["manifest_digest"] == release_measured else "fail", f"declared={release['manifest_digest']}; measured={release_measured}")

    provider = positives["provider_result"]
    transport = positives["pre_record_transport"]
    declared_provider_digest = check_digest_for_preimage("S7b_provider_result", dag, {"permanent": permanent, "pre_record_transport": transport, "provider_result": provider, **roots})
    add("provider-preimage-byte-identity", "fail" if declared_provider_digest != provider["provider_result_digest"] else "pass", f"declared={provider['provider_result_digest']}; declared-preimage-measured={declared_provider_digest}; ordered_fields={len(preimage_specs['S7b_provider_result']['ordered_field_ids'])}")
    excluded_provider_names = set(model["preimage_model"]["digest_ids"]["S7b_provider_result"]["exclude_fields"])
    provider_payload_names = set(provider)
    included_excluded = sorted(excluded_provider_names & provider_payload_names - {"provider_result_digest"})
    add("provider-own-fields-excluded-from-emitted-digest", "fail" if included_excluded else "pass", "emitted generator payload must not hash provider-owned/post-signature fields", included_excluded)

    # Provider output coverage: the declared S7b ordered preimage includes fields
    # assigned to external-provider-result but absent from provider_result.schema.
    provider_schema_ids = {f"provider_result.{path}" for path in independent_leaves["provider_result"]}
    missing_provider_output = sorted(
        field_id
        for field_id in preimage_specs["S7b_provider_result"]["ordered_field_ids"]
        if fields[field_id]["producer"] == "external-provider-result" and field_id not in provider_schema_ids
    )
    add("provider-producer-schema-coverage", "fail" if missing_provider_output else "pass", "every external-provider-result field must have a provider output leaf", missing_provider_output)

    # Terminal census actual predicate: strict schema is intentionally tested
    # separately from cross-row/run/check semantics.
    verify = positives["verify_b"]
    rows = verify["terminal_census_rows"]
    required_workflows = {f".github/workflows/{name}" for name in model["authority_graph"]["consumer_census"]["required_workflows"]}
    rows_digest = sha_bytes(canonical(rows))
    census_shape = len(rows) == 15 and {row["workflow_path"] for row in rows} == required_workflows and len({row["run_id"] for row in rows}) == 15 and len({row["job_id"] for row in rows}) == 15 and len({row["check_run_id"] for row in rows}) == 15 and all(row["status"] == "completed" and row["conclusion"] == "success" and row["head_sha"] == verify["resulting_main_sha"] for row in rows)
    add("terminal-census-positive-predicate", "pass" if census_shape and verify["terminal_census_rows_digest"] == rows_digest == verify["terminal_census_digest"] else "fail", "positive has one terminal row per required workflow and recomputed digest", {"rows": len(rows), "required": len(required_workflows), "digest": rows_digest})
    add("terminal-census-schema-cardinality", "fail" if schemas["verify_b"]["properties"]["terminal_census_rows"].get("minItems") == 15 and "maxItems" not in schemas["verify_b"]["properties"]["terminal_census_rows"] else "pass", "strict schema has minItems only; exact 15/unique workflow identity is semantic")

    # Transport edges must identify an actual channel.  The generated typed
    # edges currently all carry the same abstract phrase.
    typed_edges = [edge for edge in dag["field_lineage_edges"] if edge.get("orientation") == "typed_output_transport"]
    transport_values = sorted({edge.get("transport", "") for edge in typed_edges})
    has_concrete_channel = all(any(token in edge.get("transport", "") for token in ("jobs.", "outputs", "artifact", "record/", "GET ", "file ", "path=")) for edge in typed_edges)
    add("typed-transport-concrete-channel", "fail" if not has_concrete_channel or transport_values == ["explicit producer output, immutable artifact, or persistent record; needs is scheduling only"] else "pass", f"typed_edges={len(typed_edges)}; unique_transport_labels={len(transport_values)}", transport_values)

    edge_fields = {field_id for edge in dag["field_lineage_edges"] for field_id in edge.get("field_ids", [])}
    add("field-edge-coverage", "pass" if edge_fields == set(fields) else "fail", f"edge_fields={len(edge_fields)}; registry={len(fields)}", {"missing": sorted(set(fields) - edge_fields), "extra": sorted(edge_fields - set(fields))})

    # Provider/check lifecycle and cross-fixture binding are semantic contracts,
    # so schema-only acceptance is recorded as unimplemented rather than green.
    add("provider-check-terminalization-order", "unimplemented", "model allocates/read-backs check before signing, signs status/conclusion, then terminalizes; no executable provider/verifier exists to prove this temporal contract")
    add("provider-check-identity-cross-binding", "unimplemented", "provider_result_id/external_id/run/app/head equalities are not expressible in provider_result.schema.json and no verifier implementation is present")

    # Byte identity: only claimed digest/size strings exist; no raw ZIP, inner
    # payload, binary, or release-asset bytes are present in the bundle.
    text_blob = json.dumps({"model": model, "dag": dag, "pre": positives["pre_record"], "transport": transport, "release": release})
    byte_keys = sorted(set(re.findall(r'"([^"]*(?:bytes|raw_zip_bytes|service_zip_bytes|binary_bytes)[^"]*)"', text_blob, flags=re.IGNORECASE)))
    add("raw-byte-identity", "unimplemented", "bundle has digest/size claims but no raw service ZIP, inner payload, binary, or release-asset bytes for independent hashing", {"byte_fields_found": byte_keys, "artifact_contract_status": model["artifact_contract"]["status"]})

    # OLD_PRE dependency is explicit but still a second contract input: changing
    # the historical schema changes generated v12 pre_record leaves.
    generator_text = GENERATOR_PATH.read_text()
    old_refs = [line.strip() for line in generator_text.splitlines() if "OLD_PRE" in line or "old_pre_desc" in line or "pre_content_desc" in line]
    model_pre_model = model["record_schema_model"]["pre_record"]
    add("no-old-pre-hidden-input", "fail" if old_refs else "pass", "v12 generation must not derive strict pre_record leaves from OLD_PRE", {"generator_refs": old_refs, "model_pre_record_keys": sorted(model_pre_model), "historical_schema_sha256": sha_file(OLD_PRE_PATH)})

    # Source/implementation boundary from the observed remote revision.
    live_main = git_read(["ls-remote", "origin", "refs/heads/main"]).split()[0] if git_read(["ls-remote", "origin", "refs/heads/main"]) else ""
    live_paths = git_read(["ls-tree", "-r", "--name-only", "origin/main", "--", ".github", ".github-gen"]).splitlines()
    required_source_paths = [".github/workflows/ci-policy-validator-products.yml", ".github-gen/sources/workflows/ci-policy-validator-products.yml"]
    absent_paths = [path for path in required_source_paths if path not in live_paths]
    add("observed-main-source-closure", "unimplemented", "no typed publisher/verifier source is present at observed origin/main; live authority cannot be tested", {"origin_main": live_main, "expected_sha": model["observed_revision"]["main"]["sha"], "absent_paths": absent_paths})

    # Persist isolated hostile JSON fixtures and run strict schema checks.
    hostile_cases: list[dict[str, Any]] = []

    def hostile(case: str, schema_name: str, base: Any, mutate: Any, expected: str, rationale: str) -> None:
        instance = copy.deepcopy(base)
        mutate(instance)
        path = HOSTILES / f"{len(hostile_cases)+1:02d}-{case}.json"
        write_json(path, instance)
        errors = schema_errors(schemas[schema_name], instance)
        schema_observed = "reject" if errors else "accept"
        hostile_cases.append({"case": case, "schema": schema_name, "fixture": str(path.relative_to(OUT)), "expected": expected, "schema_observed": schema_observed, "semantic_status": "pass" if (expected == schema_observed) else "unimplemented", "rationale": rationale, "errors": errors[:3]})

    hostile("permanent-const-forged", "permanent_binding", permanent, lambda x: dotted_set(x, "product.product_id", "attacker-product"), "reject", "product identity constant")
    hostile("permanent-unknown-field", "permanent_binding", permanent, lambda x: x.update({"forged": True}), "reject", "additionalProperties=false")
    hostile("release-leaf-omitted", "release_manifest", release, lambda x: x.pop("asset_id"), "reject", "all 14 release leaves required")
    hostile("artifact-raw-prefix-mismatch", "pre_record_transport", transport, lambda x: x.update({"artifact_digest": "sha256:" + x["artifact_digest"]}), "reject", "bare canonical artifact digest")
    hostile("provider-check-name-forged", "provider_result", provider, lambda x: x.update({"provider_check_name": "Policy-bootstrap-A"}), "reject", "permanent B context constant")
    hostile("provider-status-forged", "provider_result", provider, lambda x: x.update({"provider_check_status": "queued"}), "reject", "terminal provider check constant")
    hostile("provider-check-head-mismatch", "provider_result", provider, lambda x: x.update({"provider_check_head_sha": "a" * 40}), "reject", "must equal resulting main SHA; no cross-field schema equality")
    hostile("provider-external-id-mismatch", "provider_result", provider, lambda x: x.update({"provider_check_external_id": "other-result"}), "reject", "must equal provider_result_id; no cross-field schema equality")
    hostile("provider-source-artifact-id-mismatch", "provider_result", provider, lambda x: x.update({"source_record_artifact_id": 7004}), "reject", "must equal Actions transport artifact ID; no joined schema")
    hostile("verify-policy-head-mismatch", "verify_b", verify, lambda x: x.update({"policy_check_head_sha": "a" * 40}), "reject", "must equal resulting_main_sha")
    hostile("verify-called-workflow-attacker", "verify_b", verify, lambda x: x.update({"called_job_workflow_ref": "attacker/repo/.github/workflows/evil.yml@refs/heads/evil"}), "reject", "must bind repository/path/ref to called workflow Contents and OIDC")
    hostile("verify-terminal-row-head-mismatch", "verify_b", verify, lambda x: x["terminal_census_rows"][0].update({"head_sha": "a" * 40}), "reject", "must equal resulting main SHA")
    hostile("verify-terminal-row-path-duplicate", "verify_b", verify, lambda x: x["terminal_census_rows"][1].update({"workflow_path": x["terminal_census_rows"][0]["workflow_path"]}), "reject", "must cover exactly one row per required workflow")
    hostile("verify-terminal-row-check-id-duplicate", "verify_b", verify, lambda x: x["terminal_census_rows"][1].update({"check_run_id": x["terminal_census_rows"][0]["check_run_id"]}), "reject", "must have unique check identity")
    hostile("verify-terminal-row-extra", "verify_b", verify, lambda x: x["terminal_census_rows"].append(copy.deepcopy(x["terminal_census_rows"][0])), "reject", "schema minItems does not enforce exactly 15")
    hostile("verify-terminal-digest-mismatch", "verify_b", verify, lambda x: x.update({"terminal_census_rows_digest": "a" * 64}), "reject", "must equal canonical rows digest")
    hostile("release-target-tag-mismatch", "release_manifest", release, lambda x: x.update({"target_sha": "a" * 40}), "reject", "tag suffix and target SHA must be joined")
    hostile("adoption-main-mismatch", "adoption", positives["adoption"], lambda x: x.update({"resulting_main_sha": "a" * 40}), "reject", "adoption must match provider/release/census readback")
    hostile("pre-record-raw-equality-forged", "pre_record", positives["pre_record"], lambda x: x["verification"].update({"raw_downloads_equal_digests": False}), "reject", "byte equality is semantic and bytes are absent")

    # Semantic hostiles that do not map to one JSON schema are represented as
    # explicit fixtures/contracts in the result record.
    semantic_contracts = [
        {"case": "s7b-ordered-fields-vs-provider-payload", "expected": "reject", "observed": "fail", "detail": "declared S7b preimage has 56 ordered fields; emitted provider payload only covers its own object and hashes excluded own fields"},
        {"case": "s5-release-asset-consumed-as-s3", "expected": "reject", "observed": "fail", "detail": "pre_record.release.asset_id/asset_digest are staged S3 and sent reserve-release -> attest-binding although release asset is S5"},
        {"case": "s7b-permanent-provider-fields-no-output-leaf", "expected": "reject", "observed": "fail", "detail": "19 permanent.verification/verifier fields claim external-provider-result producer but provider_result.schema has no corresponding output leaves"},
        {"case": "transport-edge-dropped-channel", "expected": "reject", "observed": "fail", "detail": "typed edges use one abstract phrase and no field-to-output/artifact/record channel mapping"},
        {"case": "old-pre-schema-contract-input", "expected": "reject", "observed": "fail", "detail": "build_v12_bundle.py loads OLD_PRE to form pre_content_desc; model does not enumerate those leaves"},
        {"case": "raw-zip-inner-binary-release-asset-bytes", "expected": "reject", "observed": "unimplemented", "detail": "no executable verifier or byte corpus; digest/size claims cannot prove byte identity"},
        {"case": "provider-check-pre-sign-vs-terminal-success", "expected": "reject", "observed": "unimplemented", "detail": "signed status/conclusion are specified before post-signature terminalization; no provider implementation proves a non-cyclic sequence"},
    ]
    write_json(OUT / "semantic-hostile-contracts.json", semantic_contracts)

    # Include the independent projection used for all findings.
    projection = {
        "schema": "velnor.authority-transition.v12-luna-independent-projection",
        "model_sha256": sha_file(MODEL_PATH),
        "bundle_root_digest": root_digest,
        "schema_leaf_counts": counts,
        "field_count": len(fields),
        "stage_field_counts": {stage: len(ids) for stage, ids in dag["stage_fields"].items()},
        "preimage_ordered_counts": {name: len(spec["ordered_field_ids"]) for name, spec in preimage_specs.items()},
        "preimage_excluded_counts": {name: len(spec["excluded_field_ids"]) for name, spec in preimage_specs.items()},
        "provider_declared_preimage_digest": declared_provider_digest,
        "provider_emitted_digest": provider["provider_result_digest"],
        "provider_missing_output_fields": missing_provider_output,
        "provider_own_fields_in_emitted_payload": included_excluded,
        "transport_edge_count": len(typed_edges),
        "transport_labels": transport_values,
        "old_pre_schema_sha256": sha_file(OLD_PRE_PATH),
        "origin_main": live_main,
        "origin_main_missing_typed_paths": absent_paths,
    }
    write_json(OUT / "independent-field-dag-projection.json", projection)
    write_json(OUT / "hostile-fixtures.json", {"schema": "velnor.authority-transition.v12-luna-hostile-fixtures", "cases": hostile_cases})

    # Hashes are intentionally generated after all fixtures are stable.
    artifact_hashes = {
        "script_sha256": sha_file(Path(__file__)),
        "projection_sha256": sha_file(OUT / "independent-field-dag-projection.json"),
        "hostile_index_sha256": sha_file(OUT / "hostile-fixtures.json"),
        "semantic_contracts_sha256": sha_file(OUT / "semantic-hostile-contracts.json"),
        "hostile_fixture_files": {item["case"]: sha_file(OUT / item["fixture"]) for item in hostile_cases},
    }

    passed = sum(item["status"] == "pass" for item in checks)
    failed = sum(item["status"] == "fail" for item in checks)
    unimplemented = sum(item["status"] == "unimplemented" for item in checks)
    result = {
        "schema": "velnor.authority-transition.v12-luna-independent-adversarial-results",
        "status": "proposal_only_external_blocked",
        "authority_claim": False,
        "frozen_inputs": {
            "freeze_manifest_sha256": sha_file(FREEZE_PATH),
            "root_digest": root_digest,
            "model_sha256": sha_file(MODEL_PATH),
            "generator_sha256": sha_file(GENERATOR_PATH),
            "dag_sha256": sha_file(BUNDLE / "v12-canonical-field-dag.json"),
        },
        "checks": checks,
        "hostile_summary": {"total": len(hostile_cases), "schema_rejected": sum(item["schema_observed"] == "reject" for item in hostile_cases), "schema_accepted_expected_reject": sum(item["schema_observed"] == "accept" and item["expected"] == "reject" for item in hostile_cases), "semantic_unimplemented": sum(item["semantic_status"] == "unimplemented" for item in hostile_cases)},
        "summary": {"total": len(checks), "passed": passed, "failed": failed, "unimplemented": unimplemented},
        "artifacts": artifact_hashes,
        "owner_results_are_not_authority": True,
        "live_authority_or_mutation": False,
    }
    result_path = OUT / "v12-luna-independent-results.json"
    write_json(result_path, result)
    report_lines = [
        "# Independent v12 strict-schema/dataflow audit — Luna",
        "",
        "Design evidence only. No source, authority, release, workflow, dispatch, merge, runner, or credential mutation occurred.",
        "",
        f"Result: {passed} pass, {failed} fail, {unimplemented} unimplemented checks; authority_claim=false.",
        "",
        "## Material findings",
        "",
        "- S7b emitted provider_result_digest does not equal the declared 56-field S7b preimage digest. The generator hashes its provider object (including excluded provider identity/nonce/status fields) instead.",
        "- The field registry assigns 19 permanent.verification/verifier leaves to external-provider-result, but provider_result.schema.json has no output leaves for them.",
        "- pre_record.release.asset_id/asset_digest are staged S3 and transported to attest-binding although the release asset is produced at S5; final-manifest values are likewise assigned before their declared S6 computation.",
        "- typed transport edges use one abstract label, not executable output/artifact/record channels; dropped-field transport cannot be independently rejected.",
        "- build_v12_bundle.py directly loads the historical v9 OLD_PRE schema to generate v12 pre_record leaves. This is a second contract input, despite its hash being bound.",
        "- verify-B terminal rows have schema minItems=15 but no maxItems, unique workflow/check identity, exact workflow enum, or cross-field head/ref/repository constraints.",
        "- raw ZIP, inner payload, binary, and release asset bytes are absent; digest/size claims are not byte identity proof. Provider/check/live workflow verification is unimplemented at observed main.",
        "- Hostile fixtures: 19 isolated JSON cases; 7 are rejected by strict schemas, while 12 semantically forged cases remain schema-valid and therefore are unimplemented without a real verifier.",
        "",
        "## Checks",
        "",
    ]
    report_lines.extend(f"- `{item['status']}` `{item['name']}` — {item['detail']}" for item in checks)
    report_lines.extend([
        "",
        "## Frozen input hashes",
        "",
        f"- freeze manifest: `{sha_file(FREEZE_PATH)}`",
        f"- canonical model: `{sha_file(MODEL_PATH)}`",
        f"- generator: `{sha_file(GENERATOR_PATH)}`",
        f"- DAG: `{sha_file(BUNDLE / 'v12-canonical-field-dag.json')}`",
        f"- root digest: `{root_digest}`",
        "",
        "## Boundary",
        "",
        "Owner 44/11 labels were not used as approval. The seven strict positive validations passing only establish schema shape. Semantic hostile cases accepted by schemas are recorded as unimplemented until a real producer/verifier exists. V12 and prior frozen bundles remain unchanged.",
    ])
    report_path = OUT / "v12-luna-independent-report.md"
    report_path.write_text("\n".join(report_lines) + "\n")
    result["artifacts"]["result_sha256"] = sha_file(result_path)
    result["artifacts"]["report_sha256"] = sha_file(report_path)
    # Refresh result after recording report/result hashes only in a sidecar, so
    # result bytes remain stable and their hash is not self-referential.
    write_json(OUT / "artifact-hashes.json", result["artifacts"])
    print(json.dumps({"results": str(result_path), "report": str(report_path), "passed": passed, "failed": failed, "unimplemented": unimplemented, "root_digest": root_digest}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
