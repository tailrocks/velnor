#!/usr/bin/env python3
"""External synthetic terminal-census contract and adversarial tests."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from typing import Any, Callable

from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
REPAIR = ROOT / "v12-canonical-repair-2026-09-20"
OUT = ROOT / "terminal-census-fixtures-2026-09-20-luna"
NEGATIVE = OUT / "negative"
MODEL_PATH = REPAIR / "canonical-model-v3.json"
FREEZE_PATH = ROOT / "v12-freeze-manifest.json"

REPOSITORY = "tailrocks/velnor"
EVENT = "push"
REF = "refs/heads/main"
HEAD_SHA = "b" * 40
WORKFLOW_SHA = "5" * 40
BLOB_SHA = "6" * 40
APP_ID = 9900
RUN_ATTEMPT = 1


def load(path: Path) -> Any:
    return json.loads(path.read_text())


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def sha_value(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def sha_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n")


def s40() -> dict[str, Any]:
    return {"type": "string", "pattern": "^[0-9a-f]{40}$"}


def s64() -> dict[str, Any]:
    return {"type": "string", "pattern": "^[0-9a-f]{64}$"}


def text() -> dict[str, Any]:
    return {"type": "string", "minLength": 1}


def number() -> dict[str, Any]:
    return {"type": "integer", "minimum": 1}


def obj(properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    return {"type": "object", "additionalProperties": False, "properties": properties, "required": sorted(required or properties)}


def schema() -> dict[str, Any]:
    step = obj({"name": text(), "number": number(), "status": {"type": "string", "const": "completed"}, "conclusion": {"type": "string", "const": "success"}})
    source = obj({"repository": text(), "path": text(), "ref": text(), "workflow_sha": s40(), "blob_sha": s40(), "source_kind": {"type": "string", "const": "workflow_contents_api"}})
    run = obj({"id": number(), "attempt": number(), "repository": text(), "workflow_path": text(), "workflow_sha": s40(), "event": text(), "ref": text(), "head_sha": s40(), "status": {"type": "string", "const": "completed"}, "conclusion": {"type": "string", "const": "success"}})
    job = obj({"id": number(), "run_id": number(), "run_attempt": number(), "key": text(), "name": text(), "check_run_id": number(), "status": {"type": "string", "const": "completed"}, "conclusion": {"type": "string", "const": "success"}, "steps": {"type": "array", "minItems": 1, "items": step}})
    check = obj({"id": number(), "name": text(), "external_id": text(), "app_id": number(), "head_sha": s40(), "status": {"type": "string", "const": "completed"}, "conclusion": {"type": "string", "const": "success"}})
    consumer = obj({"id": text(), "workflow_path": text(), "job_key": text(), "check_name": text(), "check_external_id": text(), "check_app_id": number()})
    producer = obj({"stage": {"type": "string", "const": "S7c_verify_b"}, "job": {"type": "string", "const": "verify-B"}, "step": {"type": "string", "const": "terminal-census-api"}, "run_api": {"type": "string", "format": "uri"}, "jobs_api": {"type": "string", "format": "uri"}, "check_api": {"type": "string", "format": "uri"}, "workflow_contents_api": {"type": "string", "format": "uri"}, "source_record": obj({"run_id": number(), "run_attempt": number(), "job_id": number(), "check_run_id": number()})})
    row = obj({"consumer": consumer, "source": source, "run": run, "job": job, "check": check, "producer": producer})
    excluded = obj({"id": text(), "repository": text(), "workflow_path": text(), "job_key": text(), "check_name": text(), "check_external_id": text(), "reason": text()})
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://velnor.dev/schemas/v12-terminal-census.v1.schema.json",
        "title": "Velnor exact terminal consumer census",
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "schema": {"type": "string", "const": "velnor.workflow-policy-validator.terminal-census.v1"},
            "status": {"type": "string", "const": "synthetic_fixture"},
            "synthetic_only": {"type": "boolean", "const": True},
            "authority_claim": {"type": "boolean", "const": False},
            "repository": text(), "event": {"type": "string", "const": "push"}, "ref": {"type": "string", "const": REF}, "head_sha": s40(), "run_attempt": number(),
            "consumer_rows": {"type": "array", "minItems": 15, "maxItems": 15, "uniqueItems": True, "items": row},
            "excluded_rows": {"type": "array", "minItems": 4, "maxItems": 4, "uniqueItems": True, "items": excluded},
            "rows_digest": s64(),
        },
        "required": ["schema", "status", "synthetic_only", "authority_claim", "repository", "event", "ref", "head_sha", "run_attempt", "consumer_rows", "excluded_rows", "rows_digest"],
    }


def registry(model: dict[str, Any]) -> list[dict[str, Any]]:
    result = []
    for index, workflow_name in enumerate(model["authority_graph"]["consumer_census"]["required_workflows"], 1):
        workflow_path = f".github/workflows/{workflow_name}"
        stem = workflow_name.removesuffix(".yml")
        job_key = "policy-validator-B" if workflow_name == "ci-policy-validator-products.yml" else f"{stem}-required"
        check_name = "Policy-bootstrap-B" if workflow_name == "ci-policy-validator-products.yml" else stem
        result.append({
            "consumer_id": f"{workflow_path}::{job_key}::{check_name}", "workflow_path": workflow_path, "job_key": job_key,
            "check_name": check_name, "check_external_id": f"terminal-check-{index}", "check_app_id": APP_ID,
            "repository": REPOSITORY, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "run_attempt": RUN_ATTEMPT,
            "workflow_sha": WORKFLOW_SHA, "blob_sha": BLOB_SHA, "producer_stage": "S7c_verify_b", "producer_job": "verify-B", "producer_step": "terminal-census-api",
            "fixture_run_id": 7400 + index, "fixture_job_id": 7500 + index, "fixture_check_run_id": 7600 + index,
        })
    return result


def exclusions() -> list[dict[str, str]]:
    return [
        {"id": "excluded::verify-B", "repository": REPOSITORY, "workflow_path": ".github/workflows/ci-policy-validator-products.yml", "job_key": "verify-B", "check_name": "Policy-bootstrap-B", "check_external_id": "provider-result-v12-fixture", "reason": "verify-B producer self-row excluded by exact identity tuple"},
        {"id": "excluded::Policy", "repository": REPOSITORY, "workflow_path": ".github/workflows/ci-main.yml", "job_key": "Policy", "check_name": "Policy", "check_external_id": "policy-main", "reason": "Policy wrapper excluded by exact identity tuple"},
        {"id": "excluded::Policy-bootstrap-B", "repository": REPOSITORY, "workflow_path": ".github/workflows/ci-policy-validator-products.yml", "job_key": "policy-validator-B", "check_name": "Policy-bootstrap-B", "check_external_id": "provider-result-v12-fixture", "reason": "provider policy check excluded from upstream consumer census"},
        {"id": "excluded::caller-wrapper", "repository": REPOSITORY, "workflow_path": ".github/workflows/ci-main.yml", "job_key": "caller-wrapper", "check_name": "policy-validator-B", "check_external_id": "caller-wrapper", "reason": "workflow-call caller wrapper excluded by exact identity tuple"},
    ]


def row(entry: dict[str, Any]) -> dict[str, Any]:
    run_id, job_id, check_id, path = entry["fixture_run_id"], entry["fixture_job_id"], entry["fixture_check_run_id"], entry["workflow_path"]
    return {
        "consumer": {"id": entry["consumer_id"], "workflow_path": path, "job_key": entry["job_key"], "check_name": entry["check_name"], "check_external_id": entry["check_external_id"], "check_app_id": APP_ID},
        "source": {"repository": REPOSITORY, "path": path, "ref": REF, "workflow_sha": WORKFLOW_SHA, "blob_sha": BLOB_SHA, "source_kind": "workflow_contents_api"},
        "run": {"id": run_id, "attempt": RUN_ATTEMPT, "repository": REPOSITORY, "workflow_path": path, "workflow_sha": WORKFLOW_SHA, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success"},
        "job": {"id": job_id, "run_id": run_id, "run_attempt": RUN_ATTEMPT, "key": entry["job_key"], "name": entry["job_key"], "check_run_id": check_id, "status": "completed", "conclusion": "success", "steps": [{"name": "terminal-census-source", "number": 1, "status": "completed", "conclusion": "success"}]},
        "check": {"id": check_id, "name": entry["check_name"], "external_id": entry["check_external_id"], "app_id": APP_ID, "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success"},
        "producer": {"stage": "S7c_verify_b", "job": "verify-B", "step": "terminal-census-api", "run_api": f"https://api.github.com/repos/{REPOSITORY}/actions/runs/{run_id}", "jobs_api": f"https://api.github.com/repos/{REPOSITORY}/actions/runs/{run_id}/attempts/{RUN_ATTEMPT}/jobs", "check_api": f"https://api.github.com/repos/{REPOSITORY}/check-runs/{check_id}", "workflow_contents_api": f"https://api.github.com/repos/{REPOSITORY}/contents/{path}?ref={WORKFLOW_SHA}", "source_record": {"run_id": run_id, "run_attempt": RUN_ATTEMPT, "job_id": job_id, "check_run_id": check_id}},
    }


def positive(model: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]], list[dict[str, str]]]:
    required = registry(model)
    rows = [row(entry) for entry in required]
    excluded = exclusions()
    fixture = {"schema": "velnor.workflow-policy-validator.terminal-census.v1", "status": "synthetic_fixture", "synthetic_only": True, "authority_claim": False, "repository": REPOSITORY, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "run_attempt": RUN_ATTEMPT, "consumer_rows": rows, "excluded_rows": excluded, "rows_digest": sha_value(rows)}
    return fixture, required, excluded


def errors(schema_value: dict[str, Any], instance: Any) -> list[str]:
    found = sorted(Draft202012Validator(schema_value, format_checker=FormatChecker()).iter_errors(instance), key=lambda error: list(error.absolute_path))
    return [f"{'.'.join(str(part) for part in error.absolute_path)}: {error.message}" for error in found]


def semantic(instance: dict[str, Any], required: list[dict[str, Any]], excluded: list[dict[str, str]]) -> list[str]:
    out: list[str] = []
    by_id = {entry["consumer_id"]: entry for entry in required}
    rows = instance.get("consumer_rows", [])
    ids = [r.get("consumer", {}).get("id") for r in rows if isinstance(r, dict)]
    if len(rows) != 15: out.append("exact_row_count")
    if len(ids) != len(set(ids)): out.append("duplicate_consumer_id")
    if set(ids) != set(by_id): out.append("required_consumer_identity_set")
    if any(instance.get(k) != v for k, v in {"repository": REPOSITORY, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "run_attempt": RUN_ATTEMPT}.items()): out.append("census_context")
    runs: set[Any] = set(); jobs: set[Any] = set(); checks: set[Any] = set(); composites: set[Any] = set()
    for index, current in enumerate(rows):
        if not isinstance(current, dict): out.append(f"row[{index}].object"); continue
        consumer = current.get("consumer", {}); expected = by_id.get(consumer.get("id"))
        if expected is None: out.append(f"row[{index}].unknown_consumer"); continue
        for key in ("workflow_path", "job_key", "check_name", "check_external_id", "check_app_id"):
            if consumer.get(key) != expected[key]: out.append(f"row[{index}].consumer.{key}")
        source = current.get("source", {})
        for key, value in {"repository": REPOSITORY, "path": expected["workflow_path"], "ref": REF, "workflow_sha": WORKFLOW_SHA, "blob_sha": BLOB_SHA, "source_kind": "workflow_contents_api"}.items():
            if source.get(key) != value: out.append(f"row[{index}].source.{key}")
        run = current.get("run", {}); job = current.get("job"); check = current.get("check"); producer = current.get("producer")
        if not isinstance(job, dict) or not isinstance(check, dict) or not isinstance(producer, dict): out.append(f"row[{index}].required_child_missing"); continue
        for key, value in {"repository": REPOSITORY, "workflow_path": expected["workflow_path"], "workflow_sha": WORKFLOW_SHA, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "attempt": RUN_ATTEMPT, "status": "completed", "conclusion": "success"}.items():
            if run.get(key) != value: out.append(f"row[{index}].run.{key}")
        for collection, value, label in ((runs, run.get("id"), "run_id"), (jobs, job.get("id"), "job_id"), (checks, check.get("id"), "check_id")):
            if value in collection: out.append(f"row[{index}].duplicate_{label}")
            collection.add(value)
        composite = (REPOSITORY, expected["workflow_path"], run.get("id"), run.get("attempt"), job.get("id"), check.get("id"), check.get("external_id"), check.get("app_id"))
        if composite in composites: out.append(f"row[{index}].duplicate_provenance");
        composites.add(composite)
        for key, value in {"run_id": run.get("id"), "run_attempt": RUN_ATTEMPT, "key": expected["job_key"], "name": expected["job_key"], "check_run_id": check.get("id"), "status": "completed", "conclusion": "success"}.items():
            if job.get(key) != value: out.append(f"row[{index}].job.{key}")
        if not job.get("steps"): out.append(f"row[{index}].child_step_missing")
        for key, value in {"id": check.get("id"), "name": expected["check_name"], "external_id": expected["check_external_id"], "app_id": APP_ID, "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success"}.items():
            if check.get(key) != value: out.append(f"row[{index}].check.{key}")
        urls = {"run_api": f"https://api.github.com/repos/{REPOSITORY}/actions/runs/{run.get('id')}", "jobs_api": f"https://api.github.com/repos/{REPOSITORY}/actions/runs/{run.get('id')}/attempts/{RUN_ATTEMPT}/jobs", "check_api": f"https://api.github.com/repos/{REPOSITORY}/check-runs/{check.get('id')}", "workflow_contents_api": f"https://api.github.com/repos/{REPOSITORY}/contents/{expected['workflow_path']}?ref={WORKFLOW_SHA}"}
        for key, value in {"stage": "S7c_verify_b", "job": "verify-B", "step": "terminal-census-api", **urls}.items():
            if producer.get(key) != value: out.append(f"row[{index}].producer.{key}")
        if producer.get("source_record") != {"run_id": run.get("id"), "run_attempt": RUN_ATTEMPT, "job_id": job.get("id"), "check_run_id": check.get("id")}: out.append(f"row[{index}].producer.source_record")
    expected_excluded = {item["id"]: item for item in excluded}; actual_excluded = {item.get("id"): item for item in instance.get("excluded_rows", []) if isinstance(item, dict)}
    if set(actual_excluded) != set(expected_excluded): out.append("excluded_identity_set")
    for key, value in expected_excluded.items():
        if actual_excluded.get(key) != value: out.append(f"excluded_identity:{key}")
    if instance.get("rows_digest") != sha_value(rows): out.append("rows_digest")
    return sorted(set(out))


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True); NEGATIVE.mkdir(parents=True, exist_ok=True)
    model = load(MODEL_PATH); contract_schema = schema(); fixture, required, excluded = positive(model)
    write_json(OUT / "terminal-census.schema.json", contract_schema)
    write_json(OUT / "positive-terminal-census.json", fixture)
    write_json(OUT / "terminal-census-contract.json", {"schema": "velnor.workflow-policy-validator.terminal-census-contract.v1", "status": "synthetic_contract_only", "synthetic_only": True, "authority_claim": False, "basis": {"repair_model_path": str(MODEL_PATH.relative_to(ROOT)), "repair_model_sha256": sha_file(MODEL_PATH), "repair_model_version": model["model_version"], "v12_freeze_manifest_sha256": sha_file(FREEZE_PATH), "v12_root_digest": load(FREEZE_PATH)["bundle"]["root_digest"]}, "fixture_context": {"repository": REPOSITORY, "event": EVENT, "ref": REF, "head_sha": HEAD_SHA, "run_attempt": RUN_ATTEMPT, "app_id": APP_ID, "workflow_sha": WORKFLOW_SHA, "blob_sha": BLOB_SHA}, "required_consumer_registry": required, "excluded_exact_identity_registry": excluded, "provenance_fields": ["source Contents API path/ref/repository/commit/blob", "run API id/attempt/event/ref/head/status/conclusion", "attempt Jobs API id/run/attempt/key/check_run_id/status/conclusion/steps", "Check Runs API id/name/external_id/app.id/head/status/conclusion", "verify-B stage/job/step/API URLs/source-record parent IDs"], "semantic_rules": ["exact required identity set, not minItems", "unique composite run/job/check identity", "all source/run/job/check/producers parent-bound", "exact exclusion tuples, never names-only", "canonical rows digest recomputation"]})
    positive_schema_errors, positive_semantic_errors = errors(contract_schema, fixture), semantic(fixture, required, excluded)
    cases: list[tuple[str, Callable[[dict[str, Any]], None], str]] = []
    def add(name: str, mutate: Callable[[dict[str, Any]], None], intent: str) -> None: cases.append((name, mutate, intent))
    add("omitted-consumer", lambda x: x["consumer_rows"].pop(), "required row omitted")
    def duplicate(x: dict[str, Any]) -> None:
        x["consumer_rows"][1] = copy.deepcopy(x["consumer_rows"][0]); x["consumer_rows"][1]["run"].update({"id": 8999}); x["consumer_rows"][1]["job"].update({"id": 9999, "run_id": 8999, "check_run_id": 10999}); x["consumer_rows"][1]["check"].update({"id": 10999}); x["consumer_rows"][1]["producer"].update({"run_api": "https://api.github.com/repos/tailrocks/velnor/actions/runs/8999", "jobs_api": "https://api.github.com/repos/tailrocks/velnor/actions/runs/8999/attempts/1/jobs", "check_api": "https://api.github.com/repos/tailrocks/velnor/check-runs/10999", "source_record": {"run_id": 8999, "run_attempt": 1, "job_id": 9999, "check_run_id": 10999}})
    add("duplicate-consumer", duplicate, "duplicate identity with changed provenance")
    add("substitute-consumer", lambda x: x["consumer_rows"][1]["consumer"].update(copy.deepcopy(x["consumer_rows"][2]["consumer"])), "substituted required identity")
    add("extra-success-row", lambda x: x["consumer_rows"].append(copy.deepcopy(x["consumer_rows"][0])), "extra row")
    def extra_failed(x: dict[str, Any]) -> None:
        x["consumer_rows"].append(copy.deepcopy(x["consumer_rows"][0])); x["consumer_rows"][-1]["job"]["conclusion"] = "failure"
    add("extra-failed-row", extra_failed, "extra failed child row")
    add("failed-job", lambda x: x["consumer_rows"][0]["job"].update({"conclusion": "failure"}), "failed child")
    add("skipped-job", lambda x: x["consumer_rows"][0]["job"].update({"conclusion": "skipped"}), "skipped child")
    add("neutral-check", lambda x: x["consumer_rows"][0]["check"].update({"conclusion": "neutral"}), "neutral check")
    add("wrong-app", lambda x: (x["consumer_rows"][0]["consumer"].update({"check_app_id": APP_ID + 1}), x["consumer_rows"][0]["check"].update({"app_id": APP_ID + 1})), "wrong app")
    add("wrong-source", lambda x: (x["consumer_rows"][0]["source"].update({"repository": "attacker/repo", "path": ".github/workflows/evil.yml", "workflow_sha": "a" * 40, "blob_sha": "c" * 40}), x["consumer_rows"][0]["run"].update({"repository": "attacker/repo"})), "wrong source")
    add("wrong-source-ref", lambda x: x["consumer_rows"][0]["source"].update({"ref": "refs/heads/evil"}), "wrong source ref")
    add("wrong-event", lambda x: x["consumer_rows"][0]["run"].update({"event": "pull_request"}), "wrong event")
    add("wrong-ref", lambda x: x["consumer_rows"][0]["run"].update({"ref": "refs/heads/evil"}), "wrong ref")
    add("wrong-run-attempt", lambda x: (x["consumer_rows"][0]["run"].update({"attempt": 2}), x["consumer_rows"][0]["job"].update({"run_attempt": 2}), x["consumer_rows"][0]["producer"]["source_record"].update({"run_attempt": 2})), "wrong attempt")
    add("wrong-job-parent", lambda x: x["consumer_rows"][0]["job"].update({"run_id": x["consumer_rows"][1]["run"]["id"]}), "job parent mismatch")
    add("wrong-check-parent", lambda x: x["consumer_rows"][0]["check"].update({"id": x["consumer_rows"][1]["check"]["id"]}), "check parent mismatch")
    add("wrong-check-external-id", lambda x: x["consumer_rows"][0]["check"].update({"external_id": "substituted-result"}), "check identity mismatch")
    add("wrong-producer", lambda x: x["consumer_rows"][0]["producer"].update({"job": "attacker-job", "stage": "S7b_provider_result"}), "producer mismatch")
    add("child-job-absent", lambda x: x["consumer_rows"][0].pop("job"), "child job absent")
    add("child-check-absent", lambda x: x["consumer_rows"][0].pop("check"), "child check absent")
    add("child-step-absent", lambda x: x["consumer_rows"][0]["job"].update({"steps": []}), "child step absent")
    add("rows-digest-mismatch", lambda x: x.update({"rows_digest": "a" * 64}), "digest mismatch")
    add("excluded-identity-included", lambda x: x["consumer_rows"][0]["consumer"].update({"job_key": "Policy", "check_name": "Policy"}), "excluded identity included")
    add("source-blob-absent", lambda x: x["consumer_rows"][0]["source"].pop("blob_sha"), "source blob absent")
    case_results = []
    for index, (name, mutate, intent) in enumerate(cases, 1):
        item = copy.deepcopy(fixture); mutate(item); path = NEGATIVE / f"{index:02d}-{name}.json"; write_json(path, item)
        schema_failure, semantic_failure = errors(contract_schema, item), semantic(item, required, excluded)
        case_results.append({"case": name, "fixture": str(path.relative_to(OUT)), "intent": intent, "expected": "reject", "schema_observed": "reject" if schema_failure else "accept", "semantic_observed": "reject" if semantic_failure else "accept", "schema_errors": schema_failure[:5], "semantic_errors": semantic_failure[:12], "schema_gap": not schema_failure and bool(semantic_failure)})
    results = {"schema": "velnor.workflow-policy-validator.terminal-census-independent-test-results.v1", "status": "synthetic_contract_only_external_blocked", "authority_claim": False, "readiness_claim": False, "source_mutation": False, "authority_mutation": False, "inputs": {"repair_model_sha256": sha_file(MODEL_PATH), "repair_model_version": model["model_version"], "v12_freeze_manifest_sha256": sha_file(FREEZE_PATH), "v12_root_digest": load(FREEZE_PATH)["bundle"]["root_digest"], "current_verify_b_schema_digest": sha_value(model["canonical_schemas"]["verify_b"])}, "positive": {"strict_schema": "pass" if not positive_schema_errors else "fail", "semantic_contract": "pass" if not positive_semantic_errors else "fail", "schema_errors": positive_schema_errors, "semantic_errors": positive_semantic_errors}, "negative_summary": {"total": len(case_results), "schema_rejected": sum(i["schema_observed"] == "reject" for i in case_results), "semantic_rejected": sum(i["semantic_observed"] == "reject" for i in case_results), "schema_accepts_semantic_reject": [i["case"] for i in case_results if i["schema_gap"]]}, "cases": case_results, "required_consumer_count": len(required), "required_consumer_registry_sha256": sha_value(required), "recommendation": "Integrate terminal-census.schema.json through verify-B and run the semantic verifier; minItems=15 alone is insufficient."}
    write_json(OUT / "terminal-census-test-results.json", results)
    recommendations = {"schema": "velnor.workflow-policy-validator.terminal-census-schema-integration.v1", "status": "recommendation_only", "authority_claim": False, "current_gap": {"verify_b_terminal_census_rows_minItems": model["canonical_schemas"]["verify_b"]["properties"]["terminal_census_rows"]["minItems"], "verify_b_terminal_census_rows_maxItems": model["canonical_schemas"]["verify_b"]["properties"]["terminal_census_rows"].get("maxItems"), "verify_b_terminal_census_rows_uniqueItems": model["canonical_schemas"]["verify_b"]["properties"]["terminal_census_rows"].get("uniqueItems", False)}, "exact_integration": {"new_schema": "terminal-census.schema.json", "verify_b_pointer": "/properties/terminal_census_rows", "required_schema_changes": [{"json_pointer": "/properties/terminal_census_rows/maxItems", "value": 15}, {"json_pointer": "/properties/terminal_census_rows/uniqueItems", "value": True}, {"json_pointer": "/properties/terminal_census_rows/items", "value": "strict consumer/source/run/job/check/producer row"}, {"json_pointer": "/properties/terminal_census_rows/items/properties/job/properties/steps/minItems", "value": 1}, {"json_pointer": "/properties/terminal_census_rows/items/properties/job/properties/conclusion/const", "value": "success"}, {"json_pointer": "/properties/terminal_census_rows/items/properties/check/properties/conclusion/const", "value": "success"}], "verifier_must_enforce": ["exact set equality against the trusted 15-entry registry", "unique composite repository/workflow/run/attempt/job/check/external/app identity", "source Contents path/ref/repository/commit/blob equality", "run event/ref/head/repository/attempt equality", "job/check/step parent equality and child presence", "check name/external_id/app/head/status/conclusion equality", "verify-B stage/job/step/API/source-record lineage", "exact exclusion tuples, never names-only", "canonical rows digest recomputation", "reject omitted/duplicate/substituted/extra/failed/skipped/neutral/wrong-app/wrong-source rows"], "live_boundary": "All IDs, app and hashes here are synthetic; live verifier must read Actions/Contents/Checks APIs."}, "negative_contract_mapping": {item["case"]: item["intent"] for item in case_results}}
    write_json(OUT / "schema-integration-recommendations.json", recommendations)
    report = ["# Independent terminal-census fixture/test contract", "", "Synthetic external evidence only; no source, authority, GitHub, release, workflow, dispatch, runner, or credential mutation. Not readiness approval.", "", f"Positive: strict_schema={'pass' if not positive_schema_errors else 'fail'}; semantic_contract={'pass' if not positive_semantic_errors else 'fail'}; required consumers={len(required)}.", f"Negatives: {len(case_results)}; schema rejected={sum(i['schema_observed'] == 'reject' for i in case_results)}; semantic rejected={sum(i['semantic_observed'] == 'reject' for i in case_results)}; schema gaps={sum(i['schema_gap'] for i in case_results)}.", "", "Each row binds exact consumer workflow/job/check identity, source Contents provenance, run event/ref/attempt/head, attempt-scoped job/steps, Check Runs identity/app/status, and verify-B producer/API/source-record lineage. Exact identity-set equality is semantic; minItems is insufficient.", "", "## Cases", ""]
    report.extend(f"- `{i['case']}` — schema={i['schema_observed']}; semantic={i['semantic_observed']}; {i['intent']}" for i in case_results)
    report.extend(["", "## Integration", "", "Add maxItems=15, uniqueItems=true, strict source/run/job/check/producer objects and completed/success constants to verify-B. Then run the registry/equality verifier in the recommendations JSON. JSON Schema cannot enforce the exact required set or cross-object lineage alone.", "", f"Repair model SHA: `{sha_file(MODEL_PATH)}` ({model['model_version']})", f"V12 freeze SHA: `{sha_file(FREEZE_PATH)}`; root digest `{load(FREEZE_PATH)['bundle']['root_digest']}`", "", "No synthetic result is live evidence or an authority claim."])
    (OUT / "terminal-census-test-report.md").write_text("\n".join(report) + "\n")
    hashes = {"builder_sha256": sha_file(Path(__file__)), "schema_sha256": sha_file(OUT / "terminal-census.schema.json"), "contract_sha256": sha_file(OUT / "terminal-census-contract.json"), "positive_sha256": sha_file(OUT / "positive-terminal-census.json"), "results_sha256": sha_file(OUT / "terminal-census-test-results.json"), "recommendations_sha256": sha_file(OUT / "schema-integration-recommendations.json"), "report_sha256": sha_file(OUT / "terminal-census-test-report.md"), "negative_fixtures": {i["case"]: sha_file(OUT / i["fixture"]) for i in case_results}}
    write_json(OUT / "artifact-hashes.json", hashes)
    print(json.dumps({"out": str(OUT), "positive_schema": not positive_schema_errors, "positive_semantic": not positive_semantic_errors, "negative_total": len(case_results), "schema_rejected": sum(i["schema_observed"] == "reject" for i in case_results), "semantic_rejected": sum(i["semantic_observed"] == "reject" for i in case_results), "schema_gaps": [i["case"] for i in case_results if i["schema_gap"]], "hashes": hashes}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
