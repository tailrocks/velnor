#!/usr/bin/env python3
"""Build the v11 proposal bundle from one canonical model source.

This writes only external evidence. The model source is the only contract input;
the historical v9 DAG is imported as vocabulary and is never used as authority.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
from pathlib import Path
from typing import Any

ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
OUT = ROOT / "v11-contract-bundle-2026-09-20"
MODEL_PATH = ROOT / "v11-canonical-model-source.json"
VOCAB_PATH = ROOT / "v9-independent-contract-audit-2026-09-20/canonical-dag/v9-independent-field-dag.json"
GENERATOR_PATH = ROOT / "build_v11_bundle.py"

OLD_PRE = ROOT / "authority-contract-separation-2026-09-20/v9/policy-validator-b-pre-record.v2.schema.json"


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def sha_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha_file(path: Path) -> str:
    return sha_bytes(path.read_bytes())


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n")


def normalized_fixture_sha(path: Path) -> str:
    value = load(path)
    if isinstance(value, dict):
        for key in ("canonical_root_manifest_sha256", "canonical_root_sha256"):
            if key in value:
                value[key] = "0" * 64
    return sha_bytes((json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode())


def load(path: Path) -> Any:
    return json.loads(path.read_text())


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


def old_type(meta: dict[str, Any]) -> str:
    value = meta.get("type", "string")
    if value in {"sha256", "hex64", "digest"}:
        return "sha256"
    if value in {"sha40", "sha1"}:
        return "sha40"
    if value in {"u64", "integer", "positiveInteger"}:
        return "u64"
    if value in {"bool", "boolean"}:
        return "bool"
    return value


def type_schema(type_name: str) -> dict[str, Any]:
    return {
        "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
        "sha40": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
        "u64": {"type": "integer", "minimum": 1},
        "bool": {"type": "boolean"},
        "array": {"type": "array", "minItems": 1},
        "object": {"type": "object", "minProperties": 1},
        "string": {"type": "string"},
    }.get(type_name, {"type": "string"})


def set_nested(root: dict[str, Any], dotted: str, schema: dict[str, Any]) -> None:
    bits = dotted.split(".")
    cursor = root
    for bit in bits[:-1]:
        child = cursor.setdefault("properties", {}).setdefault(
            bit, {"type": "object", "additionalProperties": False, "properties": {}, "required": []}
        )
        cursor = child
    cursor.setdefault("properties", {})[bits[-1]] = schema


def finalize_nested(node: dict[str, Any]) -> None:
    if node.get("type") == "object" or "properties" in node:
        node.setdefault("type", "object")
        node.setdefault("additionalProperties", False)
        props = node.setdefault("properties", {})
        node["required"] = sorted(props)
        for child in props.values():
            finalize_nested(child)


def generated_schema(schema_id: str, title: str, fields: dict[str, dict[str, Any]], overrides: dict[str, dict[str, Any]] | None = None) -> dict[str, Any]:
    result: dict[str, Any] = {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": f"https://velnor.dev/schemas/{schema_id}",
        "title": title,
        "type": "object",
        "additionalProperties": False,
        "properties": {},
    }
    overrides = overrides or {}
    for path, desc in sorted(fields.items()):
        set_nested(result, path, copy.deepcopy(overrides.get(path, desc)))
    finalize_nested(result)
    return result


def field_type_from_schema(desc: dict[str, Any]) -> str:
    typ = desc.get("type")
    if "const" in desc:
        value = desc["const"]
        return "bool" if isinstance(value, bool) else "u64" if isinstance(value, int) else "string"
    if typ == "integer":
        return "u64"
    if typ == "boolean":
        return "bool"
    if typ == "array":
        return "array"
    if typ == "object":
        return "object"
    pattern = desc.get("pattern", "")
    if "{40}" in pattern:
        return "sha40"
    if "{64}" in pattern:
        return "sha256"
    return "string"


def value_for(field: dict[str, Any], index: int) -> Any:
    desc = field.get("schema", field)
    if "const" in desc:
        return desc["const"]
    typ = desc.get("type")
    pattern = desc.get("pattern", "")
    if typ == "integer":
        return index + 1001
    if typ == "boolean":
        return True
    if typ == "object":
        return {"fixture_field": index + 1}
    if typ == "array":
        return [{"fixture_row": index + 1}]
    if "{40}" in pattern:
        return hashlib.sha1(f"v11-fixture:{index}".encode()).hexdigest()
    if "{64}" in pattern:
        return hashlib.sha256(f"v11-fixture:{index}".encode()).hexdigest()
    if desc.get("format") == "uri":
        return f"https://provider.invalid/fixture/{index}"
    return f"fixture-{index:04d}"


def unflatten(fields: dict[str, dict[str, Any]], prefix: str = "") -> dict[str, Any]:
    result: dict[str, Any] = {}
    for index, (path, desc) in enumerate(sorted(fields.items()), 1):
        set_value(result, path, value_for(desc, index))
    return result


def set_value(root: dict[str, Any], dotted: str, value: Any) -> None:
    parts = dotted.split(".")
    cursor = root
    for part in parts[:-1]:
        cursor = cursor.setdefault(part, {})
    cursor[parts[-1]] = value


def validate_positive_instances() -> dict[str, list[str]]:
    """Fail generation if any emitted positive violates its emitted schema."""
    try:
        from jsonschema import Draft202012Validator, FormatChecker
    except ModuleNotFoundError as error:
        raise RuntimeError("jsonschema is required: run the generator with jsonschema Draft202012Validator available") from error
    pairs = {
        "positive-release-manifest.json": "release_manifest",
        "positive-pre-record.json": "pre_record",
        "positive-pre-record-transport.json": "pre_record_transport",
        "positive-provider-result.json": "provider_result",
        "positive-verify-b.json": "verify_b",
        "positive-adoption.json": "adoption",
    }
    errors: dict[str, list[str]] = {}
    for fixture_name, schema_name in pairs.items():
        schema = load(OUT / f"{schema_name}.schema.json")
        instance = load(OUT / fixture_name)
        failures = sorted(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance), key=lambda item: list(item.absolute_path))
        if failures:
            errors[fixture_name] = [f"{'.'.join(str(part) for part in failure.absolute_path)}: {failure.message}" for failure in failures]
    if errors:
        raise AssertionError(json.dumps({"strict_schema_failures": errors}, sort_keys=True))
    return errors


def stage_key(stage: str) -> str:
    mapping = {"S0": "S0_caller_readback", "S1": "S1_build", "S2": "S2_artifact_verify", "S3": "S3_release_reserve", "S4": "S4_binding_attest", "S5": "S5_publish", "S6": "S6_release_attest", "S7a": "S7a_pre_record_transport", "S7b": "S7c_verify_b"}
    return mapping.get(stage, stage)


def path_stage(path: str) -> tuple[str, str]:
    top = path.split(".", 1)[0]
    if top in {"source", "caller_workflow", "product", "actions", "publisher", "trust"}:
        return "S0_caller_readback", "capture-caller-context.capture"
    if top in {"artifact", "binary", "upload"}:
        return "S2_artifact_verify", "artifact-verify"
    if top == "release":
        if any(x in path for x in ("published", "tag_immutable")):
            return "S5_publish", "publish"
        if any(x in path for x in ("manifest_signed", "attestation_signed")):
            return "S4_binding_attest", "attest-binding"
        return "S3_release_reserve", "reserve-release"
    if top == "attestation":
        if "binding_" in path:
            return "S4_binding_attest", "attest-binding"
        return "S6_release_attest", "attest-release"
    if top == "verification":
        return "S7c_verify_b", "verify-B"
    if top in {"run", "job", "steps"}:
        return "S2_artifact_verify", "artifact-verify"
    return "S7c_verify_b", "verify-B"


def main() -> None:
    model = load(MODEL_PATH)
    vocabulary = load(VOCAB_PATH)
    assert sha_file(MODEL_PATH) == "ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930", "model source changed; update expected input deliberately"
    assert sha_file(VOCAB_PATH) == model["source_inputs"]["historical_field_vocabulary"]["sha256"], "historical input hash changed"
    OUT.mkdir(parents=True, exist_ok=True)

    old_pre_desc = flatten_schema(load(OLD_PRE))
    permanent = vocabulary["permanent_leaf_lineage_reviewed"]

    pre_transport_names = {
        "artifact.actions_artifact_id", "artifact.digest", "artifact.name", "artifact.upload_step_id",
        "binding.record_artifact_digest", "binding.record_artifact_id",
    }
    pre_content_desc = {path: desc for path, desc in old_pre_desc.items() if path not in pre_transport_names and not path.startswith("binding.record_upload_")}
    pre_transport_desc = {path: desc for path, desc in model["record_schema_model"]["pre_record_transport"].items()}
    provider_desc = model["record_schema_model"]["provider_result"]
    verify_desc = model["record_schema_model"]["verify_b"]
    release_desc = model["release_leaf_model"]["fields"]
    adoption_desc = model["record_schema_model"]["adoption"]

    identity_overrides = {
        "product.product_id": {"type": "string", "const": model["identity"]["product_id"]},
        "product.schema": {"type": "string", "const": model["identity"]["schema"]},
        "product.platform": {"type": "string", "const": model["identity"]["platform"]},
        "product.runner": {"type": "string", "const": model["identity"]["runner"]},
        "product.asset": {"type": "string", "const": model["identity"]["asset"]},
        "product.features": {"type": "string", "const": model["identity"]["features"]},
        "product.mutable_latest": {"type": "boolean", "const": False},
        "product.overwrite": {"type": "boolean", "const": False},
        "product.application_namespace_reuse": {"type": "boolean", "const": False},
    }
    required_step_conclusions = {
        key: {"type": "string", "const": "success"}
        for key in model["record_schema_model"]["pre_record"]["required_step_conclusion_keys"]
    }
    pre_overrides = {
        "release.tag": copy.deepcopy(release_desc["release_tag"]),
        "verification.required_step_conclusions": {
            "type": "object",
            "additionalProperties": False,
            "properties": required_step_conclusions,
            "required": sorted(required_step_conclusions),
        },
    }
    permanent_desc = {path: {"type": "string"} for path in permanent}
    for path, meta in permanent.items():
        permanent_desc[path] = type_schema(old_type(meta))

    schemas = {
        "permanent_binding": generated_schema("v11-permanent-binding.v1.schema.json", "Velnor canonical permanent binding", permanent_desc, identity_overrides),
        "pre_record": generated_schema("v11-pre-record.v3.schema.json", "Velnor pre-provider content record", pre_content_desc, pre_overrides),
        "pre_record_transport": generated_schema("v11-pre-record-transport.v1.schema.json", "Velnor pre-record transport", pre_transport_desc),
        "provider_result": generated_schema("v11-provider-result.v2.schema.json", "Velnor external provider result", provider_desc),
        "verify_b": generated_schema("v11-verify-b.v1.schema.json", "Velnor verify-B composite result", verify_desc),
        "release_manifest": generated_schema("v11-release-manifest.v1.schema.json", "Velnor immutable release manifest", release_desc),
        "adoption": generated_schema("v11-validator-pin-adoption.v2.schema.json", "Velnor permanent validator adoption", adoption_desc),
    }
    for name, schema in schemas.items():
        write_json(OUT / f"{name}.schema.json", schema)

    fields: dict[str, dict[str, Any]] = {}
    for path, meta in sorted(permanent.items()):
        stage = stage_key(meta["stage"])
        fields[f"permanent.{path}"] = {"path": path, "schema": "permanent_binding", "stage": stage, "producer": stage_key(meta["stage"]) and model["stage_contract"][stage]["producer"], "type": old_type(meta), "required": True, "source": meta["source"]}
    for path, desc in sorted(pre_content_desc.items()):
        stage, producer = path_stage(path)
        fields[f"pre_record.{path}"] = {"path": path, "schema": "pre_record", "stage": stage, "producer": producer, "type": field_type_from_schema(desc), "required": True, "source": "pre-record canonical content assembled from typed upstream outputs"}
    for path, desc in sorted(pre_transport_desc.items()):
        fields[f"pre_record_transport.{path}"] = {"path": path, "schema": "pre_record_transport", "stage": "S7a_pre_record_transport", "producer": "pre-record-upload", "type": field_type_from_schema(desc), "required": True, "source": "Actions artifact REST response; excluded from its own content preimages"}
    for path, desc in sorted(provider_desc.items()):
        fields[f"provider_result.{path}"] = {"path": path, "schema": "provider_result", "stage": "S7b_provider_result", "producer": "external-provider-result", "type": field_type_from_schema(desc), "required": True, "source": "provider-owned verifier/API/signature envelope"}
    for path, desc in sorted(verify_desc.items()):
        producer = "verify-B.terminal-census-api" if path == "terminal_census_rows" else "verify-B"
        fields[f"verify_b.{path}"] = {"path": path, "schema": "verify_b", "stage": "S7c_verify_b", "producer": producer, "type": field_type_from_schema(desc), "required": True, "source": "provider-result readback, called workflow API/OIDC readback, and upstream terminal census"}
    for path, desc in sorted(release_desc.items()):
        if path in {"release_id", "release_tag", "target_sha"}:
            stage, producer = "S3_release_reserve", "reserve-release"
        elif path in {"asset_id", "asset_digest"}:
            stage, producer = "S5_publish", "publish"
        else:
            stage, producer = "S4_binding_attest", "attest-binding"
        fields[f"release_manifest.{path}"] = {"path": path, "schema": "release_manifest", "stage": stage, "producer": producer, "type": field_type_from_schema(desc), "required": True, "source": "canonical immutable release manifest"}
    for path, desc in sorted(adoption_desc.items()):
        fields[f"adoption.{path}"] = {"path": path, "schema": "adoption", "stage": "TreeB_adoption", "producer": "tree-b-adoption-verifier", "type": field_type_from_schema(desc), "required": True, "source": "reviewed Tree-B adoption PR and live Main-B proof"}
    rank = {name: i for i, name in enumerate(model["stage_order"])}
    for desc in fields.values():
        stage_rank = rank.get(desc["stage"], 99)
        desc["consumers"] = sorted({job for stage, stage_desc in model["stage_contract"].items() if rank.get(stage, 99) > stage_rank for job in [stage_desc["producer"]]})

    stage_fields = {stage: sorted(field_id for field_id, desc in fields.items() if desc["stage"] == stage) for stage in model["stage_order"]}
    lineage_edges = []
    for left, right in zip(model["stage_order"], model["stage_order"][1:]):
        producer = model["stage_contract"][left]["producer"]
        consumer = model["stage_contract"][right]["producer"]
        produced = stage_fields[left]
        if produced:
            lineage_edges.append({"producer": producer, "consumer": consumer, "orientation": "producer_outputs_to_consumer_inputs", "field_ids": produced})
    lineage_edges.append({"producer": "tree-b-adoption-verifier", "consumer": ".github/ci/validator-pin-adoption.json", "orientation": "producer_outputs_to_persistent_adoption_input", "field_ids": stage_fields["TreeB_adoption"]})
    for edge in model["workflow_graph"]["workflow_call_output_edges"]:
        lineage_edges.append({"producer": edge["producer"], "consumer": edge["consumer"], "orientation": "explicit_workflow_call_output_transport", "field_ids": edge["field_ids"], "transport": edge["transport"]})

    preimage_specs = {}
    for name, spec in model["preimage_model"]["digest_ids"].items():
        if name in {"S4_binding", "S6_release"}:
            candidates = [field_id for field_id, desc in fields.items() if desc["stage"] in spec["input_stages"]]
            suffixes = tuple(spec.get("exclude_field_suffixes", []))
            allowed = [field_id for field_id in candidates if not field_id.endswith(suffixes)]
        elif name == "S7b_provider_result":
            allowed = stage_fields["S7a_pre_record_transport"] + [field_id for field_id in stage_fields["S7b_provider_result"] if not field_id.endswith(".provider_result_id") and not field_id.endswith(".provider_result_digest")]
        else:
            allowed = ["verify_b.terminal_census_rows"]
        excluded_ids = sorted(set(fields) - set(allowed))
        preimage_specs[name] = {"ordered_field_ids": allowed, "excluded_field_ids": excluded_ids, "excluded_fields": spec.get("exclude_fields", []), "excluded_future_stages": spec.get("exclude_future_stages", []), "subject": spec["subject"], "digest_source": "sha256(canonical UTF-8 LF JSON bytes of ordered fields only)"}

    schema_leaf_paths = {name: sorted(flatten_schema(schema)) for name, schema in schemas.items()}
    release_paths = schema_leaf_paths["release_manifest"]
    assert len(release_paths) == 14, release_paths
    assert set(release_paths) == set(model["release_leaf_model"]["fields"]), release_paths

    canonical_dag = {
        "schema": "velnor.authority-transition.canonical-field-dag.v11",
        "status": "proposal_only_external_blocked",
        "authority_claim": False,
        "canonical_model_source": {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH)},
        "historical_vocabulary_input": {"path": str(VOCAB_PATH.relative_to(ROOT)), "sha256": sha_file(VOCAB_PATH), "authority": "none"},
        "identity": model["identity"],
        "observed_revision": model["observed_revision"],
        "fields": fields,
        "schema_leaf_paths": schema_leaf_paths,
        "field_lineage_edges": lineage_edges,
        "needs_edges_consumer_producer": model["workflow_graph"]["needs_edges_consumer_producer"],
        "workflow_call_output_edges": model["workflow_graph"]["workflow_call_output_edges"],
        "caller_contract": model["caller_contract"],
        "authority_graph": model["authority_graph"],
        "artifact_contract": model["artifact_contract"],
        "release_attestation_contract": model["release_attestation_contract"],
        "stage_fields": stage_fields,
        "preimage_contract": preimage_specs,
        "release_positive_fixture_required": True,
        "release_positive_fixture_leaf_count": len(release_paths),
        "metrics": {"field_count": len(fields), "permanent_leaf_count": len(schema_leaf_paths["permanent_binding"]), "pre_record_leaf_count": len(schema_leaf_paths["pre_record"]), "provider_leaf_count": len(schema_leaf_paths["provider_result"]), "verify_b_leaf_count": len(schema_leaf_paths["verify_b"]), "release_manifest_leaf_count": len(release_paths), "unique_producers": len({d["producer"] for d in fields.values()})},
        "no_parallel_contract_lists": True,
    }
    dag_path = OUT / "v11-canonical-field-dag.json"
    write_json(dag_path, canonical_dag)

    # All positive instances use one joined synthetic lifecycle. Metadata is in a
    # sidecar because every schema has additionalProperties=false.
    synthetic_main_sha = "b" * 40
    synthetic_tree_sha = "c" * 40
    synthetic_source_sha = "d" * 40
    synthetic_root_ref = "0" * 64
    synthetic_artifact_digest = "a" * 64
    synthetic_artifact_raw_digest = "sha256:" + synthetic_artifact_digest
    synthetic_raw_zip_digest = "e" * 64
    synthetic_provider_digest = "f" * 64
    synthetic_terminal_digest = "1" * 64
    synthetic_manifest_digest = "2" * 64
    synthetic_tag = model["identity"]["release_tag_prefix"] + synthetic_main_sha
    release_schema_sha = sha_file(OUT / "release_manifest.schema.json")
    lifecycle = {
        "main_sha": synthetic_main_sha,
        "tree_sha": synthetic_tree_sha,
        "source_sha": synthetic_source_sha,
        "root_ref": synthetic_root_ref,
        "release_schema_sha": release_schema_sha,
        "artifact_id": 7003,
        "artifact_name": "policy-validator-b-pre-record",
        "artifact_raw_digest": synthetic_artifact_raw_digest,
        "artifact_digest": synthetic_artifact_digest,
        "raw_zip_sha256": synthetic_raw_zip_digest,
        "provider_result_id": "provider-result-v11-fixture",
        "provider_result_digest": synthetic_provider_digest,
        "terminal_census_id": "terminal-census-v11-fixture",
        "terminal_census_digest": synthetic_terminal_digest,
        "manifest_digest": synthetic_manifest_digest,
        "release_id": 7101,
        "asset_id": 7102,
        "release_attestation_id": 7501,
        "release_attestation_digest": "4" * 64,
    }
    release_fixture = unflatten(release_desc)
    set_value(release_fixture, "schema", model["release_leaf_model"]["schema"])
    set_value(release_fixture, "release_id", lifecycle["release_id"])
    set_value(release_fixture, "release_tag", synthetic_tag)
    set_value(release_fixture, "target_sha", synthetic_main_sha)
    set_value(release_fixture, "asset_id", lifecycle["asset_id"])
    set_value(release_fixture, "asset_digest", "sha256:" + synthetic_artifact_digest)
    set_value(release_fixture, "manifest_digest", lifecycle["manifest_digest"])
    set_value(release_fixture, "source_sha", lifecycle["source_sha"])
    set_value(release_fixture, "source_tree_sha", lifecycle["tree_sha"])
    set_value(release_fixture, "closure_digest", "3" * 64)
    pre_fixture = unflatten(pre_content_desc)
    set_value(pre_fixture, "canonical_root_manifest_sha256", lifecycle["root_ref"])
    set_value(pre_fixture, "canonical_release_schema_sha256", lifecycle["release_schema_sha"])
    set_value(pre_fixture, "source.sha", lifecycle["source_sha"])
    set_value(pre_fixture, "source.tree_sha", lifecycle["tree_sha"])
    set_value(pre_fixture, "release.release_id", lifecycle["release_id"])
    set_value(pre_fixture, "release.tag", synthetic_tag)
    set_value(pre_fixture, "release.target_sha", lifecycle["main_sha"])
    set_value(pre_fixture, "release.asset_id", lifecycle["asset_id"])
    set_value(pre_fixture, "release.asset_digest", "sha256:" + synthetic_artifact_digest)
    set_value(pre_fixture, "release.manifest_digest", lifecycle["manifest_digest"])
    set_value(pre_fixture, "artifact.service_zip_digest", synthetic_artifact_digest)
    set_value(pre_fixture, "artifact.inner_payload_digest", synthetic_artifact_digest)
    set_value(pre_fixture, "artifact.binary_digest", synthetic_artifact_digest)
    set_value(pre_fixture, "binding.final_manifest_artifact_digest", "sha256:" + synthetic_artifact_digest)
    set_value(pre_fixture, "binding.subject_digest", "sha256:" + synthetic_artifact_digest)
    set_value(pre_fixture, "release_attestation.attestation_id", lifecycle["release_attestation_id"])
    set_value(pre_fixture, "release_attestation.attestation_digest", lifecycle["release_attestation_digest"])
    set_value(pre_fixture, "release_attestation.manifest_subject_sha256", lifecycle["manifest_digest"])
    set_value(pre_fixture, "release_attestation.predicate_type", "https://velnor.dev/attestations/velnor-policy-validator-release/v1")
    set_value(pre_fixture, "release_attestation.predicate_path", "attestations/velnor-policy-validator-release.v1.json")
    set_value(pre_fixture, "release_attestation.certificate_verified", True)
    set_value(pre_fixture, "release_attestation.oidc_issuer", "https://token.actions.githubusercontent.com")
    set_value(pre_fixture, "verification.required_step_conclusions", {key: "success" for key in model["record_schema_model"]["pre_record"]["required_step_conclusion_keys"]})
    transport_fixture = unflatten(pre_transport_desc)
    set_value(transport_fixture, "artifact_id", lifecycle["artifact_id"])
    set_value(transport_fixture, "artifact_name", lifecycle["artifact_name"])
    set_value(transport_fixture, "artifact_raw_digest", lifecycle["artifact_raw_digest"])
    set_value(transport_fixture, "artifact_digest", lifecycle["artifact_digest"])
    set_value(transport_fixture, "raw_zip_sha256", lifecycle["raw_zip_sha256"])
    set_value(transport_fixture, "upload_run_id", 7201)
    set_value(transport_fixture, "upload_run_attempt", 1)
    set_value(transport_fixture, "upload_job_id", 7202)
    set_value(transport_fixture, "upload_check_run_id", 7203)
    provider_fixture = unflatten(provider_desc)
    for key, value in (("canonical_root_manifest_sha256", lifecycle["root_ref"]), ("canonical_release_schema_sha256", lifecycle["release_schema_sha"]), ("provider_result_id", lifecycle["provider_result_id"]), ("provider_result_digest", lifecycle["provider_result_digest"]), ("source_record_artifact_id", lifecycle["artifact_id"]), ("source_record_artifact_raw_digest", lifecycle["artifact_raw_digest"]), ("source_record_artifact_digest", lifecycle["artifact_digest"]), ("source_record_raw_zip_sha256", lifecycle["raw_zip_sha256"]), ("provider_check_head_sha", lifecycle["main_sha"]), ("resulting_main_sha", lifecycle["main_sha"]), ("resulting_main_tree_sha", lifecycle["tree_sha"])):
        set_value(provider_fixture, key, value)
    set_value(provider_fixture, "provider_check_run_id", 7301)
    set_value(provider_fixture, "provider_check_app_id", 7303)
    set_value(provider_fixture, "provider_verifier_revision", "4" * 40)
    set_value(provider_fixture, "provider_workflow_ref", "refs/heads/main")
    set_value(provider_fixture, "provider_workflow_sha", "5" * 40)
    set_value(provider_fixture, "signer_key_id", "fixture-key-v11")
    set_value(provider_fixture, "signer_algorithm", "ed25519")
    set_value(provider_fixture, "signer_public_key_sha256", "6" * 64)
    set_value(provider_fixture, "signer_certificate_subject", "fixture-provider-v11")
    set_value(provider_fixture, "signer_certificate_sha256", "7" * 64)
    set_value(provider_fixture, "signature_base64", "ZmFrZS12MTEtc2lnbmF0dXJl")
    set_value(provider_fixture, "provider_endpoint", "https://provider.invalid/v11/results")
    set_value(provider_fixture, "provider_attestation_digest", "8" * 64)
    verify_fixture = unflatten(verify_desc)
    for key, value in (("canonical_root_manifest_sha256", lifecycle["root_ref"]), ("canonical_release_schema_sha256", lifecycle["release_schema_sha"]), ("provider_result_id", lifecycle["provider_result_id"]), ("provider_result_digest", lifecycle["provider_result_digest"]), ("resulting_main_sha", lifecycle["main_sha"]), ("called_head_sha", lifecycle["main_sha"]), ("policy_check_head_sha", lifecycle["main_sha"]), ("terminal_census_digest", lifecycle["terminal_census_digest"]), ("terminal_census_rows_digest", "9" * 64)):
        set_value(verify_fixture, key, value)
    set_value(verify_fixture, "called_workflow_ref", "refs/heads/main")
    set_value(verify_fixture, "called_workflow_sha", "5" * 40)
    set_value(verify_fixture, "called_job_workflow_ref", "tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml@refs/heads/main")
    set_value(verify_fixture, "called_job_workflow_sha", "5" * 40)
    set_value(verify_fixture, "called_workflow_file_blob_sha", "6" * 40)
    set_value(verify_fixture, "called_run_id", 7401)
    set_value(verify_fixture, "called_run_attempt", 1)
    set_value(verify_fixture, "terminal_census_id", lifecycle["terminal_census_id"])
    set_value(verify_fixture, "policy_check_run_id", 7402)
    set_value(verify_fixture, "policy_check_app_id", 7303)
    set_value(verify_fixture, "terminal_census_rows", [{"workflow_path": ".github/workflows/ci-main.yml", "workflow_sha": "5" * 40, "run_id": 7401, "attempt": 1, "job_id": 7403, "check_run_id": 7404, "head_sha": lifecycle["main_sha"], "status": "completed", "conclusion": "success"}])
    adoption_fixture = unflatten(adoption_desc)
    adoption_values = {"mode": "permanent_pin", "pr_number": 969, "pr_head_sha": model["observed_revision"]["candidate_pr"]["head_sha"], "pr_base_sha": model["observed_revision"]["candidate_pr"]["base_sha"], "resulting_main_sha": lifecycle["main_sha"], "resulting_main_tree_sha": lifecycle["tree_sha"], "provider_result_digest": lifecycle["provider_result_digest"], "terminal_census_digest": lifecycle["terminal_census_digest"], "release_manifest_digest": lifecycle["manifest_digest"], "base_policy_workflow_sha": "5" * 40, "base_policy_blob_sha": "6" * 40, "source_template_map_digest": "a" * 64, "schema_sha256": sha_file(OUT / "adoption.schema.json"), "canonical_root_sha256": lifecycle["root_ref"], "temporary_authority_removed": True, "permanent_trust_root": "fixture-trust-root-v11", "validated_utc": "2026-09-20T03:58:55Z"}
    adoption_fixture.update(adoption_values)
    write_json(OUT / "positive-release-manifest.json", release_fixture)
    write_json(OUT / "positive-pre-record.json", pre_fixture)
    write_json(OUT / "positive-pre-record-transport.json", transport_fixture)
    write_json(OUT / "positive-provider-result.json", provider_fixture)
    write_json(OUT / "positive-verify-b.json", verify_fixture)
    write_json(OUT / "positive-adoption.json", adoption_fixture)
    validate_positive_instances()
    write_json(OUT / "positive-fixture-status.json", {"schema": "velnor.authority-transition.fixture-status.v11", "live_binding": False, "live_proof_status": "not_executed", "synthetic_values_are_not_evidence": True, "root_reference_normalization": {"field": "canonical_root_manifest_sha256/canonical_root_sha256", "zero_value": synthetic_root_ref, "mode": "root_digest_injected_after_manifest_hash"}, "strict_schema_instances": {"positive-release-manifest.json": "release_manifest", "positive-pre-record.json": "pre_record", "positive-pre-record-transport.json": "pre_record_transport", "positive-provider-result.json": "provider_result", "positive-verify-b.json": "verify_b", "positive-adoption.json": "adoption"}})

    typed_values = {}
    for index, (field_id, desc) in enumerate(sorted(fields.items()), 1):
        typed_values[field_id] = {"value": value_for(type_schema(desc["type"]), index), "producer": desc["producer"], "stage": desc["stage"], "live": False, "provenance": "deterministic_contract_fixture_only"}
    typed_fixture = {"fixture_metadata": model["typed_output_fixture"], "canonical_model_sha256": sha_file(MODEL_PATH), "canonical_dag_sha256": sha_file(dag_path), "field_count": len(typed_values), "typed_outputs": typed_values}
    write_json(OUT / "positive-full-typed-output-fixture.json", typed_fixture)

    negative_cases = [{"case": case, "expected": "reject"} for case in model["required_negative_cases"]]
    write_json(OUT / "negative-fixtures.json", {"schema": "velnor.authority-transition.negative-fixtures.v11", "all_expected": "reject", "cases": negative_cases})

    bound = []
    for path in sorted(OUT.iterdir()):
        if path.name in {"canonical-root-manifest.v4.json", "bundle-index.json", "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json", "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.md", "v11-independent-audit-results.json", "v11-independent-audit-report.md"}:
            continue
        if path.is_file():
            entry = {"path": str(path.relative_to(ROOT)), "sha256": sha_file(path), "role": "generated from canonical v11 model"}
            if path.name.startswith("positive-") and path.suffix == ".json" and path.name != "positive-fixture-status.json":
                entry["sha256"] = normalized_fixture_sha(path)
                entry["normalization"] = "replace canonical_root_manifest_sha256/canonical_root_sha256 with 64 zeroes before hashing"
            bound.append(entry)
    bound.extend([
        {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH), "role": "sole contract model source"},
        {"path": str(VOCAB_PATH.relative_to(ROOT)), "sha256": sha_file(VOCAB_PATH), "role": "historical vocabulary input only"},
        {"path": str(GENERATOR_PATH.relative_to(ROOT)), "sha256": sha_file(GENERATOR_PATH), "role": "reproducible projection generator"},
    ])
    root_manifest = {
        "schema": "velnor.canonical-evidence-root-manifest.v4",
        "root_digest_rule": "SHA-256 of canonical UTF-8 LF JSON bytes with lexicographically sorted keys and compact separators; this object has no self-digest field",
        "canonical_root": "dual-lane-evidence/G1/bootstrap-transition",
        "bound_path_base": "dual-lane-evidence/G1/bootstrap-transition",
        "model_source": {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH)},
        "current_main": model["observed_revision"]["main"],
        "candidate_pr_head_is_not_resulting_main": True,
        "product_identity": model["identity"],
        "bound_files": sorted(bound, key=lambda item: item["path"]),
        "historical_exclusions": [{"path": "G1/bootstrap-transition/v9-independent-contract-audit-2026-09-20/canonical-dag/v9-independent-field-dag.json", "role": "v9 authority input; only vocabulary is imported by v11"}],
        "live_status": "no live typed verifier or provider execution; synthetic fixtures are explicitly non-live",
    }
    root_path = OUT / "canonical-root-manifest.v4.json"
    write_json(root_path, root_manifest)
    root_digest = sha_bytes(canonical(root_manifest))

    # Inject the resulting root digest into fixture fields only after the root
    # preimage is fixed. Root-bound fixture hashes use the zeroed normalization
    # above, so this is acyclic and reproducible.
    for fixture_name in ["positive-pre-record.json", "positive-provider-result.json", "positive-verify-b.json", "positive-adoption.json"]:
        fixture_path = OUT / fixture_name
        fixture = load(fixture_path)
        if "canonical_root_manifest_sha256" in fixture:
            fixture["canonical_root_manifest_sha256"] = root_digest
        if "canonical_root_sha256" in fixture:
            fixture["canonical_root_sha256"] = root_digest
        write_json(fixture_path, fixture)

    schema_hashes = {name: sha_file(OUT / f"{name}.schema.json") for name in schemas}
    fixture_hashes = {path.name: sha_file(path) for path in sorted(OUT.glob("positive-*.json"))}
    plan = {
        "schema": "velnor.authority-change-plan.v11",
        "status": "successor_draft_external_blocked",
        "execution_authorized": False,
        "mutation_performed": False,
        "observed_utc": model["observed_revision"]["main"]["observed_utc"],
        "supersedes": {"path": "G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json", "sha256": "8e5ff3e61a1d25590c88a161f9e38db260b9fa1afeccfe1c956b1194b5b66050", "preserved": True},
        "canonical_source": {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH), "single_source": True},
        "canonical_outputs": {"dag_path": str(dag_path.relative_to(ROOT)), "dag_sha256": sha_file(dag_path), "root_manifest_path": str(root_path.relative_to(ROOT)), "root_manifest_raw_sha256": sha_file(root_path), "root_digest": root_digest, "schema_hashes": schema_hashes, "fixture_hashes": fixture_hashes},
        "revision_bound_facts": model["observed_revision"],
        "transition": {"tree_a": "external Policy-bootstrap-A only; no permanent pin", "main_b": "protected resulting-main push invokes workflow_call-only publisher and exact provider verifier", "tree_b": "separate reviewed adoption PR after live Main-B proof", "order": ["read-only census", "approved Tree-A authority", "normal protected merge", "read refs/heads/main and run APIs", "publish and verify Main-B", "post-provider census", "reviewed Tree-B adoption", "remove temporary authorities"], "candidate_vs_result": "candidate_pr.head_sha is input evidence only; resulting_main.sha is null until protected merge and readback"},
        "canonical_contract": {"identity": model["identity"], "field_count": len(fields), "permanent_leaf_count": len(schema_leaf_paths["permanent_binding"]), "release_leaf_count": len(release_paths), "release_positive_fixture": str((OUT / "positive-release-manifest.json").relative_to(ROOT)), "provider_result_excludes_terminal_census": True, "terminal_census_digest_excludes_own_id_and_digest": True, "no_parallel_contract_lists": True, "caller_contract": model["caller_contract"], "authority_graph": model["authority_graph"], "artifact_contract": model["artifact_contract"], "release_attestation_contract": model["release_attestation_contract"]},
        "provider_and_readback": {"contract": model["provider_contract"], "caller_readback": model["caller_readback"], "status": "identities and live verifier unresolved; no success claim"},
        "freeze": model["freeze_contract"],
        "external_blockers": model["external_blockers"],
        "independent_review": {"required": ["authority_transition_review", "canonical_dag_independent_audit"], "status": "not_requested_until_v11_strict_schema_and_negative_reproduction_passes"},
    }
    plan_json_path = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json"
    write_json(plan_json_path, plan)
    plan_md = f"""# Velnor authority-transition plan v11 — proposal only

