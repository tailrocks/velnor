#!/usr/bin/env python3
"""Check that Velnor's documented actions/runner reference is current."""

from __future__ import annotations

import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REFERENCE_DOC = ROOT / "docs/research/latest-runner-v2-refresh-2026-06-01.md"
PROTOCOL_SOURCE = ROOT / "crates/velnor-runner/src/protocol.rs"
LATEST_RELEASE_URL = "https://api.github.com/repos/actions/runner/releases/latest"


def main() -> int:
    reference = pinned_reference()
    protocol_version = pinned_protocol_version()
    latest = latest_release()
    if protocol_version != reference.removeprefix("v"):
        print(
            "actions/runner protocol version drift: "
            f"docs pin {reference}, protocol advertises {protocol_version}",
            file=sys.stderr,
        )
        return 1
    if reference != latest:
        print(
            f"actions/runner reference drift: pinned {reference}, latest {latest}",
            file=sys.stderr,
        )
        print(
            "Refresh docs/research/latest-runner-v2-refresh-2026-06-01.md "
            "and re-audit V2 source anchors.",
            file=sys.stderr,
        )
        return 1

    print(f"actions/runner reference current: {reference}")
    return 0


def pinned_reference() -> str:
    text = REFERENCE_DOC.read_text()
    match = re.search(r"latest release checked:\s*`(v[0-9]+\.[0-9]+\.[0-9]+)`", text)
    if not match:
        raise SystemExit(f"could not find pinned runner version in {REFERENCE_DOC}")
    return match.group(1)


def pinned_protocol_version() -> str:
    text = PROTOCOL_SOURCE.read_text()
    match = re.search(r'RUNNER_VERSION:\s*&str\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"', text)
    if not match:
        raise SystemExit(f"could not find RUNNER_VERSION in {PROTOCOL_SOURCE}")
    version = match.group(1)
    expected_agent = f'RUNNER_USER_AGENT: &str = "actions-runner/{version} (velnor)"'
    if expected_agent not in text:
        raise SystemExit(
            f"RUNNER_USER_AGENT in {PROTOCOL_SOURCE} does not match RUNNER_VERSION {version}"
        )
    return version


def latest_release() -> str:
    request = urllib.request.Request(
        LATEST_RELEASE_URL,
        headers={
            "Accept": "application/vnd.github+json",
            "User-Agent": "velnor-runner-reference-check",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        raise SystemExit(f"GitHub release lookup failed: HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise SystemExit(f"GitHub release lookup failed: {error}") from error

    tag_name = payload.get("tag_name")
    if not isinstance(tag_name, str) or not tag_name.startswith("v"):
        raise SystemExit("GitHub latest release response did not include a tag_name")
    return tag_name


if __name__ == "__main__":
    raise SystemExit(main())
