#!/usr/bin/env python3
"""Synthetic offline public-CLI harness for checker ce894a81.

This deliberately creates no live facts.  It mirrors the current typed JSON
contract and the source fixture shape used by the checker unit test, then
executes the public evidence-check binary against a baseline and recomputed
hostile mutations.  A non-zero harness exit means an expected semantic guard
was absent or the baseline had an unexpected finding.
"""

from __future__ import annotations

import base64
import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys
from collections import Counter
from pathlib import Path


HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
BIN = Path(
    os.environ.get(
        "VELNOR_TOOLS_BIN",
        "/private/tmp/velnor-checker-target-ce894/debug/velnor-tools",
    )
)
WORKTREE = Path(
    os.environ.get("VELNOR_WORKTREE", "/private/tmp/velnor-checker-review-ce894")
)

MANIFEST_ID = "github-first-dual-lane-2026-09-19"
SNAPSHOT_ID = "snapshot-ce894-cli"
SOURCE_REPOSITORY = "tailrocks/velnor"
SOURCE_REVISION = "abe9ad82a2d4d01b706bbc6122ab6ccb150faad9"
SOURCE_DIGEST = "sha256:b39b3bcb5d149db66a03483bd7a19482af2657df6f962872030d820a39f13514"
SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40
DIGEST_A = "sha256:" + "a" * 64
DIGEST_C = "sha256:" + "c" * 64
OBSERVED_AT = "2026-09-20T00:00:00Z"
COMPLETED_AT = "2026-09-20T00:00:01Z"
WORKFLOW_PATH = ".github/workflows/ci.yml"
WORKFLOW_BYTES = (
    b"on: [push, pull_request]\n"
    b"jobs:\n"
    b"  scan:\n"
    b"    runs-on: ubuntu-24.04\n"
    b"    steps: []\n"
)

REPOSITORIES = [
    "tailrocks/velnor",
    "tailrocks/velnor-apt",
    "tailrocks/parallax",
    "tailrocks/tracing-request-level",
    "tailrocks/termrock",
    "tailrocks/termpane",
    "tailrocks/tablerock",
    "tailrocks/schemalane",
    "tailrocks/ruxel",
    "tailrocks/pg-bigdecimal",
    "tailrocks/parallax-telemetry-playground",
    "tailrocks/homebrew-tablerock",
    "tailrocks/homebrew-ruxel",
    "tailrocks/homebrew-parallax",
    "tailrocks/homebrew-holla",
    "tailrocks/holla-apt",
    "tailrocks/holla",
    "tailrocks/homebrew-velnor",
    "tailrocks/tailrocks-typescript-skills",
    "tailrocks/tailrocks-skill-authoring-skills",
    "tailrocks/tailrocks-rust-skills",
    "tailrocks/tailrocks-roadmap-skills",
    "tailrocks/tailrocks-pull-request-skills",
    "tailrocks/tailrocks-open-source-skills",
    "tailrocks/tailrocks-macos-skills",
    "tailrocks/tailrocks-code-quality-skills",
    "jackin-project/jackin",
    "jackin-project/jackin-agent-smith",
    "jackin-project/homebrew-tap",
    "jackin-project/jackin-the-architect",
    "jackin-project/jackin-sentinel",
    "jackin-project/jackin-role-action",
]


def digest_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def canonical_bytes(value: object) -> bytes:
    # All fixture strings are ASCII.  This matches the checker's sorted-key
    # canonical_json implementation byte-for-byte for this fixture.
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def source_blob(repository: str, path: str, data: bytes) -> dict[str, object]:
    return {
        "repository": repository,
        "path": path,
        "revision": SHA_A,
        "source_sha": SHA_A,
        "source_url": f"https://github.com/{repository}/blob/main/{path}",
        "media_type": "text/yaml",
        "canonicalization": "raw-utf8",
        "sha256": digest_bytes(data),
        "byte_length": len(data),
        "bytes_base64": b64(data),
        "raw_object_refs": ["raw-1"],
    }


def required_context() -> dict[str, str]:
    return {"context": "ci", "app_id": "123"}


def expected_job() -> dict[str, object]:
    return {
        "job_id": "scan",
        "workload_id": "scan",
        "provider": "github",
        "platform": "linux",
        "architecture": "amd64",
        "required": True,
        "child_workflow": None,
    }


def manifest_row(repository: str) -> dict[str, object]:
    return {
        "repository": repository,
        "repository_role": "library",
        "default_branch": "main",
        "expected_workload_ids": ["scan"],
        "required_check_contexts_and_apps": [required_context()],
        "workload_platform_architecture": [
            {"workload_id": "scan", "platform": "linux", "architecture": "amd64"}
        ],
        "expected_jobs": [expected_job()],
        "generated_plan_digest": DIGEST_A,
        "workflow_path": WORKFLOW_PATH,
        "workflow_revision": SHA_A,
        "provider_eligibility": {"github": "eligible", "velnor": "not-applicable"},
        "host_contracts": {},
        "release_applicability": "not-applicable",
        "generator_revision": SHA_A,
        "runtime_product_id": "velnor",
        "generator_artifact_digest": DIGEST_A,
        "configuration_digest": DIGEST_A,
        "generated_tree_digest": DIGEST_A,
        "scan_state_digest": DIGEST_A,
        "runtime_release_version": "1.0.0",
        "runtime_source_sha": SHA_A,
        "job_image_digest": DIGEST_A,
    }