Status: `{plan['status']}`. V9 remains immutable and preserved. This bundle is a design correction, not authorization: no source, ruleset, App, release, merge, dispatch, runner, or credential mutation occurred.

## Canonical source and revision binding

One model source generates the schemas, typed field registry, producer/consumer graph, preimage sets, fixtures, plan JSON, and this Markdown. It is `{plan['canonical_source']['path']}` with SHA-256 `{plan['canonical_source']['sha256']}`. The generated DAG is `{plan['canonical_outputs']['dag_path']}` with SHA-256 `{plan['canonical_outputs']['dag_sha256']}`. The root manifest has no self-digest; its canonical digest is `{root_digest}`.

Current observed `main` is `{model['observed_revision']['main']['sha']}` (parent `{model['observed_revision']['main']['parent_sha']}`, tree `{model['observed_revision']['main']['tree_sha']}`). PR 969 head `{model['observed_revision']['candidate_pr']['head_sha']}` is candidate evidence only. Resulting-main SHA/tree are deliberately null until the protected merge response, `refs/heads/main`, run API, and workflow source readback all agree. No PR-head-as-main substitution is permitted.

## Corrected contract

Product identity is exactly `{model['identity']['product_id']}` / `{model['identity']['schema']}` / `{model['identity']['asset']}` / `{model['identity']['platform']}` / `{model['identity']['runner']}`. The generated canonical DAG contains {len(schema_leaf_paths['permanent_binding'])} permanent leaves and exactly {len(release_paths)} release-manifest leaves. The release positive fixture is bound and exercises all 14 required fields.

