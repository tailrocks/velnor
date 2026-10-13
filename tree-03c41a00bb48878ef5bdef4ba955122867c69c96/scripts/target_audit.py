#!/usr/bin/env python3
"""Summarize the GitHub Actions surface used by Velnor target repositories."""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
from typing import Any

import yaml


EXPECTED_TARGET_USES = Counter(
    {
        "./.github/actions/aggregate-needs": 3,
        "./.github/actions/check-deployed-docs": 2,
        "actions/cache": 13,
        "actions/checkout": 46,
        "actions/deploy-pages": 1,
        "actions/download-artifact": 3,
        "actions/setup-python": 1,
        "actions/upload-artifact": 6,
        "actions/upload-pages-artifact": 1,
        "baptiste0928/cargo-install": 1,
        "crazy-max/ghaction-github-runtime": 2,
        "docker/bake-action": 1,
        "docker/build-push-action": 1,
        "docker/login-action": 5,
        "docker/metadata-action": 1,
        "docker/setup-buildx-action": 5,
        "dorny/paths-filter": 5,
        "dtolnay/rust-toolchain": 1,
        "extractions/setup-just": 4,
        "jdx/mise-action": 13,
        "mozilla-actions/sccache-action": 7,
        "renovatebot/github-action": 2,
        "rui314/setup-mold": 5,
        "Swatinem/rust-cache": 1,
    }
)

EXPECTED_WORKFLOW_FILES = Counter(
    {
        ".github/workflows/ansible.yml": 1,
        ".github/workflows/ci.yml": 1,
        ".github/workflows/construct.yml": 1,
        ".github/workflows/docs.yml": 1,
        ".github/workflows/kestra-build-image.yml": 1,
        ".github/workflows/kestra-build-publish.yml": 1,
        ".github/workflows/preview.yml": 1,
        ".github/workflows/release.yml": 1,
        ".github/workflows/renovate-validate.yml": 1,
        ".github/workflows/renovate.yml": 2,
        ".github/workflows/rust-docker-build.yml": 1,
        ".github/workflows/rust-docker.yml": 1,
        ".github/workflows/rust.yml": 1,
    }
)

EXPECTED_ACTION_FILES = {
    (".github/actions/aggregate-needs/action.yml", "composite"),
    (".github/actions/check-deployed-docs/action.yml", "composite"),
}

EXPECTED_REUSABLE_WORKFLOWS = Counter(
    {
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-jvm-base",
            "./.github/workflows/kestra-build-image.yml",
        ): 1,
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-kestra-backup",
            "./.github/workflows/kestra-build-image.yml",
        ): 1,
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-kestra-playwright",
            "./.github/workflows/kestra-build-image.yml",
        ): 1,
    }
)

EXPECTED_WORKFLOW_TRIGGERS = Counter(
    {
        (".github/workflows/ansible.yml", ("pull_request", "push", "workflow_dispatch")): 1,
        (".github/workflows/ci.yml", ("pull_request", "push", "workflow_dispatch")): 1,
        (".github/workflows/construct.yml", ("pull_request", "push", "workflow_dispatch")): 1,
        (
            ".github/workflows/docs.yml",
            ("pull_request", "push", "schedule", "workflow_dispatch"),
        ): 1,
        (".github/workflows/kestra-build-image.yml", ("workflow_call",)): 1,
        (".github/workflows/kestra-build-publish.yml", ("push", "workflow_dispatch")): 1,
        (".github/workflows/preview.yml", ("workflow_dispatch", "workflow_run")): 1,
        (".github/workflows/release.yml", ("push", "workflow_dispatch")): 1,
        (".github/workflows/renovate-validate.yml", ("pull_request", "push", "workflow_dispatch")): 1,
        (
            ".github/workflows/renovate.yml",
            ("merge_group", "push", "schedule", "workflow_dispatch"),
        ): 2,
        (".github/workflows/rust-docker-build.yml", ("workflow_call",)): 1,
        (".github/workflows/rust-docker.yml", ("pull_request", "push", "workflow_dispatch")): 1,
        (".github/workflows/rust.yml", ("pull_request", "push", "workflow_dispatch")): 1,
    }
)

