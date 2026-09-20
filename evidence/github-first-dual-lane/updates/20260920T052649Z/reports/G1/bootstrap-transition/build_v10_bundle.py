#!/usr/bin/env python3
"""Build the v10 proposal bundle from one canonical model source.

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
OUT = ROOT / "v10-contract-bundle-2026-09-20"
MODEL_PATH = ROOT / "v10-canonical-model-source.json"
VOCAB_PATH = ROOT / "v9-independent-contract-audit-2026-09-20/canonical-dag/v9-independent-field-dag.json"
GENERATOR_PATH = ROOT / "build_v10_bundle.py"

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
    if typ == "array":
        return [{"fixture_row": index + 1}]
    if "{40}" in pattern:
        return hashlib.sha1(f"v10-fixture:{index}".encode()).hexdigest()
    if "{64}" in pattern:
        return hashlib.sha256(f"v10-fixture:{index}".encode()).hexdigest()
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
    assert sha_file(MODEL_PATH) == "09d586416a86fae321e35fc24fbecac870a1faec93cd6d17392169ff803e36f1", "model source changed; update expected input deliberately"
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
    permanent_desc = {path: {"type": "string"} for path in permanent}
    for path, meta in permanent.items():
        permanent_desc[path] = type_schema(old_type(meta))

    schemas = {
        "permanent_binding": generated_schema("v10-permanent-binding.v1.schema.json", "Velnor canonical permanent binding", permanent_desc, identity_overrides),
        "pre_record": generated_schema("v10-pre-record.v3.schema.json", "Velnor pre-provider content record", pre_content_desc),
        "pre_record_transport": generated_schema("v10-pre-record-transport.v1.schema.json", "Velnor pre-record transport", pre_transport_desc),
        "provider_result": generated_schema("v10-provider-result.v2.schema.json", "Velnor external provider result", provider_desc),
        "verify_b": generated_schema("v10-verify-b.v1.schema.json", "Velnor verify-B composite result", verify_desc),
        "release_manifest": generated_schema("v10-release-manifest.v1.schema.json", "Velnor immutable release manifest", release_desc),
        "adoption": generated_schema("v10-validator-pin-adoption.v2.schema.json", "Velnor permanent validator adoption", adoption_desc),
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
        fields[f"verify_b.{path}"] = {"path": path, "schema": "verify_b", "stage": "S7c_verify_b", "producer": "verify-B", "type": field_type_from_schema(desc), "required": True, "source": "provider-result readback, called workflow API/OIDC readback, and upstream terminal census"}
    for path, desc in sorted(release_desc.items()):
        if path in {"release_id", "release_tag", "target_sha"}:
            stage, producer = "S3_release_reserve", "reserve-release"
        elif path in {"asset_id", "asset_digest"}:
            stage, producer = "S5_publish", "publish"
        else:
            stage, producer = "S4_binding_attest", "attest-binding"
        fields[f"release_manifest.{path}"] = {"path": path, "schema": "release_manifest", "stage": stage, "producer": producer, "type": field_type_from_schema(desc), "required": True, "source": "canonical immutable release manifest"}
    for path, desc in sorted(adoption_desc.items()):
        fields[f"adoption.{path}"] = {"path": path, "schema": "adoption", "stage": "TreeB", "producer": "tree-b-adoption-verifier", "type": field_type_from_schema(desc), "required": True, "source": "reviewed Tree-B adoption PR and live Main-B proof"}
    fields["terminal_census.rows"] = {"path": "rows", "schema": "verify_b", "stage": "S7c_verify_b", "producer": "verify-B.terminal-census-api", "type": "array", "required": True, "source": "typed upstream run/job/check API rows; input projection, not self metadata"}

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
    lineage_edges.append({"producer": "external-provider-result", "consumer": "verify-B", "orientation": "producer_outputs_to_consumer_inputs", "field_ids": stage_fields["S7b_provider_result"]})

    preimage_specs = {}
    for name, spec in model["preimage_model"]["digest_ids"].items():
        if name == "S4_binding":
            allowed = [field_id for field_id, desc in fields.items() if desc["stage"] in spec["input_stages"]]
        elif name == "S6_release":
            allowed = [field_id for field_id, desc in fields.items() if desc["stage"] in spec["input_stages"]]
        elif name == "S7b_provider_result":
            allowed = stage_fields["S7a_pre_record_transport"] + [field_id for field_id in stage_fields["S7b_provider_result"] if not field_id.endswith(".provider_result_id") and not field_id.endswith(".provider_result_digest")]
        else:
            allowed = ["terminal_census.rows"]
        preimage_specs[name] = {"ordered_field_ids": allowed, "excluded_fields": spec.get("exclude_fields", []), "excluded_future_stages": spec.get("exclude_future_stages", []), "subject": spec["subject"], "digest_source": "sha256(canonical UTF-8 LF JSON bytes of ordered fields only)"}

    schema_leaf_paths = {name: sorted(flatten_schema(schema)) for name, schema in schemas.items()}
    release_paths = schema_leaf_paths["release_manifest"]
    assert len(release_paths) == 14, release_paths
    assert set(release_paths) == set(model["release_leaf_model"]["fields"]), release_paths

    canonical_dag = {
        "schema": "velnor.authority-transition.canonical-field-dag.v10",
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
        "stage_fields": stage_fields,
        "preimage_contract": preimage_specs,
        "release_positive_fixture_required": True,
        "release_positive_fixture_leaf_count": len(release_paths),
        "metrics": {"field_count": len(fields), "permanent_leaf_count": len(schema_leaf_paths["permanent_binding"]), "pre_record_leaf_count": len(schema_leaf_paths["pre_record"]), "provider_leaf_count": len(schema_leaf_paths["provider_result"]), "verify_b_leaf_count": len(schema_leaf_paths["verify_b"]), "release_manifest_leaf_count": len(release_paths), "unique_producers": len({d["producer"] for d in fields.values()})},
        "no_parallel_contract_lists": True,
    }
    dag_path = OUT / "v10-canonical-field-dag.json"
    write_json(dag_path, canonical_dag)

    release_fixture = {"fixture_metadata": {"kind": "deterministic_contract_fixture", "live_binding": False, "live_proof_status": "not_executed", "synthetic_values_are_not_evidence": True}, **unflatten(release_desc)}
    pre_fixture = {"fixture_metadata": {"kind": "deterministic_contract_fixture", "live_binding": False, "live_proof_status": "not_executed"}, **unflatten(pre_content_desc)}
    transport_fixture = {"fixture_metadata": {"kind": "deterministic_contract_fixture", "live_binding": False, "live_proof_status": "not_executed"}, **unflatten(pre_transport_desc)}
    provider_fixture = {"fixture_metadata": {"kind": "deterministic_contract_fixture", "live_binding": False, "live_proof_status": "not_executed"}, **unflatten(provider_desc)}
    verify_fixture = {"fixture_metadata": {"kind": "deterministic_contract_fixture", "live_binding": False, "live_proof_status": "not_executed"}, **unflatten(verify_desc), "terminal_census": {"rows": [{"workflow_path": ".github/workflows/ci-main.yml", "run_id": 1001, "attempt": 1, "job_id": 2001, "check_run_id": 3001, "head_sha": "0" * 40, "conclusion": "success"}]}}
    write_json(OUT / "positive-release-manifest.json", release_fixture)
    write_json(OUT / "positive-pre-record.json", pre_fixture)
    write_json(OUT / "positive-pre-record-transport.json", transport_fixture)
    write_json(OUT / "positive-provider-result.json", provider_fixture)
    write_json(OUT / "positive-verify-b.json", verify_fixture)

    typed_values = {}
    for index, (field_id, desc) in enumerate(sorted(fields.items()), 1):
        typed_values[field_id] = {"value": value_for(type_schema(desc["type"]), index), "producer": desc["producer"], "stage": desc["stage"], "live": False, "provenance": "deterministic_contract_fixture_only"}
    typed_fixture = {"fixture_metadata": model["typed_output_fixture"], "canonical_model_sha256": sha_file(MODEL_PATH), "canonical_dag_sha256": sha_file(dag_path), "field_count": len(typed_values), "typed_outputs": typed_values}
    write_json(OUT / "positive-full-typed-output-fixture.json", typed_fixture)

    negative_cases = [{"case": case, "expected": "reject"} for case in model["required_negative_cases"]]
    write_json(OUT / "negative-fixtures.json", {"schema": "velnor.authority-transition.negative-fixtures.v10", "all_expected": "reject", "cases": negative_cases})

    bound = []
    for path in sorted(OUT.iterdir()):
        if path.name in {"canonical-root-manifest.v3.json", "bundle-index.json", "AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json", "AUTHORITY-CHANGE-PLAN-2026-09-20-v10.md", "v10-independent-audit-results.json", "v10-independent-audit-report.md"}:
            continue
        if path.is_file():
            bound.append({"path": str(path.relative_to(ROOT)), "sha256": sha_file(path), "role": "generated from canonical v10 model"})
    bound.extend([
        {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH), "role": "sole contract model source"},
        {"path": str(VOCAB_PATH.relative_to(ROOT)), "sha256": sha_file(VOCAB_PATH), "role": "historical vocabulary input only"},
        {"path": str(GENERATOR_PATH.relative_to(ROOT)), "sha256": sha_file(GENERATOR_PATH), "role": "reproducible projection generator"},
    ])
    root_manifest = {
        "schema": "velnor.canonical-evidence-root-manifest.v3",
        "root_digest_rule": "SHA-256 of canonical UTF-8 LF JSON bytes with lexicographically sorted keys and compact separators; this object has no self-digest field",
        "canonical_root": ".",
        "model_source": {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH)},
        "current_main": model["observed_revision"]["main"],
        "candidate_pr_head_is_not_resulting_main": True,
        "product_identity": model["identity"],
        "bound_files": sorted(bound, key=lambda item: item["path"]),
        "historical_exclusions": [{"path": "G1/bootstrap-transition/v9-independent-contract-audit-2026-09-20/canonical-dag/v9-independent-field-dag.json", "role": "v9 authority input; only vocabulary is imported by v10"}],
        "live_status": "no live typed verifier or provider execution; synthetic fixtures are explicitly non-live",
    }
    root_path = OUT / "canonical-root-manifest.v3.json"
    write_json(root_path, root_manifest)
    root_digest = sha_bytes(canonical(root_manifest))

    schema_hashes = {name: sha_file(OUT / f"{name}.schema.json") for name in schemas}
    fixture_hashes = {path.name: sha_file(path) for path in sorted(OUT.glob("positive-*.json"))}
    plan = {
        "schema": "velnor.authority-change-plan.v10",
        "status": "successor_draft_external_blocked",
        "execution_authorized": False,
        "mutation_performed": False,
        "observed_utc": model["observed_revision"]["main"]["observed_utc"],
        "supersedes": {"path": "G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.json", "sha256": "e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615", "preserved": True},
        "canonical_source": {"path": str(MODEL_PATH.relative_to(ROOT)), "sha256": sha_file(MODEL_PATH), "single_source": True},
        "canonical_outputs": {"dag_path": str(dag_path.relative_to(ROOT)), "dag_sha256": sha_file(dag_path), "root_manifest_path": str(root_path.relative_to(ROOT)), "root_manifest_raw_sha256": sha_file(root_path), "root_digest": root_digest, "schema_hashes": schema_hashes, "fixture_hashes": fixture_hashes},
        "revision_bound_facts": model["observed_revision"],
        "transition": {"tree_a": "external Policy-bootstrap-A only; no permanent pin", "main_b": "protected resulting-main push invokes workflow_call-only publisher and exact provider verifier", "tree_b": "separate reviewed adoption PR after live Main-B proof", "order": ["read-only census", "approved Tree-A authority", "normal protected merge", "read refs/heads/main and run APIs", "publish and verify Main-B", "post-provider census", "reviewed Tree-B adoption", "remove temporary authorities"], "candidate_vs_result": "candidate_pr.head_sha is input evidence only; resulting_main.sha is null until protected merge and readback"},
        "canonical_contract": {"identity": model["identity"], "field_count": len(fields), "permanent_leaf_count": len(schema_leaf_paths["permanent_binding"]), "release_leaf_count": len(release_paths), "release_positive_fixture": str((OUT / "positive-release-manifest.json").relative_to(ROOT)), "provider_result_excludes_terminal_census": True, "terminal_census_digest_excludes_own_id_and_digest": True, "no_parallel_contract_lists": True},
        "provider_and_readback": {"contract": model["provider_contract"], "caller_readback": model["caller_readback"], "status": "identities and live verifier unresolved; no success claim"},
        "freeze": model["freeze_contract"],
        "external_blockers": model["external_blockers"],
        "independent_review": {"required": ["authority_transition_review", "canonical_dag_independent_audit"], "status": "not_requested_until_v10_automated_negative_reproduction_passes"},
    }
    plan_json_path = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json"
    write_json(plan_json_path, plan)
    plan_md = f"""# Velnor authority-transition plan v10 — proposal only