S7 is acyclic. The provider result is emitted by `external-provider-result` and contains no terminal census. `verify-B` reads the signed result, reads the called-workflow identity, then computes a separate terminal census over typed upstream run/job/check rows. Provider-result ID/digest and census ID/digest are excluded from their own preimages; no own or future field is ordered into a digest. Transport artifact IDs are separate fields and never self-hashed.

The provider contract has concrete raw-REST transport and Checks API head binding: the provider downloads the exact pre-record artifact, validates and preserves both the raw `sha256:<64>` artifact digest and its canonical bare digest, signs canonical bytes, creates `Policy-bootstrap-B` with `head_sha == resulting_main_sha`, and `verify-B` reads back the documented `{{name, external_id, app.id, head_sha, status, conclusion}}` fields. No `integration_id` is inferred from the Check Runs API. Provider App, installation, verifier revision, signature key/trust root, result endpoint, and live verifier remain unresolved external blockers; the plan claims no success.

Caller workflow-call values are selectors, not evidence. The caller upper-bound permission union, `on.workflow_call` inputs/secrets/outputs, and `needs.policy-validator-B.outputs.*` transport are explicit in the canonical DAG. The verifier reads the run, workflow, and Contents APIs and requires event/ref/head/workflow/blob/tree equalities. Called identity uses `job_workflow_ref/job_workflow_sha`; caller identity uses `workflow_ref/workflow_sha`; workflow commit SHAs and Contents file blob SHAs remain separate. Release-attestation subject/predicate/certificate fields are bound to the release manifest in the strict pre-record.

