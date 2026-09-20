#!/usr/bin/env python3
"""Bounded, source-free Cargo prefetch construction and validation.

The builder consumes only bytes addressed by the current, clean Git commit.
It emits manifests, a lockfile, toolchain metadata, and empty target stubs;
candidate Rust and build scripts never enter the bundle.

Network admission is deliberately not implemented here.  The emitted policy
names reviewed public sources, but a separately reviewed container/firewall
boundary must enforce it before a networked Cargo fetch is allowed.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Mapping, Sequence
from urllib.parse import urlparse


SCHEMA = "bootstrap.prefetch.v2"
BASE_CONTRACT_SCHEMA = "bootstrap.prefetch.contract.v1"
BASE_CONTRACT_PRODUCER = "base-owned-prefetch-contract.v1"
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
REGISTRY_TRANSPORT = "sparse+https://index.crates.io/"
DEFAULT_PATH = "/usr/local/bin:/usr/bin:/bin"
DEFAULT_TARGET = "x86_64-unknown-linux-gnu"
DEFAULT_MAX_MANIFEST_BYTES = 512 * 1024
DEFAULT_MAX_LOCK_BYTES = 8 * 1024 * 1024
DEFAULT_MAX_FILE_BYTES = 8 * 1024 * 1024 * 1024
DEFAULT_MAX_FILES = 200_000
DEFAULT_MAX_CACHE_BYTES = 8 * 1024 * 1024 * 1024
DEFAULT_MAX_DIRECTORIES = 200_000
DEFAULT_MAX_PATH_DEPTH = 128
_HEX40 = re.compile(r"^[0-9a-f]{40}$")
_HEX64 = re.compile(r"^[0-9a-f]{64}$")
_SECTION = re.compile(r"^\s*(\[\[|\[)([A-Za-z0-9_.-]+)(\]\]|\])\s*(?:#.*)?$")
_KEY = re.compile(r"^\s*([A-Za-z0-9_.-]+)\s*=")
_TARGET_SECTIONS = {"lib", "bin", "example", "test", "bench"}


class PrefetchError(ValueError):
    """Fail-closed input or output contract violation."""


@dataclass(frozen=True)
class Limits:
    """Resource bounds used by source ingestion and trusted cache copying."""

    max_manifest_bytes: int = DEFAULT_MAX_MANIFEST_BYTES
    max_lock_bytes: int = DEFAULT_MAX_LOCK_BYTES
    max_file_bytes: int = DEFAULT_MAX_FILE_BYTES
    max_files: int = DEFAULT_MAX_FILES
    max_cache_bytes: int = DEFAULT_MAX_CACHE_BYTES
    max_path_bytes: int = 4096
    max_directories: int = DEFAULT_MAX_DIRECTORIES
    max_path_depth: int = DEFAULT_MAX_PATH_DEPTH


@dataclass(frozen=True)
class BundleContract:
    """Base-owned admission inputs; bundle self-fields are never authority."""

    source_head_sha: str
    source_tree_sha: str
    dependency_contract_digest: str
    toolchain_input_digest: str
    closure: Mapping[str, Any]
    recipe: Mapping[str, Any]
    git_sources: tuple[tuple[str, str], ...] = ()
    git_provenance: tuple[tuple[str, str, str], ...] = ()


@dataclass(frozen=True)
class BaseOwnedContract:
    """A contract loaded through the isolated base-owned transport boundary."""

    contract: BundleContract
    trusted_path: Path
    trusted_root: Path
    candidate_root: Path
    envelope_sha256: str


@dataclass(frozen=True)
class _TreeEntry:
    mode: str
    kind: str
    object_id: str


@dataclass(frozen=True)
class _GitSnapshot:
    """A clean worktree plus the exact commit/tree that supplies its bytes."""

    root: Path
    commit: str
    tree: str
    entries: Mapping[str, _TreeEntry]

    def read(self, relative: str, field: str, limit: int) -> bytes:
        relative = _safe_relative(relative, field)
        entry = self.entries.get(relative)
        if entry is None:
            _fail(f"{field} is not tracked by the reviewed Git tree")
        if entry.mode != "100644" or entry.kind != "blob":
            _fail(
                f"{field} must be a 100644 regular Git blob, "
                f"got mode={entry.mode} type={entry.kind}"
            )
        path = self.root / relative
        # Validate worktree shape, but never consume its bytes.  The source
        # content below comes only from the content-addressed Git blob.
        worktree = _lstat_regular(
            path,
            field,
            limit,
            single_link=True,
            exact_mode=0o644,
        )
        expected = _git_bytes(self.root, ["cat-file", "blob", f"{self.commit}:{relative}"])
        if len(expected) > limit:
            _fail(f"{field} exceeds {limit} bytes")
        if worktree != expected:
            _fail(f"{field} worktree bytes differ from Git {self.commit}:{relative}")
        # A status check followed by a worktree path read can race, so the
        # comparison is only a clean-shape check.  Emitted bytes are always
        # the reviewed content-addressed Git blob above.
        return expected


def _fail(message: str) -> None:
    raise PrefetchError(message)


def _canonical(value: Any) -> bytes:
    """Canonical JSON preimage: UTF-8, sorted keys, no insignificant space."""

    return json.dumps(
        value,
        ensure_ascii=True,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def bundle_contract_payload(contract: BundleContract) -> dict[str, Any]:
    """Serialize contract data; callers must still use base transport to trust it."""

    if not isinstance(contract, BundleContract):
        _fail("BundleContract is required")
    return {
        "source_head_sha": contract.source_head_sha,
        "source_tree_sha": contract.source_tree_sha,
        "dependency_contract_digest": contract.dependency_contract_digest,
        "toolchain_input_digest": contract.toolchain_input_digest,
        "closure": contract.closure,
        "recipe": contract.recipe,
        "git_sources": [
            {"url": url, "rev": rev}
            for url, rev in contract.git_sources
        ],
        "git_provenance": [
            {"url": url, "rev": rev, "tree": tree}
            for url, rev, tree in contract.git_provenance
        ],
    }


def _contract_from_payload(value: Mapping[str, Any]) -> BundleContract:
    expected_keys = {
        "source_head_sha",
        "source_tree_sha",
        "dependency_contract_digest",
        "toolchain_input_digest",
        "closure",
        "recipe",
        "git_sources",
        "git_provenance",
    }
    if not isinstance(value, Mapping) or set(value) != expected_keys:
        _fail("base-owned contract payload schema mismatch")
    git_sources_value = value["git_sources"]
    if not isinstance(git_sources_value, list):
        _fail("base-owned git_sources must be an array")
    git_sources: list[tuple[str, str]] = []
    for item in git_sources_value:
        if not isinstance(item, Mapping) or set(item) != {"url", "rev"}:
            _fail("base-owned git_sources entry schema mismatch")
        git_sources.append((item["url"], item["rev"]))
    provenance_value = value["git_provenance"]
    if not isinstance(provenance_value, list):
        _fail("base-owned git_provenance must be an array")
    provenance: list[tuple[str, str, str]] = []
    for item in provenance_value:
        if not isinstance(item, Mapping) or set(item) != {"url", "rev", "tree"}:
            _fail("base-owned git_provenance entry schema mismatch")
        provenance.append((item["url"], item["rev"], item["tree"]))
    return BundleContract(
        value["source_head_sha"],
        value["source_tree_sha"],
        value["dependency_contract_digest"],
        value["toolchain_input_digest"],
        value["closure"],
        value["recipe"],
        tuple(git_sources),
        tuple(provenance),
    )


def load_base_owned_contract(
    path: Path,
    *,
    trusted_root: Path,
    candidate_root: Path,
    expected_sha256: str,
) -> BaseOwnedContract:
    """Load only a canonical contract envelope from a disjoint trusted root.

    The expected digest and trusted-root placement are supplied by the
    base-owned transport.  This loader is a boundary check, not a cryptographic
    claim about the Python object or its producer.
    """

    _digest(expected_sha256, "base-owned contract envelope digest")
    trusted_root = trusted_root.absolute()
    candidate_root = candidate_root.absolute()
    for root, field in ((trusted_root, "trusted contract root"), (candidate_root, "candidate root")):
        try:
            info = root.lstat()
        except OSError as exc:
            _fail(f"{field} is unavailable: {exc}")
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            _fail(f"{field} must be a non-symlink directory")
    trusted_root = trusted_root.resolve()
    candidate_root = candidate_root.resolve()
    if (
        trusted_root == candidate_root
        or trusted_root.is_relative_to(candidate_root)
        or candidate_root.is_relative_to(trusted_root)
    ):
        _fail("trusted and candidate contract roots must be disjoint")
    path = path.absolute()
    try:
        relative = path.resolve().relative_to(trusted_root)
    except ValueError:
        _fail("base-owned contract path must be inside the trusted root")
    current = trusted_root
    for component in relative.parts[:-1]:
        current = current / component
        info = current.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            _fail("base-owned contract path has an unsafe parent")
    raw = _lstat_regular(path, "base-owned contract envelope", 4 * 1024 * 1024, single_link=True, exact_mode=0o644)
    if _sha256(raw) != expected_sha256:
        _fail("base-owned contract envelope digest mismatch")
    try:
        envelope = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        _fail(f"base-owned contract envelope is not valid JSON: {exc}")
    if not isinstance(envelope, Mapping) or set(envelope) != {"schema", "producer", "contract"}:
        _fail("base-owned contract envelope schema mismatch")
    if envelope["schema"] != BASE_CONTRACT_SCHEMA or envelope["producer"] != BASE_CONTRACT_PRODUCER:
        _fail("unsupported base-owned contract envelope")
    if raw != _canonical(envelope):
        _fail("base-owned contract envelope is not canonical JSON")
    return BaseOwnedContract(
        contract=_contract_from_payload(envelope["contract"]),
        trusted_path=path,
        trusted_root=trusted_root,
        candidate_root=candidate_root,
        envelope_sha256=expected_sha256,
    )


def _digest(value: str, field: str) -> None:
    if not isinstance(value, str) or not _HEX64.fullmatch(value):
        _fail(f"{field} must be a bare lowercase 64-hex SHA-256 digest")


def _identity(value: str, field: str) -> None:
    if not isinstance(value, str) or not _HEX40.fullmatch(value):
        _fail(f"{field} must be a lowercase 40-hex Git identity")


def _integer(value: Any, field: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        _fail(f"{field} must be a non-negative JSON number, not a string")


def _safe_relative(value: str, field: str, *, glob: bool = False) -> str:
    if not isinstance(value, str) or not value or "\\" in value:
        _fail(f"{field} must be a non-empty POSIX relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        _fail(f"{field} contains an absolute, empty, '.', or '..' component")
    if not glob and any(char in value for char in "*?["):
        _fail(f"{field} must not contain a glob")
    return path.as_posix()


def _bounded_relative(value: str, field: str, limits: Limits) -> str:
    relative = _safe_relative(value, field)
    if len(relative.encode("utf-8")) > limits.max_path_bytes:
        _fail(f"{field} exceeds the path-byte bound")
    if len(PurePosixPath(relative).parts) > limits.max_path_depth:
        _fail(f"{field} exceeds the path-depth bound")
    return relative


def _validate_dependency_path(
    value: str,
    field: str,
    *,
    manifest_dir: Path,
    source_root: Path,
    member_dirs: set[Path],
) -> None:
    """Allow Cargo's member-relative parent paths, but never escape."""

    if not isinstance(value, str) or not value or "\\" in value:
        _fail(f"{field} must be a non-empty POSIX relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".") for part in path.parts):
        _fail(f"{field} must be a clean relative path")
    resolved = (manifest_dir / value).resolve()
    root = source_root.resolve()
    if not resolved.is_relative_to(root) or resolved not in member_dirs:
        _fail(f"{field} escapes the allowlisted workspace members")


def _lstat_regular(
    path: Path,
    field: str,
    limit: int,
    *,
    single_link: bool = False,
    exact_mode: int | None = None,
) -> bytes:
    try:
        info = path.lstat()
    except OSError as exc:
        _fail(f"read {field}: {exc}")
    if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
        _fail(f"{field} must be a regular non-symlink file")
    if single_link and info.st_nlink != 1:
        _fail(f"{field} must not be a hardlink")
    if exact_mode is not None and stat.S_IMODE(info.st_mode) != exact_mode:
        _fail(
            f"{field} has unsafe mode {oct(stat.S_IMODE(info.st_mode))}; "
            f"expected {oct(exact_mode)}"
        )
    if info.st_size > limit:
        _fail(f"{field} exceeds {limit} bytes")
    try:
        return path.read_bytes()
    except OSError as exc:
        _fail(f"read {field}: {exc}")


def _resolve_executable(value: str, path: str, field: str) -> Path:
    if not isinstance(value, str) or not value:
        _fail(f"{field} must be a non-empty executable path or name")
    if "/" in value:
        candidate = Path(value)
    else:
        found = shutil.which(value, path=path)
        if found is None:
            _fail(f"{field} is not resolvable through the reviewed PATH")
        candidate = Path(found)
    candidate = candidate.absolute()
    try:
        info = candidate.lstat()
    except OSError as exc:
        _fail(f"{field} is unavailable: {exc}")
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        _fail(f"{field} must resolve to a regular non-symlink file")
    if not info.st_mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH):
        _fail(f"{field} is not executable")
    return candidate