EXPECTED_JOB_RUNS_ON = Counter(
    {
        (".github/workflows/ansible.yml", "syntax-check", "hetzner-sentry-ci"): 1,
        (".github/workflows/ci.yml", "build-validator", "ubuntu-latest"): 1,
        (".github/workflows/ci.yml", "changes", "ubuntu-latest"): 1,
        (".github/workflows/ci.yml", "check", "ubuntu-latest"): 1,
        (".github/workflows/ci.yml", "ci-required", "ubuntu-latest"): 1,
        (".github/workflows/ci.yml", "msrv", "ubuntu-latest"): 1,
        (".github/workflows/construct.yml", "build", "${{ matrix.runner }}"): 1,
        (".github/workflows/construct.yml", "changes", "ubuntu-latest"): 1,
        (".github/workflows/construct.yml", "construct-required", "ubuntu-latest"): 1,
        (".github/workflows/construct.yml", "publish-manifest", "ubuntu-24.04"): 1,
        (".github/workflows/construct.yml", "publish-manifest-rehearsal", "ubuntu-24.04"): 1,
        (".github/workflows/docs.yml", "changes", "ubuntu-latest"): 1,
        (".github/workflows/docs.yml", "check-deployed", "ubuntu-latest"): 1,
        (".github/workflows/docs.yml", "deploy", "ubuntu-latest"): 1,
        (".github/workflows/docs.yml", "docs-link-check", "ubuntu-latest"): 1,
        (".github/workflows/docs.yml", "docs-required", "ubuntu-latest"): 1,
        (".github/workflows/docs.yml", "repo-link-check", "ubuntu-latest"): 1,
        (".github/workflows/kestra-build-image.yml", "build", "hetzner-sentry-ci"): 1,
        (".github/workflows/preview.yml", "build-jackin-capsule", "ubuntu-latest"): 1,
        (".github/workflows/preview.yml", "build-preview", "ubuntu-latest"): 1,
        (".github/workflows/preview.yml", "publish-preview", "ubuntu-latest"): 1,
        (".github/workflows/preview.yml", "source-changed", "ubuntu-latest"): 1,
        (".github/workflows/release.yml", "build", "${{ matrix.os }}"): 1,
        (".github/workflows/release.yml", "build-jackin-capsule", "ubuntu-latest"): 1,
        (".github/workflows/release.yml", "check-version", "ubuntu-latest"): 1,
        (".github/workflows/release.yml", "homebrew", "ubuntu-latest"): 1,
        (".github/workflows/release.yml", "release", "ubuntu-latest"): 1,
        (".github/workflows/release.yml", "test", "ubuntu-latest"): 1,
        (".github/workflows/renovate-validate.yml", "validate", "ubuntu-latest"): 1,
        (".github/workflows/renovate.yml", "renovate", "hetzner-sentry-ci"): 1,
        (".github/workflows/renovate.yml", "renovate", "ubuntu-24.04"): 1,
        (".github/workflows/rust-docker-build.yml", "build", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust-docker.yml", "changes", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust-docker.yml", "docker-bake", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust-docker.yml", "docker-required", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "changes", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "check", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "rust-required", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-bitcoin-processor", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-blockchain-explorer", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-coingecko-pricing", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-eth-grpc-server", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-eth-processor", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-legacy-grpc-server", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-tron-grpc-server", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "test-tron-processor", "hetzner-sentry-ci"): 1,
        (".github/workflows/rust.yml", "warm-sccache", "hetzner-sentry-ci"): 1,
    }
)

EXPECTED_WORKFLOW_PERMISSIONS = Counter(
    {
        (".github/workflows/ansible.yml", ("contents",)): 1,
        (".github/workflows/ci.yml", ("contents",)): 1,
        (".github/workflows/construct.yml", ("contents",)): 1,
        (".github/workflows/docs.yml", ("contents",)): 1,
        (".github/workflows/kestra-build-publish.yml", ("contents",)): 1,
        (".github/workflows/preview.yml", ("contents",)): 1,
        (".github/workflows/release.yml", ("contents",)): 1,
        (".github/workflows/renovate-validate.yml", ("contents",)): 1,
        (".github/workflows/rust-docker-build.yml", ("contents",)): 1,
        (".github/workflows/rust-docker.yml", ("contents", "pull-requests")): 1,
        (".github/workflows/rust.yml", ("contents", "pull-requests")): 1,
    }
)