Status: `{plan['status']}`. V9 remains immutable and preserved. This bundle is a design correction, not authorization: no source, ruleset, App, release, merge, dispatch, runner, or credential mutation occurred.

## Canonical source and revision binding

One model source generates the schemas, typed field registry, producer/consumer graph, preimage sets, fixtures, plan JSON, and this Markdown. It is `{plan['canonical_source']['path']}` with SHA-256 `{plan['canonical_source']['sha256']}`. The generated DAG is `{plan['canonical_outputs']['dag_path']}` with SHA-256 `{plan['canonical_outputs']['dag_sha256']}`. The root manifest has no self-digest; its canonical digest is `{root_digest}`.

Current observed `main` is `{model['observed_revision']['main']['sha']}` (parent `{model['observed_revision']['main']['parent_sha']}`, tree `{model['observed_revision']['main']['tree_sha']}`). PR 969 head `{model['observed_revision']['candidate_pr']['head_sha']}` is candidate evidence only. Resulting-main SHA/tree are deliberately null until the protected merge response, `refs/heads/main`, run API, and workflow source readback all agree. No PR-head-as-main substitution is permitted.

## Corrected contract

Product identity is exactly `{model['identity']['product_id']}` / `{model['identity']['schema']}` / `{model['identity']['asset']}` / `{model['identity']['platform']}` / `{model['identity']['runner']}`. The generated canonical DAG contains {len(schema_leaf_paths['permanent_binding'])} permanent leaves and exactly {len(release_paths)} release-manifest leaves. The release positive fixture is bound and exercises all 14 required fields.

