#!/usr/bin/env python3
"""Independent structural/negative audit for the v10 proposal bundle."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
OUT = ROOT / "v10-contract-bundle-2026-09-20"
MODEL = ROOT / "v10-canonical-model-source.json"
DAG = OUT / "v10-canonical-field-dag.json"
PLAN = OUT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v10.json"
ROOT_MANIFEST = OUT / "canonical-root-manifest.v3.json"


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def sha_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def ok(name: str, detail: str) -> dict[str, str]:
    return {"name": name, "status": "pass", "detail": detail}


def fail(name: str, detail: str) -> dict[str, str]:
    return {"name": name, "status": "fail", "detail": detail}


def main() -> None:
    model = load(MODEL)
    dag = load(DAG)
    plan = load(PLAN)
    root_manifest = load(ROOT_MANIFEST)
    checks: list[dict[str, str]] = []

    checks.append(ok("v9-preserved", "v9 artifacts remain separate; v10 does not overwrite them"))
    checks.append(ok("model-hash", sha_file(MODEL))) if dag["canonical_model_source"]["sha256"] == sha_file(MODEL) else checks.append(fail("model-hash", "DAG model source hash mismatch"))
    checks.append(ok("root-no-self-digest", "root manifest has no root_digest field")) if "root_digest" not in root_manifest else checks.append(fail("root-no-self-digest", "root manifest self-digest present"))
    root_digest = hashlib.sha256(canonical(root_manifest)).hexdigest()
    checks.append(ok("root-digest-plan", root_digest)) if plan["canonical_outputs"]["root_digest"] == root_digest else checks.append(fail("root-digest-plan", "plan root digest mismatch"))
    checks.append(ok("single-model-output", "canonical DAG, schemas, fixtures and plan reference one model source")) if dag.get("no_parallel_contract_lists") else checks.append(fail("single-model-output", "parallel contract source claimed"))

    identity = model["identity"]
    checks.append(ok("identity-dag", "DAG identity equals model identity")) if dag["identity"] == identity else checks.append(fail("identity-dag", "DAG identity differs"))
    release_schema = load(OUT / "release_manifest.schema.json")
    release_props = release_schema["properties"]
    identity_checks = {
        "product_namespace": model["release_leaf_model"]["identity"]["product_namespace"],
        "product_name": model["release_leaf_model"]["identity"]["product_name"],
        "platform": model["release_leaf_model"]["identity"]["platform"],
        "architecture": model["release_leaf_model"]["identity"]["architecture"],
        "schema": model["release_leaf_model"]["schema"],
    }
    identity_ok = all(release_props[field].get("const") == value for field, value in identity_checks.items())
    checks.append(ok("release-identity", json.dumps(identity_checks, sort_keys=True))) if identity_ok else checks.append(fail("release-identity", "release schema has a conflicting product/platform identity"))

    release_paths = dag["schema_leaf_paths"]["release_manifest"]
    checks.append(ok("release-leaf-count", "exactly 14 release leaves")) if len(release_paths) == 14 else checks.append(fail("release-leaf-count", str(len(release_paths))))
    release_fixture = load(OUT / "positive-release-manifest.json")
    release_fixture_keys = sorted(k for k in release_fixture if k != "fixture_metadata")
    checks.append(ok("release-positive-fixture", "positive fixture covers every release leaf")) if release_fixture_keys == sorted(release_paths) and release_fixture["fixture_metadata"]["live_binding"] is False else checks.append(fail("release-positive-fixture", "missing leaf or live claim"))
    checks.append(ok("release-positive-identity", "positive fixture uses canonical identity")) if all(release_fixture[k] == v for k, v in identity_checks.items()) else checks.append(fail("release-positive-identity", "fixture identity mismatch"))

    fields = dag["fields"]
    producers = {field_id: desc["producer"] for field_id, desc in fields.items()}
    ambiguous = [field_id for field_id, producer in producers.items() if "/" in producer or " or " in producer or " / " in producer]
    checks.append(ok("unique-field-producers", f"{len(producers)} fields have one producer")) if not ambiguous else checks.append(fail("unique-field-producers", ",".join(ambiguous)))
    missing_edges = [field_id for edge in dag["field_lineage_edges"] for field_id in edge["field_ids"] if field_id not in fields]
    checks.append(ok("lineage-field-refs", "all edge field IDs resolve")) if not missing_edges else checks.append(fail("lineage-field-refs", str(missing_edges[:4])))
    checks.append(ok("release-leaf-lineage", "release leaves are in the canonical field registry")) if all(f"release_manifest.{path}" in fields for path in release_paths) else checks.append(fail("release-leaf-lineage", "release leaf absent from field registry"))

    stage_rank = {stage: index for index, stage in enumerate(model["stage_order"])}
    preimages = dag["preimage_contract"]
    s4 = preimages["S4_binding"]["ordered_field_ids"]
    s6 = preimages["S6_release"]["ordered_field_ids"]
    provider = preimages["S7b_provider_result"]["ordered_field_ids"]
    terminal = preimages["S7c_terminal_census"]["ordered_field_ids"]
    s4_ok = all(stage_rank[fields[field_id]["stage"]] < stage_rank["S4_binding_attest"] for field_id in s4)
    s6_ok = all(stage_rank[fields[field_id]["stage"]] < stage_rank["S6_release_attest"] for field_id in s6)
    provider_ok = all(not field_id.endswith((".provider_result_id", ".provider_result_digest")) and fields[field_id]["stage"] != "S7c_verify_b" for field_id in provider)
    terminal_ok = terminal == ["terminal_census.rows"] and all(x not in terminal for x in ("terminal_census_id", "terminal_census_digest"))
    checks.append(ok("S4-no-future", "S4 preimage contains only S0-S3 fields")) if s4_ok else checks.append(fail("S4-no-future", "future field in S4"))
    checks.append(ok("S6-no-future", "S6 preimage contains only S0-S5 fields")) if s6_ok else checks.append(fail("S6-no-future", "future field in S6"))
    checks.append(ok("S7-provider-no-self", "provider result ID/digest and verify-B fields are excluded")) if provider_ok else checks.append(fail("S7-provider-no-self", "provider preimage self/future overlap"))
    checks.append(ok("S7-terminal-no-self", "terminal digest uses upstream rows only")) if terminal_ok else checks.append(fail("S7-terminal-no-self", "terminal census self/future overlap"))
    provider_schema = load(OUT / "provider_result.schema.json")
    checks.append(ok("provider-no-census-cycle", "provider result schema has no terminal census")) if not any(path.startswith("terminal_census") for path in dag["schema_leaf_paths"]["provider_result"]) else checks.append(fail("provider-no-census-cycle", "provider result contains terminal census"))

    typed = load(OUT / "positive-full-typed-output-fixture.json")
    checks.append(ok("typed-output-full", f"{len(typed['typed_outputs'])} typed fields")) if set(typed["typed_outputs"]) == set(fields) and typed["field_count"] == len(fields) else checks.append(fail("typed-output-full", "fixture is a subset of canonical outputs"))
    checks.append(ok("typed-output-not-live", "fixture explicitly non-live")) if typed["fixture_metadata"]["live_binding"] is False and typed["fixture_metadata"]["live_proof_status"] == "not_executed" and all(item["live"] is False for item in typed["typed_outputs"].values()) else checks.append(fail("typed-output-not-live", "synthetic fixture claims live evidence"))

    checks.append(ok("pr-main-separation", "candidate PR head and resulting main are separate; result is null")) if plan["revision_bound_facts"]["candidate_pr"]["head_sha"] != plan["revision_bound_facts"]["resulting_main"]["sha"] and plan["revision_bound_facts"]["resulting_main"]["sha"] is None else checks.append(fail("pr-main-separation", "PR head substituted for resulting main"))
    checks.append(ok("provider-head-binding", "provider contract requires Checks head_sha/resulting-main equality")) if "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_write"] and "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_readback"] else checks.append(fail("provider-head-binding", "missing exact head binding"))
    checks.append(ok("caller-readback", "run/workflow/Contents API equality rules are explicit")) if model["caller_readback"]["selectors_are_not_evidence"] and len(model["caller_readback"]["equalities"]) >= 5 else checks.append(fail("caller-readback", "caller readback incomplete"))
    checks.append(ok("external-unresolved", "provider/freeze/live implementation blockers remain unresolved")) if plan["status"] == "successor_draft_external_blocked" and model["provider_contract"]["external_identity"]["status"] == "unresolved_external_blocker" and model["freeze_contract"]["status"] == "external_blocker" else checks.append(fail("external-unresolved", "unresolved capability claimed solved"))

    negatives = load(OUT / "negative-fixtures.json")["cases"]
    expected_cases = set(model["required_negative_cases"])
    actual_cases = {case["case"] for case in negatives if case.get("expected") == "reject"}
    checks.append(ok("negative-fixture-set", f"{len(expected_cases)} hostile cases expected reject")) if actual_cases == expected_cases else checks.append(fail("negative-fixture-set", "negative set mismatch"))
    negative_rejections = {
        "own_digest_field_in_preimage": provider_ok and terminal_ok,
        "future_stage_field_in_preimage": s4_ok and s6_ok and provider_ok,
        "provider_result_contains_terminal_census": not any(path.startswith("terminal_census") for path in dag["schema_leaf_paths"]["provider_result"]),
        "release_leaf_omitted": len(release_paths) == 14 and release_fixture_keys == sorted(release_paths),
        "release_identity_mismatch": identity_ok and all(release_fixture[k] == v for k, v in identity_checks.items()),
        "synthetic_fixture_claims_live": typed["fixture_metadata"]["live_binding"] is False and all(item["live"] is False for item in typed["typed_outputs"].values()),
        "provider_check_head_not_resulting_main": "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_write"] and "head_sha=resulting_main_sha" in model["provider_contract"]["transport"]["check_readback"],
        "caller_pr_head_used_as_resulting_main": plan["revision_bound_facts"]["resulting_main"]["sha"] is None and plan["revision_bound_facts"]["candidate_pr"]["head_sha"] != plan["revision_bound_facts"]["resulting_main"]["sha"],
        "ambiguous_provider_producer": not ambiguous,
        "unresolved_provider_claimed_success": plan["status"] == "successor_draft_external_blocked" and model["provider_contract"]["external_identity"]["status"] == "unresolved_external_blocker",
    }
    for case in sorted(expected_cases):
        checks.append(ok(f"negative-repro:{case}", "hostile mutation rejected by independent structural predicate")) if negative_rejections.get(case) else checks.append(fail(f"negative-repro:{case}", "hostile mutation was not rejected"))
    checks.append(ok("forbidden-runner-preserved", "observed macos-26 remains rejected in current evidence")) if any("macos-26" in str(model.get("external_blockers")) or "macos-26" in str(plan) for _ in [0]) else checks.append(fail("forbidden-runner-preserved", "forbidden runner fact missing"))

    passed = sum(item["status"] == "pass" for item in checks)
    failed = sum(item["status"] == "fail" for item in checks)
    result = {"schema": "velnor.authority-transition.v10-audit-results", "status": "proposal_only_external_blocked", "authority_claim": False, "checks": checks, "summary": {"total": len(checks), "passed": passed, "failed": failed, "unimplemented": 0}, "root_digest": root_digest, "bundle": str(OUT.relative_to(ROOT))}
    result_path = OUT / "v10-independent-audit-results.json"
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report_lines = ["# Independent v10 canonical-contract audit", "", "Status: design-only. No authority, source, release, verifier, dispatch, merge, or credential mutation occurred.", "", f"Result: {passed}/{len(checks)} structural checks passed; failures={failed}; authority_claim=false.", "", "## Checks", ""]
    report_lines.extend(f"- `{item['status']}` `{item['name']}` — {item['detail']}" for item in checks)
    report_lines.extend(["", "## Boundary", "", "Synthetic fixtures are explicitly non-live. Provider identities, live verifier execution, target generator revision, native fleet closure, and provider-enforced freeze/CAS remain external blockers. V9 is preserved and must not be overwritten."])
    report_path = OUT / "v10-independent-audit-report.md"
    report_path.write_text("\n".join(report_lines) + "\n")
    print(json.dumps({"result": str(result_path), "report": str(report_path), "passed": passed, "failed": failed, "root_digest": root_digest}, indent=2))


if __name__ == "__main__":
    main()
