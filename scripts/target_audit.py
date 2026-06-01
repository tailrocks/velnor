#!/usr/bin/env python3
"""Summarize the GitHub Actions surface used by Velnor target repositories."""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
from typing import Any

import yaml


EXPECTED_TARGET_USES = {
    "actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae",
    "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd",
    "actions/deploy-pages@cd2ce8fcbc39b97be8ca5fce6e763baed58fa128",
    "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
    "actions/setup-python@a309ff8b426b58ec0e2a45f0f869d46889d02405",
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "actions/upload-pages-artifact@fc324d3547104276b827a68afc52ff2a11cc49c9",
    "baptiste0928/cargo-install@f204293d9709061b7bc1756fec3ec4e2cd57dec0",
    "crazy-max/ghaction-github-runtime@04d248b84655b509d8c44dc1d6f990c879747487",
    "docker/bake-action@6614cfa25eff9a0b2b2697efb0b6159e7680d584",
    "docker/build-push-action@f9f3042f7e2789586610d6e8b85c8f03e5195baf",
    "docker/login-action@650006c6eb7dba73a995cc03b0b2d7f5ca915bee",
    "docker/metadata-action@80c7e94dd9b9319bd5eb7a0e0fe9291e23a2a2e9",
    "docker/setup-buildx-action@d7f5e7f509e45cec5c76c4d5afdd7de93d0b3df5",
    "dorny/paths-filter@fbd0ab8f3e69293af611ebaee6363fc25e6d187d",
    "dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8",
    "extractions/setup-just@53165ef7e734c5c07cb06b3c8e7b647c5aa16db3",
    "jdx/mise-action@1648a7812b9aeae629881980618f079932869151",
    "mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696",
    "renovatebot/github-action@693b9ef15eec82123529a37c782242f091365961",
    "rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444",
    "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
    "./.github/actions/aggregate-needs",
    "./.github/actions/check-deployed-docs",
}

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
        uses[value] += 1
        if value.startswith("./"):
            local_uses.append((short_path(path, roots), job_name, value))
        if value.startswith("actions/checkout@"):
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
    workflow_permissions: list[tuple[str, list[str]]] = []
    workflow_concurrency: list[tuple[str, str]] = []
    job_runs_on: list[tuple[str, str, str]] = []
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
        "workflow_permissions": workflow_permissions,
        "workflow_concurrency": workflow_concurrency,
        "job_runs_on": job_runs_on,
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

    unexpected_uses = sorted(set(summary["uses"]) - EXPECTED_TARGET_USES)
    if unexpected_uses:
        errors.append(f"target MVP action inventory drift: {unexpected_uses}")

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
