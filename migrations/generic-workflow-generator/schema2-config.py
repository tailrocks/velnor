#!/usr/bin/env python3
"""Parse, migrate, validate, and atomically write a schema-2 generator config."""

from __future__ import annotations

import argparse
import datetime as _datetime
import json
import math
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import tomllib
from typing import Any


class MigrationError(Exception):
    """A config cannot be converted without losing or guessing data."""


def table(fields: dict[str, Any], required: tuple[str, ...] = ()) -> tuple[Any, ...]:
    return ("table", fields, required)


def array(item: Any) -> tuple[str, Any]:
    return ("array", item)


def mapping(value: Any) -> tuple[str, Any]:
    return ("map", value)


STRING = "string"
BOOLEAN = "boolean"
INTEGER = "integer"
UNSIGNED_32 = "unsigned-32"
UNSIGNED_64 = "unsigned-64"
TOML_VALUE = "toml-value"
STRING_LIST = array(STRING)
STRING_MAP = mapping(STRING)

PROVIDERS = ("github-hosted", "github-self-hosted", "velnor")
PROVIDER_ALIASES = {
    "github": "github-hosted",
    "github-hosted": "github-hosted",
    "github-self-hosted": "github-self-hosted",
    "velnor": "velnor",
}
PLATFORMS = ("linux-x64", "linux-arm64", "macos-arm64")
PROVIDER_PLATFORMS = {
    "github-hosted": {"linux-x64", "macos-arm64"},
    "github-self-hosted": {"linux-x64"},
    "velnor": {"linux-x64"},
}

CHECK_PROFILE = table(
    {
        "id": STRING,
        "name": STRING,
        "schedule": STRING,
        "provider": STRING,
        "platform": STRING,
        "tools": STRING_LIST,
        "tasks": STRING_LIST,
        "needs": STRING_LIST,
        "timeout_minutes": INTEGER,
        "artifacts": STRING_LIST,
        "status": STRING,
        "env": STRING_MAP,
    },
    required=("provider", "platform"),
)

RELEASE_JOB = table(
    {
        "id": STRING,
        "name": STRING,
        "tasks": STRING_LIST,
        "needs": STRING_LIST,
        "provider": STRING,
        "platform": STRING,
        "modes": STRING_LIST,
        "timeout_minutes": INTEGER,
        "environment": STRING,
        "attest_subjects": STRING_LIST,
        "permissions": STRING_MAP,
        "env": STRING_MAP,
    },
    required=("provider", "platform"),
)

UNIT_CACHE = table(
    {
        "key_files": STRING_LIST,
        "paths": STRING_LIST,
        "mutable_mount_seed": BOOLEAN,
    }
)

UNIT = table(
    {
        "id": STRING,
        "label": STRING,
        "kind": STRING,
        "root": STRING,
        "watch": STRING_LIST,
        "pr_commands": STRING_LIST,
        "full_commands": STRING_LIST,
        "depends_on": STRING_LIST,
        "cache": UNIT_CACHE,
        "pinned_lockfile": BOOLEAN,
        "tool_version": STRING,
        "workspace_check": BOOLEAN,
        "ci_tasks": STRING_LIST,
        "mise_tools": STRING_LIST,
        "trust": STRING,
        "platform": STRING,
        "env": STRING_MAP,
        "mbx": BOOLEAN,
        "products": array(
            table({"name": STRING, "task": STRING, "env": STRING_MAP})
        ),
        "prerequisites": array(
            table(
                {
                    "producer": STRING,
                    "product": STRING,
                    "task": STRING,
                    "env": STRING_MAP,
                }
            )
        ),
        "docker_contexts": array(table({"name": STRING, "path": STRING})),
    }
)

WORKFLOW = table(
    {
        "providers": STRING_LIST,
        "automatic_providers": STRING_LIST,
        "default_dispatch_providers": STRING_LIST,
        "selectors": mapping(table({"runs_on": STRING_LIST})),
        "profile": STRING,
        "verified": BOOLEAN,
        "files": STRING_LIST,
        "templates": STRING,
        "version_bump_units": STRING_LIST,
        "package_update_channels": mapping(STRING_LIST),
        "default_branch": STRING,
        "rust_needs": STRING,
        "concurrency_group": STRING,
        "serial_stack_groups": BOOLEAN,
    }
)