def manifest() -> dict[str, object]:
    return {
        "schema_version": 2,
        "manifest_id": MANIFEST_ID,
        "source": {
            "repository": SOURCE_REPOSITORY,
            "revision": SOURCE_REVISION,
            "digest": SOURCE_DIGEST,
            "reviewed_by": "synthetic-cli-harness",
        },
        "repositories": [manifest_row(repository) for repository in REPOSITORIES],
    }


def job_observation(job_id: int, repository: str, event: str, host_prefix: str) -> dict[str, object]:
    return {
        "job_id": str(job_id),
        "job_name": "scan",
        "workload_id": "scan",
        "provider": "github",
        "platform": "linux",
        "architecture": "amd64",
        "status": "completed",
        "conclusion": "success",
        "event": event,
        "runner_name": "GitHub Actions 1",
        "host_id": f"{host_prefix}-{repository}",
        "runner_kind": "github-hosted",
        "runner_labels": ["ubuntu-24.04"],
        "source_url": f"https://github.com/{repository}/actions/runs/{job_id}",
    }


def check_observation(check_id: int, job_id: int, run_id: int, event: str, repository: str) -> dict[str, object]:
    return {
        "context": "ci",
        "app_id": "123",
        "status": "completed",
        "conclusion": "success",
        "run_id": run_id,
        "job_id": str(job_id),
        "source_url": f"https://github.com/{repository}/runs/{check_id}",
        "event": event,
    }


def execution(
    repository: str,
    run_id: int,
    job_id: int,
    event: str,
    trigger_sha: str,
    checkout_sha: str,
    run_prefix: str,
    check_id: int,
) -> dict[str, object]:
    return {
        "run_id": run_id,
        "run_attempt": 1,
        "run_url": f"https://github.com/{repository}/actions/runs/{run_id}",
        "workflow_path": WORKFLOW_PATH,
        "workflow_revision": SHA_A,
        "event": event,
        "trigger_source_sha": trigger_sha,
        "actual_checkout_sha": checkout_sha,
        "status": "completed",
        "conclusion": "success",
        "provider": "github",
        "runner_name": "GitHub Actions 1",
        "host_id": f"{run_prefix}-{repository}",
        "runner_kind": "github-hosted",
        "runner_labels": ["ubuntu-24.04"],
        "jobs": [job_observation(job_id, repository, event, run_prefix)],
        "required_checks": [check_observation(check_id, job_id, run_id, event, repository)],
        "child_runs": [],
    }


def snapshot_row(repository: str, repository_id: int) -> dict[str, object]:
    main_run = 10_000 + repository_id
    main_check = 30_000 + repository_id
    main_job = 40_000 + repository_id
    pr_run = 50_000 + repository_id
    pr_check = 70_000 + repository_id
    pr_job = 80_000 + repository_id
    main_execution = execution(
        repository, main_run, main_job, "push", SHA_A, SHA_A, "github-main", main_check
    )
    pr_execution = execution(
        repository, pr_run, pr_job, "pull_request", SHA_B, SHA_C, "github-pr", pr_check
    )
    return {
        "repository": repository,
        "repository_id": repository_id,
        "default_branch": "main",
        "default_branch_sha": SHA_A,
        "ruleset": {
            "required_checks": [required_context()],
            "source_url": f"https://github.com/{repository}/settings/rules",
            "pages_complete": True,
        },
        "workflows": [
            {
                "path": WORKFLOW_PATH,
                "revision": SHA_A,
                "source_sha": SHA_A,
                "event": "push",
                "source_url": f"https://github.com/{repository}/blob/main/{WORKFLOW_PATH}",
            }
        ],
        "main_executions": [main_execution],
        "open_prs": [
            {
                "number": 1,
                "state": "open",
                "draft": False,
                "author": "author",
                "author_association": "CONTRIBUTOR",
                "head_repository": repository,
                "head_sha": SHA_B,
                "base_sha": SHA_A,
                "merge_sha": SHA_C,
                "merge_group_sha": None,
                "source_url": f"https://github.com/{repository}/pull/1",
                "executions": [pr_execution],
            }
        ],
    }


def snapshot() -> dict[str, object]:
    return {
        "schema_version": 2,
        "snapshot_id": SNAPSHOT_ID,
        "manifest_id": MANIFEST_ID,
        "observed_at_utc": OBSERVED_AT,
        "source": {
            "collector": "synthetic-cli-harness",
            "collector_revision": SHA_A,
            "api_base": "https://api.github.com",
            "captured_at_utc": OBSERVED_AT,
            "read_only": True,
            "page_count": 1,
            "permission_scopes": ["metadata:read"],
        },
        "repositories": [snapshot_row(repository, i + 1) for i, repository in enumerate(REPOSITORIES)],
    }


