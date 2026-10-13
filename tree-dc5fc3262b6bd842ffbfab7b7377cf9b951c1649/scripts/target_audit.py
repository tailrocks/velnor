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
    github = root / ".github"
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
            jobs = data.get("jobs") or {}
            if not isinstance(jobs, dict):
                continue
            for job_name, job in jobs.items():
                if not isinstance(job, dict):
                    continue
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