ROOT = table(
    {
        "schema": INTEGER,
        "generator": table({"repository": STRING, "revision": STRING}),
        "workflow": WORKFLOW,
        "scan": table({"exclude": STRING_LIST}),
        "policy": table(
            {
                "dco_required": BOOLEAN,
                "ci_required": BOOLEAN,
                "ruleset_required_status_checks": STRING_LIST,
                "ruleset_external_status_checks": STRING_LIST,
                "action_pin_admission": STRING,
                "actionlint_config_variables_null": BOOLEAN,
            }
        ),
        "release": table(
            {
                "enabled": BOOLEAN,
                "reason": STRING,
                "kind": STRING,
                "package": STRING,
                "packages": STRING_LIST,
                "binary": STRING,
                "targets": STRING_LIST,
                "image": STRING,
                "image_package": STRING,
                "source_repository": STRING,
                "consumer_repository": STRING,
                "artifact_path": STRING,
                "description": STRING,
                "manifest_schema": STRING,
                "apt_arches": STRING_LIST,
                "signer_fingerprint": STRING,
                "passphrase_secret": STRING,
                "signing_key_secret": STRING,
                "keyring_path": STRING,
                "apt_origin": STRING,
                "apt_identity_dir": STRING,
                "apt_feed_url": STRING,
                "retention": INTEGER,
                "dockerfile": STRING,
                "context": STRING,
                "platforms": STRING_LIST,
                "producer_workflow": STRING,
                "producer_conclusion": STRING,
                "modes": STRING_LIST,
                "archive_members": STRING_LIST,
                "archive_checksum": STRING,
                "archive_retention_days": INTEGER,
                "credential": array(
                    table({"name": STRING, "setup": STRING, "teardown": STRING})
                ),
                "tag_pattern": STRING,
                "registry": STRING,
                "registry_username_secret": STRING,
                "registry_password_secret": STRING,
                "job": array(RELEASE_JOB),
            }
        ),
        "renovate": table(
            {
                "enabled": BOOLEAN,
                "reason": STRING,
                "token": STRING,
                "config": STRING,
                "validate": BOOLEAN,
                "cache": BOOLEAN,
                "repositories": STRING_LIST,
                "host_rules_secret": STRING,
                "author": STRING,
                "signoff": BOOLEAN,
                "allowed_commands": STRING_LIST,
            }
        ),
        "maintenance": table(
            {"schedule": STRING, "producers": STRING_LIST, "max_deletes": UNSIGNED_32}
        ),
        "docs": table(
            {
                "enabled": BOOLEAN,
                "reason": STRING,
                "site_url": STRING,
                "site_dir": STRING,
                "sitemap_path": STRING,
                "schedule": STRING,
                "build_commands": STRING_LIST,
                "source_link_commands": STRING_LIST,
                "site_link_commands": STRING_LIST,
                "spell_commands": STRING_LIST,
                "verify_commands": STRING_LIST,
                "external_link_commands": STRING_LIST,
                "docs_paths": STRING_LIST,
            }
        ),
        "check_profile": array(CHECK_PROFILE),
        "units": array(UNIT),
        "static_files": array(table({"file": STRING, "source": STRING})),
        "declare": array(
            table(
                {
                    "primitive": STRING,
                    "units": STRING_LIST,
                    "file": STRING,
                    "args": mapping(TOML_VALUE),
                }
            )
        ),
        "cache": table(
            {
                "github": table(
                    {
                        "budget_bytes": UNSIGNED_64,
                        "producer_window_seconds": UNSIGNED_64,
                        "mbx_generation_bound": UNSIGNED_32,
                    }
                ),
                "velnor": table(
                    {
                        "budget_bytes": UNSIGNED_64,
                        "producer_window_seconds": UNSIGNED_64,
                        "mbx_generation_bound": UNSIGNED_32,
                    }
                ),
            }
        ),
    },
    required=("schema",),
)

LEGACY_WORKFLOW_FIELDS = {
    "runners",
    "automatic",
    "automatic_lanes",
    "default_dispatch_runner",
    "github_runner",
    "velnor_labels",
}


def _is_integer(value: Any) -> bool:
    return type(value) is int