def _executable_record(value: str, path: str, field: str) -> dict[str, Any]:
    executable = _resolve_executable(value, path, field)
    raw = _lstat_regular(
        executable,
        field,
        DEFAULT_MAX_FILE_BYTES,
        single_link=True,
    )
    info = executable.lstat()
    return {
        "path": str(executable),
        "blob_sha": _sha256(raw),
        "size": len(raw),
        "mode": stat.S_IMODE(info.st_mode),
    }


def _feature_list(features: Sequence[str]) -> list[str]:
    values = list(features)
    if any(not isinstance(item, str) or not item for item in values):
        _fail("features must be a non-empty-string sequence")
    if len(values) != len(set(values)):
        _fail("features must not contain duplicates")
    return values


def _cargo_feature_args(features: Sequence[str]) -> list[str]:
    values = _feature_list(features)
    if values == ["default"]:
        return []
    if "default" in values:
        values = [item for item in values if item != "default"]
        return ["--features", ",".join(values)] if values else []
    result = ["--no-default-features"]
    if values:
        result.extend(("--features", ",".join(values)))
    return result


def builder_recipe_contract(
    *,
    cargo_bin: str,
    rustc_bin: str,
    path: str,
    builder_base_digest: str | None,
    target_package: str,
    target_triple: str,
    manifest: str,
    features: Sequence[str] = ("default",),
) -> dict[str, Any]:
    """Return measured executable/base identities for the fixed closure recipe."""

    if builder_base_digest is not None:
        _digest(builder_base_digest, "builder_base_digest")
    _safe_relative(manifest, "recipe manifest")
    if not isinstance(target_package, str) or not target_package:
        _fail("recipe target_package is required")
    if not isinstance(target_triple, str) or not target_triple:
        _fail("recipe target_triple is required")
    if not isinstance(path, str) or not path:
        _fail("recipe PATH is required")
    feature_values = _feature_list(features)
    command = [
        "cargo",
        "tree",
        "--locked",
        "--offline",
        "--target",
        target_triple,
        "--edges",
        "normal,build",
        "--prefix",
        "none",
        "--format",
        "{p}",
        *_cargo_feature_args(feature_values),
        "--manifest-path",
        manifest,
    ]
    return {
        "schema": "bootstrap.prefetch.recipe.v1",
        "base_image_digest": builder_base_digest,
        "cargo": _executable_record(cargo_bin, path, "cargo_bin"),
        "rustc": _executable_record(rustc_bin, path, "rustc_bin"),
        "path": path,
        "target_package": target_package,
        "target_triple": target_triple,
        "features": feature_values,
        "manifest": manifest,
        "command": command,
    }


def _parse_toml(raw: bytes, field: str) -> dict[str, Any]:
    try:
        parsed = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        _fail(f"parse {field}: {exc}")
    if not isinstance(parsed, dict):
        _fail(f"{field} must decode to a TOML table")
    return parsed


def _load_toml(path: Path, field: str, limits: Limits) -> tuple[bytes, dict[str, Any]]:
    limit = limits.max_lock_bytes if path.name == "Cargo.lock" else limits.max_manifest_bytes
    raw = _lstat_regular(path, field, limit)
    return raw, _parse_toml(raw, field)


def _git_environment() -> dict[str, str]:
    return {
        "HOME": tempfile.gettempdir(),
        "PATH": os.defpath,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_ASKPASS": "/bin/false",
    }


def _git_process(
    directory: Path,
    args: Sequence[str],
    *,
    binary: bool = False,
) -> subprocess.CompletedProcess[bytes] | subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(directory), *args],
        check=False,
        capture_output=True,
        text=not binary,
        env=_git_environment(),
    )


def _git_run(directory: Path, args: Sequence[str]) -> str:
    result = _git_process(directory, args)
    if result.returncode != 0:
        _fail(
            f"git command failed in {directory}: {args}: "
            f"{result.stderr.strip()}"
        )
    return result.stdout.strip()


def _git_origin_value(directory: Path, field: str) -> str:
    result = _git_process(directory, ["config", "--get-all", "remote.origin.url"])
    if result.returncode != 0 or not result.stdout.strip():
        _fail(f"{field} lacks a single recorded Git origin URL")
    values = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if len(values) != 1:
        _fail(f"{field} has multiple Git origin URLs")
    return values[0]


def _git_origin_url(directory: Path, field: str) -> str:
    value = _git_origin_value(directory, field)
    return _validate_git_url(value, field)


def _git_local_origin(directory: Path, field: str) -> Path:
    value = _git_origin_value(directory, field)
    if value.startswith(("https://", "http://", "ssh://", "git@")):
        _fail(f"{field} must point at its exact local Cargo Git DB")
    path = Path(value)
    if not path.is_absolute():
        _fail(f"{field} local origin must be absolute")
    return path.absolute()


def _git_bytes(directory: Path, args: Sequence[str]) -> bytes:
    result = _git_process(directory, args, binary=True)
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        _fail(f"git command failed in {directory}: {args}: {stderr}")
    return result.stdout


def _validated_git_snapshot(
    source_root: Path,
    *,
    expected_revision: str | None,
) -> _GitSnapshot:
    """Resolve and verify the current clean worktree before reading any input."""

    source_root = source_root.absolute()
    try:
        info = source_root.lstat()
    except OSError as exc:
        _fail(f"source root is not accessible: {exc}")
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        _fail("source root must be a non-symlink directory")
    try:
        inside = _git_run(source_root, ["rev-parse", "--is-inside-work-tree"])
        top = Path(_git_run(source_root, ["rev-parse", "--show-toplevel"])).resolve()
    except PrefetchError:
        _fail("source root must be a Git worktree")
    if inside != "true" or top != source_root.resolve():
        _fail("source root is not the exact Git worktree being reviewed")
    commit = _git_run(source_root, ["rev-parse", "--verify", "HEAD^{commit}"])
    tree = _git_run(source_root, ["rev-parse", "--verify", "HEAD^{tree}"])
    _identity(commit, "observed source HEAD")
    _identity(tree, "observed source tree")
    if expected_revision is not None:
        _identity(expected_revision, "source_revision")
        if commit != expected_revision:
            _fail(
                "source_revision does not equal the current Git HEAD: "
                f"expected={expected_revision} observed={commit}"
            )
    status = _git_run(
        source_root,
        ["status", "--porcelain=v1", "--untracked-files=all"],
    )
    if status:
        _fail(f"source Git worktree is not clean: {status}")
    raw_tree = _git_bytes(
        source_root,
        ["ls-tree", "-r", "-z", "--full-tree", "HEAD"],
    )
    entries: dict[str, _TreeEntry] = {}
    for record in raw_tree.split(b"\0"):
        if not record:
            continue
        try:
            metadata, raw_path = record.split(b"\t", 1)
            mode, kind, object_id = metadata.decode("ascii").split(" ")
            relative = raw_path.decode("utf-8")
        except (ValueError, UnicodeDecodeError) as exc:
            _fail(f"malformed Git tree entry: {exc}")
        relative = _safe_relative(relative, "Git tree path", glob=True)
        if relative in entries:
            _fail(f"duplicate Git tree path: {relative}")
        entries[relative] = _TreeEntry(mode, kind, object_id)
    if not entries:
        _fail("reviewed Git tree is empty")
    return _GitSnapshot(source_root.resolve(), commit, tree, entries)