The full typed-output fixture is explicitly `deterministic_contract_fixture`, `live_binding=false`, `live_proof_status=not_executed`. Synthetic fixture values are not live proof. Current main has no typed publisher/verifier implementation; this bundle does not pretend otherwise.

## Transition and blockers

Tree A admits the source/generator change with one externally approved `Policy-bootstrap-A` audit and no permanent B pin. A normal protected merge creates the actual resulting main. That exact main push invokes a `workflow_call`-only publisher from the guarded `ci-main` caller. Main B publishes the immutable typed product, provider verifies it, and `verify-B` completes the post-provider census. Only then does a separate Tree-B PR add the permanent pin. Temporary authorities are removed only after permanent Policy verification.

Unresolved blockers: target generator revision; typed publisher source/output and fixed-point generation; provider identities/credential/trust root/live verifier; full xcode-27/Linux-X64/Linux-ARM64 closure; enforceable freeze/CAS/recovery excluding actor 5; and user threat-model choice. Observed `macos-26` remains forbidden. No authority operation is proposed as executed.

## Generated evidence

Schema hashes, fixture hashes, root-manifest digest, and machine graph are in `AUTHORITY-CHANGE-PLAN-2026-09-20-v11.json`. Automated strict-schema validation and negative reproduction must pass before independent review is requested.
"""
    md_path = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v11.md"
    md_path.write_text(plan_md)
    bundle_index = {"schema": "velnor.authority-transition.bundle-index.v11", "root_digest": root_digest, "root_manifest_sha256": sha_file(root_path), "plan_json_sha256": sha_file(plan_json_path), "plan_md_sha256": sha_file(md_path), "canonical_model_sha256": sha_file(MODEL_PATH), "canonical_dag_sha256": sha_file(dag_path), "generated_at": model["observed_revision"]["main"]["observed_utc"], "v10_preserved": True}
    write_json(OUT / "bundle-index.json", bundle_index)
    print(json.dumps({"out": str(OUT), "root_digest": root_digest, "plan_json_sha256": sha_file(plan_json_path), "plan_md_sha256": sha_file(md_path), "dag_sha256": sha_file(dag_path), "release_leaves": len(release_paths), "field_count": len(fields)}, indent=2))


if __name__ == "__main__":
    main()