def _validate_toml_value(value: Any, path: str) -> None:
    if value is None:
        raise MigrationError(f"{path} is not a TOML value")
    if type(value) in (str, bool, int, float):
        if type(value) is int and not (-(2**63) <= value < 2**63):
            raise MigrationError(f"{path} exceeds TOML's signed 64-bit integer range")
        return
    if isinstance(value, (_datetime.date, _datetime.time, _datetime.datetime)):
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            _validate_toml_value(item, f"{path}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise MigrationError(f"{path} has a non-string TOML key")
            _validate_toml_value(item, f"{path}.{key}")
        return
    raise MigrationError(f"{path} has unsupported TOML value type {type(value).__name__}")


def _validate_value(value: Any, spec: Any, path: str) -> None:
    if spec == STRING:
        valid = isinstance(value, str)
    elif spec == BOOLEAN:
        valid = type(value) is bool
    elif spec == INTEGER:
        valid = _is_integer(value) and -(2**63) <= value < 2**63
    elif spec == UNSIGNED_32:
        valid = _is_integer(value) and 0 <= value < 2**32
    elif spec == UNSIGNED_64:
        valid = _is_integer(value) and 0 <= value < 2**64
    elif spec == TOML_VALUE:
        _validate_toml_value(value, path)
        return
    elif isinstance(spec, tuple) and spec[0] == "array":
        if not isinstance(value, list):
            raise MigrationError(f"{path} must be an array")
        for index, item in enumerate(value):
            _validate_value(item, spec[1], f"{path}[{index}]")
        return
    elif isinstance(spec, tuple) and spec[0] == "map":
        if not isinstance(value, dict):
            raise MigrationError(f"{path} must be a table")
        for key, item in value.items():
            if not isinstance(key, str):
                raise MigrationError(f"{path} must have string keys")
            _validate_value(item, spec[1], f"{path}.{key}")
        return
    elif isinstance(spec, tuple) and spec[0] == "table":
        _validate_table(value, spec[1], spec[2], path)
        return
    else:
        raise MigrationError(f"internal error: unsupported field spec {spec!r}")

    if not valid:
        expected = {
            STRING: "a string",
            BOOLEAN: "a boolean",
            INTEGER: "a signed 64-bit integer",
            UNSIGNED_32: "an unsigned 32-bit integer",
            UNSIGNED_64: "an unsigned 64-bit integer",
        }[spec]
        raise MigrationError(f"{path} must be {expected}")


def _validate_table(value: Any, fields: dict[str, Any], required: tuple[str, ...], path: str) -> None:
    if not isinstance(value, dict):
        raise MigrationError(f"{path} must be a TOML table")

    unknown = sorted(key for key in value if key not in fields)
    if unknown:
        field = unknown[0]
        if field == "runner":
            hint = "; replace it with explicit provider/platform fields or rerun from schema = 1 for legacy conversion"
        else:
            hint = "; remove it or add an explicit S2 field mapping"
        raise MigrationError(f"{path}.{field} is not an S2 field{hint}")

    for field in required:
        if field not in value:
            raise MigrationError(f"{path} is missing required S2 field `{field}`")
    for field, item in value.items():
        _validate_value(item, fields[field], f"{path}.{field}")


def _legacy_provider_values(value: Any, path: str) -> list[str]:
    if isinstance(value, str):
        values: list[Any] = [value]
    elif isinstance(value, list):
        values = value
    else:
        raise MigrationError(f"{path} must be a provider name or array of provider names")

    translated: list[str] = []
    for item in values:
        if not isinstance(item, str):
            raise MigrationError(f"{path} contains a non-string provider")
        if item == "both":
            translated.extend(("github-hosted", "velnor"))
        elif item in PROVIDER_ALIASES:
            translated.append(PROVIDER_ALIASES[item])
        else:
            raise MigrationError(
                f"{path} value {item!r} cannot migrate; use github, velnor, both, or a typed S2 provider id"
            )
    if len(set(translated)) != len(translated):
        raise MigrationError(f"{path} repeats a provider after legacy aliases are expanded")
    if not translated:
        raise MigrationError(f"{path} must name at least one provider")
    return translated


def _legacy_selector_labels(value: Any, path: str) -> list[str]:
    if isinstance(value, str):
        labels = [value]
    elif isinstance(value, list) and all(isinstance(item, str) for item in value):
        labels = value
    else:
        raise MigrationError(f"{path} must be a runner label or an array of labels")
    if not labels or any(not label or any(ord(char) < 32 or ord(char) == 127 for char in label) for label in labels):
        raise MigrationError(f"{path} must contain non-empty labels without control characters")
    return labels


def _set_provider_field(workflow: dict[str, Any], field: str, value: Any, source: str) -> None:
    translated = _legacy_provider_values(value, f"[workflow] {source}")
    if field in workflow:
        existing = _legacy_provider_values(workflow[field], f"[workflow] {field}")
        if set(existing) != set(translated):
            raise MigrationError(
                f"[workflow] {field} conflicts with legacy {source}; update one source so the migration does not discard a provider choice"
            )
        workflow[field] = existing
    else:
        workflow[field] = translated


def _legacy_provider(value: Any, path: str) -> str:
    if not isinstance(value, str):
        raise MigrationError(f"{path} must be a provider name")
    try:
        return PROVIDER_ALIASES[value]
    except KeyError as error:
        raise MigrationError(
            f"{path} value {value!r} cannot migrate; set provider to github-hosted, github-self-hosted, or velnor"
        ) from error


def _legacy_platform(value: Any, path: str) -> str:
    if not isinstance(value, str):
        raise MigrationError(f"{path} must be a platform name")
    aliases = {
        "linux": "linux-x64",
        "linux-x64": "linux-x64",
        "linux-arm64": "linux-arm64",
        "macos": "macos-arm64",
        "macos-arm64": "macos-arm64",
    }
    try:
        return aliases[value]
    except KeyError as error:
        raise MigrationError(
            f"{path} value {value!r} cannot migrate; use linux-x64, linux-arm64, or macos-arm64"
        ) from error


def _migrate_profile_or_release_job(row: Any, path: str) -> None:
    if not isinstance(row, dict):
        raise MigrationError(f"{path} must be a TOML table")

    runner = row.pop("runner", None)
    legacy_provider: str | None = None
    legacy_platform: str | None = None
    if runner is not None:
        if not isinstance(runner, str):
            raise MigrationError(f"{path}.runner must be a legacy runner name")
        routes = {
            "velnor": ("velnor", "linux-x64"),
            "macos": ("github-hosted", "macos-arm64"),
            "macos-arm64": ("github-hosted", "macos-arm64"),
            "github": ("github-hosted", None),
            "github-hosted": ("github-hosted", None),
            "linux": ("github-hosted", "linux-x64"),
            "linux-x64": ("github-hosted", "linux-x64"),
        }
        if runner not in routes:
            raise MigrationError(
                f"{path}.runner value {runner!r} cannot migrate; use runner = \"velnor\", \"macos\", or \"github\" in schema 1, or provide typed provider/platform fields"
            )
        legacy_provider, legacy_platform = routes[runner]

    if "provider" in row:
        row["provider"] = _legacy_provider(row["provider"], f"{path}.provider")
    elif legacy_provider is not None:
        row["provider"] = legacy_provider

    if "platform" in row:
        row["platform"] = _legacy_platform(row["platform"], f"{path}.platform")
    elif legacy_platform is not None:
        row["platform"] = legacy_platform

    if legacy_provider is not None and row.get("provider") != legacy_provider:
        raise MigrationError(f"{path}.runner conflicts with explicit provider")
    if legacy_platform is not None and row.get("platform") != legacy_platform:
        raise MigrationError(f"{path}.runner conflicts with explicit platform")

    # Old `runner = "github"` named a provider but did not choose a host image.
    # The old default lane was Ubuntu, so write that choice explicitly.
    if legacy_provider is not None and "platform" not in row:
        row["platform"] = legacy_platform or "linux-x64"

    if "provider" not in row or "platform" not in row:
        raise MigrationError(
            f"{path} needs provider and platform; add both explicitly or supply a supported schema-1 runner alias"
        )


def _migrate_unit_runner(row: Any, path: str) -> None:
    if not isinstance(row, dict):
        raise MigrationError(f"{path} must be a TOML table")
    runner = row.pop("runner", None)
    if runner is None:
        if "platform" in row and row["platform"] in ("linux", "macos"):
            row["platform"] = _legacy_platform(row["platform"], f"{path}.platform")
        return
    if not isinstance(runner, str):
        raise MigrationError(f"{path}.runner must be a legacy runner name")
    if runner in ("velnor", "github", "github-hosted", "github-self-hosted"):
        raise MigrationError(
            f"{path}.runner = {runner!r} selected a provider, but S2 units have no per-provider runner field; move placement to workflow/job declarations"
        )
    expected = _legacy_platform(runner, f"{path}.runner")
    if "platform" in row:
        actual = _legacy_platform(row["platform"], f"{path}.platform")
        if actual != expected:
            raise MigrationError(f"{path}.runner conflicts with explicit platform")
        row["platform"] = actual
    else:
        row["platform"] = expected


def _add_or_check_selector(
    selectors: dict[str, Any], provider: str, labels: list[str], source: str
) -> None:
    current = selectors.get(provider)
    if current is not None:
        if not isinstance(current, dict) or current.get("runs_on") != labels:
            raise MigrationError(
                f"{source} conflicts with [workflow.selectors.{provider}].runs_on; keep one selector value"
            )
        return
    selectors[provider] = {"runs_on": labels}


def _migrate_schema1(data: dict[str, Any], repository: str, revision: str, branch: str) -> None:
    generator = data.setdefault("generator", {})
    workflow = data.setdefault("workflow", {})
    if not isinstance(generator, dict):
        raise MigrationError("[generator] must be a TOML table")
    if not isinstance(workflow, dict):
        raise MigrationError("[workflow] must be a TOML table")

    if "repository" in generator and generator["repository"] != repository:
        raise MigrationError(
            f"[generator] repository is {generator['repository']!r}, but this migration targets {repository!r}; correct it before migrating"
        )
    generator["repository"] = repository
    generator["revision"] = revision

    if "providers" in workflow:
        workflow["providers"] = _legacy_provider_values(workflow["providers"], "[workflow] providers")
        if "runners" in workflow:
            old_providers = _legacy_provider_values(workflow["runners"], "[workflow] runners")
            if set(old_providers) != set(workflow["providers"]):
                raise MigrationError("[workflow] runners conflicts with providers; resolve the old and typed provider sets")
    elif "runners" in workflow:
        workflow["providers"] = _legacy_provider_values(workflow["runners"], "[workflow] runners")
    else:
        workflow["providers"] = ["github-hosted", "velnor"]

    if "automatic_providers" in workflow:
        workflow["automatic_providers"] = _legacy_provider_values(
            workflow["automatic_providers"], "[workflow] automatic_providers"
        )
    elif "automatic_lanes" in workflow:
        # In schema 1 automatic_lanes took precedence over the broader,
        # transitional `automatic` alias.
        workflow["automatic_providers"] = _legacy_provider_values(
            workflow["automatic_lanes"], "[workflow] automatic_lanes"
        )
    elif "automatic" in workflow:
        workflow["automatic_providers"] = _legacy_provider_values(
            workflow["automatic"], "[workflow] automatic"
        )
    else:
        workflow["automatic_providers"] = ["github-hosted"]

    if "default_dispatch_providers" in workflow:
        workflow["default_dispatch_providers"] = _legacy_provider_values(
            workflow["default_dispatch_providers"], "[workflow] default_dispatch_providers"
        )
    elif "default_dispatch_runner" in workflow:
        workflow["default_dispatch_providers"] = _legacy_provider_values(
            workflow["default_dispatch_runner"], "[workflow] default_dispatch_runner"
        )
    else:
        workflow["default_dispatch_providers"] = ["github-hosted"]

    raw_selectors = workflow.setdefault("selectors", {})
    if not isinstance(raw_selectors, dict):
        raise MigrationError("[workflow.selectors] must be a TOML table")
    selectors: dict[str, Any] = {}
    for provider, selector in raw_selectors.items():
        migrated_provider = PROVIDER_ALIASES.get(provider)
        if migrated_provider is None:
            migrated_provider = provider
        if migrated_provider in selectors and selectors[migrated_provider] != selector:
            raise MigrationError(
                f"[workflow.selectors] aliases for {migrated_provider} have conflicting values"
            )
        selectors[migrated_provider] = selector
    workflow["selectors"] = selectors

    for old_field, provider in (("github_runner", "github-hosted"), ("velnor_labels", "velnor")):
        if old_field in workflow:
            labels = _legacy_selector_labels(workflow[old_field], f"[workflow] {old_field}")
            _add_or_check_selector(selectors, provider, labels, f"[workflow] {old_field}")

    if "default_branch" not in workflow:
        workflow["default_branch"] = branch

    for old_field in LEGACY_WORKFLOW_FIELDS:
        workflow.pop(old_field, None)

    # Schema 1 represented scheduled and release-job placement as an untyped
    # runner label. Emit the exact S2 provider/platform pair before dropping it.
    profiles = data.get("check_profile", [])
    if isinstance(profiles, list):
        for index, row in enumerate(profiles):
            _migrate_profile_or_release_job(row, f"[[check_profile]] row {index + 1}")
    release = data.get("release", {})
    if isinstance(release, dict):
        jobs = release.get("job", [])
        if isinstance(jobs, list):
            for index, row in enumerate(jobs):
                _migrate_profile_or_release_job(row, f"[[release.job]] row {index + 1}")
    units = data.get("units", [])
    if isinstance(units, list):
        for index, row in enumerate(units):
            _migrate_unit_runner(row, f"[[units]] row {index + 1}")

    # Add only selectors the migrated provider set uses. Existing labels are
    # preserved; unknown local provider placement must be stated explicitly.
    providers = set(workflow["providers"])
    if "github-hosted" in providers and "github-hosted" not in selectors:
        selectors["github-hosted"] = {"runs_on": ["ubuntu-24.04"]}
    if "velnor" in providers and "velnor" not in selectors:
        selectors["velnor"] = {"runs_on": ["self-hosted", "velnor-target-mvp"]}
    if "github-self-hosted" in providers and "github-self-hosted" not in selectors:
        raise MigrationError(
            "[workflow] providers selects github-self-hosted but no selector exists; add [workflow.selectors.github-self-hosted].runs_on"
        )

    data["schema"] = 2


def _validate_repo_slug(repository: str) -> None:
    parts = repository.split("/")
    if len(parts) != 2:
        raise MigrationError(f"repository must be owner/repository, found {repository!r}")
    for role, segment in zip(("owner", "repository"), parts):
        if (
            not segment
            or segment.startswith(".")
            or not re.fullmatch(r"[A-Za-z0-9._-]+", segment)
            or segment.lower().endswith(".git")
        ):
            raise MigrationError(f"{role} is not a valid GitHub name in {repository!r}")


def _validate_branch(branch: str) -> None:
    if not isinstance(branch, str) or not branch or not re.fullmatch(r"[A-Za-z0-9._/-]+", branch):
        raise MigrationError(f"default branch {branch!r} is outside the generator's branch alphabet")


def _provider_array(workflow: dict[str, Any], field: str, default: tuple[str, ...]) -> set[str]:
    values = workflow.get(field, list(default))
    if not isinstance(values, list) or not values:
        raise MigrationError(f"[workflow] {field} must name at least one provider")
    if any(not isinstance(value, str) or value not in PROVIDERS for value in values):
        bad = next((value for value in values if not isinstance(value, str) or value not in PROVIDERS), None)
        raise MigrationError(
            f"[workflow] {field} contains {bad!r}; S2 provider ids are github-hosted, github-self-hosted, and velnor"
        )
    if len(values) != len(set(values)):
        raise MigrationError(f"[workflow] {field} repeats a provider")
    return set(values)


def _validate_s2(data: dict[str, Any]) -> None:
    _validate_table(data, ROOT[1], ROOT[2], "config")
    if data.get("schema") != 2:
        raise MigrationError("output schema must be exactly 2")

    generator = data.get("generator", {})
    if not isinstance(generator, dict):
        raise MigrationError("[generator] must be a TOML table")
    repository = generator.get("repository")
    if not isinstance(repository, str):
        raise MigrationError("[generator] repository is required and must be a string")
    _validate_repo_slug(repository)
    revision = generator.get("revision")
    if not isinstance(revision, str) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise MigrationError("[generator] revision must be a lowercase 40-character commit SHA")

    workflow = data.get("workflow", {})
    if not isinstance(workflow, dict):
        raise MigrationError("[workflow] must be a TOML table")
    if "templates" in workflow:
        raise MigrationError("[workflow] templates is not supported by S2; remove imported workflow templates")
    if "default_branch" in workflow:
        _validate_branch(workflow["default_branch"])

    universe = _provider_array(workflow, "providers", PROVIDERS)
    for field in ("automatic_providers", "default_dispatch_providers"):
        if field in workflow:
            selected = _provider_array(workflow, field, PROVIDERS)
            outside = selected - universe
            if outside:
                raise MigrationError(
                    f"[workflow] {field} selects providers outside [workflow] providers: {', '.join(sorted(outside))}"
                )

    selectors = workflow.get("selectors", {})
    if not isinstance(selectors, dict):
        raise MigrationError("[workflow.selectors] must be a TOML table")
    for provider, selector in selectors.items():
        if provider not in PROVIDERS:
            raise MigrationError(f"[workflow.selectors] unknown provider {provider!r}")
        if not isinstance(selector, dict):
            raise MigrationError(f"[workflow.selectors.{provider}] must be a TOML table")
        runs_on = selector.get("runs_on")
        if not isinstance(runs_on, list) or not runs_on:
            raise MigrationError(f"[workflow.selectors.{provider}] runs_on must name at least one label")
        if any(not isinstance(label, str) or not label or any(ord(c) < 32 or ord(c) == 127 for c in label) for label in runs_on):
            raise MigrationError(f"[workflow.selectors.{provider}] runs_on labels must be non-empty strings without controls")

    effective_selectors = {
        "github-hosted": ["ubuntu-24.04"],
        "github-self-hosted": ["bastion-scale-set"],
        "velnor": ["velnor-native"],
    }
    effective_selectors.update({provider: selector["runs_on"] for provider, selector in selectors.items()})
    labels_seen: dict[str, str] = {}
    for provider, labels in effective_selectors.items():
        if provider == "github-hosted":
            valid_image_label = (
                len(labels) == 1
                and re.fullmatch(r"[A-Za-z0-9._-]+", labels[0]) is not None
                and labels[0].lower().startswith(("ubuntu-", "macos-", "windows-"))
                and "${{" not in labels[0]
            )
            if not valid_image_label:
                raise MigrationError(
                    "[workflow.selectors.github-hosted] runs_on must be one static GitHub-hosted image label"
                )
        if len({label.lower() for label in labels}) != len(labels):
            raise MigrationError(f"[workflow.selectors.{provider}] repeats a label")
        for label in labels:
            normalized = label.lower()
            previous = labels_seen.get(normalized)
            if previous is not None and previous != provider:
                raise MigrationError(
                    f"runner label {label!r} is claimed by {previous} and {provider}; provider selectors must be disjoint"
                )
            labels_seen[normalized] = provider

    def validate_job_placement(row: dict[str, Any], owner: str) -> None:
        provider = row["provider"]
        platform = row["platform"]
        if provider not in PROVIDERS:
            raise MigrationError(f"{owner} provider {provider!r} is not an S2 provider id")
        if platform not in PLATFORMS:
            raise MigrationError(f"{owner} platform {platform!r} is not an S2 platform id")
        if provider not in universe:
            raise MigrationError(f"{owner} selects provider {provider!r}, outside [workflow] providers")
        if platform not in PROVIDER_PLATFORMS[provider]:
            raise MigrationError(f"{owner} selects unsupported platform {platform!r} for provider {provider!r}")
        if provider in ("velnor", "github-self-hosted") and provider not in selectors:
            raise MigrationError(f"{owner} selects local provider {provider!r} without [workflow.selectors.{provider}]")

    profiles = data.get("check_profile", [])
    for index, row in enumerate(profiles):
        validate_job_placement(row, f"[[check_profile]] row {index + 1}")
    release = data.get("release", {})
    for index, row in enumerate(release.get("job", [])):
        validate_job_placement(row, f"[[release.job]] row {index + 1}")
    for index, unit in enumerate(data.get("units", [])):
        if "platform" in unit and unit["platform"] not in PLATFORMS:
            raise MigrationError(
                f"[[units]] row {index + 1} platform {unit['platform']!r} is not an S2 platform id"
            )


def _quote_key(value: str) -> str:
    if re.fullmatch(r"[A-Za-z0-9_-]+", value):
        return value
    return json.dumps(value, ensure_ascii=False)


def _inline_value(value: Any, path: str) -> str:
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if type(value) is bool:
        return "true" if value else "false"
    if _is_integer(value):
        return str(value)
    if isinstance(value, float):
        if math.isnan(value):
            return "nan"
        if math.isinf(value):
            return "inf" if value > 0 else "-inf"
        return repr(value)
    if isinstance(value, (_datetime.datetime, _datetime.date, _datetime.time)):
        return value.isoformat()
    if isinstance(value, list):
        inner = ", ".join(_inline_value(item, f"{path}[]") for item in value)
        return f"[{inner}]"
    if isinstance(value, dict):
        inner = ", ".join(
            f"{_quote_key(key)} = {_inline_value(item, f'{path}.{key}')}"
            for key, item in value.items()
        )
        return f"{{ {inner} }}" if inner else "{}"
    raise MigrationError(f"{path} has unsupported value type {type(value).__name__}")


def _is_table_array(value: Any) -> bool:
    return isinstance(value, list) and bool(value) and all(isinstance(item, dict) for item in value)


def _format_array(value: list[Any], path: str) -> str:
    if not value:
        return "[]"
    rows = [f"  {_inline_value(item, f'{path}[]')}," for item in value]
    return "[\n" + "\n".join(rows) + "\n]"


def _toml_text(data: dict[str, Any]) -> str:
    lines: list[str] = []

    def emit(mapping_value: dict[str, Any], path: tuple[str, ...], header: str | None = None) -> None:
        if header is not None:
            if lines and lines[-1] != "":
                lines.append("")
            table_path = ".".join(_quote_key(part) for part in path)
            closing = "]]" if header == "[[" else "]"
            lines.append(f"{header}{table_path}{closing}")

        nested: list[tuple[str, Any]] = []
        for key, value in mapping_value.items():
            if isinstance(value, dict) and value:
                nested.append((key, value))
            elif _is_table_array(value):
                nested.append((key, value))
            elif isinstance(value, list):
                lines.append(f"{_quote_key(key)} = {_format_array(value, '.'.join((*path, key)))}")
            else:
                lines.append(f"{_quote_key(key)} = {_inline_value(value, '.'.join((*path, key)))}")

        for key, value in nested:
            child_path = (*path, key)
            if isinstance(value, dict):
                emit(value, child_path, "[")
            else:
                for row in value:
                    emit(row, child_path, "[[")

    emit(data, ())
    return "\n".join(lines).rstrip() + "\n"


def _default_document(repository: str, revision: str, branch: str) -> dict[str, Any]:
    return {
        "schema": 2,
        "generator": {"repository": repository, "revision": revision},
        "workflow": {
            "providers": ["github-hosted", "velnor"],
            "automatic_providers": ["github-hosted"],
            "default_dispatch_providers": ["github-hosted"],
            "default_branch": branch,
            "selectors": {
                "github-hosted": {"runs_on": ["ubuntu-24.04"]},
                "velnor": {"runs_on": ["self-hosted", "velnor-target-mvp"]},
            },
        },
    }


def _migrate(content: bytes | None, path: Path, repository: str, revision: str, branch: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise MigrationError("--revision must be a lowercase 40-character commit SHA")
    _validate_repo_slug(repository)
    _validate_branch(branch)

    if content is None:
        data = _default_document(repository, revision, branch)
    else:
        try:
            source = content.decode("utf-8")
        except UnicodeDecodeError as error:
            raise MigrationError(f"{path} must be UTF-8 TOML: {error}") from error
        try:
            data = tomllib.loads(source)
        except tomllib.TOMLDecodeError as error:
            raise MigrationError(f"{path} is not valid TOML: {error}; no file was written") from error
        if not isinstance(data, dict):
            raise MigrationError(f"{path} must contain a TOML document")

        schema = data.get("schema")
        if type(schema) is not int or schema not in (1, 2):
            raise MigrationError(
                f"{path} must declare schema = 1 or schema = 2; found {schema!r}; no file was written"
            )
        if schema == 1:
            _migrate_schema1(data, repository, revision, branch)
        else:
            generator = data.setdefault("generator", {})
            workflow = data.setdefault("workflow", {})
            if not isinstance(generator, dict):
                raise MigrationError("[generator] must be a TOML table")
            if not isinstance(workflow, dict):
                raise MigrationError("[workflow] must be a TOML table")
            if "repository" in generator and generator["repository"] != repository:
                raise MigrationError(
                    f"[generator] repository is {generator['repository']!r}, but this migration targets {repository!r}; no file was written"
                )
            generator["repository"] = repository
            generator["revision"] = revision
            workflow.setdefault("default_branch", branch)

    _validate_s2(data)
    migrated = _toml_text(data)
    # Round-trip the exact bytes we plan to replace the file with. This catches
    # serializer mistakes before an existing config can be touched.
    try:
        round_trip = tomllib.loads(migrated)
    except tomllib.TOMLDecodeError as error:
        raise MigrationError(f"internal TOML serialization error: {error}; no file was written") from error
    _validate_s2(round_trip)
    return migrated


def _atomic_write(path: Path, content: bytes) -> None:
    try:
        parent_metadata = path.parent.lstat()
    except FileNotFoundError:
        parent_metadata = None
    except OSError as error:
        raise MigrationError(f"cannot inspect config directory {path.parent}: {error}; no file was written") from error
    if parent_metadata is not None and stat.S_ISLNK(parent_metadata.st_mode):
        raise MigrationError(
            f"refusing to write through symlinked config directory {path.parent}; no file was written"
        )
    if parent_metadata is not None and not stat.S_ISDIR(parent_metadata.st_mode):
        raise MigrationError(
            f"config directory {path.parent} is not a directory; no file was written"
        )

    try:
        metadata = path.lstat()
    except FileNotFoundError:
        metadata = None
    except OSError as error:
        raise MigrationError(f"cannot inspect {path}: {error}; no file was written") from error

    if metadata is not None and (stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode)):
        raise MigrationError(f"refusing to replace non-regular config path {path}; no file was written")

    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise MigrationError(f"cannot create config directory {path.parent}: {error}") from error

    temporary_path: Path | None = None
    try:
        descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.s2-", dir=path.parent)
        temporary_path = Path(temporary_name)
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        if metadata is not None:
            os.chmod(temporary_path, stat.S_IMODE(metadata.st_mode))
        else:
            os.chmod(temporary_path, 0o644)
        os.replace(temporary_path, path)
        temporary_path = None
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except OSError as error:
        raise MigrationError(f"cannot atomically write {path}: {error}") from error
    finally:
        if temporary_path is not None:
            try:
                temporary_path.unlink()
            except OSError:
                pass


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--default-branch", required=True)
    arguments = parser.parse_args()

    try:
        try:
            content = arguments.config.read_bytes()
        except FileNotFoundError:
            content = None
        except OSError as error:
            raise MigrationError(f"cannot read {arguments.config}: {error}; no file was written") from error

        migrated = _migrate(
            content,
            arguments.config,
            arguments.repository,
            arguments.revision,
            arguments.default_branch,
        ).encode("utf-8")
        if content == migrated:
            return 0
        _atomic_write(arguments.config, migrated)
    except MigrationError as error:
        print(f"Cannot migrate generation config: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
