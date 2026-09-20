"""Independent recomputation of the repaired S7b preimage.

This file deliberately does not import declared_preimage.py. Its selector and
canonical encoder are a second implementation used to catch an incorrect
shared implementation or accidental whole-object hashing.
"""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parent


def canonical(value: Any) -> bytes:
    encoded = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )
    return (encoded + "\n").encode("utf-8")


def digest(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def lookup(roots: dict[str, Any], field_id: str) -> Any:
    root_name, *parts = field_id.split(".")
    value = roots[root_name]
    for part in parts:
        if not isinstance(value, dict) or part not in value:
            raise KeyError(field_id)
        value = value[part]
    return value


def selected_preimage(model: dict[str, Any], roots: dict[str, Any]) -> dict[str, Any]:
    values: dict[str, Any] = {}
    contract = model["canonical_preimage_contract"]["S7b_provider_result"]
    for field_id in contract["ordered_field_ids"]:
        value = lookup(roots, field_id)
        if field_id.rsplit(".", 1)[1] in {
            "canonical_root_manifest_sha256",
            "canonical_root_sha256",
        }:
            value = "0" * 64
        values[field_id] = value
    return values


def load_roots() -> tuple[dict[str, Any], dict[str, Any]]:
    model = json.loads((ROOT / "canonical-model-v3.json").read_text())
    schema_fixtures = {
        path.stem: json.loads(path.read_text())
        for path in (ROOT / "fixtures").glob("*.json")
    }
    roots = {
        "permanent": schema_fixtures["permanent_binding"],
        "pre_record": schema_fixtures["pre_record"],
        "pre_record_transport": schema_fixtures["pre_record_transport"],
        "provider_result": schema_fixtures["provider_result"],
        "release_manifest": schema_fixtures["release_manifest"],
        "verify_b": schema_fixtures["verify_b"],
    }
    return model, roots


def main() -> int:
    model, roots = load_roots()
    provider = roots["provider_result"]
    schema_errors = [
        error.message
        for error in Draft202012Validator(model["canonical_schemas"]["provider_result"]).iter_errors(provider)
    ]
    if schema_errors:
        raise RuntimeError(f"provider fixture is not strict-schema-valid: {schema_errors}")

    selected = selected_preimage(model, roots)
    recomputed = digest(selected)
    emitted = provider["provider_result_digest"]
    if recomputed != emitted:
        raise RuntimeError(f"independent S7b mismatch: recomputed={recomputed} emitted={emitted}")

    whole_object_digest = digest(provider)
    if whole_object_digest == emitted:
        raise RuntimeError("fixture accidentally uses whole provider object as S7b digest")

    excluded_mutation = copy.deepcopy(roots)
    excluded_mutation["provider_result"]["provider_result_id"] = "independent-excluded-field-mutation"
    excluded_selected_digest = digest(selected_preimage(model, excluded_mutation))
    excluded_whole_digest = digest(excluded_mutation["provider_result"])
    if excluded_selected_digest != recomputed:
        raise RuntimeError("excluded provider_result_id changed declared S7b preimage")
    if excluded_whole_digest == whole_object_digest:
        raise RuntimeError("excluded provider_result_id failed to change whole-object digest")

    ordered_mutation = copy.deepcopy(roots)
    ordered_mutation["provider_result"]["source_record_artifact_digest"] = "b" * 64
    ordered_selected_digest = digest(selected_preimage(model, ordered_mutation))
    if ordered_selected_digest == recomputed:
        raise RuntimeError("ordered source_record_artifact_digest failed to change S7b preimage")

    results = {
        "schema": "velnor.independent-preimage-results.v1",
        "status": "pass",
        "model_sha256": hashlib.sha256((ROOT / "canonical-model-v3.json").read_bytes()).hexdigest(),
        "declared_field_count": len(selected),
        "emitted_provider_result_digest": emitted,
        "independently_recomputed_digest": recomputed,
        "whole_provider_object_digest": whole_object_digest,
        "excluded_mutation_field": "provider_result.provider_result_id",
        "excluded_mutation_selected_digest": excluded_selected_digest,
        "excluded_mutation_whole_digest": excluded_whole_digest,
        "ordered_mutation_field": "provider_result.source_record_artifact_digest",
        "ordered_mutation_selected_digest": ordered_selected_digest,
        "strict_schema_errors": schema_errors,
        "authority_mutation": False,
        "source_mutation": False,
    }
    (ROOT / "independent-preimage-results.json").write_bytes(canonical(results))
    report = "\n".join(
        [
            "# Independent S7b preimage recomputation",
            "",
            "Status: `pass`",
            f"Declared fields: `{len(selected)}`",
            f"Recomputed digest: `{recomputed}`",
            f"Whole-object digest: `{whole_object_digest}` (different)",
            "Excluded provider_result_id mutation: selected digest unchanged; whole-object digest changed.",
            "Ordered source_record_artifact_digest mutation: selected digest changed.",
            "No source or authority mutation occurred.",
        ]
    )
    (ROOT / "independent-preimage-report.md").write_text(report + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
