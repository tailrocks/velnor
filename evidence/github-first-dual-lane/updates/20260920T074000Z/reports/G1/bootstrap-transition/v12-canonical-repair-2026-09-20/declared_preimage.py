"""Single canonical preimage implementation for the bounded repair checkpoint."""

from __future__ import annotations

import hashlib
import json
from typing import Any


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def dotted_get(root: dict[str, Any], path: str) -> Any:
    value: Any = root
    for component in path.split("."):
        if not isinstance(value, dict) or component not in value:
            raise KeyError(path)
        value = value[component]
    return value


def declared_preimage_values(
    name: str,
    preimage_contract: dict[str, Any],
    field_registry: dict[str, dict[str, Any]],
    fixtures: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    """Select exactly the declared ordered field IDs; reject hidden extras."""
    spec = preimage_contract[name]
    values: dict[str, Any] = {}
    for field_id in spec["ordered_field_ids"]:
        descriptor = field_registry.get(field_id)
        if descriptor is None:
            raise KeyError(f"undeclared field: {field_id}")
        schema_name, path = field_id.split(".", 1)
        fixture = fixtures.get(schema_name)
        if fixture is None:
            raise KeyError(f"missing fixture root: {schema_name}")
        value = dotted_get(fixture, path)
        if path in {"canonical_root_manifest_sha256", "canonical_root_sha256"}:
            value = "0" * 64
        values[field_id] = value
    return values


def declared_preimage_bytes(
    name: str,
    preimage_contract: dict[str, Any],
    field_registry: dict[str, dict[str, Any]],
    fixtures: dict[str, dict[str, Any]],
) -> bytes:
    return canonical(declared_preimage_values(name, preimage_contract, field_registry, fixtures))


def declared_preimage_digest(
    name: str,
    preimage_contract: dict[str, Any],
    field_registry: dict[str, dict[str, Any]],
    fixtures: dict[str, dict[str, Any]],
) -> str:
    return sha256(declared_preimage_bytes(name, preimage_contract, field_registry, fixtures))