def raw_object() -> dict[str, object]:
    data = b"{}"
    return {
        "raw_id": "raw-1",
        "request_id": "request-1",
        "object_kind": "github-response",
        "canonicalization": "jcs",
        "sha256": digest_bytes(data),
        "byte_length": len(data),
        "bytes_base64": b64(data),
        "media_type": "application/json",
        "storage_ref": "artifact://sha256/" + hashlib.sha256(data).hexdigest(),
    }


def workflow_inventory(repository: str) -> dict[str, object]:
    workflow = source_blob(repository, WORKFLOW_PATH, WORKFLOW_BYTES)
    state = {
        "name": f"generated-state-{repository}",
        "schema": "velnor.generated-state.v1",
        "source_url": workflow["source_url"],
        "sha256": digest_bytes(b"{}"),
        "source_revision": SHA_A,
        "source_digest": DIGEST_C,
        "observed_at_utc": OBSERVED_AT,
        "raw_object_refs": ["raw-1"],
    }
    return {
        "source": workflow,
        "events": ["push", "pull_request"],
        "reusable_workflows": [],
        "actions": [],
        "scanners": [],
        "generated_state": state,
        "raw_object_refs": ["raw-1"],
    }


def producer(
    repository: str,
    context: str,
    app_id: str,
    suite_id: int,
    check_id: int,
    run_id: int,
    job_id: int,
    source_sha: str,
    checkout_sha: str,
    event: str,
) -> dict[str, object]:
    return {
        "context": context,
        "app_id": app_id,
        "check_suite_id": suite_id,
        "check_run_id": check_id,
        "workflow_run_id": run_id,
        "run_attempt": 1,
        "job_id": job_id,
        "source_sha": source_sha,
        "actual_checkout_sha": checkout_sha,
        "event": event,
        "status": "completed",
        "conclusion": "success",
        "source_url": f"https://github.com/{repository}/runs/{check_id}",
        "raw_object_refs": ["raw-1"],
    }


def collector_repository(repository: str, repository_id: int) -> dict[str, object]:
    main_run = 10_000 + repository_id
    main_suite = 20_000 + repository_id
    main_check = 30_000 + repository_id
    main_job = 40_000 + repository_id
    pr_run = 50_000 + repository_id
    pr_suite = 60_000 + repository_id
    pr_check = 70_000 + repository_id
    pr_job = 80_000 + repository_id
    return {
        "repository": repository,
        "repository_id": repository_id,
        "default_branch": "main",
        "default_branch_sha": SHA_A,
        "rulesets": [
            {
                "ruleset_id": repository_id,
                "name": "required-ci",
                "source_url": f"https://github.com/{repository}/settings/rules",
                "complete": True,
                "required_checks": [
                    {
                        "context": "ci",
                        "app_id": "123",
                        "ruleset_id": repository_id,
                        "raw_object_refs": ["raw-1"],
                    }
                ],
                "raw_object_refs": ["raw-1"],
            }
        ],
        "workflows": [workflow_inventory(repository)],
        "open_prs": [
            {
                "number": 1,
                "state": "open",
                "draft": False,
                "author": "author",
                "author_association": "CONTRIBUTOR",
                "head_repository": repository,
                "head_sha": SHA_B,
                "base_sha": SHA_A,
                "tested_merge_sha": SHA_C,
                "merge_group_sha": None,
                "trust": {
                    "state": "trusted",
                    "reason": "synthetic fixture trust observation",
                    "raw_object_refs": ["raw-1"],
                },
                "applicability": "required",
                "source_url": f"https://github.com/{repository}/pull/1",
                "workflow_bindings": [
                    {
                        "workflow_path": WORKFLOW_PATH,
                        "workflow_revision": SHA_A,
                        "event": "pull_request",
                        "source_sha": SHA_B,
                        "actual_checkout_sha": SHA_C,
                        "run_ids": [pr_run],
                        "raw_object_refs": ["raw-1"],
                    }
                ],
                "required_check_producers": [
                    producer(
                        repository,
                        "ci",
                        "123",
                        pr_suite,
                        pr_check,
                        pr_run,
                        pr_job,
                        SHA_B,
                        SHA_C,
                        "pull_request",
                    )
                ],
                "raw_object_refs": ["raw-1"],
            }
        ],
        "main_checks": [
            producer(
                repository,
                "ci",
                "123",
                main_suite,
                main_check,
                main_run,
                main_job,
                SHA_A,
                SHA_A,
                "push",
            )
        ],
        "raw_object_refs": ["raw-1"],
    }


def revision_row(repository: str) -> dict[str, object]:
    return {
        "repository": repository,
        "default_branch_sha": SHA_A,
        "prs": [
            {
                "number": 1,
                "head_sha": SHA_B,
                "base_sha": SHA_A,
                "tested_merge_sha": SHA_C,
                "merge_group_sha": None,
                "raw_object_refs": ["raw-1"],
            }
        ],
        "raw_object_refs": ["raw-1"],
    }


