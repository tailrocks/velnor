#!/usr/bin/env python3
"""Independent, read-only audit of the frozen v6 Main-B contract.

This is deliberately separate from v6-executable-contract-check-2026-09-20.
It consumes the frozen proposal, canonical A/B schemas, origin/main, and
hostile local fixtures. It never calls a GitHub mutation API, dispatches a
workflow, publishes a release, reads credentials, or claims G1 approval.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


TRANSITION = Path(__file__).resolve().parent.parent
AUDIT = TRANSITION / "v6-independent-contract-audit-2026-09-20"
REPO = TRANSITION.parent.parent.parent / "velnor3"
PLAN_MD = TRANSITION / "AUTHORITY-CHANGE-PLAN-2026-09-20-v6.md"
PLAN_JSON = TRANSITION / "AUTHORITY-CHANGE-PLAN-2026-09-20-v6.json"
FREEZE = TRANSITION / "AUTHORITY-CHANGE-PLAN-2026-09-20-v6-freeze-manifest.json"
V5_DIR = TRANSITION / "v5-executable-contract-check-2026-09-20"
V6_OWNER_DIR = TRANSITION / "v6-executable-contract-check-2026-09-20"
SCHEMA_DIR = TRANSITION / "authority-contract-separation-2026-09-20"
TEMP_SCHEMA = SCHEMA_DIR / "temporary-bootstrap-admission.schema.json"
PERM_SCHEMA = SCHEMA_DIR / "permanent-b-product-binding.schema.json"
TEMP_POSITIVE = SCHEMA_DIR / "temporary-admission-positive.json"
PERM_POSITIVE = SCHEMA_DIR / "permanent-b-binding-positive.json"
CANONICAL_MANIFEST = SCHEMA_DIR / "canonical-root-manifest.json"
PREVIOUS_ROOT = Path("/Users/donbeave/Projects/tailrocks/dual-lane-evidence")
ORIGIN_MAIN = "origin/main"
ACTIONLINT = Path("/Users/donbeave/.local/share/mise/installs/actionlint/1.7.12/actionlint")
FIXTURE_ROOT = AUDIT / "fixtures" / "actionlint" / "repo"

EXPECTED_V6_MD = "4aea2c28eb06f21e094adad1b4b0dfe8a8d6918d1cbe18c3a2befb77c849e593"
EXPECTED_V6_JSON = "556eb0485a18fe9abb91c132ff2a0cddd21e6eb3f549f66ebd90dbdf9da89947"
EXPECTED_MAIN = "325719f1e05d3d46322c9fd3eeb9ad545e175638"
EXPECTED_MAIN_PARENT = "e94b48406c4ed206fce2bbf39b788264e72cf39c"
EXPECTED_MAIN_TREE = "e9019f00578d34c3f339c7e8d55b66f7e53f8567"
EXPECTED_TEMP_SCHEMA = "7db213e1845657345e9c40b28902ffffa96d2de3429d594c31a2038b4f51aace"
EXPECTED_PERM_SCHEMA = "427cbae0d9a238ae5e22a1afac2c8f840e4a8ad136983ac28a863651e533aed7"
EXPECTED_V5_REPORT = "16259a66f4e4cd6df4da8dd80cdfb82e73022b44a4b8137f12c90866bdf01fbe"
EXPECTED_V5_RESULTS = "187e2c01766741cae0e8caf614c307beaadbbd7d88a7135757b3481d6840f4e1"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_cmd(args: list[str], cwd: Path | None = None) -> tuple[int, str, str]:
    proc = subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=False)
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def git_ref(ref: str) -> str | None:
    code, out, _ = run_cmd(["git", "rev-parse", "--verify", ref], REPO)
    return out if code == 0 else None


def git_tree(ref: str) -> str | None:
    code, out, _ = run_cmd(["git", "show", "-s", "--format=%T", ref], REPO)
    return out if code == 0 else None


def git_parent(ref: str) -> str | None:
    code, out, _ = run_cmd(["git", "show", "-s", "--format=%P", ref], REPO)
    return out.split()[0] if code == 0 and out else None


def git_path_exists(ref: str, path: str) -> bool:
    code, _, _ = run_cmd(["git", "cat-file", "-e", f"{ref}:{path}"], REPO)
    return code == 0


def typed(tokens: list[str]) -> dict[str, str]:
    result: dict[str, str] = {}
    for token in tokens:
        if ":" in token:
            name, value_type = token.split(":", 1)
            result[name] = value_type
    return result


def first_cycle(edges: list[list[str]]) -> list[str]:
    graph: dict[str, list[str]] = {}
    for child, parent in edges:
        graph.setdefault(child, []).append(parent)
        graph.setdefault(parent, [])
    active: set[str] = set()
    complete: set[str] = set()

    def visit(node: str, trail: list[str]) -> list[str]:
        if node in active:
            return trail[trail.index(node) :] + [node]
        if node in complete:
            return []
        active.add(node)
        for parent in graph[node]:
            found = visit(parent, trail + [parent])
            if found:
                return found
        active.remove(node)
        complete.add(node)
        return []

    for node in graph:
        found = visit(node, [node])
        if found:
            return found
    return []


def deep_get(value: dict[str, Any], path: str) -> Any:
    current: Any = value
    for part in path.split("."):
        if not isinstance(current, dict) or part not in current:
            return None
        current = current[part]
    return current


def schema_paths(schema: dict[str, Any]) -> set[str]:
    paths: set[str] = set()
    for group, definition in schema.get("properties", {}).items():
        paths.add(group)
        if isinstance(definition, dict):
            for field in definition.get("properties", {}):
                paths.add(f"{group}.{field}")
    return paths


def validate_document(schema: dict[str, Any], document: dict[str, Any]) -> list[str]:
    try:
        from jsonschema import Draft202012Validator
    except Exception as exc:  # pragma: no cover - environment evidence
        return [f"jsonschema unavailable: {exc}"]
    validator = Draft202012Validator(schema)
    return [error.message for error in sorted(validator.iter_errors(document), key=str)]


def actionlint(path: Path) -> dict[str, Any]:
    if not ACTIONLINT.exists():
        return {"available": False, "returncode": None, "stdout": "", "stderr": "binary absent"}
    relative = path if not path.is_absolute() else path.relative_to(FIXTURE_ROOT)
    code, stdout, stderr = run_cmd([str(ACTIONLINT), str(relative)], FIXTURE_ROOT)
    return {"available": True, "returncode": code, "stdout": stdout, "stderr": stderr}


def build_fixture_contract(pub: dict[str, Any], schema: dict[str, Any], positive: dict[str, Any]) -> list[dict[str, Any]]:
    jobs = pub["jobs"]
    fixtures: list[dict[str, Any]] = []

    def fixture(fid: str, attack: str, expected: str, evidence: str) -> None:
        fixtures.append({"id": fid, "attack": attack, "expected": expected, "evidence": evidence})

    fixture("N01-forged-run-id", "replace producer_run_id with an unrelated positive integer", "reject", "run/attempt/job/check must be API-bound, not names")
    fixture("N02-forged-run-attempt", "replace producer_run_attempt with a different positive integer", "reject", "attempt must equal the producer run API record")
    fixture("N03-forged-artifact-id", "substitute a positive Actions artifact ID from another run", "reject", "artifact ID must be fetched and compared to producer run")
    fixture("N04-artifact-name-only", "keep artifact name while replacing numeric artifact ID", "reject", "names are not identity")
    fixture("N05-release-asset-as-actions-artifact", "put release_asset_id in artifact_id", "reject", "release assets and Actions artifacts are disjoint namespaces")
    fixture("N06-service-zip-digest-mismatch", "replace service ZIP digest while retaining inner payload", "reject", "raw REST ZIP digest must equal signed subject")
    fixture("N07-inner-payload-mismatch", "replace inner payload digest while retaining service ZIP", "reject", "extracted payload is independently hashed")
    fixture("N08-binary-hash-mismatch", "replace binary digest while retaining payload digest", "reject", "binary digest is independently hashed")
    fixture("N09-source-sha-not-main", "replace source SHA with an unrelated commit", "reject", "source SHA must equal live refs/heads/main and caller SHA")
    fixture("N10-source-tree-closure-mismatch", "replace source tree or closure digest", "reject", "tree and closure are signed and independently recomputed")
    fixture("N11-called-workflow-ref-unbound", "omit job_workflow_ref/job_workflow_sha identity from the signed trust claim", "reject", "reusable workflow OIDC identity must not be a caller-name assertion")
    fixture("N12-producer-job-key-dropped", "drop producer_job_key after reserve-release", "reject", "typed role must remain paired with numeric IDs")
    fixture("N13-producer-workflow-dropped", "drop producer workflow path/SHA before publish", "reject", "signer/producer workflow must remain bound")
    fixture("N14-upload-step-dropped", "drop immutable artifact upload step ID", "reject", "artifact upload producer must be auditable")
    fixture("N15-binding-subject-inner-only", "attest extracted inner payload instead of raw service ZIP", "reject", "binding subject is raw immutable service ZIP")
    fixture("N16-forged-predicate-type", "replace binding or release predicate type", "reject", "predicate type is a strict constant")
    fixture("N17-forged-predicate-path", "replace release predicate path with an unrecognized path", "reject", "predicate path is schema-bound")
    fixture("N18-workflow-ref-name-self-claim", "use a workflow name/path without OIDC job_workflow_ref and blob SHA", "reject", "path strings are not trusted workflow identity")
    fixture("N19-release-id-before-draft", "sign release data before fresh draft reservation", "reject", "draft-first ordering prevents release-ID circularity")
    fixture("N20-caller-output-reexport", "add outputs to a reusable-workflow caller uses job", "reject", "caller uses jobs cannot declare outputs")
    fixture("N21-pseudo-workflow-call-needs", "add needs: [workflow_call publisher] as a caller job", "reject", "workflow_call is not a job ID")
    fixture("N22-undefined-attestation-bundle", "consume aggregate attestation_bundle_digest while only split digests are produced", "reject", "every consumed field needs a typed producer")
    fixture("N23-manifest-attestation-cycle", "include release attestation digest in the manifest signed by that attestation", "reject", "manifest/signature graph must be acyclic")
    fixture("N24-unknown-schema-attestation-field", "add attestation.release_asset_id to strict canonical B document", "reject", "additionalProperties=false must reject unmodeled trust fields")
    fixture("N25-unknown-schema-trust-field", "add trust.certificate_identity to strict canonical B document", "reject", "certificate identity needs an explicit schema revision")
    fixture("N26-missing-run-completion", "omit completed/successful run and job conclusions", "reject", "canonical B requires terminal completion")
    fixture("N27-forged-certificate", "set certificate_verified true without certificate identity and OIDC policy evidence", "reject", "boolean self-claims cannot establish signer trust")
    fixture("N28-forged-actions-digest", "replace upload action digest output with a caller-provided value", "reject", "REST metadata and upload output must compare")
    fixture("N29-fabricated-record-artifact", "invent policy_validator_b_record ID/SHA without an upload producer", "reject", "canonical record artifact needs a real upload and namespace")
    fixture("N30-temporary-key-to-B", "set temporary authority/key as permanent B trust", "reject", "temporary admission cannot mint permanent B authority")
    fixture("N31-raw-zip-inner-swap", "reuse service ZIP bytes with a different extracted payload/binary", "reject", "raw bytes, inner payload, and binary all bind")
    fixture("N32-publisher-event-alias", "use called workflow event workflow_call as canonical publisher push event", "reject", "caller push event and called workflow identity are distinct")
    fixture("N33-publish-output-asset-alias", "map published_asset_id to Actions artifact ID", "reject", "release asset REST ID is not Actions artifact ID")
    fixture("N34-absent-real-verifier", "claim hostile fixture rejection from unimplemented verifier", "reject", "structural fixture expectations are not execution evidence")
    return fixtures


def build_supplemental_fixture_contract() -> list[dict[str, str]]:
    return [
        {
            "id": "N35-binding-predicate-future-fields",
            "attack": "put release asset and final-manifest IDs/digests in the binding predicate before publish/manifest upload",
            "expected": "reject",
            "evidence": "field-stage dependency graph rejects values unavailable at binding-attestation creation",
        },
        {
            "id": "N36-binding-attestation-self-id",
            "attack": "put binding_attestation_id or binding_attestation_digest into the predicate signed by that attestation",
            "expected": "reject",
            "evidence": "attestation API ID/digest is an output, not an input preimage field",
        },
        {
            "id": "N37-adoption-file-self-hash",
            "attack": "hash exact adoption-file bytes while the hash field is inside those same bytes",
            "expected": "reject",
            "evidence": "self-referential adoption content cannot have a stable digest",
        },
        {
            "id": "N38-tree-b-pr-number-before-pr",
            "attack": "require tree_b_pr_number inside the commit before the PR that allocates the number exists",
            "expected": "reject",
            "evidence": "PR identity needs an external draft-first binding or a post-creation immutable update",
        },
        {
            "id": "N39-rest-database-id-alias",
            "attack": "invent caller_run_database_id/producer_job_database_id instead of binding real REST id fields",
            "expected": "reject",
            "evidence": "GitHub REST run/job records expose id; aliases need explicit typed derivation and namespace",
        },
        {
            "id": "N40-workflow-oidc-sha-blob-mismatch",
            "attack": "treat OIDC job_workflow_sha and downloaded workflow blob SHA as one unnamed field",
            "expected": "reject",
            "evidence": "distinct sources must be separately carried and compared",
        },
        {
            "id": "N41-verifier-app-credential-unbound",
            "attack": "accept checks/provider credentials by permission names without provider_app_id/integration_id/revision binding",
            "expected": "reject",
            "evidence": "permissions do not identify the trusted App/check integration",
        },
    ]


def main() -> int:
    plan = json.loads(PLAN_JSON.read_text())
    freeze = json.loads(FREEZE.read_text())
    pub = plan["main_b_publisher"]
    jobs = pub["jobs"]
    checks: list[dict[str, Any]] = []

    def record(check_id: str, ok: bool, detail: Any, *, severity: str = "high", false_status: str = "finding") -> None:
        checks.append({
            "id": check_id,
            "status": "pass" if ok else false_status,
            "severity": severity,
            "detail": detail,
        })

    # Frozen input and ownership/hash checks.
    md_hash = sha256(PLAN_MD)
    json_hash = sha256(PLAN_JSON)
    record("H01-v6-markdown-exact-hash", md_hash == EXPECTED_V6_MD and md_hash == freeze["plan"]["markdown_sha256"] and md_hash == plan["plan_markdown"]["sha256"], {"measured": md_hash, "expected": EXPECTED_V6_MD})
    record("H02-v6-json-exact-hash", json_hash == EXPECTED_V6_JSON and json_hash == freeze["plan"]["json_sha256"], {"measured": json_hash, "expected": EXPECTED_V6_JSON})
    record("H03-frozen-no-authority", freeze["status"] == "proposal-frozen-for-independent-review" and freeze["execution_authorized"] is False and freeze["mutation_performed"] is False and plan["execution_authorized"] is False and plan["mutation_performed"] is False, {"freeze_status": freeze["status"], "plan_status": plan["status"]})
    record("H04-origin-main-authoritative-revision", git_ref(ORIGIN_MAIN) == EXPECTED_MAIN and git_parent(ORIGIN_MAIN) == EXPECTED_MAIN_PARENT and git_tree(ORIGIN_MAIN) == EXPECTED_MAIN_TREE, {"ref": git_ref(ORIGIN_MAIN), "parent": git_parent(ORIGIN_MAIN), "tree": git_tree(ORIGIN_MAIN)})
    record("H05-proposed-workflow-absent-at-origin-main", not git_path_exists(ORIGIN_MAIN, pub["workflow_source"]) and not git_path_exists(ORIGIN_MAIN, pub["workflow_output"]), {"source": pub["workflow_source"], "output": pub["workflow_output"], "origin_main": EXPECTED_MAIN}, false_status="unimplemented")

    owner_paths = {
        "script": V6_OWNER_DIR / "check_v6_contract.py",
        "results": V6_OWNER_DIR / "results.json",
        "report": V6_OWNER_DIR / "REPORT.md",
    }
    owner_expected = {
        "script": freeze["regression_check"]["script_sha256"],
        "results": freeze["regression_check"]["results_sha256"],
        "report": freeze["regression_check"]["report_sha256"],
    }
    owner_measured = {key: sha256(path) for key, path in owner_paths.items()}
    record("H06-owner-bundle-integrity-not-authority", owner_measured == owner_expected, {"measured": owner_measured, "declared": owner_expected, "owner_result_is_authority": False}, severity="medium")
    record("H07-v5-negative-baseline-preserved", sha256(V5_DIR / "REPORT.md") == EXPECTED_V5_REPORT and sha256(V5_DIR / "v5-contract-check-results.json") == EXPECTED_V5_RESULTS, {"report": sha256(V5_DIR / "REPORT.md"), "results": sha256(V5_DIR / "v5-contract-check-results.json"), "status": "negative baseline preserved"}, severity="medium")
    stable_hashes = CANONICAL_MANIFEST.read_text()
    manifest_data = json.loads(stable_hashes)
    manifest_mismatch: dict[str, Any] = {}
    for relative, expected in manifest_data.get("stable_file_hashes", {}).items():
        path = Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence") / relative
        measured = sha256(path) if path.exists() else None
        if measured != expected:
            manifest_mismatch[relative] = {"measured": measured, "declared": expected}
    record("H08-canonical-root-manifest-measured", not manifest_mismatch, {"mismatches": manifest_mismatch, "manifest": str(CANONICAL_MANIFEST)}, severity="medium")
    record("H09-previous-handoff-root-absent", not PREVIOUS_ROOT.exists(), {"previous_handoff_root": str(PREVIOUS_ROOT), "status": manifest_data.get("previous_handoff_root_status")}, severity="medium")

    # Independent graph and typed output checks.
    edges = pub["needs_edges"]
    cycle = first_cycle(edges)
    record("G01-needs-dag-acyclic", not cycle, {"edges": edges, "cycle": cycle})
    output_maps = {name: typed(job.get("outputs", [])) for name, job in jobs.items()}
    edge_results: list[dict[str, Any]] = []
    edge_ok = True
    for consumer, producer in edges:
        consumed = typed(jobs[consumer].get("consumes", []))
        produced = output_maps.get(producer, {})
        missing = sorted(set(consumed) - set(produced))
        mismatches = sorted(name for name in set(consumed) & set(produced) if consumed[name] != produced[name])
        edge_ok &= not missing and not mismatches
        edge_results.append({"consumer": consumer, "producer": producer, "missing": missing, "type_mismatch": mismatches})
    record("G02-every-declared-edge-type-checks", edge_ok, edge_results)
    call_outputs = typed(pub["workflow_call_outputs"])
    verify_outputs = output_maps["verify-B"]
    record("G03-workflow-call-outputs-from-verify-B", set(call_outputs) <= set(verify_outputs) and pub["called_workflow_outputs_source"] == "verify-B outputs directly; no caller-job re-export", {"missing": sorted(set(call_outputs) - set(verify_outputs)), "source": pub["called_workflow_outputs_source"]})
    policy_consumes = typed(pub["ci_main_graph"]["policy_job"]["consumes"])
    record("G04-policy-consumes-declared-called-outputs", set(policy_consumes) == set(call_outputs), {"only_call_outputs": sorted(set(call_outputs) - set(policy_consumes)), "only_policy_inputs": sorted(set(policy_consumes) - set(call_outputs))})
    caller = pub["ci_main_graph"]["caller_job"]
    record("G05-caller-uses-job-has-no-outputs-or-shell", caller["outputs_declared"] is False and caller["shell_steps"] is False and caller["uses"] == pub["workflow_output"], caller)
    record("G06-policy-needs-real-caller-job", pub["ci_main_graph"]["policy_job"]["needs"] == [caller["id"]] and "workflow_call" not in pub["ci_main_graph"]["policy_job"]["needs"], pub["ci_main_graph"]["policy_job"])

    # Per-job permissions and caller union are checked against the machine contract.
    expected_permissions = {
        "build-linux-x64": ["contents:read", "actions:read"],
        "artifact-verify": ["contents:read", "actions:read"],
        "reserve-release": ["contents:write"],
        "attest": ["contents:read", "actions:read", "id-token:write", "attestations:write"],
        "publish": ["contents:write"],
        "verify-B": ["metadata:read", "contents:read", "actions:read", "checks:read/write"],
    }
    permission_results = {name: {"declared": job.get("permissions", []), "expected": expected_permissions[name], "forbidden": job.get("forbidden", [])} for name, job in jobs.items()}
    permissions_ok = all(set(item["declared"]) == set(item["expected"]) for item in permission_results.values())
    record("P01-per-job-permissions-machine-contract", permissions_ok, permission_results)
    valid_action_permission_scopes = {"actions", "artifact-metadata", "attestations", "checks", "contents", "deployments", "discussions", "id-token", "issues", "models", "packages", "pages", "pull-requests", "repository-projects", "security-events", "statuses"}
    declared_permission_scopes = {item.split(":", 1)[0] for item in pub["caller_permissions"]}
    record("P02-actions-permission-scopes-valid", declared_permission_scopes <= valid_action_permission_scopes, {"invalid_scopes": sorted(declared_permission_scopes - valid_action_permission_scopes), "declared": pub["caller_permissions"]}, false_status="finding")
    record("P03-caller-permission-union-explicit", set(pub["caller_permissions"]) == {"metadata:read", "actions:read", "contents:write", "checks:write", "id-token:write", "attestations:write"}, {"declared": pub["caller_permissions"], "generated_yaml_proof": False}, false_status="unimplemented")

    # Semantic lineage and namespace checks. Exact edge lineage can pass while
    # a trust-bearing value disappears from the signed record; these checks are
    # intentionally stricter than the owner 23-assertion script.
    required = set(pub["attestation_contract"]["required_fields"])
    attest_outputs = output_maps["attest"]
    publish_outputs = output_maps["publish"]
    verify_consumes = typed(jobs["verify-B"].get("consumes", []))
    field_provenance = pub["field_provenance"]
    missing_attest_transport = {
        "actions_artifact_name": "artifact_name only exists through artifact-verify; attest has no typed artifact name input/output",
        "service_zip_digest": "verified service ZIP digest is dropped by reserve-release and absent from attest outputs",
        "service_zip_size": "verified service ZIP size is dropped by reserve-release and absent from attest outputs",
        "inner_payload_digest": "verified inner payload digest is dropped by reserve-release and absent from attest outputs",
        "inner_payload_size": "verified inner payload size is dropped by reserve-release and absent from attest outputs",
        "binary_digest": "verified binary digest is dropped by reserve-release and absent from attest outputs",
        "binary_size": "verified binary size is dropped by reserve-release and absent from attest outputs",
        "binary_architecture": "verified binary architecture is dropped by reserve-release and absent from attest outputs",
        "producer_job_key": "reserve-release drops producer_job_key; attest cannot sign it",
    }
    record("L01-signed-binding-fields-survive-to-attest", not missing_attest_transport, {"missing": missing_attest_transport, "attest_outputs": sorted(attest_outputs)}, false_status="finding")
    missing_publish_transport = {
        "producer_workflow_path": "publish outputs omit producer workflow path",
        "producer_workflow_sha": "publish outputs omit producer workflow SHA",
        "upload_step_id": "publish outputs omit immutable upload step ID",
        "producer_job_key": "publish outputs omit producer job key",
        "actions_artifact_name": "publish outputs omit Actions artifact name",
    }
    record("L02-trust-fields-survive-to-verify-B", False, {"missing": missing_publish_transport, "publish_outputs": sorted(publish_outputs)}, false_status="finding")
    record("L03-verify-B-has-raw-actions-artifact-derivation", any("raw" in item.lower() and "artifact" in item.lower() for item in jobs["verify-B"].get("consumes", [])), {"verify_B_consumes": jobs["verify-B"].get("consumes", []), "field_provenance": field_provenance}, false_status="finding")
    provenance_missing = sorted(required - set(field_provenance))
    record("L04-required-attestation-fields-have-provenance-entries", not provenance_missing, {"missing": provenance_missing, "provenance_keys": sorted(field_provenance)}, false_status="finding")
    record("L05-producer-workflow-identity-is-typed", "producer_workflow_path" in field_provenance and "producer_workflow_sha" in field_provenance, {"producer_fields": [field for field in required if "producer_workflow" in field], "field_provenance": field_provenance}, false_status="finding")
    record("L06-actions-artifact-name-is-typed-through-signing", "artifact_name:string" in jobs["attest"].get("consumes", []) or "actions_artifact_name" in field_provenance, {"attest_consumes": jobs["attest"].get("consumes", []), "field_provenance": field_provenance}, false_status="finding")
    record("L07-record-artifact-has-upload-producer", "policy_validator_b_record" in field_provenance or any("record" in token.lower() and "artifact" in token.lower() for token in jobs["verify-B"].get("consumes", [])), {"workflow_call_output": "policy_validator_b_record:canonical-json-artifact-id-and-sha256", "producer": "not explicitly declared"}, false_status="finding")
    record("L08-called-workflow-OIDC-claims-bound", "job_workflow_ref" in " ".join(pub["attestation_contract"]["required_fields"]) and "job_workflow_sha" in " ".join(pub["attestation_contract"]["required_fields"]), {"required_fields": pub["attestation_contract"]["required_fields"], "called_workflow_sha_provenance": field_provenance.get("called_workflow_sha")}, false_status="finding")
    record("L09-actions-and-release-asset-namespaces-distinct", "release_asset_id" in field_provenance and "never Actions artifact" in field_provenance["release_asset_id"] and "artifact_id" in field_provenance, {"release_asset": field_provenance.get("release_asset_id"), "artifact": field_provenance.get("artifact_id")})
    record("L10-canonical-record-ID-namespace-explicit", "canonical-json-artifact-id-and-sha256" not in pub["workflow_call_outputs"][0], {"output": pub["workflow_call_outputs"][0]}, false_status="finding")
    md_json_divergence = {
        "markdown_published_asset_token": "published_asset_id" in PLAN_MD.read_text(),
        "json_published_asset_token": "published_asset_id" in PLAN_JSON.read_text(),
        "markdown_caller_workflow_fields": "caller_workflow_path" in PLAN_MD.read_text() and "caller_workflow_sha" in PLAN_MD.read_text(),
        "json_workflow_call_caller_fields": "caller_workflow_path:string" in pub["workflow_call_outputs"] or "caller_workflow_sha:sha1" in pub["workflow_call_outputs"],
        "markdown_aggregate_attestation": "attestation_bundle_digest" in PLAN_MD.read_text(),
        "json_aggregate_attestation": "attestation_bundle_digest:sha256" in pub["workflow_call_outputs"],
    }
    record("L11-markdown-and-machine-transport-contract-agree", md_json_divergence["markdown_published_asset_token"] == md_json_divergence["json_published_asset_token"] and md_json_divergence["markdown_caller_workflow_fields"] == md_json_divergence["json_workflow_call_caller_fields"] and md_json_divergence["markdown_aggregate_attestation"] == md_json_divergence["json_aggregate_attestation"], md_json_divergence, false_status="finding")

    # Acyclic draft-first digest/signature ordering and run completion.
    validation = pub["validation_contract"]
    protocol = pub["release_protocol"]
    digest_order = {
        "binding_before_manifest": protocol.index("create binding after real release/producer IDs") < protocol.index("create/upload final manifest"),
        "manifest_before_release_attestation": protocol.index("create/upload final manifest") < protocol.index("attest release manifest subject"),
        "publish_after_release_attestation": protocol.index("attest release manifest subject") < protocol.index("publish exactly once"),
        "no_self_preimage_contract": "no digest self-preimage or manifest/attestation future reference" in validation,
        "binding_excludes_later_fields": "excluding binding_digest and later manifest/attestation fields" in pub["field_provenance"]["binding_digest"],
    }
    record("D01-draft-first-acyclic-signing-order", all(digest_order.values()), digest_order)
    record("D02-binding-and-release-predicates-both-required", bool(pub["attestation_contract"]["binding_predicate_type"]) and bool(pub["attestation_contract"]["release_predicate_type"]) and "release_manifest_subject_sha256" in required, pub["attestation_contract"])
    record("D03-verify-B-excludes-own-census", "verify-B own job/check" in jobs["verify-B"]["upstream_terminal_census_excludes"] and "Policy-bootstrap-B check" in jobs["verify-B"]["upstream_terminal_census_excludes"], jobs["verify-B"]["upstream_terminal_census_excludes"])
    completion_fields = {"status", "conclusion"}
    record("D04-run-and-job-terminal-completion-is-signed", completion_fields <= required or any(field in " ".join(pub["attestation_contract"]["required_fields"]) for field in ["run_status", "run_conclusion", "job_status", "job_conclusion"]), {"required_fields": pub["attestation_contract"]["required_fields"], "canonical_B_requires": ["run.status", "run.conclusion", "job.status", "job.conclusion"]}, false_status="finding")
    record("D05-called-job-check-and-attempt-outputs-present", all(name in call_outputs for name in ["called_producer_run_id", "called_producer_run_attempt", "called_producer_job_database_id", "called_producer_check_id"]), {"call_outputs": sorted(call_outputs)})
    record("D06-called-producer-job-key-survives", "producer_job_key" in " ".join(pub["workflow_call_outputs"]) or "producer_job_key" in output_maps["verify-B"], {"workflow_call_outputs": pub["workflow_call_outputs"], "verify_B_outputs": jobs["verify-B"]["outputs"]}, false_status="finding")
    # Model actual availability at each preimage/attestation stage. The v6
    # list says both predicates carry all fields, but several are created only
    # after the binding predicate must already be signed.
    field_stage = {
        "binding_attestation_id": "binding-attestation-output",
        "binding_attestation_digest": "binding-attestation-output",
        "release_manifest_artifact_id": "manifest-upload-output",
        "release_manifest_digest": "manifest-upload-output",
        "release_manifest_subject_sha256": "manifest-upload-output",
        "release_asset_id": "publish-output",
        "release_asset_digest": "publish-output",
    }
    binding_future_fields = sorted(field for field in field_stage if field in required)
    record("D07-binding-predicate-has-no-future-or-own-output-fields", not binding_future_fields, {"binding_predicate_stage": "before binding attestation", "future_or_own_fields": binding_future_fields, "required_fields": sorted(required)}, false_status="finding")
    adoption_text = PLAN_MD.read_text()
    record("D08-adoption-file-hash-is-not-self-referential", '"adoption_file_sha256": "sha256:<canonical exact bytes of this file>"' not in adoption_text, {"declared": "adoption_file_sha256 = sha256(canonical exact bytes of this file)", "adoption_file": ".github/ci/validator-pin-adoption.json"}, false_status="finding")
    record("D09-tree-b-pr-number-has-draft-first-creation-sequence", False, {"tree_b_pr_number": "<positive-integer>", "plan_order": "Tree-B PR adds adoption file; PR number is required inside that file", "required_fix": "external PR identity binding or explicit post-creation immutable revision"}, false_status="finding")
    database_tokens = sorted(token for token in pub["workflow_call_outputs"] + pub["transport_fields"] if "database_id" in token)
    record("L12-rest-database-id-fields-have-real-API-derivation", False, {"declared_aliases": database_tokens, "field_provenance_keys": sorted(field_provenance), "canonical_rest_shape": "run/job/check REST records expose concrete id fields; no database_id alias is defined"}, false_status="finding")
    workflow_sha_provenance = field_provenance.get("called_workflow_sha", "")
    record("L13-oidc-workflow-sha-and-blob-sha-are-separate-typed-fields", False, {"provenance": workflow_sha_provenance, "required": ["oidc job_workflow_sha", "downloaded workflow blob SHA", "explicit equality/relationship check"]}, false_status="finding")
    record("P04-verifier-App-check-credential-transport-bound", all(field in required for field in ["provider_app_id", "integration_id", "verifier_revision"]), {"missing_from_v6_attestation_required_fields": ["provider_app_id", "integration_id", "verifier_revision"], "verify_B_permissions": jobs["verify-B"].get("permissions", []), "canonical_verifier_fields": ["provider_app_id", "integration_id", "verifier_revision"]}, false_status="finding")

    # Canonical schema mapping and strict hostile mutations.
    temp_schema = json.loads(TEMP_SCHEMA.read_text())
    perm_schema = json.loads(PERM_SCHEMA.read_text())
    temp_positive = json.loads(TEMP_POSITIVE.read_text())["document"]
    perm_positive_wrapper = json.loads(PERM_POSITIVE.read_text())
    perm_positive = perm_positive_wrapper["document"]
    temp_errors = validate_document(temp_schema, temp_positive)
    perm_errors = validate_document(perm_schema, perm_positive)
    record("S01-canonical-temporary-positive-validates", not temp_errors, {"errors": temp_errors})
    record("S02-canonical-permanent-positive-validates", not perm_errors, {"errors": perm_errors})
    canonical_paths = schema_paths(perm_schema)
    mapping = {
        "repository_id": ["source.repository_id", "publisher.repository_id", "trust.repository_id", "run.repository_id"],
        "source_ref": ["source.ref", "trust.source_ref", "publisher.ref"],
        "source_sha": ["source.sha", "trust.source_sha"],
        "source_tree": ["source.tree_sha"],
        "closure_digest": ["source.closure"],
        "caller_workflow_path": [],
        "caller_workflow_sha": [],
        "caller_run_id": ["run.id"],
        "caller_run_attempt": ["run.attempt"],
        "caller_run_database_id": ["run.database_id"],
        "called_workflow_path": ["publisher.path"],
        "called_workflow_sha": ["publisher.sha", "run.workflow_sha"],
        "producer_workflow_path": [],
        "producer_workflow_sha": [],
        "producer_run_id": ["job.run_id", "artifact.workflow_run_id"],
        "producer_run_attempt": ["job.run_attempt"],
        "producer_job_key": ["job.key"],
        "producer_job_database_id": ["job.id"],
        "producer_check_id": ["job.check_run_id"],
        "upload_step_id": ["upload.step_id"],
        "actions_artifact_id": ["artifact.id", "upload.artifact_id_output"],
        "actions_artifact_name": ["artifact.name"],
        "actions_artifact_digest": ["upload.artifact_digest_output"],
        "service_zip_digest": ["artifact.rest_service_zip_sha256", "artifact.service_zip_sha256"],
        "service_zip_size": ["artifact.rest_service_zip_size", "artifact.service_zip_size"],
        "inner_payload_digest": ["artifact.inner_payload_sha256"],
        "inner_payload_size": ["artifact.inner_payload_size"],
        "binary_digest": ["binary.sha256"],
        "binary_size": ["binary.size"],
        "binary_architecture": ["binary.architecture"],
        "release_id": ["release.id"],
        "release_tag": ["release.tag"],
        "release_target": ["release.target_sha"],
        "release_asset_id": [],
        "release_asset_digest": [],
        "binding_attestation_id": [],
        "binding_attestation_digest": [],
        "binding_predicate_type": ["attestation.binding_predicate_type"],
        "binding_predicate_path": ["attestation.binding_predicate_path"],
        "binding_subject_name": ["attestation.binding_subject_name"],
        "binding_subject_digest": ["attestation.binding_subject_sha256"],
        "release_manifest_artifact_id": [],
        "release_manifest_digest": ["manifest.sha256"],
        "release_manifest_subject_sha256": ["attestation.release_manifest_subject_sha256"],
        "release_predicate_type": ["attestation.release_predicate_type"],
        "release_predicate_path": ["attestation.release_predicate_path"],
        "signer_repository": ["attestation.signer_repository", "trust.signer_repository"],
        "signer_workflow": ["attestation.signer_workflow", "trust.signer_workflow"],
        "signer_source_ref": ["attestation.signer_source_ref"],
        "oidc_issuer": ["trust.oidc_issuer"],
        "certificate_identity": [],
        "certificate_verified": ["attestation.certificate_verified"],
        "oidc_policy_revision": ["trust.oidc_policy_revision_sha256"],
        "required_step_conclusions": ["steps.build", "steps.upload", "steps.reserve_release", "steps.attest_binding", "steps.attest_release", "steps.publish"],
    }
    mapping_missing = {field: paths for field, paths in mapping.items() if not any(path in canonical_paths for path in paths)}
    mapping_ambiguous = {field: paths for field, paths in mapping.items() if len([path for path in paths if path in canonical_paths]) > 1}
    record("S03-full-attestation-field-mapping", not mapping_missing, {"missing": mapping_missing, "ambiguous": mapping_ambiguous, "canonical_paths": sorted(canonical_paths)}, false_status="finding")
    canonical_release_path = deep_get(perm_schema, "properties.attestation.properties.release_predicate_path.const")
    proposed_release_path = pub["attestation_contract"]["release_predicate_path"]
    record("S04-release-predicate-path-matches-canonical-schema", proposed_release_path == canonical_release_path, {"proposed": proposed_release_path, "canonical": canonical_release_path}, false_status="finding")
    record("S05-binding-predicate-path-matches-canonical-schema", pub["attestation_contract"]["binding_predicate_path"] == deep_get(perm_schema, "properties.attestation.properties.binding_predicate_path.const"), {"proposed": pub["attestation_contract"]["binding_predicate_path"], "canonical": deep_get(perm_schema, "properties.attestation.properties.binding_predicate_path.const")})
    mutated_unknown_attestation = copy.deepcopy(perm_positive)
    mutated_unknown_attestation["attestation"]["release_asset_id"] = 90002
    unknown_attestation_errors = validate_document(perm_schema, mutated_unknown_attestation)
    record("S06-strict-schema-rejects-unmodeled-release-asset-field", bool(unknown_attestation_errors), {"errors": unknown_attestation_errors})
    mutated_unknown_trust = copy.deepcopy(perm_positive)
    mutated_unknown_trust["trust"]["certificate_identity"] = "forged"
    unknown_trust_errors = validate_document(perm_schema, mutated_unknown_trust)
    record("S07-strict-schema-rejects-unmodeled-certificate-identity", bool(unknown_trust_errors), {"errors": unknown_trust_errors})
    mutated_v6_release_path = copy.deepcopy(perm_positive)
    mutated_v6_release_path["attestation"]["release_predicate_path"] = proposed_release_path
    v6_release_path_errors = validate_document(perm_schema, mutated_v6_release_path)
    record("S08-schema-rejects-v6-release-path-until-revised", bool(v6_release_path_errors), {"errors": v6_release_path_errors})
    declared_action_contract_fields = {"upload_artifact_sha", "attest_sha", "artifact_metadata_write", "push_to_registry", "create_storage_record"}
    machine_action_fields = {key for key in declared_action_contract_fields if key in plan}
    record("S09-action-attest-storage-contract-machine-typed", machine_action_fields == declared_action_contract_fields, {"machine_fields_found": sorted(machine_action_fields), "required_fields": sorted(declared_action_contract_fields), "canonical_schema_values": {"upload_artifact_sha": perm_schema["properties"]["actions"]["properties"]["upload_artifact_sha"].get("const"), "attest_sha": perm_schema["properties"]["actions"]["properties"]["attest_sha"].get("const"), "create_storage_record": perm_schema["properties"]["actions"]["properties"]["create_storage_record"].get("const")}}, false_status="finding")

    # Real YAML is absent at the authoritative revision; actionlint fixtures
    # prove only the syntax boundary and cannot substitute for generated YAML.
    valid_result = actionlint(Path(".github/workflows/caller-valid.yml"))
    invalid_outputs_result = actionlint(Path(".github/workflows/caller-invalid-uses-outputs.yml"))
    invalid_path_result = actionlint(Path(".github/workflows/caller-invalid-uses-path.yml"))
    record("Y01-actionlint-valid-reusable-caller-fixture", valid_result["available"] and valid_result["returncode"] == 0, valid_result, severity="medium", false_status="unimplemented")
    record("Y02-actionlint-rejects-caller-uses-outputs", invalid_outputs_result["available"] and invalid_outputs_result["returncode"] != 0, invalid_outputs_result, severity="medium", false_status="finding")
    record("Y03-actionlint-rejects-invalid-reusable-path", invalid_path_result["available"] and invalid_path_result["returncode"] != 0, invalid_path_result, severity="medium", false_status="finding")
    record("Y04-real-v6-source-and-output-actionlinted", False, {"source": pub["workflow_source"], "output": pub["workflow_output"], "origin_main": EXPECTED_MAIN, "reason": "both paths absent at authoritative origin/main"}, severity="high", false_status="unimplemented")
    record("Y05-local-reusable-workflow-path-has-required-prefix", caller["uses"].startswith("./.github/workflows/"), {"declared_uses": caller["uses"], "required_form": "./.github/workflows/file.yml"}, false_status="finding")
    declared_pins = {
        "upload_artifact": "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        "attest": "1e69f48acb82d1966a394da916b4c1698aa569d6",
        "source": "v6 Markdown declaration only; not independently verified as generated source",
    }
    record("Y06-declared-action-pins-are-not-live-source-proof", False, declared_pins, severity="medium", false_status="unimplemented")

    fixtures = build_fixture_contract(pub, perm_schema, perm_positive_wrapper)
    fixture_bytes = (json.dumps({"schema": "velnor.v6-independent-negative-fixtures.v1", "execution": "contract-only-unexecuted", "real_verifier": "unimplemented", "fixtures": fixtures}, indent=2, sort_keys=True) + "\n").encode()
    fixture_path = AUDIT / "fixtures" / "v6-negative-fixtures.json"
    fixture_path.write_bytes(fixture_bytes)
    fixture_hash = hashlib.sha256(fixture_bytes).hexdigest()
    record("F01-hostile-fixture-contract-complete", len(fixtures) == 34 and all(item["expected"] == "reject" for item in fixtures), {"count": len(fixtures), "fixture_sha256": fixture_hash, "real_verifier": "unimplemented"}, severity="medium", false_status="unimplemented")
    supplemental_fixtures = build_supplemental_fixture_contract()
    supplemental_bytes = (json.dumps({"schema": "velnor.v6-independent-supplemental-negative-fixtures.v1", "execution": "contract-only-unexecuted", "real_verifier": "unimplemented", "fixtures": supplemental_fixtures}, indent=2, sort_keys=True) + "\n").encode()
    supplemental_path = AUDIT / "fixtures" / "v6-supplemental-negative-fixtures.json"
    supplemental_path.write_bytes(supplemental_bytes)
    supplemental_hash = hashlib.sha256(supplemental_bytes).hexdigest()
    record("F02-supplemental-security-fixture-contract-complete", len(supplemental_fixtures) == 7 and all(item["expected"] == "reject" for item in supplemental_fixtures), {"count": len(supplemental_fixtures), "fixture_sha256": supplemental_hash, "real_verifier": "unimplemented"}, severity="medium", false_status="unimplemented")

    failures = [item for item in checks if item["status"] != "pass"]
    status_counts = {status: sum(1 for item in checks if item["status"] == status) for status in ["pass", "finding", "unimplemented"]}
    result = {
        "schema": "velnor.authority-v6-independent-contract-audit-results.v1",
        "status": "read-only-independent-audit",
        "gate_claim": False,
        "live_api_execution": False,
        "cryptographic_execution": False,
        "real_verifier": "unimplemented",
        "owner_result_not_authority": True,
        "inputs": {
            "plan_markdown": str(PLAN_MD),
            "plan_markdown_sha256": md_hash,
            "plan_json": str(PLAN_JSON),
            "plan_json_sha256": json_hash,
            "freeze_manifest": str(FREEZE),
            "freeze_manifest_sha256": sha256(FREEZE),
            "origin_main": EXPECTED_MAIN,
            "origin_main_tree": EXPECTED_MAIN_TREE,
            "temporary_schema_sha256": sha256(TEMP_SCHEMA),
            "permanent_schema_sha256": sha256(PERM_SCHEMA),
            "canonical_root_manifest_sha256": sha256(CANONICAL_MANIFEST),
            "v5_negative_report_sha256": sha256(V5_DIR / "REPORT.md"),
            "v5_negative_results_sha256": sha256(V5_DIR / "v5-contract-check-results.json"),
            "owner_v6_results_sha256": sha256(V6_OWNER_DIR / "results.json"),
            "fixture_contract_sha256": fixture_hash,
            "supplemental_fixture_contract_sha256": supplemental_hash,
        },
        "checks": checks,
        "check_count": len(checks),
        "pass_count": status_counts["pass"],
        "finding_count": status_counts["finding"],
        "unimplemented_count": status_counts["unimplemented"],
        "verdict": "findings-and-unimplemented-no-approval" if failures else "structural-pass-with-execution-blockers",
        "limitations": [
            "No GitHub API calls or live run/job/artifact/release IDs",
            "No cryptographic attestation verification",
            "No generated v6 source/output at authoritative origin/main",
            "Canonical schema is existing v3 contract and has not been revised for v6 fields",
            "Hostile fixtures are contract descriptors, not verifier execution results",
            "Declared action pins are proposal text, not live generated-source evidence",
        ],
    }
    results_bytes = (json.dumps(result, indent=2, sort_keys=True) + "\n").encode()
    results_path = AUDIT / "results.json"
    results_path.write_bytes(results_bytes)
    results_hash = hashlib.sha256(results_bytes).hexdigest()

    finding_lines = []
    for item in checks:
        if item["status"] != "pass":
            detail = json.dumps(item["detail"], sort_keys=True, separators=(",", ":"))
            finding_lines.append(f"- `{item['id']}` ({item['status']}, {item['severity']}): {detail}")
    report = f"""# Frozen v6 independent contract audit — 2026-09-20

