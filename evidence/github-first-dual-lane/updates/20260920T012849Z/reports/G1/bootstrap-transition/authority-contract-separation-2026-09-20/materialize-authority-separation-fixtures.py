#!/usr/bin/env python3
"""Materialize disjoint temporary-admission/permanent-B hostile fixtures."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
BUNDLE = ROOT / "authority-separation-fixtures.json"
TEMP = json.loads((ROOT / "temporary-admission-positive.json").read_text())
PERM = json.loads((ROOT / "permanent-b-binding-positive.json").read_text())
OUT = ROOT / "fixtures"


def parent_and_key(document: object, pointer: str) -> tuple[object, str]:
    parts = pointer.lstrip("/").split("/")
    current = document
    for part in parts[:-1]:
        part = part.replace("~1", "/").replace("~0", "~")
        current = current[int(part)] if isinstance(current, list) else current[part]
    key = parts[-1].replace("~1", "/").replace("~0", "~")
    return current, key


def apply_mutation(document: object, mutation: dict[str, object]) -> None:
    parent, key = parent_and_key(document, str(mutation["path"]))
    operation = mutation["op"]
    if operation == "replace":
        if isinstance(parent, list):
            parent[int(key)] = mutation["value"]
        else:
            parent[key] = mutation["value"]
    elif operation == "add":
        if isinstance(parent, list):
            parent.insert(int(key), mutation["value"])
        else:
            parent[key] = mutation["value"]
    elif operation == "delete":
        if isinstance(parent, list):
            parent.pop(int(key))
        else:
            del parent[key]
    else:
        raise ValueError(f"unknown operation: {operation}")


def main() -> None:
    bundle = json.loads(BUNDLE.read_text())
    bases = {"temporary": TEMP["document"], "permanent": PERM["document"]}
    OUT.mkdir(exist_ok=True)
    records = []
    expected_ids = set()
    for case in bundle["cases"]:
        case_id = str(case["id"])
        expected_ids.add(case_id)
        document = copy.deepcopy(bases[str(case["base"])])
        for mutation in case["mutations"]:
            apply_mutation(document, mutation)
        payload = {
            "schema": "velnor.authority-contract-separation-fixture.v1",
            "fixture_id": case_id,
            "base": case["base"],
            "expected": case["expected"],
            "reason": case["reason"],
            "mutations": case["mutations"],
            "document": document,
        }
        path = OUT / f"{case_id}.json"
        path.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n")
        records.append({
            "id": case_id,
            "base": case["base"],
            "expected": case["expected"],
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "bytes": path.stat().st_size,
        })
    actual = {path.stem for path in OUT.glob("T*.json")} | {path.stem for path in OUT.glob("B*.json")}
    if actual != expected_ids:
        raise SystemExit(f"fixture mismatch: {sorted(actual ^ expected_ids)}")
    index = {
        "schema": "velnor.authority-contract-separation-index.v1",
        "bundle_sha256": hashlib.sha256(BUNDLE.read_bytes()).hexdigest(),
        "temporary_base_sha256": hashlib.sha256(json.dumps(TEMP["document"], sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        "permanent_base_sha256": hashlib.sha256(json.dumps(PERM["document"], sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        "case_count": len(records),
        "fixtures": sorted(records, key=lambda item: item["id"]),
    }
    (ROOT / "authority-separation-fixtures.index.json").write_text(json.dumps(index, sort_keys=True, indent=2) + "\n")
    print(f"materialized {len(records)} separation fixtures")


if __name__ == "__main__":
    main()