def _member_matches(pattern: str, relative: str) -> bool:
    return fnmatch.fnmatchcase(relative, pattern)


def _workspace_member_paths(
    snapshot: _GitSnapshot,
    root_data: Mapping[str, Any],
    limits: Limits,
) -> list[str]:
    workspace = root_data.get("workspace")
    if not isinstance(workspace, dict):
        _fail("root Cargo.toml has no [workspace] table")
    members = workspace.get("members")
    if not isinstance(members, list) or not members or not all(
        isinstance(item, str) for item in members
    ):
        _fail("[workspace].members must be a non-empty string array")
    excluded = workspace.get("exclude", [])
    if not isinstance(excluded, list) or not all(isinstance(item, str) for item in excluded):
        _fail("[workspace].exclude must be a string array")
    member_patterns = [
        _safe_relative(item, "workspace member", glob=True) for item in members
    ]
    excluded_patterns = [
        _safe_relative(item, "workspace exclude", glob=True) for item in excluded
    ]
    candidates = sorted(
        path[: -len("/Cargo.toml")]
        for path in snapshot.entries
        if path.endswith("/Cargo.toml")
    )
    result: set[str] = set()
    for relative in candidates:
        if not any(_member_matches(pattern, relative) for pattern in member_patterns):
            continue
        if any(_member_matches(pattern, relative) for pattern in excluded_patterns):
            continue
        snapshot.read(
            f"{relative}/Cargo.toml",
            f"{relative}/Cargo.toml",
            limits.max_manifest_bytes,
        )
        result.add(relative)
    if not result:
        _fail("workspace member selection is empty")
    return sorted(result)


def _bundle_workspace_paths(bundle_root: Path, root_data: Mapping[str, Any]) -> list[str]:
    workspace = root_data.get("workspace")
    if not isinstance(workspace, dict):
        _fail("root Cargo.toml has no [workspace] table")
    members = workspace.get("members")
    if not isinstance(members, list) or not members or not all(
        isinstance(item, str) for item in members
    ):
        _fail("[workspace].members must be a non-empty string array")
    excluded = workspace.get("exclude", [])
    if not isinstance(excluded, list) or not all(isinstance(item, str) for item in excluded):
        _fail("[workspace].exclude must be a string array")
    member_patterns = [
        _safe_relative(item, "workspace member", glob=True) for item in members
    ]
    excluded_patterns = [
        _safe_relative(item, "workspace exclude", glob=True) for item in excluded
    ]
    candidates = sorted(
        path.relative_to(bundle_root).parent.as_posix()
        for path in bundle_root.rglob("Cargo.toml")
        if path.relative_to(bundle_root).as_posix() != "Cargo.toml"
    )
    result = {
        relative
        for relative in candidates
        if any(_member_matches(pattern, relative) for pattern in member_patterns)
        and not any(_member_matches(pattern, relative) for pattern in excluded_patterns)
    }
    if not result:
        _fail("bundle workspace member selection is empty")
    return sorted(result)


def _dependency_tables(
    data: Mapping[str, Any],
) -> Iterable[tuple[str, Mapping[str, Any]]]:
    for name in ("dependencies", "dev-dependencies", "build-dependencies"):
        table = data.get(name)
        if table is not None:
            if not isinstance(table, dict):
                _fail(f"{name} must be a TOML table")
            yield name, table
    targets = data.get("target", {})
    if targets is not None:
        if not isinstance(targets, dict):
            _fail("[target] must be a TOML table")
        for target_name, target_data in targets.items():
            if not isinstance(target_data, dict):
                _fail(f"target {target_name} must be a TOML table")
            for name in ("dependencies", "dev-dependencies", "build-dependencies"):
                table = target_data.get(name)
                if table is not None:
                    if not isinstance(table, dict):
                        _fail(f"target {target_name}.{name} must be a TOML table")
                    yield f"target.{target_name}.{name}", table
    workspace = data.get("workspace")
    if workspace is not None:
        if not isinstance(workspace, dict):
            _fail("[workspace] must be a TOML table")
        table = workspace.get("dependencies")
        if table is not None:
            if not isinstance(table, dict):
                _fail("workspace.dependencies must be a TOML table")
            yield "workspace.dependencies", table


def _validate_git_url(value: str, field: str) -> str:
    if not isinstance(value, str) or not value:
        _fail(f"{field} must be a URL")
    candidate = value[4:] if value.startswith("git+") else value
    parsed = urlparse(candidate)
    if (
        parsed.scheme != "https"
        or parsed.hostname != "github.com"
        or parsed.username is not None
        or parsed.password is not None
        or parsed.port is not None
        or not parsed.path
        or parsed.query
        or parsed.fragment
    ):
        _fail(f"{field} must be an HTTPS github.com URL without credentials/query")
    return candidate.rstrip("/")


def _reviewed_git_set(
    reviewed_git: Sequence[tuple[str, str]],
) -> set[tuple[str, str]]:
    result: set[tuple[str, str]] = set()
    for url, rev in reviewed_git:
        url = _validate_git_url(url, "reviewed Git URL")
        if not isinstance(rev, str) or not _HEX40.fullmatch(rev):
            _fail("reviewed Git entries need a URL and full 40-hex revision")
        result.add((url, rev))
    return result


def _git_provenance_map(
    provenance: Sequence[tuple[str, str, str]],
) -> dict[tuple[str, str], str]:
    result: dict[tuple[str, str], str] = {}
    for item in provenance:
        if not isinstance(item, (tuple, list)) or len(item) != 3:
            _fail("Git provenance entries need URL, revision, and tree")
        url, rev, tree = item
        url = _validate_git_url(url, "Git provenance URL")
        if not isinstance(rev, str) or not _HEX40.fullmatch(rev):
            _fail("Git provenance revision must be full lowercase 40-hex")
        _identity(tree, "Git provenance tree")
        key = (url, rev)
        if key in result:
            _fail("Git provenance contains duplicate URL/revision")
        result[key] = tree
    return result


def _validate_dependency_table(
    table_name: str,
    table: Mapping[str, Any],
    *,
    manifest_dir: Path,
    source_root: Path,
    member_dirs: set[Path],
    reviewed_git: set[tuple[str, str]],
) -> set[tuple[str, str]]:
    sources: set[tuple[str, str]] = set()
    for dependency_name, specification in table.items():
        if isinstance(specification, str):
            continue
        if not isinstance(specification, dict):
            _fail(f"{table_name}.{dependency_name} must be a string or table")
        if "registry" in specification and specification["registry"] not in ("crates-io", None):
            _fail(f"unreviewed registry in {table_name}.{dependency_name}")
        for key in ("source", "replace", "path+file"):
            if key in specification:
                _fail(f"unreviewed source key {key} in {table_name}.{dependency_name}")
        if "path" in specification:
            _validate_dependency_path(
                specification["path"],
                f"{table_name}.{dependency_name}.path",
                manifest_dir=manifest_dir,
                source_root=source_root,
                member_dirs=member_dirs,
            )
        if "git" in specification:
            url = specification["git"]
            rev = specification.get("rev")
            if not isinstance(url, str) or not isinstance(rev, str) or not _HEX40.fullmatch(rev):
                _fail(f"git dependency {table_name}.{dependency_name} needs full rev")
            url = _validate_git_url(url, f"git dependency {table_name}.{dependency_name}")
            if any(key in specification for key in ("branch", "tag")):
                _fail(f"floating git selector in {table_name}.{dependency_name}")
            source = (url, rev)
            if source not in reviewed_git:
                _fail(f"unreviewed git dependency {url}@{rev}")
            sources.add(source)
    return sources


def _validate_manifest_sources(
    data: Mapping[str, Any],
    *,
    field: str,
    manifest_dir: Path,
    source_root: Path,
    member_dirs: set[Path],
    reviewed_git: set[tuple[str, str]],
) -> set[tuple[str, str]]:
    for forbidden in ("patch", "replace", "source"):
        if forbidden in data:
            _fail(f"unreviewed top-level [{forbidden}] in {field}")
    sources: set[tuple[str, str]] = set()
    for table_name, table in _dependency_tables(data):
        sources.update(
            _validate_dependency_table(
                table_name,
                table,
                manifest_dir=manifest_dir,
                source_root=source_root,
                member_dirs=member_dirs,
                reviewed_git=reviewed_git,
            )
        )
    package = data.get("package")
    if package is not None and not isinstance(package, dict):
        _fail(f"{field} package table is malformed")
    if package is not None and not isinstance(package.get("name"), str):
        _fail(f"{field} package.name is required")
    return sources


def _target_specs(data: Mapping[str, Any], package_name: str) -> list[tuple[str, int]]:
    specs: list[tuple[str, int]] = []
    for name in ("lib", "bin", "example", "test", "bench"):
        value = data.get(name)
        if value is None:
            continue
        values = [value] if name == "lib" else value
        if not isinstance(values, list) or any(not isinstance(item, dict) for item in values):
            _fail(f"target table {name} is malformed in {package_name}")
        specs.extend((name, index) for index, _ in enumerate(values))
    if not specs:
        specs.append(("lib", 0))
    return specs


def _expected_target_stubs(
    data: Mapping[str, Any],
    *,
    package_name: str,
    member_relative: str,
) -> list[str]:
    specs = _target_specs(data, package_name)
    package_safe = re.sub(r"[^A-Za-z0-9_.-]", "_", package_name)
    stubs: list[str] = []
    for ordinal, (kind, index) in enumerate(specs):
        value = data.get(kind)
        values = [value] if kind == "lib" else value
        if not isinstance(values, list) or index >= len(values):
            _fail(f"target table {kind} is missing in {member_relative}")
        target = values[index]
        if not isinstance(target, dict):
            _fail(f"target table {kind}[{index}] is malformed in {member_relative}")
        expected = f".prefetch-targets/{package_safe}-{kind}-{ordinal}.rs"
        if target.get("path") != expected:
            _fail(f"target {kind}[{index}] is not bound to its trusted stub")
        stubs.append(f"{member_relative}/{expected}")
    return stubs


def _section(line: str) -> str | None:
    match = _SECTION.fullmatch(line.rstrip("\r\n"))
    if not match:
        return None
    opening, name, closing = match.groups()
    if opening == "[" and closing != "]":
        return None
    if opening == "[[" and closing != "]]":
        return None
    return name