EXPECTED_JOB_PERMISSIONS = Counter(
    {
        (".github/workflows/construct.yml", "build", ("contents",)): 1,
        (".github/workflows/construct.yml", "publish-manifest", ("contents",)): 1,
        (".github/workflows/construct.yml", "publish-manifest-rehearsal", ("contents",)): 1,
        (".github/workflows/docs.yml", "check-deployed", ("contents",)): 1,
        (".github/workflows/docs.yml", "deploy", ("contents", "id-token", "pages")): 1,
        (".github/workflows/preview.yml", "publish-preview", ("contents",)): 1,
        (".github/workflows/release.yml", "release", ("contents",)): 1,
    }
)

EXPECTED_JOB_ENVIRONMENTS = Counter(
    {
        (
            ".github/workflows/docs.yml",
            "deploy",
            "{name: github-pages, url: ${{ steps.deployment.outputs.page_url }}}",
        ): 1,
    }
)

EXPECTED_WORKFLOW_CONCURRENCY = Counter(
    {
        (
            ".github/workflows/ci.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: ci-${{ github.ref }}}",
        ): 1,
        (
            ".github/workflows/construct.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: construct-${{ github.ref }}}",
        ): 1,
        (
            ".github/workflows/docs.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: pages-${{ github.event_name }}-${{ github.ref }}}",
        ): 1,
        (
            ".github/workflows/preview.yml",
            "{cancel-in-progress: true, group: homebrew-tap-publish}",
        ): 1,
        (".github/workflows/release.yml", "{cancel-in-progress: false, group: release}"): 1,
        (
            ".github/workflows/renovate-validate.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: renovate-validate-${{ github.ref }}}",
        ): 1,
        (
            ".github/workflows/renovate.yml",
            "{cancel-in-progress: true, group: renovate}",
        ): 1,
        (
            ".github/workflows/rust-docker.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: rust-docker-${{ github.ref }}}",
        ): 1,
        (
            ".github/workflows/rust.yml",
            "{cancel-in-progress: ${{ github.event_name == 'pull_request' }}, group: rust-ci-${{ github.ref }}}",
        ): 1,
    }
)

EXPECTED_JOB_DEFAULTS = Counter(
    {
        (
            ".github/workflows/ansible.yml",
            "syntax-check",
            "{run: {shell: bash, working-directory: ./ansible-configs}}",
        ): 1,
        (
            ".github/workflows/kestra-build-image.yml",
            "build",
            "{run: {shell: bash, working-directory: ./kestra-docker-containers}}",
        ): 1,
    }
)