def graph(repository: str) -> tuple[dict[str, object], dict[str, object]]:
    workload = f"workload:{repository}:scan"
    check = f"check:{repository}:ci"
    return (
        {
            "id": workload,
            "kind": "workload",
            "repository": repository,
            "workload_id": "scan",
            "applicability": "required",
            "source_sha": SHA_A,
            "source_ref": "refs/heads/main",
            "raw_object_refs": ["raw-1"],
        },
        {
            "id": check,
            "kind": "check",
            "repository": repository,
            "workload_id": "scan",
            "applicability": "required",
            "source_sha": SHA_A,
            "source_ref": "refs/heads/main",
            "raw_object_refs": ["raw-1"],
        },
    )


def collector() -> dict[str, object]:
    nodes: list[dict[str, object]] = []
    edges: list[dict[str, object]] = []
    for repository in REPOSITORIES:
        workload, check = graph(repository)
        nodes.extend([workload, check])
        edges.append(
            {
                "from": workload["id"],
                "to": check["id"],
                "kind": "workload-to-check",
                "required": True,
                "source_sha": SHA_A,
                "source_ref": "refs/heads/main",
                "target_source_sha": SHA_A,
                "target_source_ref": "refs/heads/main",
                "raw_object_refs": ["raw-1"],
            }
        )
    return {
        "schema_version": 2,
        "snapshot_id": SNAPSHOT_ID,
        "manifest_id": MANIFEST_ID,
        "phase": "G0",
        "observed_at_utc": OBSERVED_AT,
        "completed_at_utc": COMPLETED_AT,
        "collector": {
            "name": "synthetic-cli-harness",
            "revision": SHA_A,
            "mode": "read_only",
            "api_base": "https://api.github.com",
            "api_versions": ["2022-11-28"],
        },
        "auth": {
            "provider": "github",
            "viewer_id": "viewer-id",
            "viewer_login": "viewer",
            "safe_scopes": [
                "actions:read",
                "administration:read",
                "checks:read",
                "contents:read",
                "metadata:read",
                "pull_requests:read",
                "statuses:read",
                "workflows:read",
            ],
            "secret_excluded": True,
        },
        "rate_limit": {
            "api": "core",
            "limit": 5000,
            "remaining": 4999,
            "used": 1,
            "reset_at_utc": "2026-09-20T01:00:00Z",
            "observed_at_utc": COMPLETED_AT,
        },
        "requests": [
            {
                "request_id": "request-1",
                "api": "rest",
                "method": "GET",
                "endpoint_or_operation": "/repos/tailrocks/velnor",
                "query_base64": b64(b"page=1"),
                "variables_base64": b64(b"{}"),
                "query_sha256": digest_bytes(b"page=1"),
                "variables_sha256": digest_bytes(b"{}"),
                "auth_identity_ref": "collector.auth",
                "started_at_utc": OBSERVED_AT,
                "completed_at_utc": COMPLETED_AT,
                "http_status": 200,
                "api_request_id": "request-id",
                "rate_limit_ref": "collector.rate_limit",
                "page": {
                    "number": 1,
                    "per_page": 100,
                    "link_next": None,
                    "cursor_in": None,
                    "cursor_out": None,
                    "has_next_page": False,
                    "items_returned": 1,
                },
                "response_raw_ref": "raw-1",
                "error_raw_ref": None,
                "state": "complete",
                "complete": True,
                "truncation_reason": None,
            }
        ],
        "raw_objects": [raw_object()],
        "repositories": [collector_repository(repository, i + 1) for i, repository in enumerate(REPOSITORIES)],
        "reconciliation": {
            "pre_state": [revision_row(repository) for repository in REPOSITORIES],
            "post_state": [revision_row(repository) for repository in REPOSITORIES],
            "changed_refs": [],
            "invalidated": [],
        },
        "dependency_graph": {
            "nodes": nodes,
            "edges": edges,
            "raw_object_refs": ["raw-1"],
        },
        "model_session": {
            "session_id": "session",
            "effective": True,
            "orchestrator_model": "gpt-6-astra",
            "orchestrator_effort": "low",
            "agents": [
                {
                    "agent_id": "agent",
                    "model": "gpt-5.6-luna",
                    "effort": "max",
                    "effective": True,
                    "raw_object_refs": ["raw-1"],
                }
            ],
            "raw_object_refs": ["raw-1"],
        },
        "access": [
            {
                "repository": repository,
                "state": "complete",
                "scopes": ["metadata:read"],
                "gaps": [],
                "raw_object_refs": ["raw-1"],
            }
            for repository in REPOSITORIES
        ],
        "workload_artifact": {
            "name": "workload-matrix",
            "schema": "velnor.workload-matrix.v1",
            "source_url": "https://github.com/tailrocks/velnor/blob/reviewed/matrix.json",
            "sha256": digest_bytes(b"{}"),
            "source_revision": SHA_A,
            "source_digest": DIGEST_C,
            "observed_at_utc": OBSERVED_AT,
            "raw_object_refs": ["raw-1"],
        },
    }