S7 is acyclic. The provider result is emitted by `external-provider-result` and contains no terminal census. `verify-B` reads the signed result, reads the called-workflow identity, then computes a separate terminal census over typed upstream run/job/check rows. Provider-result ID/digest and census ID/digest are excluded from their own preimages; no own or future field is ordered into a digest. Transport artifact IDs are separate fields and never self-hashed.

The provider contract has concrete raw-REST transport and Checks API head binding: the provider downloads the exact pre-record artifact, signs canonical bytes, creates `Policy-bootstrap-B` with `head_sha == resulting_main_sha`, and `verify-B` reads back `{{context, integration_id, app, head_sha, conclusion}}`. Provider App, installation, integration, verifier revision, signature key/trust root, result endpoint, and live verifier remain unresolved external blockers; the plan claims no success.

Caller workflow-call values are selectors, not evidence. The verifier reads the run, workflow, and Contents APIs and requires event/ref/head/workflow/blob/tree equalities. Called identity uses `job_workflow_ref/job_workflow_sha`; caller identity uses `workflow_ref/workflow_sha`.

The full typed-output fixture is explicitly `deterministic_contract_fixture`, `live_binding=false`, `live_proof_status=not_executed`. Synthetic fixture values are not live proof. Current main has no typed publisher/verifier implementation; this bundle does not pretend otherwise.

