"""Audit the bounded repair output using the shared declared preimage code."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator

from declared_preimage import declared_preimage_digest


ROOT = Path(__file__).resolve().parent


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def stage_number(model: dict[str, Any], stage: str) -> int:
    return model["stage_order"].index(stage)


def roots_from_fixtures(model: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    fixture_dir = ROOT / "fixtures"
    schemas = model["canonical_schemas"]
    schema_fixtures = {
        name: json.loads((fixture_dir / f"{name}.json").read_text())
        for name in schemas
    }
    roots = {
        "permanent": schema_fixtures["permanent_binding"],
        "pre_record": schema_fixtures["pre_record"],
        "pre_record_transport": schema_fixtures["pre_record_transport"],
        "provider_result": schema_fixtures["provider_result"],
        "release_manifest": schema_fixtures["release_manifest"],
        "verify_b": schema_fixtures["verify_b"],
    }
    return schema_fixtures, roots


def main() -> int:
    model_path = ROOT / "canonical-model-v3.json"
    model_bytes = model_path.read_bytes()
    model = json.loads(model_bytes)
    schema_fixtures, roots = roots_from_fixtures(model)
    schema_errors = {
        name: [error.message for error in Draft202012Validator(model["canonical_schemas"][name]).iter_errors(value)]
        for name, value in schema_fixtures.items()
    }

    registry = model["canonical_field_registry"]
    repair = model["repair_contract"]
    temporal_failures: list[str] = []
    for field_id, expected_stage in repair["temporal_lint_targets"].items():
        actual_stage = registry[field_id]["stage"]
        if actual_stage != expected_stage:
            temporal_failures.append(f"{field_id}: {actual_stage} != {expected_stage}")
    s7a = stage_number(model, "S7a_pre_record_transport")
    for field_id, descriptor in registry.items():
        if descriptor["schema"] == "pre_record" and stage_number(model, descriptor["stage"]) >= s7a:
            temporal_failures.append(f"late pre-record field: {field_id}")

    provider_schema = model["canonical_schemas"]["provider_result"]
    provider_gaps = [
        field_id
        for field_id, descriptor in registry.items()
        if descriptor["producer"] == "external-provider-result"
        and descriptor["path"] not in provider_schema.get("properties", {})
    ]
    permanent_external = [
        field_id
        for field_id, descriptor in registry.items()
        if descriptor["schema"] == "permanent_binding"
        and descriptor["producer"] == "external-provider-result"
    ]
    preimage = model["canonical_preimage_contract"]["S7b_provider_result"]
    ordered = set(preimage["ordered_field_ids"])
    excluded = set(preimage["excluded_field_ids"])
    ordered_excluded = sorted(ordered & excluded)
    future_stages = set(preimage["excluded_future_stages"])
    future_ordered = sorted(
        field_id
        for field_id in preimage["ordered_field_ids"]
        if registry[field_id]["stage"] in future_stages
    )
    digest = declared_preimage_digest(
        "S7b_provider_result",
        model["canonical_preimage_contract"],
        registry,
        roots,
    )
    emitted = roots["provider_result"]["provider_result_digest"]

    failures = {
        "schema": {name: messages for name, messages in schema_errors.items() if messages},
        "temporal": temporal_failures,
        "provider_schema_gaps": sorted(set(provider_gaps)),
        "permanent_external_provider_leaves": permanent_external,
        "ordered_excluded": ordered_excluded,
        "future_ordered": future_ordered,
        "digest_mismatch": [] if digest == emitted else [f"{digest} != {emitted}"],
    }
    passed = not any(failures.values())
    results = {
        "schema": "velnor.canonical-repair-audit-results.v1",
        "status": "pass" if passed else "fail",
        "model_sha256": hashlib.sha256(model_bytes).hexdigest(),
        "strict_schema_pass": not failures["schema"],
        "temporal_lint_pass": not failures["temporal"],
        "provider_schema_coverage_pass": not failures["provider_schema_gaps"] and not failures["permanent_external_provider_leaves"],
        "provider_preimage_pass": not failures["ordered_excluded"] and not failures["future_ordered"],
        "shared_provider_digest": digest,
        "emitted_provider_digest": emitted,
        "failures": failures,
        "authority_mutation": False,
        "source_mutation": False,
    }
    (ROOT / "repair-audit-results.json").write_bytes(canonical(results))
    report = "\n".join(
        [
            "# Canonical repair audit",
            "",
            f"Status: `{results['status']}`",
            f"Strict schema: `{results['strict_schema_pass']}`",
            f"Temporal lint: `{results['temporal_lint_pass']}`",
            f"Provider schema coverage: `{results['provider_schema_coverage_pass']}`",
            f"Provider preimage: `{results['provider_preimage_pass']}`",
            f"Shared S7b digest: `{digest}`",
            "",
            "The audit imports the shared declared_preimage.py implementation.",
            "No source or authority mutation occurred.",
        ]
    )
    (ROOT / "repair-audit-report.md").write_text(report + "\n")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