def typed_inventory() -> dict[str, object]:
    snap = collector()
    raw = canonical_bytes(snap)
    return {
        "collector_snapshot": snap,
        "collector_snapshot_bytes_base64": b64(raw),
        "collector_snapshot_sha256": digest_bytes(raw),
        "collector_snapshot_storage_ref": "artifact://sha256/" + hashlib.sha256(raw).hexdigest(),
    }


def evidence_record(repository: str) -> dict[str, object]:
    return {
        "repository": repository,
        "repository_role": "library",
        "evidence_role": "inventory",
        "default_branch": "main",
        "default_branch_sha": SHA_A,
        "observed_at_utc": OBSERVED_AT,
        "generator_revision": SHA_A,
        "runtime_product_id": "velnor",
        "generator_artifact_digest": DIGEST_A,
        "configuration_digest": DIGEST_A,
        "generated_tree_digest": DIGEST_A,
        "scan_state_digest": DIGEST_A,
        "runtime_release_version": "1.0.0",
        "runtime_source_sha": SHA_A,
        "job_image_digest": DIGEST_A,
        "expected_workload_ids": ["scan"],
        "required_check_contexts_and_apps": [required_context()],
        "workload_platform_architecture": [
            {"workload_id": "scan", "platform": "linux", "architecture": "amd64"}
        ],
        "provider_eligibility": {"github": "eligible", "velnor": "not-applicable"},
        "justified_exclusions": [],
        "pr_number": None,
        "pr_head_sha": None,
        "pr_base_sha": None,
        "tested_merge_sha": None,
        "merge_group_sha": None,
        "workflow_path": WORKFLOW_PATH,
        "workflow_revision": SHA_A,
        "event": "inventory",
        "run_id": 0,
        "run_attempt": 0,
        "run_url": "",
        "trigger_source_sha": SHA_A,
        "actual_checkout_sha": SHA_A,
        "provider": "inventory",
        "runner_name": "",
        "host_id": "",
        "runner_kind": "",
        "runner_labels": [],
        "run_status": "",
        "run_conclusion": "",
        "expected_jobs": [expected_job()],
        "actual_job_ids": [],
        "actual_job_conclusions": {},
        "logs": [],
        "child_run_links": [],
        "required_checks": [],
        "release": None,
        "install": None,
        "owner": "inventory",
        "reviewer": "inventory",
        "gate_status": "inventory",
        "blocker": None,
        "next_action": None,
    }


def evidence_document() -> dict[str, object]:
    return {
        "schema_version": 2,
        "manifest_id": MANIFEST_ID,
        "snapshot_id": SNAPSHOT_ID,
        "stage": "G0",
        "records": [evidence_record(repository) for repository in REPOSITORIES],
        "reviewer_attestation": None,
        "g0_inventory": typed_inventory(),
    }


def refresh_inventory(evidence: dict[str, object]) -> None:
    inventory = evidence["g0_inventory"]
    assert isinstance(inventory, dict)
    snap = inventory["collector_snapshot"]
    assert isinstance(snap, dict)
    for raw in snap.get("raw_objects", []):
        data = base64.b64decode(raw["bytes_base64"])
        raw["byte_length"] = len(data)
        raw["sha256"] = digest_bytes(data)
    data = canonical_bytes(snap)
    inventory["collector_snapshot_bytes_base64"] = b64(data)
    inventory["collector_snapshot_sha256"] = digest_bytes(data)
    inventory["collector_snapshot_storage_ref"] = "artifact://sha256/" + hashlib.sha256(data).hexdigest()


def refresh_outer_only(evidence: dict[str, object]) -> None:
    inventory = evidence["g0_inventory"]
    snap = inventory["collector_snapshot"]
    data = canonical_bytes(snap)
    inventory["collector_snapshot_bytes_base64"] = b64(data)
    inventory["collector_snapshot_sha256"] = digest_bytes(data)
    inventory["collector_snapshot_storage_ref"] = "artifact://sha256/" + hashlib.sha256(data).hexdigest()


def source_bytes(source: dict[str, object], data: bytes) -> None:
    source["bytes_base64"] = b64(data)
    source["byte_length"] = len(data)
    source["sha256"] = digest_bytes(data)


def root_workflow(evidence: dict[str, object]) -> dict[str, object]:
    inventory = evidence["g0_inventory"]
    snap = inventory["collector_snapshot"]
    return snap["repositories"][0]["workflows"][0]


def mutation_wrong_ref(base: dict[str, object]) -> dict[str, object]:
    return mutation_pinned_action(base, "main")