## Transition and blockers

Tree A admits the source/generator change with one externally approved `Policy-bootstrap-A` audit and no permanent B pin. A normal protected merge creates the actual resulting main. That exact main push invokes a `workflow_call`-only publisher from the guarded `ci-main` caller. Main B publishes the immutable typed product, provider verifies it, and `verify-B` completes the post-provider census. Only then does a separate Tree-B PR add the permanent pin. Temporary authorities are removed only after permanent Policy verification.

Unresolved blockers: target generator revision; typed publisher source/output and fixed-point generation; provider identities/credential/trust root/live verifier; full xcode-27/Linux-X64/Linux-ARM64 closure; enforceable freeze/CAS/recovery excluding actor 5; and user threat-model choice. Observed `macos-26` remains forbidden. No authority operation is proposed as executed.

## Generated evidence

Schema hashes, fixture hashes, root-manifest digest, and machine graph are in `AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json`. Automated negative reproduction must reject own/future preimages, omitted release leaves, identity mismatches, synthetic-as-live claims, PR-head-as-main claims, provider-head mismatches, ambiguous producers, terminal-census cycles, and unresolved-provider success before independent review is requested.
"""
    md_path = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v10.md"
    md_path.write_text(plan_md)
    bundle_index = {"schema": "velnor.authority-transition.bundle-index.v10", "root_digest": root_digest, "root_manifest_sha256": sha_file(root_path), "plan_json_sha256": sha_file(plan_json_path), "plan_md_sha256": sha_file(md_path), "canonical_model_sha256": sha_file(MODEL_PATH), "canonical_dag_sha256": sha_file(dag_path), "generated_at": model["observed_revision"]["main"]["observed_utc"], "v9_preserved": True}
    write_json(OUT / "bundle-index.json", bundle_index)
    print(json.dumps({"out": str(OUT), "root_digest": root_digest, "plan_json_sha256": sha_file(plan_json_path), "plan_md_sha256": sha_file(md_path), "dag_sha256": sha_file(dag_path), "release_leaves": len(release_paths), "field_count": len(fields)}, indent=2))


if __name__ == "__main__":
    main()