Status: **read-only independent audit; no live authority, API mutation, release,
dispatch, or cryptographic execution**.

Gate claim: **false**. Real verifier: **unimplemented**. Owner v6 result is
historical structural evidence only, not authority.

## Frozen input hashes

- v6 Markdown: `{PLAN_MD}` — `{md_hash}`
- v6 JSON: `{PLAN_JSON}` — `{json_hash}`
- v6 freeze manifest: `{FREEZE}` — `{sha256(FREEZE)}`
- authoritative `origin/main`: `{EXPECTED_MAIN}`; parent `{EXPECTED_MAIN_PARENT}`; tree `{EXPECTED_MAIN_TREE}`
- temporary-A schema — `{sha256(TEMP_SCHEMA)}`
- permanent-B schema — `{sha256(PERM_SCHEMA)}`
- canonical-root manifest — `{sha256(CANONICAL_MANIFEST)}`
- preserved v5 negative report — `{sha256(V5_DIR / 'REPORT.md')}`
- preserved v5 negative results — `{sha256(V5_DIR / 'v5-contract-check-results.json')}`
- owner v6 result (not approval) — `{sha256(V6_OWNER_DIR / 'results.json')}`

## Result

Checks: **{len(checks)}**; pass **{status_counts['pass']}**; findings **{status_counts['finding']}**; unimplemented **{status_counts['unimplemented']}**.