def mutation_pinned_action(base: dict[str, object], pinned_ref: str) -> dict[str, object]:
    result = copy.deepcopy(base)
    workflow = root_workflow(result)
    data = (
        b"on: [push, pull_request]\n"
        b"jobs:\n"
        b"  scan:\n"
        b"    runs-on: ubuntu-24.04\n"
        b"    steps:\n"
        + f"      - uses: actions/checkout/action.yml@{pinned_ref}\n".encode()
    )
    source = workflow["source"]
    source_bytes(source, data)
    action_data = b"name: checkout\n"
    workflow["actions"] = [
        {
            "kind": "action",
            "source": source_blob("actions/checkout", "action.yml", action_data),
        }
    ]
    refresh_inventory(result)
    return result


def mutation_source_wrong_valid_ref(base: dict[str, object]) -> dict[str, object]:
    return mutation_pinned_action(base, SHA_B)


def mutation_valid_ref(base: dict[str, object]) -> dict[str, object]:
    return mutation_pinned_action(base, SHA_A)


def mutation_nested_child(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    workflow = root_workflow(result)
    root_data = (
        b"on: [push]\n"
        b"jobs:\n"
        b"  scan:\n"
        b"    uses: ./.github/workflows/reusable.yml@"
        + SHA_A.encode()
        + b"\n"
    )
    source_bytes(workflow["source"], root_data)
    reusable_data = (
        b"on: workflow_call\n"
        b"jobs:\n"
        b"  inner:\n"
        b"    uses: ./.github/workflows/nested.yml@"
        + SHA_A.encode()
        + b"\n"
    )
    nested_data = b"on: workflow_call\njobs:\n  nested:\n    runs-on: ubuntu-24.04\n    steps: []\n"
    workflow["events"] = ["push"]
    workflow["reusable_workflows"] = [
        {
            "kind": "reusable_workflow",
            "source": source_blob(SOURCE_REPOSITORY, ".github/workflows/reusable.yml", reusable_data),
        },
        {
            "kind": "reusable_workflow",
            "source": source_blob(SOURCE_REPOSITORY, ".github/workflows/nested.yml", nested_data),
        },
    ]
    refresh_inventory(result)
    return result


def mutation_graph_wrong_target(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    inventory = result["g0_inventory"]
    snap = inventory["collector_snapshot"]
    edges = snap["dependency_graph"]["edges"]
    edges[0]["to"] = "check:tailrocks/velnor-apt:ci"
    refresh_inventory(result)
    return result


def mutation_graph_wrong_target_sha(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    inventory = result["g0_inventory"]
    snap = inventory["collector_snapshot"]
    edge = snap["dependency_graph"]["edges"][0]
    edge["to"] = "check:tailrocks/velnor-apt:ci"
    edge["target_source_sha"] = SHA_B
    refresh_inventory(result)
    return result


def mutation_evil_api(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    inventory = result["g0_inventory"]
    inventory["collector_snapshot"]["collector"]["api_base"] = "https://api.github.com.evil.example"
    refresh_inventory(result)
    return result


def mutation_page_missing(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    request = result["g0_inventory"]["collector_snapshot"]["requests"][0]
    request["page"]["has_next_page"] = True
    request["page"]["link_next"] = "https://api.github.com/repos/tailrocks/velnor?page=2"
    refresh_inventory(result)
    return result


def mutation_page_wrong_path(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    request = result["g0_inventory"]["collector_snapshot"]["requests"][0]
    request["page"]["has_next_page"] = True
    request["page"]["link_next"] = "https://api.github.com/repos/tailrocks/velnor-apt?page=2"
    refresh_inventory(result)
    return result


def mutation_source_wrong_url(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    source = root_workflow(result)["source"]
    source["source_url"] = f"https://github.com/{SOURCE_REPOSITORY}/blob/main/.github/workflows/other.yml"
    refresh_inventory(result)
    return result


def mutation_cas_without_store(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    inventory = result["g0_inventory"]
    snap = inventory["collector_snapshot"]
    raw = snap["raw_objects"][0]
    raw["storage_ref"] = "cas://sha256/" + raw["sha256"].split(":", 1)[1]
    refresh_inventory(result)
    outer_digest = inventory["collector_snapshot_sha256"].split(":", 1)[1]
    inventory["collector_snapshot_storage_ref"] = "cas://sha256/" + outer_digest
    return result


def mutation_tampered_raw_replay(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    raw = result["g0_inventory"]["collector_snapshot"]["raw_objects"][0]
    data = b"tampered-cas-bytes"
    raw["bytes_base64"] = b64(data)
    raw["byte_length"] = len(data)
    refresh_outer_only(result)
    return result


def mutation_source_bytes_replay(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    source = root_workflow(result)["source"]
    source_bytes(source, WORKFLOW_BYTES + b"# caller-replaced-source\n")
    refresh_inventory(result)
    return result


def logical_manifest() -> dict[str, object]:
    result = manifest()
    row = result["repositories"][0]
    row["expected_workload_ids"] = ["unit"]
    row["workload_platform_architecture"][0]["workload_id"] = "unit"
    row["expected_jobs"][0]["job_id"] = "unit-github"
    row["expected_jobs"][0]["workload_id"] = "unit"
    return result


def mutation_logical_job_mapping(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    workflow = root_workflow(result)
    data = (
        b"on: [push, pull_request]\n"
        b"jobs:\n"
        b"  unit-github:\n"
        b"    runs-on: ubuntu-24.04\n"
        b"    steps: []\n"
    )
    source_bytes(workflow["source"], data)
    record = result["records"][0]
    record["expected_workload_ids"] = ["unit"]
    record["workload_platform_architecture"][0]["workload_id"] = "unit"
    record["expected_jobs"][0]["job_id"] = "unit-github"
    record["expected_jobs"][0]["workload_id"] = "unit"
    for node in result["g0_inventory"]["collector_snapshot"]["dependency_graph"]["nodes"]:
        if node["repository"] == SOURCE_REPOSITORY:
            node["workload_id"] = "unit"
    refresh_inventory(result)
    return result


def mutation_static_condition(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    workflow = root_workflow(result)
    data = (
        b"on: [push, pull_request]\n"
        b"jobs:\n"
        b"  scan:\n"
        b"    if: false\n"
        b"    runs-on: ubuntu-24.04\n"
        b"    steps: []\n"
    )
    source_bytes(workflow["source"], data)
    refresh_inventory(result)
    return result


def mutation_static_matrix(base: dict[str, object]) -> dict[str, object]:
    result = copy.deepcopy(base)
    workflow = root_workflow(result)
    data = (
        b"on: [push, pull_request]\n"
        b"jobs:\n"
        b"  scan:\n"
        b"    strategy:\n"
        b"      matrix:\n"
        b"        os: [ubuntu-24.04, ubuntu-22.04]\n"
        b"    runs-on: ubuntu-24.04\n"
        b"    steps: []\n"
    )
    source_bytes(workflow["source"], data)
    refresh_inventory(result)
    return result


def write_case(
    name: str,
    evidence: dict[str, object],
    manifest_value: dict[str, object] | None = None,
) -> tuple[Path, Path, Path, Path]:
    case = FIXTURES / name
    if case.exists() or case.is_symlink():
        shutil.rmtree(case)
    case.mkdir(parents=True, exist_ok=True)
    manifest_path = case / "manifest.json"
    snapshot_path = case / "snapshot.json"
    evidence_path = case / "evidence.json"
    store_root = case / "store"
    store_objects = store_root / "sha256"
    store_objects.mkdir(parents=True, exist_ok=True)
    write_json(manifest_path, manifest_value if manifest_value is not None else manifest())
    write_json(snapshot_path, snapshot())
    write_json(evidence_path, evidence)
    if name != "cas-ref-without-store":
        inventory = evidence["g0_inventory"]
        snapshot_bytes = base64.b64decode(inventory["collector_snapshot_bytes_base64"])
        snapshot_hex = inventory["collector_snapshot_sha256"].split(":", 1)[1]
        (store_objects / snapshot_hex).write_bytes(snapshot_bytes)
        raw_override = b"{}" if name == "tampered-raw-replay" else None
        for raw in inventory["collector_snapshot"]["raw_objects"]:
            raw_bytes = raw_override if raw_override is not None else base64.b64decode(raw["bytes_base64"])
            raw_hex = raw["sha256"].split(":", 1)[1]
            (store_objects / raw_hex).write_bytes(raw_bytes)
        if name == "symlink-inside-cas":
            raw = inventory["collector_snapshot"]["raw_objects"][0]
            raw_path = store_objects / raw["sha256"].split(":", 1)[1]
            alias = store_root / "inside-alias"
            alias.write_bytes(raw_path.read_bytes())
            raw_path.unlink()
            raw_path.symlink_to(Path("..") / alias.name)
        elif name == "symlink-outside-cas":
            raw = inventory["collector_snapshot"]["raw_objects"][0]
            raw_path = store_objects / raw["sha256"].split(":", 1)[1]
            outside = case / "outside-object"
            outside.write_bytes(raw_path.read_bytes())
            raw_path.unlink()
            raw_path.symlink_to(outside)
        elif name == "ancestor-symlink-outside-cas":
            outside_dir = case / "outside-store"
            outside_dir.mkdir()
            for child in store_objects.iterdir():
                child.rename(outside_dir / child.name)
            store_objects.rmdir()
            (store_root / "sha256").symlink_to(outside_dir)
        elif name == "hardlink-outside-cas":
            raw = inventory["collector_snapshot"]["raw_objects"][0]
            raw_path = store_objects / raw["sha256"].split(":", 1)[1]
            outside = case / "hardlink-object"
            outside.write_bytes(raw_path.read_bytes())
            raw_path.unlink()
            os.link(outside, raw_path)
    return manifest_path, snapshot_path, evidence_path, store_root


def run_cli(name: str, paths: tuple[Path, Path, Path, Path]) -> dict[str, object]:
    manifest_path, snapshot_path, evidence_path, store_root = paths
    command = [
        str(BIN),
        "evidence-check",
        "--stage",
        "G0",
        "--manifest",
        str(manifest_path),
        "--snapshot",
        str(snapshot_path),
        "--evidence",
        str(evidence_path),
        "--evidence-root",
        str(store_root),
        "--json",
    ]
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        cwd=WORKTREE,
    )
    try:
        report = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"{name}: CLI did not emit JSON (exit {completed.returncode}): {completed.stdout!r}; {completed.stderr!r}"
        ) from error
    report["cli_exit"] = completed.returncode
    report["stderr"] = completed.stderr
    report["codes"] = dict(Counter(f["code"] for f in report.get("findings", [])))
    return report


def main() -> int:
    if not BIN.is_file():
        raise SystemExit(f"missing VELNOR_TOOLS_BIN: {BIN}")
    FIXTURES.mkdir(parents=True, exist_ok=True)
    base = evidence_document()
    cases: list[tuple] = [
        (
            "baseline",
            base,
            {"g0-authoritative-proof-missing": 1},
        ),
        (
            "reusable-wrong-ref",
            mutation_wrong_ref(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-derivation": 1},
        ),
        (
            "nested-child-omission",
            mutation_nested_child(base),
            {
                "g0-authoritative-proof-missing": 1,
                "g0-dependency-obligation": 1,
                "g0-workflow-child": 2,
            },
        ),
        (
            "graph-wrong-target",
            mutation_graph_wrong_target(base),
            {"g0-authoritative-proof-missing": 1, "g0-dependency-edge": 1},
        ),
        (
            "evil-api-origin",
            mutation_evil_api(base),
            {"g0-authoritative-proof-missing": 1, "g0-collector-source": 1},
        ),
        (
            "valid-40hex-ref",
            mutation_valid_ref(base),
            {"g0-authoritative-proof-missing": 1},
        ),
        (
            "wrong-40hex-ref",
            mutation_source_wrong_valid_ref(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-derivation": 1},
        ),
        (
            "page-missing-next",
            mutation_page_missing(base),
            {"g0-authoritative-proof-missing": 1, "g0-pagination": 1},
        ),
        (
            "page-wrong-next-path",
            mutation_page_wrong_path(base),
            {
                "g0-authoritative-proof-missing": 1,
                "g0-request-incomplete": 1,
                "g0-pagination": 1,
            },
        ),
        (
            "source-wrong-path-url",
            mutation_source_wrong_url(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-source": 1},
        ),
        (
            "cas-ref-without-store",
            mutation_cas_without_store(base),
            {"g0-authoritative-proof-missing": 1, "g0-storage-ref": 2},
        ),
        (
            "tampered-raw-replay",
            mutation_tampered_raw_replay(base),
            {
                "g0-authoritative-proof-missing": 1,
                "g0-raw-object": 1,
                "g0-storage-mismatch": 1,
            },
        ),
        (
            "source-bytes-replay",
            mutation_source_bytes_replay(base),
            {"g0-authoritative-proof-missing": 1},
        ),
        (
            "symlink-inside-cas",
            base,
            {"g0-authoritative-proof-missing": 1},
        ),
        (
            "symlink-outside-cas",
            base,
            {"g0-authoritative-proof-missing": 1, "g0-storage-ref": 1},
        ),
        (
            "ancestor-symlink-outside-cas",
            base,
            {"g0-authoritative-proof-missing": 1, "g0-storage-ref": 2},
        ),
        (
            "hardlink-outside-cas",
            base,
            {"g0-authoritative-proof-missing": 1},
        ),
        (
            "graph-wrong-target-sha",
            mutation_graph_wrong_target_sha(base),
            {"g0-authoritative-proof-missing": 1, "g0-dependency-edge": 1},
        ),
        (
            "logical-job-mapping",
            mutation_logical_job_mapping(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-plan": 1},
            logical_manifest(),
        ),
        (
            "static-condition-rejected",
            mutation_static_condition(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-derivation": 1},
        ),
        (
            "static-matrix-rejected",
            mutation_static_matrix(base),
            {"g0-authoritative-proof-missing": 1, "g0-workflow-derivation": 1},
        ),
    ]
    results: dict[str, object] = {
        "commit": "ce894a81158706439205b7460811a4f680cbf800",
        "synthetic": True,
        "binary": str(BIN),
        "worktree": str(WORKTREE),
        "cases": {},
    }
    failures = 0
    for case in cases:
        name, evidence, expected, *manifest_values = case
        paths = write_case(name, evidence, manifest_values[0] if manifest_values else None)
        report = run_cli(name, paths)
        actual = report["codes"]
        ok = actual == expected
        failures += int(not ok)
        results["cases"][name] = {
            "expected_codes": expected,
            "actual_codes": actual,
            "cli_exit": report["cli_exit"],
            "status": report["status"],
            "assertion": "pass" if ok else "FAIL",
            "finding_count": len(report.get("findings", [])),
            "findings": report.get("findings", []),
            "paths": [str(path) for path in paths],
        }
    write_json(HERE / "harness-results.json", results)
    print(json.dumps(results, indent=2, sort_keys=True))
    if failures:
        print(f"semantic harness: {failures} expected guard assertion(s) failed", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