def _sanitized_manifest(
    raw: bytes,
    data: Mapping[str, Any],
    *,
    package_name: str,
    member_relative: str,
) -> tuple[bytes, list[str]]:
    """Replace every candidate target path with an empty trusted stub path."""

    if not isinstance(data.get("package"), dict):
        return raw, []
    text = raw.decode("utf-8")
    lines = text.splitlines(keepends=True)
    starts: list[tuple[int, str]] = []
    for index, line in enumerate(lines):
        section = _section(line)
        if section is not None:
            starts.append((index, section))
    target_positions = [
        (index, section)
        for index, section in starts
        if section in _TARGET_SECTIONS
    ]
    specs = _target_specs(data, package_name)
    if len(target_positions) > len(specs):
        _fail(f"target table count changed while sanitizing {member_relative}")
    for kind in ("lib", "bin", "example", "test", "bench"):
        value = data.get(kind)
        if value is None:
            continue
        values = [value] if kind == "lib" else value
        for ordinal, target in enumerate(values):
            if "path" in target:
                _safe_relative(
                    target["path"],
                    f"{member_relative}.{kind}[{ordinal}].path",
                )
    package_safe = re.sub(r"[^A-Za-z0-9_.-]", "_", package_name)
    stubs = [
        f".prefetch-targets/{package_safe}-{kind}-{ordinal}.rs"
        for ordinal, (kind, _) in enumerate(specs)
    ]

    sanitized: list[str] = []
    target_ordinal = 0
    index = 0
    while index < len(lines):
        section = _section(lines[index])
        next_index = index + 1
        while next_index < len(lines) and _section(lines[next_index]) is None:
            next_index += 1
        if section in _TARGET_SECTIONS:
            if target_ordinal >= len(stubs):
                _fail(f"target table overflow in {member_relative}")
            block = list(lines[index:next_index])
            path_indices = [
                offset
                for offset, line in enumerate(block)
                if _KEY.match(line) and _KEY.match(line).group(1) == "path"
            ]
            replacement = f'path = "{stubs[target_ordinal]}"\n'
            if path_indices:
                block[path_indices[0]] = replacement
            else:
                block.insert(1, replacement)
            sanitized.extend(block)
            target_ordinal += 1
            index = next_index
            continue
        for line in lines[index:next_index]:
            if section == "package" and _KEY.match(line):
                key = _KEY.match(line).group(1)
                if key == "build":
                    continue
            sanitized.append(line)
        index = next_index

    if not target_positions:
        if sanitized and not sanitized[-1].endswith("\n"):
            sanitized.append("\n")
        sanitized.extend(["\n[lib]\n", f'path = "{stubs[0]}"\n'])
    return "".join(sanitized).encode("utf-8"), stubs


def _git_source(source: str) -> tuple[str, str] | None:
    if not isinstance(source, str) or not source.startswith("git+"):
        return None
    value = source[4:]
    if "#" not in value or "?rev=" not in value:
        _fail(f"git lock source lacks exact rev: {source}")
    before_fragment, fragment = value.rsplit("#", 1)
    url, rev = before_fragment.split("?rev=", 1)
    if (
        not url
        or not rev
        or fragment != rev
        or not _HEX40.fullmatch(rev)
        or "?" in rev
        or "#" in rev
    ):
        _fail(f"malformed git lock source: {source}")
    return url, rev


def _validate_lock(
    data: Mapping[str, Any],
    *,
    reviewed_git: set[tuple[str, str]],
    member_names: set[str],
) -> tuple[int, list[dict[str, str]], list[dict[str, str]], set[tuple[str, str]]]:
    packages = data.get("package")
    if not isinstance(packages, list) or not packages:
        _fail("Cargo.lock package array is required")
    registries: list[dict[str, str]] = []
    git_sources: set[tuple[str, str]] = set()
    path_names: set[str] = set()
    seen: set[tuple[str, str, str | None]] = set()
    for package in packages:
        if not isinstance(package, dict):
            _fail("Cargo.lock package entry must be a table")
        name = package.get("name")
        version = package.get("version")
        if not isinstance(name, str) or not isinstance(version, str):
            _fail("Cargo.lock package name/version must be strings")
        source = package.get("source")
        identity = (name, version, source if isinstance(source, str) else None)
        if identity in seen:
            _fail(f"duplicate Cargo.lock package identity: {identity}")
        seen.add(identity)
        if source is None:
            path_names.add(name)
            continue
        if source == REGISTRY_SOURCE:
            checksum = package.get("checksum")
            if not isinstance(checksum, str) or not _HEX64.fullmatch(checksum):
                _fail(f"registry package lacks a strict checksum: {name}@{version}")
            registries.append(
                {"name": name, "version": version, "checksum": checksum}
            )
            continue
        parsed_git = _git_source(source)
        if parsed_git is None or parsed_git not in reviewed_git:
            _fail(f"unreviewed Cargo.lock source: {source}")
        git_sources.add(parsed_git)
    if path_names != member_names:
        _fail(
            "Cargo.lock path-package census differs from workspace members: "
            f"lock={sorted(path_names)} members={sorted(member_names)}"
        )
    if git_sources != reviewed_git:
        _fail(
            "Cargo.lock git-source census differs from reviewed set: "
            f"lock={sorted(git_sources)} reviewed={sorted(reviewed_git)}"
        )
    registries.sort(key=lambda item: (item["name"], item["version"], item["checksum"]))
    git_records = [
        {"url": url, "rev": rev}
        for url, rev in sorted(git_sources)
    ]
    return len(packages), registries, git_records, git_sources


def _file_record(bundle_root: Path, relative: str) -> dict[str, Any]:
    path = bundle_root / relative
    raw = _lstat_regular(
        path,
        relative,
        DEFAULT_MAX_FILE_BYTES,
        single_link=True,
        exact_mode=0o644,
    )
    return {"path": relative, "blob_sha": _sha256(raw), "size": len(raw)}


def _write_new(bundle_root: Path, relative: str, content: bytes) -> None:
    safe = _safe_relative(relative, "bundle output")
    path = bundle_root / safe
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() or path.is_symlink():
        _fail(f"bundle output already exists: {safe}")
    path.write_bytes(content)
    path.chmod(0o644)


def _directory_records(bundle_root: Path, limits: Limits) -> list[str]:
    directories: list[str] = []
    for path in bundle_root.rglob("*"):
        relative = _bounded_relative(
            path.relative_to(bundle_root).as_posix(),
            "bundle directory path",
            limits,
        )
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            _fail(f"bundle contains a symlink: {relative}")
        if stat.S_ISDIR(info.st_mode):
            directories.append(relative)
        elif not stat.S_ISREG(info.st_mode):
            _fail(f"bundle contains a special file: {relative}")
    directories.sort()
    if len(directories) > limits.max_directories:
        _fail("bundle directory-count quota exceeded")
    return directories


