"""Build the bounded canonical-generator repair checkpoint.

This script intentionally has one input: canonical-model-v3.json.  It does
not import or read any historical bundle/schema.  The temporary reproduction
copies only that model and the fixtures it emits.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

from jsonschema import Draft202012Validator

from declared_preimage import declared_preimage_digest


ROOT = Path(__file__).resolve().parent
MODEL_PATH = ROOT / "canonical-model-v3.json"
OUT = ROOT


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_text(value: str) -> str:
    return sha256_bytes(value.encode())


def pattern_value(pattern: str, path: str, minimum: int = 0) -> str:
    """Return a deterministic value for the simple patterns in the model."""
    if "sha256:" in pattern:
        return "sha256:" + "a" * 64
    if "{64}" in pattern:
        return "a" * 64
    if "{40}" in pattern:
        if "velnor-workflow-policy-validator-v1-" in pattern:
            return "velnor-workflow-policy-validator-v1-" + "a" * 40
        return "a" * 40
    if "[0-9a-f]" in pattern:
        return "a" * max(64, minimum)
    if "^refs/" in pattern:
        return "refs/heads/main"
    if "^https?" in pattern:
        return "https://example.invalid/value"
    # The embedded model currently has no other patterns. Keep this fail-closed
    # so a future schema extension cannot silently produce an invalid fixture.
    candidate = "x" * max(1, minimum)
    if re.fullmatch(pattern, candidate):
        return candidate
    raise ValueError(f"no fixture value for pattern at {path}: {pattern}")


def example_for_schema(schema: dict[str, Any], path: str = "$") -> Any:
    if "const" in schema:
        return schema["const"]
    if "enum" in schema:
        return schema["enum"][0]
    if "oneOf" in schema:
        return example_for_schema(schema["oneOf"][0], path)
    if "anyOf" in schema:
        return example_for_schema(schema["anyOf"][0], path)

    kind = schema.get("type")
    if kind == "object":
        properties = schema.get("properties", {})
        required = schema.get("required", list(properties))
        return {
            name: example_for_schema(properties[name], f"{path}.{name}")
            for name in required
        }
    if kind == "array":
        count = schema.get("minItems", 1)
        return [example_for_schema(schema["items"], f"{path}[]") for _ in range(count)]
    if kind == "boolean":
        return False
    if kind == "integer":
        return max(1, schema.get("minimum", 0))
    if kind == "number":
        return max(1, schema.get("minimum", 0))
    if kind == "string":
        minimum = schema.get("minLength", 1)
        if "pattern" in schema:
            return pattern_value(schema["pattern"], path, minimum)
        if schema.get("format") == "uri":
            return "https://example.invalid/provider"
        return "x" * minimum
    raise ValueError(f"unsupported schema at {path}: {schema}")


def errors(schema: dict[str, Any], value: Any) -> list[str]:
    return [error.message for error in Draft202012Validator(schema).iter_errors(value)]


def model_source_digest(model_bytes: bytes) -> str:
    return sha256_bytes(model_bytes)


def stage_number(model: dict[str, Any], stage: str) -> int:
    return model["stage_order"].index(stage)


def validate_model_contract(model: dict[str, Any]) -> dict[str, Any]:
    registry = model["canonical_field_registry"]
    schemas = model["canonical_schemas"]
    repair = model["repair_contract"]
    stage_order = model["stage_order"]
    stage_a = stage_number(model, "S7a_pre_record_transport")
    temporal_failures: list[str] = []
    temporal_targets = repair["temporal_lint_targets"]

    for field_id, expected_stage in temporal_targets.items():
        actual = registry[field_id]["stage"]
        if actual != expected_stage:
            temporal_failures.append(f"{field_id}: expected {expected_stage}, got {actual}")

    for field_id, descriptor in registry.items():
        if descriptor["schema"] != "pre_record":
            continue
        if stage_number(model, descriptor["stage"]) >= stage_a:
            temporal_failures.append(
                f"pre-record field {field_id} is produced at/after S7a: {descriptor['stage']}"
            )

    provider_gaps = [
        field_id
        for field_id, descriptor in registry.items()
        if descriptor["producer"] == "external-provider-result"
        and descriptor["path"] not in schemas["provider_result"].get("properties", {})
    ]
    provider_gaps.extend(
        field_id
        for field_id, descriptor in registry.items()
        if descriptor["schema"] == "permanent_binding"
        and descriptor["producer"] == "external-provider-result"
    )

    preimage = model["canonical_preimage_contract"]["S7b_provider_result"]
    excluded = set(preimage["excluded_field_ids"])
    ordered = set(preimage["ordered_field_ids"])
    preimage_failures = sorted(
        field_id for field_id in ordered if field_id in excluded
    )
    future_failures = sorted(
        field_id
        for field_id in preimage["ordered_field_ids"]
        if registry[field_id]["stage"] in set(preimage["excluded_future_stages"])
        and field_id not in {
            "provider_result.provider_result_digest",
        }
    )

    return {
        "temporal_failures": temporal_failures,
        "provider_schema_gaps": sorted(set(provider_gaps)),
        "provider_preimage_excluded_ordered": preimage_failures,
        "provider_preimage_future_fields": future_failures,
        "temporal_targets": {
            field_id: registry[field_id]["stage"] for field_id in temporal_targets
        },
    }


def main() -> int:
    model_bytes = MODEL_PATH.read_bytes()
    model = json.loads(model_bytes)
    sole_input_rule = model.get("sole_input_rule", "")
    if not isinstance(sole_input_rule, str) or "sole" not in sole_input_rule.lower() or "historical" not in sole_input_rule.lower():
        raise RuntimeError("model does not declare the historical-read prohibition")

    fixture_names = tuple(model["canonical_schemas"])
    with TemporaryDirectory(prefix="velnor-canonical-repair-") as temporary:
        temporary_root = Path(temporary)
        temporary_model = temporary_root / "canonical-model-v3.json"
        temporary_model.write_bytes(model_bytes)
        temporary_fixtures = temporary_root / "fixtures"
        temporary_fixtures.mkdir()

        schema_fixtures: dict[str, dict[str, Any]] = {}
        schema_errors: dict[str, list[str]] = {}
        for name in fixture_names:
            value = example_for_schema(model["canonical_schemas"][name], name)
            schema_fixtures[name] = value
            schema_errors[name] = errors(model["canonical_schemas"][name], value)
            (temporary_fixtures / f"{name}.json").write_bytes(canonical(value))

        # Field IDs use canonical roots (permanent, pre_record, ...), while
        # schema files have explicit names. Keep this mapping in one place.
        fixtures: dict[str, dict[str, Any]] = {
            "permanent": schema_fixtures["permanent_binding"],
            "pre_record": schema_fixtures["pre_record"],
            "pre_record_transport": schema_fixtures["pre_record_transport"],
            "provider_result": schema_fixtures["provider_result"],
            "release_manifest": schema_fixtures["release_manifest"],
            "verify_b": schema_fixtures["verify_b"],
        }

        provider_digest = declared_preimage_digest(
            "S7b_provider_result",
            model["canonical_preimage_contract"],
            model["canonical_field_registry"],
            fixtures,
        )
        fixtures["provider_result"]["provider_result_digest"] = provider_digest
        schema_errors["provider_result"] = errors(
            model["canonical_schemas"]["provider_result"], fixtures["provider_result"]
        )
        (temporary_fixtures / "provider_result.json").write_bytes(
            canonical(fixtures["provider_result"])
        )

        contract = validate_model_contract(model)
        if any(schema_errors.values()):
            raise RuntimeError(f"strict schema fixture failure: {schema_errors}")
        if contract["temporal_failures"]:
            raise RuntimeError(f"temporal lint failure: {contract['temporal_failures']}")
        if contract["provider_schema_gaps"]:
            raise RuntimeError(f"provider schema coverage failure: {contract['provider_schema_gaps']}")
        if contract["provider_preimage_excluded_ordered"]:
            raise RuntimeError("excluded field entered provider preimage")
        if contract["provider_preimage_future_fields"]:
            raise RuntimeError(f"future field entered provider preimage: {contract['provider_preimage_future_fields']}")

        # Re-open only the temporary model/fixtures. This proves the isolated
        # reproduction has no ambient schema or historical-bundle dependency.
        isolated_model = json.loads(temporary_model.read_text())
        isolated_schema_fixtures = {
            name: json.loads((temporary_fixtures / f"{name}.json").read_text())
            for name in fixture_names
        }
        isolated_fixtures = {
            "permanent": isolated_schema_fixtures["permanent_binding"],
            "pre_record": isolated_schema_fixtures["pre_record"],
            "pre_record_transport": isolated_schema_fixtures["pre_record_transport"],
            "provider_result": isolated_schema_fixtures["provider_result"],
            "release_manifest": isolated_schema_fixtures["release_manifest"],
            "verify_b": isolated_schema_fixtures["verify_b"],
        }
        isolated_digest = declared_preimage_digest(
            "S7b_provider_result",
            isolated_model["canonical_preimage_contract"],
            isolated_model["canonical_field_registry"],
            isolated_fixtures,
        )
        if isolated_digest != provider_digest:
            raise RuntimeError("isolated re-open changed provider digest")

        durable_fixtures = OUT / "fixtures"
        if durable_fixtures.exists():
            shutil.rmtree(durable_fixtures)
        durable_fixtures.mkdir()
        for name, value in schema_fixtures.items():
            (durable_fixtures / f"{name}.json").write_bytes(canonical(value))

        results = {
            "schema": "velnor.canonical-repair-checkpoint-results.v1",
            "status": "bounded_repair_external_blocked",
            "model_path": str(MODEL_PATH),
            "model_sha256": model_source_digest(model_bytes),
            "model_version": model["model_version"],
            "fixture_directory": str(durable_fixtures),
            "strict_schema_errors": schema_errors,
            "strict_schema_pass": not any(schema_errors.values()),
            "provider_result_digest": provider_digest,
            "provider_preimage_field_count": len(model["canonical_preimage_contract"]["S7b_provider_result"]["ordered_field_ids"]),
            "provider_preimage_excluded_field_count": len(model["canonical_preimage_contract"]["S7b_provider_result"]["excluded_field_ids"]),
            "provider_preimage_pass": not contract["provider_preimage_excluded_ordered"] and not contract["provider_preimage_future_fields"],
            "provider_schema_coverage_pass": not contract["provider_schema_gaps"],
            "temporal_lint_pass": not contract["temporal_failures"],
            "temporal_targets": contract["temporal_targets"],
            "temporary_directory_entries": sorted(
                str(path.relative_to(temporary_root))
                for path in temporary_root.rglob("*")
                if path.is_file()
            ),
            "isolated_reopen_digest": isolated_digest,
            "authority_mutation": False,
            "source_mutation": False,
            "remaining_external_gates": [
                "typed transport channels and terminal semantic census require independent implementation/review",
                "raw ZIP/inner payload/binary/release byte equality requires live external verification",
                "provider identities, freeze authority, and GitHub production execution remain unresolved",
            ],
        }
        (OUT / "repair-checkpoint-results.json").write_bytes(canonical(results))
        report = "\n".join(
            [
                "# V12 canonical repair checkpoint",
                "",
                f"Status: `{results['status']}`",
                f"Model: `{results['model_sha256']}` (`{results['model_version']}`)",
                f"Strict schema fixtures: `{results['strict_schema_pass']}`",
                f"S7b declared preimage fields: `{results['provider_preimage_field_count']}`",
                f"S7b provider digest: `{provider_digest}`",
                f"Temporal lint: `{results['temporal_lint_pass']}`",
                f"Provider schema coverage: `{results['provider_schema_coverage_pass']}`",
                f"Isolated re-open digest: `{isolated_digest}`",
                "",
                "This is a bounded repair checkpoint, not an approval or release.",
                "No source, authority, GitHub, package, or release mutation occurred.",
            ]
        )
        (OUT / "repair-checkpoint-report.md").write_text(report + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