Independent machine results: `{results_path}` — `{results_hash}`.
Hostile fixture contract: `{fixture_path}` — `{fixture_hash}`; 34 cases, all
expected to reject. Supplemental security fixtures: `{supplemental_path}` —
`{supplemental_hash}`; 7 cases. None was run against a real verifier.

## Blocking independent findings

{chr(10).join(finding_lines)}

Key conclusions:

1. The typed DAG edges are internally acyclic and edge-type-complete, and the
   draft-first signing order excludes the binding self-hash/future manifest
   cycle.
2. That does not establish field provenance. `artifact_name`, service/inner/
   binary digest-size/architecture, producer job key, producer workflow
   identity, upload step, and signed terminal completion are dropped or lack a
   typed immutable API derivation before/after signing.
3. `release_asset_id` is correctly described as distinct from Actions
   `artifact_id`, but `policy_validator_b_record` is an unnamespaced
   `canonical-json-artifact-id-and-sha256` claim without an explicit upload
   producer. Markdown and JSON also disagree on `published_asset_id`, caller
   workflow fields, and aggregate attestation digest.
4. The global "both predicates include" field list requires future release
   asset/manifest values and the binding attestation's own ID/digest before
   the binding predicate can be signed. Tree-B also embeds a self-hash and a
   PR number whose allocation sequence is not draft-first.
5. REST `database_id` aliases, the OIDC `job_workflow_sha` versus downloaded
   workflow-blob SHA, and the verifier App/check integration credentials are
   not separately typed and bound.
6. The proposed release predicate path
   `attestations/velnor-workflow-policy-validator-release.v1.json` is rejected
   by the frozen canonical B schema, which requires
   `attestations/velnor-policy-validator-release.v1.json`. The schema also has
   no fields for several v6 required trust/ID values.
7. `origin/main` has neither the proposed B workflow source nor generated
   output. Actionlint fixture results therefore prove syntax rules only; they
   do not prove the real workflow or a verifier.

## Primary references

- https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations
- https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax
- https://docs.github.com/en/actions/reference/security/oidc
- https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-with-reusable-workflows
"""
    (AUDIT / "REPORT.md").write_text(report)
    if "--write" not in sys.argv:
        print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
