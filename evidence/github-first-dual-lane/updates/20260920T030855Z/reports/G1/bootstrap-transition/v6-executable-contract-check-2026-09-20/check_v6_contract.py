#!/usr/bin/env python3
"""Read-only structural regression check for the frozen v6 proposal.

This checks the v5 rejection classes against v6's machine-readable contract:
caller `uses` jobs do not declare outputs, the called DAG has typed lineage,
release and Actions-artifact IDs remain distinct, signing is acyclic, and
ruleset identities are provider-bound. It does not call GitHub, verify an
attestation cryptographically, or approve execution.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PLAN_MD = ROOT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v6.md"
PLAN_JSON = ROOT / "AUTHORITY-CHANGE-PLAN-2026-09-20-v6.json"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cycle(edges: list[list[str]]) -> list[str]:
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


def typed(tokens: list[str]) -> dict[str, str]:
    return {token.split(":", 1)[0]: token.split(":", 1)[1] for token in tokens if ":" in token}


def main() -> int:
    plan = json.loads(PLAN_JSON.read_text())
    publisher = plan["main_b_publisher"]
    jobs = publisher["jobs"]
    checks: list[dict[str, object]] = []

    def record(check_id: str, ok: bool, detail: object, severity: str = "high") -> None:
        checks.append({"id": check_id, "status": "pass" if ok else "fail", "severity": severity, "detail": detail})

    measured_md = sha256(PLAN_MD)
    declared_md = plan["plan_markdown"]["sha256"]
    record("P01-v6-paired-markdown-hash", measured_md == declared_md, {"measured": measured_md, "declared": declared_md})
    record("P02-current-main-binding", plan["revision_bound_facts"]["main_sha"] == "325719f1e05d3d46322c9fd3eeb9ad545e175638", plan["revision_bound_facts"]["main_sha"])
    record("P03-approval-state", plan["status"] == "approval_required" and plan["independent_approval"]["final_status"] == "not_approved", {"status": plan["status"], "review": plan["independent_approval"]["final_status"]})
    record("C01-workflow-call-only", publisher["event_guard"]["publisher_trigger"] == "workflow_call only" and not publisher["event_guard"]["standalone_push"] and not publisher["event_guard"]["standalone_workflow_dispatch"], publisher["event_guard"])
    caller = publisher["ci_main_graph"]["caller_job"]
    record("C02-uses-job-no-outputs", caller["outputs_declared"] is False and caller["shell_steps"] is False, caller)
    record("C03-direct-policy-needs", publisher["ci_main_graph"]["policy_job"]["needs"] == ["policy-validator-B"], publisher["ci_main_graph"]["policy_job"])
    edges = publisher["needs_edges"]
    record("G01-dag-acyclic", not cycle(edges), {"edges": edges, "cycle": cycle(edges)})

    output_maps = {name: typed(job.get("outputs", [])) for name, job in jobs.items()}
    lineage = []
    lineage_ok = True
    for consumer, producer in edges:
        consumed = typed(jobs[consumer].get("consumes", []))
        produced = output_maps[producer]
        missing = sorted(set(consumed) - set(produced))
        mismatch = sorted(name for name in set(consumed) & set(produced) if consumed[name] != produced[name])
        lineage.append({"consumer": consumer, "producer": producer, "missing": missing, "type_mismatch": mismatch})
        lineage_ok &= not missing and not mismatch
    record("G02-typed-edge-lineage", lineage_ok, lineage)
    call_outputs = set(typed(publisher["workflow_call_outputs"]))
    verify_outputs = set(output_maps["verify-B"])
    record("G03-called-output-provenance", call_outputs <= verify_outputs and not publisher["called_workflow_outputs_source"].startswith("caller"), {"missing": sorted(call_outputs - verify_outputs), "source": publisher["called_workflow_outputs_source"]})

    record("G04-no-reserve-binding-before-create", "binding_artifact_id:u64" not in jobs["reserve-release"]["outputs"], jobs["reserve-release"]["outputs"])
    record("G05-release-asset-distinct-namespace", "release_asset_id" in " ".join(publisher["transport_fields"]) and "release_asset_id" in publisher["field_provenance"], publisher["field_provenance"]["release_asset_id"])
    validation = publisher["validation_contract"]
    record("G06-no-digest-self-preimage", "no digest self-preimage or manifest/attestation future reference" in validation, validation)
    record("G07-verifier-self-check-excluded", "verify-B own job/check" in publisher["jobs"]["verify-B"]["upstream_terminal_census_excludes"], publisher["jobs"]["verify-B"]["upstream_terminal_census_excludes"])

    product = publisher["product"]
    record("S01-canonical-product-fields", product["features"] == "" and product["application_namespace_reuse"] is False, product)
    attestation = publisher["attestation_contract"]
    record("S02-both-release-and-binding-predicates", bool(attestation["binding_predicate_type"]) and bool(attestation["release_predicate_type"]) and "release_manifest_subject_sha256" in attestation["required_fields"], attestation)
    record("S03-oidc-certificate-policy-fields", all(field in attestation["required_fields"] for field in ["oidc_issuer", "certificate_identity", "certificate_verified", "oidc_policy_revision"]), attestation["required_fields"])

    contexts = plan["ruleset_transition"]["contexts"]
    contexts_ok = all(isinstance(item, dict) and set(["context", "integration_id"]) == set(item) for stage in contexts.values() for item in stage)
    record("R01-context-integration-binding", contexts_ok, contexts)
    coordinator = plan["ruleset_transition"]["coordinator"]
    record("R02-enforceable-fence-required", coordinator["all_mutations_through_proxy"] and "monotonic fencing token" in coordinator["type"], coordinator)
    record("R03-independent-recovery", plan["ruleset_transition"]["recovery"]["watchdog_independent"] and plan["ruleset_transition"]["recovery"]["postmerge"] == "no rollback; watchdog freezes and forward-completes staged transition", plan["ruleset_transition"]["recovery"])

    record("W01-supporting-closure", all(name in plan["active_workflows"] for name in ["nightly.yml", "ci-release-package-signer.yml"]), plan["active_workflows"])
    record("W02-native-required", plan["required_workloads"]["runtime_matrix"] == ["build[Linux-X64]", "build[Linux-ARM64]", "build[xcode-27]", "publish"], plan["required_workloads"]["runtime_matrix"])
    record("B01-target-remains-unresolved", plan["revision_bound_facts"]["target_generator_revision"] is None, plan["revision_bound_facts"]["target_generator_revision"])
    record("B02-live-verifier-remains-unresolved", plan["hard_blockers"][12]["id"] == "real_verifier" and plan["evidence"]["authority_contract_separation"]["real_verifier"] == "unimplemented", plan["evidence"]["authority_contract_separation"])

    failures = [item for item in checks if item["status"] == "fail"]
    result = {
        "schema": "velnor.authority-v6-executable-contract-check-results.v1",
        "status": "read-only-structural-regression",
        "gate_claim": False,
        "live_api_execution": False,
        "cryptographic_execution": False,
        "inputs": {"plan_markdown": str(PLAN_MD), "plan_markdown_sha256": measured_md, "plan_json": str(PLAN_JSON), "plan_json_sha256": sha256(PLAN_JSON), "v5_negative_report_sha256": "16259a66f4e4cd6df4da8dd80cdfb82e73022b44a4b8137f12c90866bdf01fbe", "v5_negative_results_sha256": "187e2c01766741cae0e8caf614c307beaadbbd7d88a7135757b3481d6840f4e1"},
        "checks": checks,
        "pass_count": len(checks) - len(failures),
        "fail_count": len(failures),
        "verdict": "structural-pass-with-execution-blockers" if not failures else "fail",
        "limitations": ["No GitHub API calls", "No real Actions run/job/artifact/release IDs", "No cryptographic attestation verification", "No target generator or B source revision"],
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
