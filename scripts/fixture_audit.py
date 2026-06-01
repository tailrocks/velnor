#!/usr/bin/env python3
"""Audit the public fixture repository feature surface.

This is intentionally a fixture contract, not a target-workflow contract. It
checks that the public fixture still contains the small paired GitHub/Velnor
workflows used before manual target repository validation.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import urllib.request
from pathlib import Path


DEFAULT_REPO = "donbeave/velnor-actions-fixture"
DEFAULT_REF = "main"

REQUIRED_SNIPPETS: dict[str, list[tuple[str, str]]] = {
    ".github/workflows/compat.yml": [
        ("GitHub-hosted lane", "runs-on: ubuntu-latest"),
        ("Velnor lane", "runs-on: [self-hosted, velnor-target-mvp]"),
        ("path filtering", "dorny/paths-filter@v4"),
        ("Rust toolchain setup", "dtolnay/rust-toolchain@stable"),
        ("just setup", "extractions/setup-just@v4"),
        ("cache action", "actions/cache@v5"),
        ("artifact upload", "actions/upload-artifact@v7"),
        ("artifact download", "actions/download-artifact@v8"),
        ("command env file", "GITHUB_ENV"),
        ("command output file", "GITHUB_OUTPUT"),
        ("command path file", "GITHUB_PATH"),
        ("step summary file", "GITHUB_STEP_SUMMARY"),
        ("matrix packages", "matrix:"),
        ("compare needs", "needs: [compat-github, compat-velnor]"),
        ("Velnor result gate", "needs.compat-velnor.result"),
        ("fixture output composite", "./.github/actions/check-fixture-output"),
        ("aggregate composite", "./.github/actions/aggregate-needs"),
    ],
    ".github/workflows/docker.yml": [
        ("GitHub-hosted Docker lane", "docker-github:"),
        ("Velnor Docker lane", "docker-velnor:"),
        ("Velnor runner label", "runs-on: [self-hosted, velnor-target-mvp]"),
        ("Buildx setup", "docker/setup-buildx-action@v4"),
        ("Docker build action", "docker/build-push-action@v7"),
        ("non-push build", "push: false"),
        ("loaded image", "load: true"),
        ("container execution", "docker run --rm"),
    ],
    ".github/actions/aggregate-needs/action.yml": [
        ("aggregate composite metadata", "runs:"),
    ],
    ".github/actions/check-fixture-output/action.yml": [
        ("fixture output composite metadata", "runs:"),
    ],
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=DEFAULT_REPO, help="GitHub repo slug")
    parser.add_argument("--ref", default=DEFAULT_REF, help="Git ref to audit")
    parser.add_argument(
        "--fixture-root",
        type=Path,
        help="Audit a local fixture checkout/root instead of GitHub",
    )
    return parser.parse_args()


def github_headers() -> dict[str, str]:
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "velnor-fixture-audit",
    }
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def fetch_github_file(repo: str, ref: str, path: str) -> str:
    url = f"https://api.github.com/repos/{repo}/contents/{path}?ref={ref}"
    request = urllib.request.Request(url, headers=github_headers())
    with urllib.request.urlopen(request, timeout=20) as response:
        payload = json.load(response)
    if payload.get("type") != "file" or "content" not in payload:
        raise RuntimeError(f"{path} is not a file in {repo}@{ref}")
    return base64.b64decode(payload["content"]).decode("utf-8")


def read_fixture_file(args: argparse.Namespace, path: str) -> str:
    if args.fixture_root is not None:
        return (args.fixture_root / path).read_text(encoding="utf-8")
    return fetch_github_file(args.repo, args.ref, path)


def main() -> int:
    args = parse_args()
    failures: list[str] = []

    for path, snippets in REQUIRED_SNIPPETS.items():
        try:
            content = read_fixture_file(args, path)
        except Exception as exc:  # noqa: BLE001 - CLI should report all paths.
            failures.append(f"{path}: missing or unreadable: {exc}")
            continue

        for label, snippet in snippets:
            if snippet not in content:
                failures.append(f"{path}: missing {label}: {snippet}")

    if failures:
        print("fixture audit failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    source = str(args.fixture_root) if args.fixture_root else f"{args.repo}@{args.ref}"
    print(f"fixture audit passed for {source}")
    print(f"checked {len(REQUIRED_SNIPPETS)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
