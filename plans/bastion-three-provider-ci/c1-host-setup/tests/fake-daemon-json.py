#!/usr/bin/env python3
"""Fake-file tests for the daemon.json merge helper; never reads /etc."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile


TEST_DIR = pathlib.Path(__file__).resolve().parent
HELPER = TEST_DIR.parent / "merge-daemon-json.py"


def run_merge(path: pathlib.Path, want_pools: bool) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(HELPER), str(path), "1" if want_pools else "0"],
        capture_output=True,
        check=False,
        text=True,
    )


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)
    print(f"PASS {message}")


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="velnor-c1-daemon-") as temp_dir:
        root = pathlib.Path(temp_dir)
        config = root / "daemon.json"
        original = {
            "data-root": "/srv/docker-data",
            "live-restore": True,
            "log-opts": {"max-file": "8", "max-size": "2m"},
            "default-address-pools": [{"base": "10.20.0.0/16", "size": 24}],
            "debug": False,
        }
        config.write_text(json.dumps(original, indent=4) + "\n", encoding="utf-8")
        inspected = subprocess.run(
            [sys.executable, str(HELPER), str(config), "inspect"],
            capture_output=True,
            check=False,
            text=True,
        )
        require(inspected.returncode == 0, "read-only Docker config inspection succeeds")
        inventory = json.loads(inspected.stdout)
        require("data-root" in inventory["review-settings"], "preflight reports relevant Docker settings")
        require(inventory["top-level-keys"] == sorted(original), "preflight inventories all top-level config keys")
        first = run_merge(config, False)
        require(first.returncode == 0, "valid custom config merges")
        merged = json.loads(first.stdout)
        require(merged["data-root"] == original["data-root"], "data-root preserved")
        require(merged["live-restore"] is True, "live-restore preserved")
        require(merged["debug"] is False, "unmanaged top-level setting preserved")
        require(merged["log-opts"] == {"max-file": "8", "max-size": "10m"}, "only managed log option changes")
        require(merged["default-address-pools"] == original["default-address-pools"], "unrequested address pools preserved")

        config.write_text(first.stdout, encoding="utf-8")
        second = run_merge(config, False)
        require(second.returncode == 0 and second.stdout == first.stdout, "second merge is byte-idempotent")

        before_conflict = config.read_bytes()
        conflicting_pool = run_merge(config, True)
        require(conflicting_pool.returncode != 0, "requested conflicting pool rejected")
        require(config.read_bytes() == before_conflict, "pool rejection leaves source untouched")

        duplicate = '{"data-root":"/one","data-root":"/two"}\n'
        config.write_text(duplicate, encoding="utf-8")
        duplicate_result = run_merge(config, False)
        require(duplicate_result.returncode != 0, "duplicate JSON keys rejected")
        require(config.read_text(encoding="utf-8") == duplicate, "duplicate-key rejection leaves source untouched")

        config.write_text('{"log-opts":[]}\n', encoding="utf-8")
        invalid_type = run_merge(config, False)
        require(invalid_type.returncode != 0, "managed field with unknown type rejected")

        config.write_text('{"default-address-pools":[{"base":"172.30.0.0/16","size":24}]}\n', encoding="utf-8")
        desired_pool = run_merge(config, True)
        require(desired_pool.returncode == 0, "exact requested pool accepted")
        desired = json.loads(desired_pool.stdout)
        require(desired["default-address-pools"] == [{"base": "172.30.0.0/16", "size": 24}], "exact requested pool preserved")

        target = root / "target.json"
        target.write_text("{}\n", encoding="utf-8")
        link = root / "linked.json"
        link.symlink_to(target)
        symlink_result = run_merge(link, False)
        require(symlink_result.returncode != 0, "symlinked daemon config rejected")
        require(target.read_text(encoding="utf-8") == "{}\n", "symlink rejection leaves target untouched")


if __name__ == "__main__":
    main()