def build_bundle(
    source_root: Path,
    output: Path,
    *,
    source_revision: str | None,
    target_package: str,
    cargo_home: Path,
    builder_base_digest: str | None = None,
    target_triple: str = DEFAULT_TARGET,
    cargo_bin: str = "cargo",
    rustc_bin: str = "rustc",
    path: str = DEFAULT_PATH,
    reviewed_git: Sequence[tuple[str, str]],
    features: Sequence[str] = ("default",),
    limits: Limits = Limits(),
) -> dict[str, Any]:
    """Construct a source-free prefetch bundle from an exact clean Git tree."""

    if not isinstance(target_package, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", target_package):
        _fail("target_package is not a Cargo package name")
    if not isinstance(target_triple, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", target_triple):
        _fail("target_triple is not a target triple")
    feature_values = _feature_list(features)
    reviewed_git_set = _reviewed_git_set(reviewed_git)
    snapshot = _validated_git_snapshot(
        source_root,
        expected_revision=source_revision,
    )
    output = output.absolute()
    if output.exists():
        _fail(f"bundle output must not already exist: {output}")
    try:
        output.relative_to(snapshot.root)
    except ValueError:
        pass
    else:
        _fail("bundle output must be outside the reviewed Git worktree")

    root_raw = snapshot.read("Cargo.toml", "Cargo.toml", limits.max_manifest_bytes)
    root_data = _parse_toml(root_raw, "Cargo.toml")
    members = _workspace_member_paths(snapshot, root_data, limits)
    member_dirs = {(snapshot.root / relative).resolve() for relative in members}
    declared_git = _validate_manifest_sources(
        root_data,
        field="Cargo.toml",
        manifest_dir=snapshot.root,
        source_root=snapshot.root,
        member_dirs=member_dirs,
        reviewed_git=reviewed_git_set,
    )
    root_manifest, _ = _sanitized_manifest(
        root_raw,
        root_data,
        package_name="workspace",
        member_relative=".",
    )

    member_names: set[str] = set()
    package_members: dict[str, str] = {}
    member_outputs: list[tuple[str, bytes, list[str]]] = []
    for relative in members:
        manifest_relative = f"{relative}/Cargo.toml"
        raw = snapshot.read(
            manifest_relative,
            manifest_relative,
            limits.max_manifest_bytes,
        )
        data = _parse_toml(raw, manifest_relative)
        declared_git.update(
            _validate_manifest_sources(
                data,
                field=manifest_relative,
                manifest_dir=(snapshot.root / relative).resolve(),
                source_root=snapshot.root,
                member_dirs=member_dirs,
                reviewed_git=reviewed_git_set,
            )
        )
        package = data.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            _fail(f"{manifest_relative} lacks package.name")
        package_name = package["name"]
        if package_name in member_names:
            _fail(f"duplicate workspace package name: {package_name}")
        member_names.add(package_name)
        package_members[package_name] = relative
        sanitized, stubs = _sanitized_manifest(
            raw,
            data,
            package_name=package_name,
            member_relative=relative,
        )
        member_outputs.append((relative, sanitized, stubs))

    lock_raw = snapshot.read("Cargo.lock", "Cargo.lock", limits.max_lock_bytes)
    lock_data = _parse_toml(lock_raw, "Cargo.lock")
    lock_count, registries, git_records, lock_git = _validate_lock(
        lock_data,
        reviewed_git=reviewed_git_set,
        member_names=member_names,
    )
    if not declared_git <= lock_git:
        _fail(
            "manifest Git dependency census contains sources absent from Cargo.lock: "
            f"manifest={sorted(declared_git)} lock={sorted(lock_git)}"
        )
    if target_package not in package_members:
        _fail(f"target_package is not an allowlisted workspace package: {target_package}")

    toolchain_names = [
        candidate
        for candidate in ("rust-toolchain.toml", "rust-toolchain")
        if candidate in snapshot.entries
    ]
    if len(toolchain_names) != 1:
        _fail("exactly one reviewed toolchain input is required")
    toolchain_name = toolchain_names[0]
    toolchain_raw = snapshot.read(
        toolchain_name,
        toolchain_name,
        limits.max_manifest_bytes,
    )

    target_manifest = f"{package_members[target_package]}/Cargo.toml"
    closure_contract = measure_closure_contract(
        snapshot.root,
        manifest=target_manifest,
        package_name=target_package,
        source_head_sha=snapshot.commit,
        source_tree_sha=snapshot.tree,
        target=target_triple,
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        rustc_bin=rustc_bin,
        path=path,
        features=feature_values,
    )
    recipe = builder_recipe_contract(
        cargo_bin=cargo_bin,
        rustc_bin=rustc_bin,
        path=path,
        builder_base_digest=builder_base_digest,
        target_package=target_package,
        target_triple=target_triple,
        manifest=target_manifest,
        features=feature_values,
    )

    output.mkdir(parents=True)
    _write_new(output, "Cargo.toml", root_manifest)
    target_stubs: list[str] = []
    for relative, sanitized, stubs in member_outputs:
        _write_new(output, f"{relative}/Cargo.toml", sanitized)
        for stub in stubs:
            full = f"{relative}/{stub}"
            _write_new(output, full, b"")
            target_stubs.append(full)
    _write_new(output, "Cargo.lock", lock_raw)
    _write_new(output, toolchain_name, toolchain_raw)

    relative_files = sorted(
        ["Cargo.toml", "Cargo.lock", toolchain_name]
        + [f"{relative}/Cargo.toml" for relative in members]
        + target_stubs
    )
    files = [_file_record(output, relative) for relative in relative_files]
    directories = _directory_records(output, limits)
    dependency_files = [
        record
        for record in files
        if record["path"] == "Cargo.lock"
        or record["path"] == "Cargo.toml"
        or record["path"].endswith("/Cargo.toml")
    ]
    toolchain_files = [record for record in files if record["path"] == toolchain_name]
    dependency_contract_digest = _sha256(_canonical({"files": dependency_files}))
    toolchain_input_digest = _sha256(
        _canonical(
            {
                "target_triple": target_triple,
                "files": toolchain_files,
            }
        )
    )
    registry_preimage = [
        {"name": item["name"], "version": item["version"], "checksum": item["checksum"]}
        for item in registries
    ]
    base_manifest: dict[str, Any] = {
        "schema": SCHEMA,
        "source_head_sha": snapshot.commit,
        "source_tree_sha": snapshot.tree,
        "target_package": target_package,
        "target_triple": target_triple,
        "features": feature_values,
        "workspace_manifest_count": len(members),
        "lock_package_count": lock_count,
        "closure_package_count": closure_contract["package_count"],
        "closure_output_sha256": closure_contract["output_sha256"],
        "files": files,
        "directories": directories,
        "dependency_contract_digest": dependency_contract_digest,
        "toolchain_input_digest": toolchain_input_digest,
        "builder_recipe_digest": _sha256(_canonical(recipe)),
        "registry_sources": [REGISTRY_SOURCE],
        "registry_transport": REGISTRY_TRANSPORT,
        "registry_checksum_count": len(registries),
        "registry_checksum_digest": _sha256(_canonical(registry_preimage)),
        "git_sources": git_records,
        "network_policy": network_policy(),
    }
    manifest = dict(base_manifest)
    manifest["bundle_sha256"] = _sha256(_canonical(base_manifest))
    _write_new(
        output,
        "manifest.json",
        (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8"),
    )
    return manifest


def _manifest_keys() -> set[str]:
    return {
        "schema",
        "source_head_sha",
        "source_tree_sha",
        "target_package",
        "target_triple",
        "features",
        "workspace_manifest_count",
        "lock_package_count",
        "closure_package_count",
        "closure_output_sha256",
        "files",
        "directories",
        "dependency_contract_digest",
        "toolchain_input_digest",
        "builder_recipe_digest",
        "registry_sources",
        "registry_transport",
        "registry_checksum_count",
        "registry_checksum_digest",
        "git_sources",
        "network_policy",
        "bundle_sha256",
    }


def _read_manifest(bundle_root: Path) -> dict[str, Any]:
    raw = _lstat_regular(
        bundle_root / "manifest.json",
        "manifest.json",
        4 * 1024 * 1024,
        single_link=True,
        exact_mode=0o644,
    )
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        _fail(f"manifest.json is not valid UTF-8 JSON: {exc}")
    if not isinstance(value, dict):
        _fail("manifest.json must be an object")
    if set(value) != _manifest_keys():
        _fail(
            "manifest schema keys mismatch: "
            f"extra={sorted(set(value) - _manifest_keys())} "
            f"missing={sorted(_manifest_keys() - set(value))}"
        )
    return value


def _manifest_git_records(value: Any) -> set[tuple[str, str]]:
    if not isinstance(value, list):
        _fail("git_sources must be an array")
    result: set[tuple[str, str]] = set()
    for item in value:
        if not isinstance(item, dict) or set(item) != {"url", "rev"}:
            _fail("git_sources entries must have url/rev")
        url, rev = item["url"], item["rev"]
        url = _validate_git_url(url, "manifest Git URL")
        if not isinstance(rev, str) or not _HEX40.fullmatch(rev):
            _fail("git_sources entries need URL and full 40-hex revision")
        result.add((url, rev))
    if len(result) != len(value):
        _fail("git_sources contains duplicates")
    return result


def _validated_closure_contract(
    value: Mapping[str, Any],
    *,
    source_head_sha: str,
    source_tree_sha: str,
) -> dict[str, Any]:
    expected_keys = {
        "source_head_sha",
        "source_tree_sha",
        "target_package",
        "target_triple",
        "features",
        "package_count",
        "output_sha256",
    }
    if not isinstance(value, Mapping) or set(value) != expected_keys:
        _fail("trusted closure contract schema mismatch")
    _identity(value["source_head_sha"], "trusted closure source_head_sha")
    _identity(value["source_tree_sha"], "trusted closure source_tree_sha")
    if value["source_head_sha"] != source_head_sha or value["source_tree_sha"] != source_tree_sha:
        _fail("trusted closure source identity differs from trusted source")
    if not isinstance(value["target_package"], str) or not value["target_package"]:
        _fail("trusted closure target package is invalid")
    if not isinstance(value["target_triple"], str) or not value["target_triple"]:
        _fail("trusted closure target triple is invalid")
    if not isinstance(value["features"], list):
        _fail("trusted closure features must be an array")
    features = _feature_list(value["features"])
    _integer(value["package_count"], "trusted closure package_count")
    _digest(value["output_sha256"], "trusted closure output_sha256")
    return {
        "source_head_sha": value["source_head_sha"],
        "source_tree_sha": value["source_tree_sha"],
        "target_package": value["target_package"],
        "target_triple": value["target_triple"],
        "features": features,
        "package_count": value["package_count"],
        "output_sha256": value["output_sha256"],
    }


def _validated_recipe_contract(value: Mapping[str, Any]) -> dict[str, Any]:
    expected_keys = {
        "schema",
        "base_image_digest",
        "cargo",
        "rustc",
        "path",
        "target_package",
        "target_triple",
        "features",
        "manifest",
        "command",
    }
    if not isinstance(value, Mapping) or set(value) != expected_keys:
        _fail("trusted builder recipe schema mismatch")
    if value["schema"] != "bootstrap.prefetch.recipe.v1":
        _fail("unsupported trusted builder recipe schema")
    _digest(value["base_image_digest"], "trusted base_image_digest")
    if not isinstance(value["path"], str) or not value["path"]:
        _fail("trusted recipe PATH is invalid")
    if not isinstance(value["target_package"], str) or not value["target_package"]:
        _fail("trusted recipe target package is invalid")
    if not isinstance(value["target_triple"], str) or not value["target_triple"]:
        _fail("trusted recipe target triple is invalid")
    if not isinstance(value["features"], list):
        _fail("trusted recipe features must be an array")
    features = _feature_list(value["features"])
    manifest = _safe_relative(value["manifest"], "trusted recipe manifest")
    command = value["command"]
    if not isinstance(command, list) or any(not isinstance(item, str) for item in command):
        _fail("trusted recipe command must be a string array")
    records: dict[str, dict[str, Any]] = {}
    for name in ("cargo", "rustc"):
        record = value[name]
        if not isinstance(record, Mapping) or set(record) != {"path", "blob_sha", "size", "mode"}:
            _fail(f"trusted recipe {name} identity schema mismatch")
        if not isinstance(record["path"], str) or not Path(record["path"]).is_absolute():
            _fail(f"trusted recipe {name} path must be absolute")
        _digest(record["blob_sha"], f"trusted recipe {name}.blob_sha")
        _integer(record["size"], f"trusted recipe {name}.size")
        if (
            isinstance(record["mode"], bool)
            or not isinstance(record["mode"], int)
            or record["mode"] <= 0
        ):
            _fail(f"trusted recipe {name}.mode is invalid")
        records[name] = dict(record)
    expected_command = [
        "cargo",
        "tree",
        "--locked",
        "--offline",
        "--target",
        value["target_triple"],
        "--edges",
        "normal,build",
        "--prefix",
        "none",
        "--format",
        "{p}",
        *_cargo_feature_args(features),
        "--manifest-path",
        manifest,
    ]
    if command != expected_command:
        _fail("trusted recipe command is not the fixed offline closure command")
    for name in ("cargo", "rustc"):
        actual = _executable_record(records[name]["path"], value["path"], f"trusted {name}")
        if actual != records[name]:
            _fail(f"trusted {name} executable identity changed")
    return {
        "schema": value["schema"],
        "base_image_digest": value["base_image_digest"],
        "cargo": records["cargo"],
        "rustc": records["rustc"],
        "path": value["path"],
        "target_package": value["target_package"],
        "target_triple": value["target_triple"],
        "features": features,
        "manifest": manifest,
        "command": list(command),
    }


def _validated_bundle_contract(
    contract: BundleContract,
) -> tuple[dict[str, Any], dict[str, Any], set[tuple[str, str]], tuple[tuple[str, str, str], ...]]:
    if not isinstance(contract, BundleContract):
        _fail("trusted BundleContract is required; manifest fields are not authority")
    _identity(contract.source_head_sha, "trusted source_head_sha")
    _identity(contract.source_tree_sha, "trusted source_tree_sha")
    _digest(contract.dependency_contract_digest, "trusted dependency_contract_digest")
    _digest(contract.toolchain_input_digest, "trusted toolchain_input_digest")
    trusted_git = _reviewed_git_set(contract.git_sources)
    trusted_provenance = tuple(contract.git_provenance)
    if set(_git_provenance_map(trusted_provenance)) != trusted_git:
        _fail("trusted Git provenance must cover every trusted Git source")
    closure = _validated_closure_contract(
        contract.closure,
        source_head_sha=contract.source_head_sha,
        source_tree_sha=contract.source_tree_sha,
    )
    recipe = _validated_recipe_contract(contract.recipe)
    if recipe["target_package"] != closure["target_package"]:
        _fail("trusted recipe and closure target packages differ")
    if recipe["target_triple"] != closure["target_triple"]:
        _fail("trusted recipe and closure target triples differ")
    if recipe["features"] != closure["features"]:
        _fail("trusted recipe and closure feature selections differ")
    return closure, recipe, trusted_git, trusted_provenance


def validate_bundle(
    bundle_root: Path,
    *,
    contract: BaseOwnedContract | None = None,
    reviewed_git: Sequence[tuple[str, str]] | None = None,
    limits: Limits = Limits(),
    cargo_home: Path | None = None,
    verify_git_cache: bool = True,
) -> dict[str, Any]:
    """Validate against base-owned source, closure, and recipe identities."""

    bundle_root = bundle_root.absolute()
    if not bundle_root.is_dir() or bundle_root.is_symlink():
        _fail("bundle root must be a non-symlink directory")
    if not isinstance(contract, BaseOwnedContract):
        _fail("BaseOwnedContract is required; plain contract objects are not authority")
    bundle_root = bundle_root.resolve()
    if not bundle_root.is_relative_to(contract.candidate_root):
        _fail("bundle is outside the candidate root bound to the base contract")
    if bundle_root.is_relative_to(contract.trusted_root):
        _fail("bundle overlaps the trusted contract root")
    closure_contract, recipe_contract, trusted_git, trusted_provenance = _validated_bundle_contract(
        contract.contract
    )
    if cargo_home is None:
        _fail("trusted closure validation requires an offline Cargo home")
    manifest = _read_manifest(bundle_root)
    if manifest["schema"] != SCHEMA:
        _fail(f"unsupported schema: {manifest['schema']!r}")
    _identity(manifest["source_head_sha"], "source_head_sha")
    _identity(manifest["source_tree_sha"], "source_tree_sha")
    _digest(manifest["dependency_contract_digest"], "dependency_contract_digest")
    _digest(manifest["toolchain_input_digest"], "toolchain_input_digest")
    _digest(manifest["closure_output_sha256"], "closure_output_sha256")
    _digest(manifest["builder_recipe_digest"], "builder_recipe_digest")
    _digest(manifest["registry_checksum_digest"], "registry_checksum_digest")
    _digest(manifest["bundle_sha256"], "bundle_sha256")
    for field in (
        "workspace_manifest_count",
        "lock_package_count",
        "closure_package_count",
        "registry_checksum_count",
    ):
        _integer(manifest[field], field)
    if not isinstance(manifest["features"], list) or any(
        not isinstance(item, str) for item in manifest["features"]
    ):
        _fail("features must be a string array")
    if manifest["source_head_sha"] != contract.contract.source_head_sha:
        _fail("bundle source HEAD differs from the trusted source contract")
    if manifest["source_tree_sha"] != contract.contract.source_tree_sha:
        _fail("bundle source tree differs from the trusted source contract")
    if manifest["dependency_contract_digest"] != contract.contract.dependency_contract_digest:
        _fail("bundle dependency contract differs from the trusted source contract")
    if manifest["toolchain_input_digest"] != contract.contract.toolchain_input_digest:
        _fail("bundle toolchain input differs from the trusted source contract")
    if manifest["target_package"] != closure_contract["target_package"]:
        _fail("bundle target package differs from the trusted closure contract")
    if manifest["target_triple"] != closure_contract["target_triple"]:
        _fail("bundle target triple differs from the trusted closure contract")
    if manifest["features"] != closure_contract["features"]:
        _fail("bundle features differ from the trusted closure contract")
    if manifest["closure_package_count"] != closure_contract["package_count"]:
        _fail("bundle closure count differs from the trusted closure contract")
    if manifest["closure_output_sha256"] != closure_contract["output_sha256"]:
        _fail("bundle closure output differs from the trusted closure contract")
    if manifest["builder_recipe_digest"] != _sha256(_canonical(recipe_contract)):
        _fail("bundle builder recipe differs from the trusted recipe contract")
    if manifest["registry_sources"] != [REGISTRY_SOURCE]:
        _fail("registry source census is not exactly the reviewed crates.io source")
    if manifest["registry_transport"] != REGISTRY_TRANSPORT:
        _fail("registry transport is not the reviewed sparse endpoint")
    if reviewed_git is not None and _reviewed_git_set(reviewed_git) != trusted_git:
        _fail("caller Git source policy differs from the base-owned contract")
    manifest_git = _manifest_git_records(manifest["git_sources"])
    if manifest_git != trusted_git:
        _fail("manifest git-source census differs from the base-owned contract")
    policy = manifest["network_policy"]
    if not isinstance(policy, dict) or policy.get("enforced") is not False:
        _fail("network policy must remain an explicit unenforced container gate")
    if policy.get("allowed_registry_hosts") != ["index.crates.io", "static.crates.io"]:
        _fail("network registry host allowlist changed")
    if policy.get("allowed_git_hosts") != ["github.com"]:
        _fail("network Git host allowlist changed")
    base = dict(manifest)
    del base["bundle_sha256"]
    if _sha256(_canonical(base)) != manifest["bundle_sha256"]:
        _fail("bundle_sha256 does not match the canonical self-excluding preimage")

    records = manifest["files"]
    if not isinstance(records, list) or not records:
        _fail("files must be a non-empty array")
    declared_directories = manifest["directories"]
    if not isinstance(declared_directories, list):
        _fail("directories must be a string array")
    directory_paths: set[str] = set()
    for value in declared_directories:
        relative = _bounded_relative(value, "manifest directory path", limits)
        if relative in directory_paths:
            _fail(f"duplicate manifest directory path: {relative}")
        directory_paths.add(relative)
    if len(directory_paths) > limits.max_directories:
        _fail("bundle directory-count quota exceeded")
    paths: set[str] = set()
    total_bytes = 0
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "blob_sha", "size"}:
            _fail("each files record must have exactly path/blob_sha/size")
        relative = _bounded_relative(record["path"], "manifest file path", limits)
        if relative == "manifest.json" or relative in paths:
            _fail(f"manifest file path is duplicate or self-referential: {relative}")
        paths.add(relative)
        _digest(record["blob_sha"], f"{relative}.blob_sha")
        _integer(record["size"], f"{relative}.size")
        raw = _lstat_regular(
            bundle_root / relative,
            relative,
            limits.max_file_bytes,
            single_link=True,
            exact_mode=0o644,
        )
        if len(raw) != record["size"] or _sha256(raw) != record["blob_sha"]:
            _fail(f"file record mismatch: {relative}")
        total_bytes += len(raw)
        if len(paths) > limits.max_files or total_bytes > limits.max_cache_bytes:
            _fail("bundle file count/bytes exceed the bounded census")

    actual: set[str] = set()
    actual_directories: set[str] = set()
    for path in bundle_root.rglob("*"):
        relative = _bounded_relative(
            path.relative_to(bundle_root).as_posix(),
            "bundle path",
            limits,
        )
        if relative == "manifest.json":
            continue
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            _fail(f"bundle contains a symlink: {relative}")
        if stat.S_ISDIR(info.st_mode):
            actual_directories.add(relative)
            continue
        if not stat.S_ISREG(info.st_mode):
            _fail(f"bundle contains a special file: {relative}")
        actual.add(relative)
    if actual_directories != directory_paths:
        _fail(
            "bundle directory census mismatch: "
            f"extra={sorted(actual_directories - directory_paths)} "
            f"missing={sorted(directory_paths - actual_directories)}"
        )
    if actual != paths:
        _fail(
            "bundle file census mismatch: "
            f"extra={sorted(actual - paths)} missing={sorted(paths - actual)}"
        )
    for relative in sorted(paths):
        if relative.endswith(".rs"):
            if "/.prefetch-targets/" not in f"/{relative}":
                _fail(f"untrusted Rust source in bundle: {relative}")
            if (bundle_root / relative).stat().st_size != 0:
                _fail(f"non-empty trusted target stub in bundle: {relative}")
    if any(
        Path(relative).name in {"build.rs", "main.rs", "lib.rs"}
        and "/.prefetch-targets/" not in f"/{relative}"
        for relative in paths
    ):
        _fail("candidate source target path leaked into bundle")
    manifest_paths = sorted(
        relative
        for relative in paths
        if relative == "Cargo.toml" or relative.endswith("/Cargo.toml")
    )
    if len(manifest_paths) != manifest["workspace_manifest_count"] + 1:
        _fail("workspace manifest count does not include exactly one root manifest")
    if "Cargo.lock" not in paths:
        _fail("Cargo.lock missing from bundle census")
    toolchain_paths = [
        relative
        for relative in paths
        if relative in {"rust-toolchain.toml", "rust-toolchain"}
    ]
    if len(toolchain_paths) != 1:
        _fail("exactly one toolchain input is required")

    _, lock_data = _load_toml(bundle_root / "Cargo.lock", "bundle Cargo.lock", limits)
    _, root_data = _load_toml(bundle_root / "Cargo.toml", "bundle Cargo.toml", limits)
    member_dirs = {
        (bundle_root / relative.rsplit("/", 1)[0]).resolve()
        for relative in manifest_paths
        if relative != "Cargo.toml"
    }
    declared_git = _validate_manifest_sources(
        root_data,
        field="Cargo.toml",
        manifest_dir=bundle_root,
        source_root=bundle_root,
        member_dirs=member_dirs,
        reviewed_git=trusted_git,
    )
    bundle_members = _bundle_workspace_paths(bundle_root, root_data)
    expected_member_manifests = {f"{relative}/Cargo.toml" for relative in bundle_members}
    observed_member_manifests = {
        relative for relative in manifest_paths if relative != "Cargo.toml"
    }
    if observed_member_manifests != expected_member_manifests:
        _fail(
            "workspace member manifest census differs from root Cargo.toml: "
            f"observed={sorted(observed_member_manifests)} "
            f"declared={sorted(expected_member_manifests)}"
        )
    member_names: set[str] = set()
    expected_stub_paths: set[str] = set()
    target_manifest_relative: str | None = None
    for relative in manifest_paths:
        if relative == "Cargo.toml":
            continue
        _, data = _load_toml(bundle_root / relative, relative, limits)
        package = data.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            _fail(f"bundle manifest lacks package.name: {relative}")
        package_name = package["name"]
        member_names.add(package_name)
        expected_stub_paths.update(
            _expected_target_stubs(
                data,
                package_name=package_name,
                member_relative=relative.rsplit("/", 1)[0],
            )
        )
        if package_name == closure_contract["target_package"]:
            if target_manifest_relative is not None:
                _fail("trusted target package appears in multiple manifests")
            target_manifest_relative = relative
        declared_git.update(
            _validate_manifest_sources(
                data,
                field=relative,
                manifest_dir=(bundle_root / relative).parent,
                source_root=bundle_root,
                member_dirs=member_dirs,
                reviewed_git=trusted_git,
            )
        )
    if target_manifest_relative is None:
        _fail("trusted target package is absent from bundle workspace members")
    if target_manifest_relative != recipe_contract["manifest"]:
        _fail("bundle target manifest differs from the trusted recipe")
    expected_paths = (
        set(manifest_paths)
        | {"Cargo.lock"}
        | set(toolchain_paths)
        | expected_stub_paths
    )
    if paths != expected_paths:
        _fail(
            "bundle file paths differ from the sanitized workspace contract: "
            f"extra={sorted(paths - expected_paths)} "
            f"missing={sorted(expected_paths - paths)}"
        )
    expected_directories: set[str] = set()
    for relative in expected_paths:
        parts = PurePosixPath(relative).parts[:-1]
        for index in range(1, len(parts) + 1):
            expected_directories.add("/".join(parts[:index]))
    if directory_paths != expected_directories:
        _fail("bundle directories are not exactly the sanitized workspace parents")
    lock_count, registries, git_records, lock_git = _validate_lock(
        lock_data,
        reviewed_git=trusted_git,
        member_names=member_names,
    )
    if not declared_git <= lock_git:
        _fail("manifest Git dependency census contains a source absent from Cargo.lock")
    if lock_count != manifest["lock_package_count"]:
        _fail("lock package count does not match manifest")
    if git_records != manifest["git_sources"]:
        _fail("lock Git census does not match manifest")
    registry_preimage = [
        {"name": item["name"], "version": item["version"], "checksum": item["checksum"]}
        for item in registries
    ]
    if len(registries) != manifest["registry_checksum_count"]:
        _fail("registry checksum count does not match lock")
    if _sha256(_canonical(registry_preimage)) != manifest["registry_checksum_digest"]:
        _fail("registry checksum census digest does not match lock")

    dependency_records = [
        record
        for record in records
        if record["path"] == "Cargo.lock"
        or record["path"] == "Cargo.toml"
        or record["path"].endswith("/Cargo.toml")
    ]
    expected_dependency_digest = _sha256(_canonical({"files": dependency_records}))
    if expected_dependency_digest != manifest["dependency_contract_digest"]:
        _fail("dependency contract digest does not match the recorded preimage")
    toolchain_record = next(record for record in records if record["path"] in toolchain_paths)
    expected_toolchain_digest = _sha256(
        _canonical(
            {
                "target_triple": manifest["target_triple"],
                "files": [toolchain_record],
            }
        )
    )
    if expected_toolchain_digest != manifest["toolchain_input_digest"]:
        _fail("toolchain input digest does not match the recorded preimage")
    actual_closure = measure_closure_contract(
        bundle_root,
        manifest=target_manifest_relative,
        package_name=closure_contract["target_package"],
        source_head_sha=contract.contract.source_head_sha,
        source_tree_sha=contract.contract.source_tree_sha,
        target=closure_contract["target_triple"],
        cargo_home=cargo_home,
        cargo_bin=recipe_contract["cargo"]["path"],
        rustc_bin=recipe_contract["rustc"]["path"],
        path=recipe_contract["path"],
        features=closure_contract["features"],
    )
    if actual_closure != closure_contract:
        _fail(
            "offline Cargo closure differs from the trusted contract: "
            f"actual={actual_closure} expected={closure_contract}"
        )
    if trusted_git and verify_git_cache:
        git_census(cargo_home, trusted_git, provenance=trusted_provenance)
    return manifest


def fetch_environment(
    home: Path,
    cargo_home: Path,
    *,
    path: str = DEFAULT_PATH,
    target_triple: str = DEFAULT_TARGET,
) -> dict[str, str]:
    """Return the only environment admitted to the networked Cargo process."""

    if not home.is_absolute() or not cargo_home.is_absolute():
        _fail("fetch HOME and CARGO_HOME must be absolute")
    if not isinstance(path, str) or not path:
        _fail("fetch PATH must be a non-empty string")
    return {
        "HOME": str(home),
        "PATH": path,
        "CARGO_HOME": str(cargo_home),
        "CARGO_NET_OFFLINE": "false",
        "CARGO_NET_RETRY": "0",
        "CARGO_HTTP_TIMEOUT": "30",
        "CARGO_REGISTRIES_CRATES_IO_PROTOCOL": "sparse",
        "CARGO_TERM_COLOR": "never",
        "CARGO_NET_GIT_FETCH_WITH_CLI": "false",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_ASKPASS": "/bin/false",
        "SSH_AUTH_SOCK": os.devnull,
        "RUSTC": "/bin/false",
        "RUSTDOC": "/bin/false",
        "CARGO_BUILD_RUSTC_WRAPPER": "/bin/false",
        "CARGO_BUILD_TARGET": target_triple,
    }


def network_policy() -> dict[str, Any]:
    """Describe the reviewed allowlist without falsely claiming enforcement."""

    return {
        "enforced": False,
        "allowed_registry_hosts": ["index.crates.io", "static.crates.io"],
        "allowed_git_hosts": ["github.com"],
        "reason": "host egress requires a reviewed container/firewall boundary",
    }


def _direct_directories(path: Path, field: str) -> list[Path]:
    try:
        info = path.lstat()
    except OSError as exc:
        _fail(f"{field} is unavailable: {exc}")
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        _fail(f"{field} must be a non-symlink directory: {path}")
    values = []
    for child in sorted(path.iterdir()):
        child_info = child.lstat()
        if stat.S_ISLNK(child_info.st_mode):
            _fail(f"{field} contains a symlink: {child.name}")
        if not stat.S_ISDIR(child_info.st_mode):
            _fail(f"{field} contains a non-directory entry: {child.name}")
        values.append(child)
    return values


def _checkout_directories(root: Path) -> list[Path]:
    result: list[Path] = []
    for namespace in _direct_directories(root, "Cargo git checkout root"):
        for revision in _direct_directories(namespace, "Cargo git checkout namespace"):
            marker = revision / ".git"
            try:
                marker_info = marker.lstat()
            except OSError as exc:
                _fail(f"Cargo checkout lacks .git marker: {revision}: {exc}")
            if stat.S_ISLNK(marker_info.st_mode) or not (
                stat.S_ISDIR(marker_info.st_mode) or stat.S_ISREG(marker_info.st_mode)
            ):
                _fail(f"Cargo checkout has unsafe .git marker: {revision}")
            result.append(revision)
    return result


def git_census(
    cargo_home: Path,
    reviewed_git: Sequence[tuple[str, str]] | set[tuple[str, str]],
    *,
    provenance: Sequence[tuple[str, str, str]] | None = None,
) -> list[dict[str, str]]:
    """Require exact clean cache objects and trusted public Git provenance."""

    expected = _reviewed_git_set(tuple(reviewed_git))
    if not expected:
        _fail("reviewed Git source set must not be empty")
    if provenance is None:
        _fail("trusted Git URL/tree provenance is required")
    provenance_map = _git_provenance_map(provenance)
    if set(provenance_map) != expected:
        _fail("trusted Git provenance differs from the reviewed source set")
    cargo_home = cargo_home.absolute()
    dbs = _direct_directories(cargo_home / "git" / "db", "Cargo git DB root")
    checkouts = _checkout_directories(cargo_home / "git" / "checkouts")
    if len(dbs) != len(expected) or len(checkouts) != len(expected):
        _fail(
            "Cargo Git census has extra or missing repositories: "
            f"dbs={len(dbs)} checkouts={len(checkouts)} expected={len(expected)}"
        )
    db_matches: dict[str, Path] = {}
    for _, rev in expected:
        matches = [
            db
            for db in dbs
            if _git_run(db, ["cat-file", "-e", f"{rev}^{{commit}}"]) == ""
        ]
        if len(matches) != 1:
            _fail(f"Git DB census has {len(matches)} matches for reviewed revision {rev}")
        db_matches[rev] = matches[0]
        url = next(url for url, candidate_rev in expected if candidate_rev == rev)
        if _git_origin_url(db_matches[rev], f"Git DB {db_matches[rev]}") != url:
            _fail(f"Git DB origin URL differs from trusted source for {url}@{rev}")
    checkout_records: list[dict[str, str]] = []
    for checkout in checkouts:
        status = _git_run(
            checkout,
            ["status", "--porcelain=v1", "--untracked-files=all"],
        )
        if status:
            _fail(f"Cargo Git checkout is dirty or has untracked files: {checkout}: {status}")
        head = _git_run(checkout, ["rev-parse", "--verify", "HEAD^{commit}"])
        _identity(head, f"Cargo checkout HEAD {checkout}")
        matching = [url for url, rev in expected if rev == head]
        if len(matching) != 1:
            _fail(f"unreviewed or duplicate Cargo checkout HEAD: {checkout} -> {head}")
        db = db_matches[head]
        if _git_local_origin(checkout, f"Cargo checkout {checkout}").resolve() != db.resolve():
            _fail(f"Cargo checkout origin does not identify its reviewed Git DB: {checkout}")
        tree = _git_run(checkout, ["rev-parse", "HEAD^{tree}"])
        checkout_records.append(
            {
                "url": matching[0],
                "rev": head,
                "checkout": checkout.relative_to(cargo_home).as_posix(),
                "tree": tree,
            }
        )
    for url, rev in expected:
        db = db_matches[rev]
        db_tree = _git_run(db, ["rev-parse", f"{rev}^{{tree}}"])
        url = next(url for url, candidate_rev in expected if candidate_rev == rev)
        if db_tree != provenance_map[(url, rev)]:
            _fail(f"Git DB tree differs from trusted provenance for {url}@{rev}")
        checkout_tree = next(item["tree"] for item in checkout_records if item["rev"] == rev)
        if db_tree != checkout_tree:
            _fail(f"Git DB/checkout tree mismatch for {url}@{rev}")
        _git_run(db, ["cat-file", "-e", f"{rev}^{{commit}}"])
    checkout_records.sort(key=lambda item: (item["url"], item["rev"]))
    return checkout_records


def copy_cache_bounded(
    source: Path,
    destination: Path,
    *,
    max_files: int = DEFAULT_MAX_FILES,
    max_bytes: int = DEFAULT_MAX_CACHE_BYTES,
    max_path_bytes: int = 4096,
) -> dict[str, int]:
    """Copy a trusted cache without following links or exceeding quotas."""

    source = source.absolute()
    destination = destination.absolute()
    if not source.is_dir() or source.is_symlink():
        _fail("cache source must be a non-symlink directory")
    if destination.exists() or destination.is_symlink():
        _fail("cache destination must not already exist")
    destination.mkdir(parents=True)
    files = 0
    total = 0
    stack = [(source, destination)]
    while stack:
        current, target = stack.pop()
        for entry in sorted(current.iterdir(), key=lambda item: item.name):
            relative = entry.relative_to(source).as_posix()
            if len(relative.encode("utf-8")) > max_path_bytes:
                _fail(f"cache path exceeds bound: {relative}")
            info = entry.lstat()
            if stat.S_ISLNK(info.st_mode) or not (
                stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)
            ):
                _fail(f"cache contains a link or special file: {relative}")
            target_entry = target / entry.name
            if stat.S_ISDIR(info.st_mode):
                target_entry.mkdir()
                stack.append((entry, target_entry))
                continue
            if info.st_nlink != 1:
                _fail(f"cache hardlink is not trusted: {relative}")
            files += 1
            if files > max_files:
                _fail("cache file-count quota exceeded")
            if info.st_size > max_bytes - total:
                _fail("cache byte quota exceeded")
            with entry.open("rb") as reader, target_entry.open("xb") as writer:
                shutil.copyfileobj(reader, writer, length=1024 * 1024)
            copied = target_entry.stat()
            if copied.st_size != info.st_size:
                _fail(f"cache copy size changed during copy: {relative}")
            total += copied.st_size
    return {"files": files, "bytes": total}


def _measure_closure_output(
    source_root: Path,
    *,
    manifest: str,
    package_name: str,
    target: str = DEFAULT_TARGET,
    cargo_home: Path,
    cargo_bin: str = "cargo",
    rustc_bin: str = "rustc",
    path: str = DEFAULT_PATH,
    features: Sequence[str] = ("default",),
) -> tuple[int, str]:
    """Measure exact normal/build package identities with Cargo offline only."""

    _safe_relative(manifest, "closure manifest")
    if not isinstance(package_name, str) or not package_name:
        _fail("closure package_name is required")
    if not isinstance(target, str) or not target:
        _fail("closure target is required")
    feature_values = _feature_list(features)
    home = Path(tempfile.mkdtemp(prefix="bootstrap-measure-home-"))
    try:
        environment = fetch_environment(
            home,
            cargo_home.absolute(),
            path=path,
            target_triple=target,
        )
        environment["CARGO_NET_OFFLINE"] = "true"
        environment["RUSTC"] = rustc_bin
        environment.pop("CARGO_BUILD_RUSTC_WRAPPER", None)
        result = subprocess.run(
            [
                cargo_bin,
                "tree",
                "--locked",
                "--offline",
                "--target",
                target,
                "--edges",
                "normal,build",
                "--prefix",
                "none",
                "--format",
                "{p}",
                *_cargo_feature_args(feature_values),
                "--manifest-path",
                manifest,
            ],
            cwd=source_root,
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )
    finally:
        shutil.rmtree(home, ignore_errors=True)
    if result.returncode != 0:
        _fail(f"offline Cargo closure measurement failed: {result.stderr.strip()}")
    values: set[str] = set()
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        value = re.sub(r" \(\*.*$", "", line)
        value = re.sub(r" \(.*$", "", value)
        if not value.startswith(f"{package_name} "):
            values.add(value)
    normalized = sorted(values)
    return len(normalized), _sha256(_canonical({"packages": normalized}))


def measure_closure_contract(
    source_root: Path,
    *,
    manifest: str,
    package_name: str,
    source_head_sha: str,
    source_tree_sha: str,
    target: str = DEFAULT_TARGET,
    cargo_home: Path,
    cargo_bin: str = "cargo",
    rustc_bin: str = "rustc",
    path: str = DEFAULT_PATH,
    features: Sequence[str] = ("default",),
) -> dict[str, Any]:
    """Return a closure contract with an independently hashed package list."""

    _identity(source_head_sha, "closure source_head_sha")
    _identity(source_tree_sha, "closure source_tree_sha")
    feature_values = _feature_list(features)
    count, output_sha256 = _measure_closure_output(
        source_root,
        manifest=manifest,
        package_name=package_name,
        target=target,
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        rustc_bin=rustc_bin,
        path=path,
        features=feature_values,
    )
    return {
        "source_head_sha": source_head_sha,
        "source_tree_sha": source_tree_sha,
        "target_package": package_name,
        "target_triple": target,
        "features": feature_values,
        "package_count": count,
        "output_sha256": output_sha256,
    }


def measure_closure(
    source_root: Path,
    *,
    manifest: str,
    package_name: str,
    target: str = DEFAULT_TARGET,
    cargo_home: Path,
    cargo_bin: str = "cargo",
    rustc_bin: str = "rustc",
    path: str = DEFAULT_PATH,
    features: Sequence[str] = ("default",),
) -> int:
    """Return only the count for callers that do not need admission proof."""

    count, _ = _measure_closure_output(
        source_root,
        manifest=manifest,
        package_name=package_name,
        target=target,
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        rustc_bin=rustc_bin,
        path=path,
        features=features,
    )
    return count


def _parse_reviewed_git(value: str) -> tuple[str, str]:
    if "@" not in value:
        _fail("--reviewed-git must be URL@full-40-hex-revision")
    url, rev = value.rsplit("@", 1)
    if not url or not _HEX40.fullmatch(rev):
        _fail("--reviewed-git must be URL@full-40-hex-revision")
    return url, rev


def _read_bundle_contract(
    path: Path,
    *,
    trusted_root: Path,
    candidate_root: Path,
    expected_sha256: str,
) -> BaseOwnedContract:
    return load_base_owned_contract(
        path,
        trusted_root=trusted_root,
        candidate_root=candidate_root,
        expected_sha256=expected_sha256,
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("build")
    build.add_argument("--source-root", type=Path, required=True)
    build.add_argument("--output", type=Path, required=True)
    build.add_argument("--source-revision")
    build.add_argument("--target-package", required=True)
    build.add_argument("--target-triple", default=DEFAULT_TARGET)
    build.add_argument("--cargo-home", type=Path, required=True)
    build.add_argument("--builder-base-digest", required=True)
    build.add_argument("--cargo-bin", default="cargo")
    build.add_argument("--rustc-bin", default="rustc")
    build.add_argument("--path", default=DEFAULT_PATH)
    build.add_argument("--reviewed-git", action="append", required=True)
    validate = commands.add_parser("validate")
    validate.add_argument("--bundle", type=Path, required=True)
    validate.add_argument("--contract", type=Path, required=True)
    validate.add_argument("--trusted-root", type=Path, required=True)
    validate.add_argument("--candidate-root", type=Path, required=True)
    validate.add_argument("--expected-contract-sha256", required=True)
    validate.add_argument("--cargo-home", type=Path, required=True)
    validate.add_argument("--reviewed-git", action="append")
    copy = commands.add_parser("copy-cache")
    copy.add_argument("--source", type=Path, required=True)
    copy.add_argument("--destination", type=Path, required=True)
    env = commands.add_parser("fetch-env")
    env.add_argument("--home", type=Path, required=True)
    env.add_argument("--cargo-home", type=Path, required=True)
    commands.add_parser("network-fetch")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "build":
            manifest = build_bundle(
                args.source_root,
                args.output,
                source_revision=args.source_revision,
                target_package=args.target_package,
                target_triple=args.target_triple,
                cargo_home=args.cargo_home,
                builder_base_digest=args.builder_base_digest,
                cargo_bin=args.cargo_bin,
                rustc_bin=args.rustc_bin,
                path=args.path,
                reviewed_git=tuple(_parse_reviewed_git(item) for item in args.reviewed_git),
            )
            print(json.dumps(manifest, indent=2, sort_keys=True))
            return 0
        if args.command == "validate":
            reviewed = (
                tuple(_parse_reviewed_git(item) for item in args.reviewed_git)
                if args.reviewed_git
                else None
            )
            manifest = validate_bundle(
                args.bundle,
                contract=_read_bundle_contract(
                    args.contract,
                    trusted_root=args.trusted_root,
                    candidate_root=args.candidate_root,
                    expected_sha256=args.expected_contract_sha256,
                ),
                reviewed_git=reviewed,
                cargo_home=args.cargo_home,
            )
            print(json.dumps(manifest, indent=2, sort_keys=True))
            return 0
        if args.command == "copy-cache":
            print(
                json.dumps(
                    copy_cache_bounded(args.source, args.destination),
                    sort_keys=True,
                )
            )
            return 0
        if args.command == "fetch-env":
            print(
                json.dumps(
                    fetch_environment(args.home, args.cargo_home),
                    sort_keys=True,
                )
            )
            return 0
        _fail(
            "networked prefetch is gated: host egress cannot be enforced by this "
            "prototype; use a separately reviewed container/firewall boundary"
        )
    except PrefetchError as exc:
        print(f"bootstrap-prefetch: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
