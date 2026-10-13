#!/usr/bin/env python3
"""Summarize the GitHub Actions surface used by Velnor target repositories."""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
from typing import Any

import yaml


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
) -> None:
    if "uses" in step:
        value = str(step["uses"])
        uses[value] += 1
        if value.startswith("./"):
            local_uses.append((short_path(path, roots), job_name, value))
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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "roots",
        nargs="+",
        type=Path,
        help="Repository roots containing .github directories",
    )
    args = parser.parse_args()
    roots = [root.resolve() for root in args.roots]
    print_report(audit(roots))


if __name__ == "__main__":
    main()
