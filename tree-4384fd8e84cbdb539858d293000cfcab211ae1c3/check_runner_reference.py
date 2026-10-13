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
LATEST_RELEASE_URL = "https://api.github.com/repos/actions/runner/releases/latest"


def main() -> int:
    reference = pinned_reference()
    latest = latest_release()
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
