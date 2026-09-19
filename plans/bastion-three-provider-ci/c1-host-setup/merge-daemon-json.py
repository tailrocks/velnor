#!/usr/bin/env python3
"""Render a safe merge for the C1-managed Docker daemon settings."""

from __future__ import annotations

import json
import pathlib
import sys
from typing import Any


class DuplicateKeyError(ValueError):
    pass


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read_config(path: pathlib.Path) -> dict[str, Any]:
    if path.is_symlink():
        raise ValueError(f"refusing symlinked daemon config: {path}")
    if not path.exists():
        return {}
    if not path.is_file():
        raise ValueError(f"refusing non-regular daemon config: {path}")
    with path.open(encoding="utf-8") as source:
        config = json.load(source, object_pairs_hook=unique_object)
    if not isinstance(config, dict):
        raise ValueError("daemon.json root must be a JSON object")
    return config


def merge_config(path: pathlib.Path, want_pools: bool) -> dict[str, Any]:
    config = read_config(path)
    log_options = config.setdefault("log-opts", {})
    if not isinstance(log_options, dict):
        raise ValueError('daemon.json "log-opts" must be an object')
    log_options["max-size"] = "10m"

    if want_pools:
        desired_pools = [{"base": "172.30.0.0/16", "size": 24}]
        current_pools = config.get("default-address-pools")
        if current_pools is not None and current_pools != desired_pools:
            raise ValueError(
                'refusing to replace existing "default-address-pools"; '
                "review the configured Docker networks first"
            )
        config["default-address-pools"] = desired_pools
    return config


def inspect_config(path: pathlib.Path) -> None:
    config = read_config(path)
    visible_settings = (
        "data-root",
        "live-restore",
        "log-driver",
        "log-opts",
        "default-address-pools",
        "iptables",
        "ip-forward",
        "ip-masq",
        "userland-proxy",
        "bridge",
        "fixed-cidr",
        "cgroup-parent",
        "exec-opts",
    )
    summary = {
        "top-level-keys": sorted(config),
        "review-settings": {
            key: config[key] for key in visible_settings if key in config
        },
    }
    json.dump(summary, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")


def main(argv: list[str]) -> int:
    if len(argv) != 3 or argv[2] not in {"0", "1", "inspect"}:
        print(f"usage: {argv[0]} <daemon.json path> <want-pools: 0|1|inspect>", file=sys.stderr)
        return 2
    path = pathlib.Path(argv[1])
    try:
        if argv[2] == "inspect":
            inspect_config(path)
            return 0
        config = merge_config(path, argv[2] == "1")
    except (OSError, UnicodeError, json.JSONDecodeError, DuplicateKeyError, ValueError) as error:
        print(f"daemon.json preflight failed: {error}", file=sys.stderr)
        return 2
    json.dump(config, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
