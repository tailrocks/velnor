#!/usr/bin/env python3
"""Read-only v7 field-availability and provider-contract audit.

No GitHub API, release, authority, or cryptographic operation occurs here.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PLAN_MD = ROOT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v7.md"
PLAN_JSON = ROOT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v7.json"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tokens(values: list[str]) -> dict[str, str]:
    return {item.split(":", 1)[0]: item.split(":", 1)[1] for item in values if ":" in item}


def find_cycle(edges: list[list[str]]) -> list[str]:
    graph: dict[str, list[str]] = {}
    for child, parent in edges:
        graph.setdefault(child, []).append(parent)
        graph.setdefault(parent, [])
    active: set[str] = set()
    done: set[str] = set()

    def visit(node: str, trail: list[str]) -> list[str]:
        if node in active:
            return trail[trail.index(node) :] + [node]
        if node in done:
            return []
        active.add(node)
        for parent in graph[node]:
            found = visit(parent, trail + [parent])
            if found:
                return found
        active.remove(node)
        done.add(node)
        return []

    for node in graph:
        found = visit(node, [node])
        if found:
            return found
    return []


def main() -> int:
    plan = json.loads(PLAN_JSON.read_text())
    md = PLAN_MD.read_text()
    p = plan["main_b_publisher"]
    jobs = p["jobs"]
    checks: list[dict[str, object]] = []

    def check(check_id: str, ok: bool, detail: object, severity: str = "high") -> None:
        checks.append({"id": check_id, "status": "pass" if ok else "fail", "severity": severity, "detail": detail})

    check("P01-markdown-pair", sha256(PLAN_MD) == plan["plan_markdown"]["sha256"], {"measured": sha256(PLAN_MD), "declared": plan["plan_markdown"]["sha256"]})
    check("P02-current-main", plan["revision_bound_facts"]["main_sha"] == "325719f1e05d3d46322c9fd3eeb9ad545e175638", plan["revision_bound_facts"]["main_sha"])
    check("P03-not-approved", plan["status"] == "approval_required" and plan["independent_approval"]["final_status"] == "not_approved", {"status": plan["status"], "review": plan["independent_approval"]["final_status"]})

    check("C01-workflow-call-only", p["event_guard"]["publisher_trigger"] == "workflow_call only" and not p["event_guard"]["standalone_push"] and not p["event_guard"]["standalone_workflow_dispatch"], p["event_guard"])
    check("C02-local-uses-prefix", p["ci_main_graph"]["caller_job"]["uses"].startswith("./.github/workflows/"), p["ci_main_graph"]["caller_job"]["uses"])
    check("C03-no-caller-outputs", p["ci_main_graph"]["caller_job"]["outputs_declared"] is False and p["ci_main_graph"]["caller_job"]["shell_steps"] is False, p["ci_main_graph"]["caller_job"])
    check("C04-direct-policy-needs", p["ci_main_graph"]["policy_job"]["needs"] == ["policy-validator-B"], p["ci_main_graph"]["policy_job"])

    edges = p["needs_edges"]
    check("G01-dag-acyclic", not find_cycle(edges), {"edges": edges, "cycle": find_cycle(edges)})
    output_types = {name: tokens(job.get("outputs", [])) for name, job in jobs.items()}
    lineage: list[dict[str, object]] = []
    lineage_ok = True
    for consumer, producer in edges:
        consumed = tokens(jobs[consumer].get("consumes", []))
        produced = output_types[producer]
        missing = sorted(set(consumed) - set(produced))
        mismatch = sorted(name for name in set(consumed) & set(produced) if consumed[name] != produced[name])
        lineage.append({"consumer": consumer, "producer": producer, "missing": missing, "type_mismatch": mismatch})
        lineage_ok &= not missing and not mismatch
    check("G02-edge-field-availability", lineage_ok, lineage)

    stage_order = {stage["stage"]: index for index, stage in enumerate(p["field_availability_dag"])}
    stage_for: dict[str, int] = {}
    for stage in p["field_availability_dag"]:
        for field in stage["produces"]:
            if field in stage_for:
                check("G03-single-field-producer", False, {"field": field, "first": stage_for[field], "again": stage["stage"]})
            stage_for[field] = stage_order[stage["stage"]]
    job_stage = {"build-linux-x64": 1, "artifact-verify": 2, "reserve-release": 3, "attest": 4, "publish": 5, "record-upload": 6, "verify-B": 7}
    availability_failures = []
    for job_name, job in jobs.items():
        for field in tokens(job.get("consumes", [])):
            producer_stage = stage_for.get(field)
            if producer_stage is not None and producer_stage > job_stage[job_name]:
                availability_failures.append({"job": job_name, "field": field, "producer_stage": producer_stage, "consumer_stage": job_stage[job_name]})
    check("G04-no-later-stage-consumption", not availability_failures, availability_failures)

    required = p["attestation_contract"]["required_fields"]
    provenance = p["attestation_contract"]["required_field_provenance"]
    check("G05-all-attestation-fields-have-provenance", set(required) == set(provenance), {"missing": sorted(set(required) - set(provenance)), "extra": sorted(set(provenance) - set(required))})
    excluded = set(p["attestation_contract"]["own_id_digest_excluded"])
    check("G06-own-attestation-fields-excluded", {"binding_attestation_id", "binding_attestation_digest", "release_attestation_id", "release_attestation_digest"}.issubset(excluded), sorted(excluded))
    check("G07-canonical-release-predicate", p["attestation_contract"]["release_predicate_path"] == "attestations/velnor-policy-validator-release.v1.json", p["attestation_contract"]["release_predicate_path"])
    check("G08-canonical-record-producer", set(p["record_contract"]["required_outputs"]).issubset(output_types["record-upload"]), p["record_contract"])
    check("G09-record-is-not-generic-claim", p["record_contract"]["actual_producer_job"] == "record-upload numeric job id" and p["record_contract"]["actual_upload_step"] == "upload-record", p["record_contract"])
    check("G10-no-invented-rest-database-id", "database_id" not in json.dumps(p, sort_keys=True), "database_id absent from transport/publisher contract")
    check("G11-oidc-blob-distinct", "job_workflow_sha" in json.dumps(p["attestation_contract"]["trust"]) and "workflow_file_blob_sha" in json.dumps(p["attestation_contract"]["trust"]) and p["attestation_contract"]["trust"]["oidc_claims"] != p["attestation_contract"]["trust"]["workflow_file_blob_sha"], p["attestation_contract"]["trust"])

    check("S01-product-canonical", p["product"]["features"] == "" and p["product"]["application_namespace_reuse"] is False, p["product"])
    check("S02-actions-and-attestation-fields", p["attestation_contract"]["actions"]["upload_artifact_sha"] == "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" and p["attestation_contract"]["actions"]["attest_sha"] == "1e69f48acb82d1966a394da916b4c1698aa569d6", p["attestation_contract"]["actions"])
    check("S03-consumer-read-scopes", all(scope in jobs[name]["permissions"] for name in ["artifact-verify", "verify-B"] for scope in ["actions:read", "attestations:read"]), {name: jobs[name]["permissions"] for name in ["artifact-verify", "verify-B"]})
    action_scope_names = {"actions", "attestations", "checks", "contents", "id-token"}
    job_permission_failures = []
    for name, job in jobs.items():
        scopes = job.get("permissions", [])
        names = [scope.split(":", 1)[0] for scope in scopes]
        if len(names) != len(set(names)):
            job_permission_failures.append({"job": name, "reason": "duplicate Actions permission scope", "permissions": scopes})
        invalid = sorted(set(names) - action_scope_names)
        if invalid:
            job_permission_failures.append({"job": name, "reason": "invalid Actions permission scope", "scopes": invalid})
        if "metadata:read" in scopes:
            job_permission_failures.append({"job": name, "reason": "metadata is an App installation permission, not an Actions job scope"})
        if "attestations:read" in scopes and "attestations:write" in scopes:
            job_permission_failures.append({"job": name, "reason": "duplicate attestations read/write scope"})
    permission_model = p["event_guard"]["permissions_model"]
    check("S03b-actions-permission-model", not job_permission_failures and permission_model["metadata_read_location"] == "external_checks_app_installation_only", {"failures": job_permission_failures, "model": permission_model})
    check("S04-record-upload-write-isolated", jobs["record-upload"]["permissions"] == ["actions:write"] and "contents:write" not in jobs["record-upload"]["permissions"], jobs["record-upload"])
    check("S05-checks-app-separated", p["external_checks_app"]["github_token_impersonation"] is False and p["external_checks_app"]["status"] == "unproven external capability blocker", p["external_checks_app"])

    adoption = plan["tree_b_adoption"]
    check("T01-treeb-pr-late", adoption["pr_number"] == "created only after Main-B" and "TREE_B_PR_NUMBER" not in json.dumps(plan["tree_a_admission"], sort_keys=True), adoption)
    check("T02-adoption-no-self-hash", adoption["self_digest_field"] is False and "no self-digest field" in adoption["digest_preimage"], adoption)
    check("T03-policy-state-machine", plan["transition"]["tree_a"]["policy_state"].startswith("TREE_A_AUDIT") and plan["transition"]["main_b"]["policy_state"].startswith("MAIN_B_HANDOFF") and adoption["state_machine"] == ["TREE_A_AUDIT", "MAIN_B_HANDOFF", "TREE_B_FILE", "PERMANENT"], plan["transition"])

    rules = plan["ruleset_transition"]
    context_ok = all(isinstance(item, dict) and set(item) == {"context", "integration_id"} for state in rules["contexts"].values() for item in state)
    check("R01-context-provider-pairs", context_ok, rules["contexts"])
    check("R02-full-ruleset-snapshots", all(key in rules["snapshots"] for key in ["before", "tree_a", "main_b", "final"]) and rules["snapshots"]["each_reread"].startswith("full body"), rules["snapshots"])
    check("R03-admin-freeze-honest", rules["provider_admin_freeze"]["provider_enforces"] == "unproven" and rules["provider_admin_freeze"]["proxy_cannot_enforce_admin_direct_api"] is True and rules["provider_admin_freeze"]["blocker"] is True, rules["provider_admin_freeze"])
    check("R04-coordinator-not-claimed", rules["coordinator"]["status"].startswith("unproven") and rules["bypass_actor_5"].startswith("provider freeze unproven"), {"coordinator": rules["coordinator"], "bypass": rules["bypass_actor_5"]})

    command_tokens = ["RECORD_ARTIFACT_ID", "RECORD_ARTIFACT_DIGEST", "actions:read", "attestations:read", "POLICY_BOOTSTRAP_B_CHECK_ID", "checks:read"]
    check("W01-policy-download-tree", all(token in md for token in command_tokens), {token: token in md for token in command_tokens})
    check("W02-native-matrix", plan["required_workloads"]["runtime_matrix"] == ["build[Linux-X64]", "build[Linux-ARM64]", "build[xcode-27]", "publish"], plan["required_workloads"]["runtime_matrix"])
    check("W03-supporting-workflows", all(item in plan["active_workflows"] for item in ["nightly.yml", "ci-release-package-signer.yml"]), plan["active_workflows"])
    check("B01-target-unresolved", plan["revision_bound_facts"]["target_generator_revision"] is None, plan["revision_bound_facts"]["target_generator_revision"])
    check("B02-provider-unresolved", rules["provider_admin_freeze"]["blocker"] and p["external_checks_app"]["status"].startswith("unproven"), "execution remains blocked")

    failures = [item for item in checks if item["status"] == "fail"]
    result = {
        "schema": "velnor.authority-v7-field-availability-audit-results.v1",
        "status": "read-only-independent-structural-audit",
        "gate_claim": False,
        "live_api_execution": False,
        "cryptographic_execution": False,
        "inputs": {"plan_markdown": str(PLAN_MD), "plan_markdown_sha256": sha256(PLAN_MD), "plan_json": str(PLAN_JSON), "plan_json_sha256": sha256(PLAN_JSON), "v6_independent_report_sha256": "60e15e3a45c9c7d2739be0db30e7f642e0696567881806613a263f8283a0a7f0", "v6_independent_results_sha256": "0b889e2a567b8d0f87900cc888900abeb63693eb07a2a4aaf4b045969322c1879"},
        "checks": checks,
        "pass_count": len(checks) - len(failures),
        "fail_count": len(failures),
        "verdict": "structural-pass-with-external-blockers" if not failures else "fail",
        "limitations": ["No GitHub API calls", "No live source/output actionlint", "No real App credentials", "No cryptographic attestation verification", "No authority mutation"],
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