EXPECTED_CONTINUE_ON_ERROR = Counter(
    {
        (
            ".github/workflows/ci.yml",
            "check",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/ci.yml",
            "build-validator",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/preview.yml",
            "build-preview",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/preview.yml",
            "build-jackin-capsule",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/release.yml",
            "test",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/release.yml",
            "build",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
        (
            ".github/workflows/release.yml",
            "build-jackin-capsule",
            "mozilla-actions/sccache-action",
            True,
        ): 1,
    }
)


def load_yaml(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return yaml.safe_load(handle) or {}


def iter_yaml_files(root: Path) -> list[Path]:
    github = root if root.name == ".github" else root / ".github"
    return sorted(
        path
        for path in github.rglob("*")
        if path.suffix in {".yml", ".yaml"} and path.is_file()
    )


def short_path(path: Path, roots: list[Path]) -> str:
    for root in roots:
        try:
            return str(path.relative_to(root))
        except ValueError:
            continue
    return str(path)


def normalize_uses(value: str) -> str:
    if value.startswith("./") or value.startswith("../") or value.startswith("docker://"):
        return value
    return value.split("@", 1)[0]


def collect_step(
    path: Path,
    job_name: str,
    step: dict[str, Any],
    roots: list[Path],
    uses: Counter[str],
    local_uses: list[tuple[str, str, str]],
    shells: set[str],
    continue_on_error: list[tuple[str, str, str, Any]],
    unsupported_checkout_inputs: list[tuple[str, str, str, Any]],
) -> None:
    if "uses" in step:
        value = str(step["uses"])
        normalized = normalize_uses(value)
        uses[normalized] += 1
        if value.startswith("./"):
            local_uses.append((short_path(path, roots), job_name, value))
        if normalized == "actions/checkout":
            inputs = step.get("with") or {}
            if isinstance(inputs, dict):
                for name in ["submodules", "sparse-checkout", "lfs"]:
                    if input_enabled(inputs.get(name)):
                        unsupported_checkout_inputs.append(
                            (short_path(path, roots), job_name, name, inputs[name])
                        )
    if "shell" in step:
        shells.add(str(step["shell"]))
    if "continue-on-error" in step:
        label = str(step.get("uses") or step.get("name") or step.get("id") or "run")
        if "uses" in step:
            label = normalize_uses(label)
        continue_on_error.append(
            (short_path(path, roots), job_name, label, step["continue-on-error"])
        )


def list_keys(value: Any) -> list[str]:
    if isinstance(value, dict):
        return sorted(str(key) for key in value.keys())
    if isinstance(value, list):
        return [str(item) for item in value]
    if isinstance(value, str):
        return [value]
    if value is None:
        return []
    return [str(value)]


def compact_value(value: Any) -> str:
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, (str, int, float)) or value is None:
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(compact_value(item) for item in value) + "]"
    if isinstance(value, dict):
        parts = [f"{key}: {compact_value(value[key])}" for key in sorted(value.keys())]
        return "{" + ", ".join(parts) + "}"
    return str(value)


def input_enabled(value: Any) -> bool:
    if value is None:
        return False
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return value != 0
    if isinstance(value, str):
        return value.strip().lower() not in {"", "false", "0", "no", "off"}
    return True


def audit(roots: list[Path]) -> dict[str, Any]:
    workflow_files: list[str] = []
    action_files: list[tuple[str, str]] = []
    uses: Counter[str] = Counter()
    local_uses: list[tuple[str, str, str]] = []
    reusable_workflows: list[tuple[str, str, str]] = []
    shells: set[str] = set()
    containers: list[tuple[str, str, Any]] = []
    services: list[tuple[str, str, list[str]]] = []
    continue_on_error: list[tuple[str, str, str, Any]] = []
    workflow_triggers: list[tuple[str, list[str]]] = []
    workflow_env: list[tuple[str, str]] = []
    workflow_permissions: list[tuple[str, list[str]]] = []
    workflow_concurrency: list[tuple[str, str]] = []
    job_runs_on: list[tuple[str, str, str]] = []
    job_ifs: list[tuple[str, str, str]] = []
    job_needs: list[tuple[str, str, str]] = []
    job_strategy: list[tuple[str, str, str]] = []
    job_env: list[tuple[str, str, str]] = []
    job_outputs: list[tuple[str, str, str]] = []
    job_permissions: list[tuple[str, str, list[str]]] = []
    job_concurrency: list[tuple[str, str, str]] = []
    job_defaults: list[tuple[str, str, str]] = []
    job_environments: list[tuple[str, str, str]] = []
    job_timeouts: list[tuple[str, str, Any]] = []
    unsupported_checkout_inputs: list[tuple[str, str, str, Any]] = []

    for root in roots:
        for path in iter_yaml_files(root):
            data = load_yaml(path)
            display_path = short_path(path, roots)
            runs = data.get("runs")
            if isinstance(runs, dict):
                action_files.append((display_path, str(runs.get("using", ""))))
                for step in runs.get("steps") or []:
                    if isinstance(step, dict):
                        collect_step(
                            path,
                            "action",
                            step,
                            roots,
                            uses,
                            local_uses,
                            shells,
                            continue_on_error,
                            unsupported_checkout_inputs,
                        )
                continue

            workflow_files.append(display_path)
            workflow_triggers.append((display_path, list_keys(data.get("on") or data.get(True))))
            if "env" in data:
                workflow_env.append((display_path, compact_value(data["env"])))
            if "permissions" in data:
                workflow_permissions.append((display_path, list_keys(data["permissions"])))
            if "concurrency" in data:
                workflow_concurrency.append((display_path, compact_value(data["concurrency"])))

            jobs = data.get("jobs") or {}
            if not isinstance(jobs, dict):
                continue
            for job_name, job in jobs.items():
                if not isinstance(job, dict):
                    continue
                job_name_str = str(job_name)
                if "runs-on" in job:
                    job_runs_on.append(
                        (display_path, job_name_str, compact_value(job["runs-on"]))
                    )
                if "if" in job:
                    job_ifs.append((display_path, job_name_str, str(job["if"])))
                if "needs" in job:
                    job_needs.append((display_path, job_name_str, compact_value(job["needs"])))
                if "strategy" in job:
                    job_strategy.append(
                        (display_path, job_name_str, compact_value(job["strategy"]))
                    )
                if "env" in job:
                    job_env.append((display_path, job_name_str, compact_value(job["env"])))
                if "outputs" in job:
                    job_outputs.append(
                        (display_path, job_name_str, compact_value(job["outputs"]))
                    )
                if "permissions" in job:
                    job_permissions.append(
                        (display_path, job_name_str, list_keys(job["permissions"]))
                    )
                if "concurrency" in job:
                    job_concurrency.append(
                        (display_path, job_name_str, compact_value(job["concurrency"]))
                    )
                if "environment" in job:
                    job_environments.append(
                        (display_path, job_name_str, compact_value(job["environment"]))
                    )
                if "timeout-minutes" in job:
                    job_timeouts.append((display_path, job_name_str, job["timeout-minutes"]))
                if "uses" in job:
                    reusable_workflows.append((display_path, str(job_name), str(job["uses"])))
                if "container" in job:
                    containers.append((display_path, str(job_name), job["container"]))
                if isinstance(job.get("services"), dict):
                    services.append(
                        (display_path, str(job_name), sorted(job["services"].keys()))
                    )
                defaults = job.get("defaults")
                if isinstance(defaults, dict):
                    job_defaults.append((display_path, job_name_str, compact_value(defaults)))
                    run_defaults = defaults.get("run")
                    if isinstance(run_defaults, dict) and "shell" in run_defaults:
                        shells.add(str(run_defaults["shell"]))
                for step in job.get("steps") or []:
                    if isinstance(step, dict):
                        collect_step(
                            path,
                            str(job_name),
                            step,
                            roots,
                            uses,
                            local_uses,
                            shells,
                            continue_on_error,
                            unsupported_checkout_inputs,
                        )

    return {
        "workflow_files": workflow_files,
        "action_files": action_files,
        "uses": uses,
        "local_uses": local_uses,
        "reusable_workflows": reusable_workflows,
        "shells": sorted(shells),
        "containers": containers,
        "services": services,
        "continue_on_error": continue_on_error,
        "workflow_triggers": workflow_triggers,
        "workflow_env": workflow_env,
        "workflow_permissions": workflow_permissions,
        "workflow_concurrency": workflow_concurrency,
        "job_runs_on": job_runs_on,
        "job_ifs": job_ifs,
        "job_needs": job_needs,
        "job_strategy": job_strategy,
        "job_env": job_env,
        "job_outputs": job_outputs,
        "job_permissions": job_permissions,
        "job_concurrency": job_concurrency,
        "job_defaults": job_defaults,
        "job_environments": job_environments,
        "job_timeouts": job_timeouts,
        "unsupported_checkout_inputs": unsupported_checkout_inputs,
    }


def print_report(summary: dict[str, Any]) -> None:
    print("Workflow files:")
    for path in summary["workflow_files"]:
        print(f"- {path}")

    print("\nComposite/action metadata:")
    for path, using in summary["action_files"]:
        print(f"- {path}: {using}")

    print("\nUses inventory:")
    for name, count in sorted(summary["uses"].items(), key=lambda item: item[0].lower()):
        print(f"- {count} {name}")

    print("\nShells:")
    for shell in summary["shells"]:
        print(f"- {shell}")

    print("\nWorkflow triggers:")
    for path, triggers in summary["workflow_triggers"]:
        print(f"- {path}: {', '.join(triggers)}")

    print("\nWorkflow env:")
    if summary["workflow_env"]:
        for path, value in summary["workflow_env"]:
            print(f"- {path}: {value}")
    else:
        print("- none")

    print("\nWorkflow permissions:")
    if summary["workflow_permissions"]:
        for path, permissions in summary["workflow_permissions"]:
            print(f"- {path}: {', '.join(permissions)}")
    else:
        print("- none")

    print("\nWorkflow concurrency:")
    if summary["workflow_concurrency"]:
        for path, value in summary["workflow_concurrency"]:
            print(f"- {path}: {value}")
    else:
        print("- none")

    print("\nRuns-on:")
    for path, job, value in summary["job_runs_on"]:
        print(f"- {path} :: {job} :: {value}")

    print("\nJob if:")
    if summary["job_ifs"]:
        for path, job, value in summary["job_ifs"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob needs:")
    if summary["job_needs"]:
        for path, job, value in summary["job_needs"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob strategy:")
    if summary["job_strategy"]:
        for path, job, value in summary["job_strategy"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob env:")
    if summary["job_env"]:
        for path, job, value in summary["job_env"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob outputs:")
    if summary["job_outputs"]:
        for path, job, value in summary["job_outputs"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob permissions:")
    if summary["job_permissions"]:
        for path, job, permissions in summary["job_permissions"]:
            print(f"- {path} :: {job} :: {', '.join(permissions)}")
    else:
        print("- none")

    print("\nJob concurrency:")
    if summary["job_concurrency"]:
        for path, job, value in summary["job_concurrency"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob defaults:")
    if summary["job_defaults"]:
        for path, job, value in summary["job_defaults"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob environments:")
    if summary["job_environments"]:
        for path, job, value in summary["job_environments"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob timeouts:")
    if summary["job_timeouts"]:
        for path, job, value in summary["job_timeouts"]:
            print(f"- {path} :: {job} :: {value}")
    else:
        print("- none")

    print("\nJob containers:")
    if summary["containers"]:
        for item in summary["containers"]:
            print(f"- {item}")
    else:
        print("- none")

    print("\nServices:")
    if summary["services"]:
        for item in summary["services"]:
            print(f"- {item}")
    else:
        print("- none")

    print("\nLocal action uses:")
    for path, job, value in summary["local_uses"]:
        print(f"- {path} :: {job} :: {value}")

    print("\nReusable workflow jobs:")
    for path, job, value in summary["reusable_workflows"]:
        print(f"- {path} :: {job} :: {value}")

    print("\nContinue-on-error:")
    for path, job, label, value in summary["continue_on_error"]:
        print(f"- {path} :: {job} :: {label} = {value}")

    print("\nUnsupported checkout inputs:")
    if summary["unsupported_checkout_inputs"]:
        for path, job, name, value in summary["unsupported_checkout_inputs"]:
            print(f"- {path} :: {job} :: {name} = {compact_value(value)}")
    else:
        print("- none")


def check_target_mvp(summary: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    workflow_files = Counter(summary["workflow_files"])
    if workflow_files != EXPECTED_WORKFLOW_FILES:
        errors.append(
            "target MVP workflow file drift: "
            f"expected {dict(EXPECTED_WORKFLOW_FILES)}, got {dict(workflow_files)}"
        )

    action_files = set(summary["action_files"])
    if action_files != EXPECTED_ACTION_FILES:
        errors.append(
            "target MVP local action metadata drift: "
            f"expected {sorted(EXPECTED_ACTION_FILES)}, got {sorted(action_files)}"
        )

    reusable_workflows = Counter(summary["reusable_workflows"])
    if reusable_workflows != EXPECTED_REUSABLE_WORKFLOWS:
        errors.append(
            "target MVP reusable workflow drift: "
            f"expected {dict(EXPECTED_REUSABLE_WORKFLOWS)}, got {dict(reusable_workflows)}"
        )

    workflow_triggers = Counter(
        (path, tuple(triggers)) for path, triggers in summary["workflow_triggers"]
    )
    if workflow_triggers != EXPECTED_WORKFLOW_TRIGGERS:
        errors.append(
            "target MVP workflow trigger drift: "
            f"expected {dict(EXPECTED_WORKFLOW_TRIGGERS)}, got {dict(workflow_triggers)}"
        )

    job_runs_on = Counter(summary["job_runs_on"])
    if job_runs_on != EXPECTED_JOB_RUNS_ON:
        errors.append(
            "target MVP runs-on drift: "
            f"expected {dict(EXPECTED_JOB_RUNS_ON)}, got {dict(job_runs_on)}"
        )

    workflow_permissions = Counter(
        (path, tuple(permissions)) for path, permissions in summary["workflow_permissions"]
    )
    if workflow_permissions != EXPECTED_WORKFLOW_PERMISSIONS:
        errors.append(
            "target MVP workflow permission drift: "
            f"expected {dict(EXPECTED_WORKFLOW_PERMISSIONS)}, got {dict(workflow_permissions)}"
        )

    job_permissions = Counter(
        (path, job, tuple(permissions))
        for path, job, permissions in summary["job_permissions"]
    )
    if job_permissions != EXPECTED_JOB_PERMISSIONS:
        errors.append(
            "target MVP job permission drift: "
            f"expected {dict(EXPECTED_JOB_PERMISSIONS)}, got {dict(job_permissions)}"
        )

    job_environments = Counter(summary["job_environments"])
    if job_environments != EXPECTED_JOB_ENVIRONMENTS:
        errors.append(
            "target MVP job environment drift: "
            f"expected {dict(EXPECTED_JOB_ENVIRONMENTS)}, got {dict(job_environments)}"
        )

    workflow_concurrency = Counter(summary["workflow_concurrency"])
    if workflow_concurrency != EXPECTED_WORKFLOW_CONCURRENCY:
        errors.append(
            "target MVP workflow concurrency drift: "
            f"expected {dict(EXPECTED_WORKFLOW_CONCURRENCY)}, got {dict(workflow_concurrency)}"
        )

    job_defaults = Counter(summary["job_defaults"])
    if job_defaults != EXPECTED_JOB_DEFAULTS:
        errors.append(
            "target MVP job defaults drift: "
            f"expected {dict(EXPECTED_JOB_DEFAULTS)}, got {dict(job_defaults)}"
        )

    continue_on_error = Counter(summary["continue_on_error"])
    if continue_on_error != EXPECTED_CONTINUE_ON_ERROR:
        errors.append(
            "target MVP continue-on-error drift: "
            f"expected {dict(EXPECTED_CONTINUE_ON_ERROR)}, got {dict(continue_on_error)}"
        )

    for label, key in [
        ("job containers", "containers"),
        ("services", "services"),
        ("job-level concurrency", "job_concurrency"),
        ("job timeouts", "job_timeouts"),
        ("unsupported checkout inputs", "unsupported_checkout_inputs"),
    ]:
        if summary[key]:
            errors.append(f"target MVP does not support {label}: {summary[key]}")

    unexpected_shells = [shell for shell in summary["shells"] if shell != "bash"]
    if unexpected_shells:
        errors.append(f"target MVP supports only explicit bash shells: {unexpected_shells}")

    direct_docker_uses = [
        name for name in summary["uses"] if str(name).startswith("docker://")
    ]
    if direct_docker_uses:
        errors.append(f"target MVP does not support direct docker:// uses: {direct_docker_uses}")

    unexpected_uses = sorted(set(summary["uses"]) - set(EXPECTED_TARGET_USES))
    if unexpected_uses:
        errors.append(f"target MVP action inventory drift: {unexpected_uses}")
    if summary["uses"] != EXPECTED_TARGET_USES:
        errors.append(
            "target MVP action count drift: "
            f"expected {dict(EXPECTED_TARGET_USES)}, got {dict(summary['uses'])}"
        )

    return errors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "roots",
        nargs="+",
        type=Path,
        help="Repository roots containing .github directories",
    )
    parser.add_argument(
        "--check-target-mvp",
        action="store_true",
        help="Fail if target repositories use features outside the current MVP contract",
    )
    args = parser.parse_args()
    roots = [root.resolve() for root in args.roots]
    summary = audit(roots)
    print_report(summary)
    if args.check_target_mvp:
        errors = check_target_mvp(summary)
        if errors:
            for error in errors:
                print(error)
            raise SystemExit(1)


if __name__ == "__main__":
    main()
