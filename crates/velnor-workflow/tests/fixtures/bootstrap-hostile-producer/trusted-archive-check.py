#!/usr/bin/env python3
"""Base-owned safe archive and disposable-tree checker.

This helper is not candidate code. The hosted wrapper measures this file before
using it. It never follows archive links, extracts outside a fresh directory,
or prints member names/bytes. Exit 0 means a tree/archive is acceptable; exit 2
means a deliberately hostile disposable output tree was rejected; exit 1 is a
checker error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
import tarfile
import zipfile
from pathlib import Path
from typing import BinaryIO, Iterable, NamedTuple


MAX_ARCHIVE_BYTES = 64 * 1024 * 1024
MAX_MEMBERS = 4096
MAX_UNPACKED_BYTES = 256 * 1024 * 1024
MAX_OUTPUT_BYTES = 64 * 1024 * 1024
MAX_OUTPUT_ENTRIES = 4096
MAX_JSON_BYTES = 4 * 1024 * 1024


class Entry(NamedTuple):
    name: str
    directory: bool
    size: int
    source: object


class TreeAudit(NamedTuple):
    status: str
    entries: int
    bytes: int
    read_errors: int
    unexpected: int
    missing: int
    violations: int


class CheckError(Exception):
    pass


def fail(code: str, exit_code: int = 1) -> None:
    print(f"trusted-archive-check:{code}", file=sys.stderr)
    raise SystemExit(exit_code)


def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise CheckError("duplicate-key")
        result[key] = value
    return result


def load_json(path: Path) -> object:
    try:
        if path.stat().st_size > MAX_JSON_BYTES or not path.is_file() or path.is_symlink():
            raise CheckError("json-limit")
        with path.open("r", encoding="utf-8") as stream:
            return json.load(stream, object_pairs_hook=reject_duplicate_keys)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CheckError("json-invalid") from exc


def validate_schema(value: object, schema: dict[str, object]) -> None:
    if "const" in schema and value != schema["const"]:
        raise CheckError("schema-const")
    schema_type = schema.get("type")
    if schema_type == "object":
        if not isinstance(value, dict):
            raise CheckError("schema-object")
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            raise CheckError("schema-properties")
        required = schema.get("required", [])
        if not isinstance(required, list) or any(
            not isinstance(name, str) for name in required
        ):
            raise CheckError("schema-required")
        if any(name not in value for name in required):
            raise CheckError("schema-missing")
        if schema.get("additionalProperties") is False:
            if any(name not in properties for name in value):
                raise CheckError("schema-extra")
        for name, child_schema in properties.items():
            if name in value:
                if not isinstance(child_schema, dict):
                    raise CheckError("schema-child")
                validate_schema(value[name], child_schema)
    elif schema_type == "array":
        if not isinstance(value, list):
            raise CheckError("schema-array")
        item_schema = schema.get("items")
        if item_schema is not None:
            if not isinstance(item_schema, dict):
                raise CheckError("schema-items")
            for item in value:
                validate_schema(item, item_schema)
    elif schema_type == "string":
        if not isinstance(value, str):
            raise CheckError("schema-string")
        minimum = schema.get("minLength")
        if isinstance(minimum, int) and len(value) < minimum:
            raise CheckError("schema-string-length")
        pattern = schema.get("pattern")
        if isinstance(pattern, str) and re.fullmatch(pattern, value) is None:
            raise CheckError("schema-string-pattern")
    elif schema_type == "integer":
        if not isinstance(value, int) or isinstance(value, bool):
            raise CheckError("schema-integer")
        minimum = schema.get("minimum")
        if isinstance(minimum, int) and value < minimum:
            raise CheckError("schema-integer-minimum")
    elif schema_type == "number":
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise CheckError("schema-number")
    elif schema_type is not None:
        raise CheckError("schema-type")


def validate_handoff(schema_path: Path, handoff_path: Path) -> None:
    schema = load_json(schema_path)
    handoff = load_json(handoff_path)
    if not isinstance(schema, dict) or not isinstance(handoff, dict):
        raise CheckError("schema-root")
    validate_schema(handoff, schema)


def normalize_name(raw: str) -> str:
    if not raw or "\x00" in raw or "\\" in raw:
        raise CheckError("name")
    if raw.startswith("/") or (len(raw) >= 2 and raw[1] == ":"):
        raise CheckError("absolute")
    parts = raw.split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise CheckError("traversal")
    return "/".join(parts)


def validate_entries(entries: Iterable[Entry]) -> list[Entry]:
    result = list(entries)
    if not result or len(result) > MAX_MEMBERS:
        raise CheckError("member-count")
    seen: dict[str, bool] = {}
    total = 0
    for entry in result:
        if entry.size < 0 or entry.size > MAX_UNPACKED_BYTES:
            raise CheckError("member-size")
        if entry.name in seen or any(
            entry.name.startswith(parent + "/") and not seen[parent]
            for parent in seen
        ):
            raise CheckError("duplicate-parent")
        if any(
            parent.startswith(entry.name + "/") and not entry.directory
            for parent in seen
        ):
            raise CheckError("file-parent")
        seen[entry.name] = entry.directory
        total += entry.size
        if total > MAX_UNPACKED_BYTES:
            raise CheckError("unpacked-size")
    return result


def tar_entries(archive: Path) -> tuple[list[Entry], tarfile.TarFile]:
    try:
        handle = tarfile.open(archive, mode="r:*")
    except (OSError, tarfile.TarError) as exc:
        raise CheckError("tar-open") from exc
    entries: list[Entry] = []
    try:
        for member in handle:
            try:
                name = normalize_name(member.name)
            except CheckError:
                raise
            if member.issym() or member.islnk() or member.isdev() or not (
                member.isdir() or member.isreg()
            ):
                raise CheckError("tar-link-type")
            entries.append(Entry(name, member.isdir(), member.size, member))
    except Exception:
        handle.close()
        raise
    return validate_entries(entries), handle


def zip_entries(archive: Path) -> tuple[list[Entry], zipfile.ZipFile]:
    try:
        handle = zipfile.ZipFile(archive, mode="r")
    except (OSError, zipfile.BadZipFile) as exc:
        raise CheckError("zip-open") from exc
    entries: list[Entry] = []
    try:
        for member in handle.infolist():
            if member.flag_bits & 0x1:
                raise CheckError("zip-encrypted")
            name = normalize_name(member.filename.rstrip("/") if member.is_dir() else member.filename)
            mode = (member.external_attr >> 16) & 0o170000
            if mode not in (0, stat.S_IFREG, stat.S_IFDIR):
                raise CheckError("zip-link-type")
            entries.append(Entry(name, member.is_dir(), member.file_size, member))
    except Exception:
        handle.close()
        raise
    return validate_entries(entries), handle


def archive_entries(archive: Path) -> tuple[str, list[Entry], object]:
    try:
        size = archive.stat().st_size
    except OSError as exc:
        raise CheckError("archive-stat") from exc
    if not archive.is_file() or archive.is_symlink() or size > MAX_ARCHIVE_BYTES:
        raise CheckError("archive-limit")
    if tarfile.is_tarfile(archive):
        entries, handle = tar_entries(archive)
        return "tar", entries, handle
    if zipfile.is_zipfile(archive):
        entries, handle = zip_entries(archive)
        return "zip", entries, handle
    raise CheckError("archive-format")


def safe_target(root: Path, name: str) -> Path:
    target = root.joinpath(*name.split("/"))
    root_resolved = root.resolve()
    target_parent = target.parent.resolve()
    if os.path.commonpath((str(root_resolved), str(target_parent))) != str(root_resolved):
        raise CheckError("extract-path")
    return target


def create_directory(root: Path, name: str) -> None:
    target = safe_target(root, name)
    target.mkdir(mode=0o755, parents=True, exist_ok=True)
    if target.is_symlink() or not target.is_dir():
        raise CheckError("extract-directory")


def write_file(root: Path, name: str, source: BinaryIO, expected_size: int) -> None:
    target = safe_target(root, name)
    target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    if target.is_symlink() or target.exists():
        raise CheckError("extract-collision")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(target, flags, 0o555)
    copied = 0
    try:
        with os.fdopen(descriptor, "wb") as output:
            while True:
                block = source.read(1024 * 1024)
                if not block:
                    break
                copied += len(block)
                if copied > expected_size or copied > MAX_UNPACKED_BYTES:
                    raise CheckError("extract-size")
                output.write(block)
    except Exception:
        try:
            target.unlink()
        except OSError:
            pass
        raise
    if copied != expected_size:
        raise CheckError("extract-short")


def extract_entries(kind: str, entries: list[Entry], handle: object, destination: Path) -> None:
    if destination.exists() or destination.is_symlink():
        raise CheckError("extract-destination")
    destination.mkdir(mode=0o700, parents=False)
    directories = [entry for entry in entries if entry.directory]
    files = [entry for entry in entries if not entry.directory]
    for entry in sorted(directories, key=lambda item: item.name.count("/")):
        create_directory(destination, entry.name)
    for entry in files:
        if kind == "tar":
            assert isinstance(handle, tarfile.TarFile)
            source = handle.extractfile(entry.source)
        else:
            assert isinstance(handle, zipfile.ZipFile)
            source = handle.open(entry.source, mode="r")
        if source is None:
            raise CheckError("extract-source")
        try:
            write_file(destination, entry.name, source, entry.size)
        finally:
            source.close()


def open_entry(kind: str, entry: Entry, handle: object) -> BinaryIO:
    if kind == "tar":
        assert isinstance(handle, tarfile.TarFile)
        source = handle.extractfile(entry.source)
    else:
        assert isinstance(handle, zipfile.ZipFile)
        source = handle.open(entry.source, mode="r")
    if source is None:
        raise CheckError("member-source")
    return source


def member_sha256(kind: str, entries: list[Entry], handle: object, name: str) -> tuple[str, int]:
    normalized = normalize_name(name)
    matches = [entry for entry in entries if entry.name == normalized and not entry.directory]
    if len(matches) != 1:
        raise CheckError("member-missing")
    digest = hashlib.sha256()
    source = open_entry(kind, matches[0], handle)
    copied = 0
    try:
        while True:
            block = source.read(1024 * 1024)
            if not block:
                break
            copied += len(block)
            if copied > matches[0].size:
                raise CheckError("member-size")
            digest.update(block)
    finally:
        source.close()
    if copied != matches[0].size:
        raise CheckError("member-short")
    return digest.hexdigest(), copied


def check_tree(
    root: Path,
    allowed_files: set[str],
    allowed_directories: set[str],
    required_names: set[str],
) -> TreeAudit:
    if not root.is_dir() or root.is_symlink():
        return TreeAudit("rejected", 0, 0, 1, 0, len(required_names), 1)
    entries = 0
    bytes_total = 0
    read_errors = 0
    unexpected = 0
    violations = 0
    seen: set[str] = set()
    inodes: set[tuple[int, int]] = set()
    pending: list[tuple[Path, str]] = [(root, "")]
    while pending:
        current, prefix = pending.pop()
        try:
            with os.scandir(current) as iterator:
                children = list(iterator)
        except OSError:
            read_errors += 1
            violations += 1
            continue
        for child in children:
            relative = f"{prefix}/{child.name}" if prefix else child.name
            try:
                relative = normalize_name(relative)
            except CheckError:
                violations += 1
                continue
            seen.add(relative)
            entries += 1
            if entries > MAX_OUTPUT_ENTRIES:
                violations += 1
                continue
            try:
                info = child.stat(follow_symlinks=False)
            except OSError:
                read_errors += 1
                violations += 1
                continue
            is_directory = stat.S_ISDIR(info.st_mode)
            is_file = stat.S_ISREG(info.st_mode)
            is_link_or_special = stat.S_ISLNK(info.st_mode) or not (is_directory or is_file)
            if is_directory:
                pending.append((Path(child.path), relative))
            if is_directory and relative not in allowed_directories:
                unexpected += 1
            if is_file and relative not in allowed_files:
                unexpected += 1
            if not is_directory and not is_file:
                violations += 1
            if is_link_or_special:
                violations += 1
                continue
            if is_file:
                if info.st_nlink > 1:
                    violations += 1
                bytes_total += info.st_size
                if bytes_total > MAX_OUTPUT_BYTES:
                    violations += 1
                inode = (info.st_dev, info.st_ino)
                if inode in inodes:
                    violations += 1
                inodes.add(inode)
    missing = len(required_names - seen)
    violations += missing + unexpected + read_errors
    status = "accepted" if violations == 0 else "rejected"
    return TreeAudit(status, entries, bytes_total, read_errors, unexpected, missing, violations)


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--extract", metavar="DEST")
    parser.add_argument("--check-tree", metavar="ROOT")
    parser.add_argument("--allow-file", action="append", default=[], metavar="NAME")
    parser.add_argument("--allow-dir", action="append", default=[], metavar="NAME")
    parser.add_argument("--require-name", action="append", default=[], metavar="NAME")
    parser.add_argument("--validate-handoff", nargs=2, metavar=("SCHEMA", "HANDOFF"))
    parser.add_argument("--member-sha256", metavar="NAME")
    parser.add_argument("archive", nargs="?")
    args = parser.parse_args()
    mode_count = sum(
        value is not None
        for value in (args.extract, args.check_tree, args.validate_handoff, args.member_sha256)
    )
    if mode_count > 1:
        fail("arguments")
    if args.validate_handoff:
        if args.archive or args.allow_file or args.allow_dir or args.require_name:
            fail("arguments")
        try:
            validate_handoff(Path(args.validate_handoff[0]), Path(args.validate_handoff[1]))
        except (CheckError, OSError, ValueError):
            fail("handoff-invalid")
        print(
            json.dumps(
                {"schema": "velnor.bootstrap-handoff-validation.v1", "status": "valid"},
                separators=(",", ":"),
            )
        )
        return 0
    if args.check_tree:
        if args.archive:
            fail("arguments")
        try:
            allowed_files = {normalize_name(name) for name in args.allow_file}
            allowed_directories = {normalize_name(name) for name in args.allow_dir}
            required_names = {normalize_name(name) for name in args.require_name}
            if allowed_files & allowed_directories:
                raise CheckError("tree-type-duplicate")
            audit = check_tree(
                Path(args.check_tree), allowed_files, allowed_directories, required_names
            )
        except (CheckError, OSError, ValueError):
            fail("tree-invalid")
        print(
            json.dumps(
                {
                    "schema": "velnor.bootstrap-hostile-tree.v1",
                    "status": audit.status,
                    "entries": audit.entries,
                    "bytes": audit.bytes,
                    "read_errors": audit.read_errors,
                    "unexpected": audit.unexpected,
                    "missing": audit.missing,
                    "violations": audit.violations,
                },
                separators=(",", ":"),
            )
        )
        return 0 if audit.status == "accepted" else 2
    if args.member_sha256 and not args.archive:
        fail("archive-required")
    if args.member_sha256:
        try:
            kind, entries, handle = archive_entries(Path(args.archive))
            digest, size = member_sha256(kind, entries, handle, args.member_sha256)
        except (CheckError, OSError, ValueError, tarfile.TarError, zipfile.BadZipFile):
            fail("member-invalid")
        finally:
            if "handle" in locals():
                handle.close()
        print(
            json.dumps(
                {
                    "schema": "velnor.bootstrap-member.v1",
                    "status": "valid",
                    "sha256": digest,
                    "bytes": size,
                },
                separators=(",", ":"),
            )
        )
        return 0
    if args.allow_file or args.allow_dir or args.require_name:
        fail("arguments")
    if not args.archive:
        fail("archive-required")
    try:
        kind, entries, handle = archive_entries(Path(args.archive))
        if args.extract:
            extract_entries(kind, entries, handle, Path(args.extract))
    except (CheckError, OSError, ValueError, tarfile.TarError, zipfile.BadZipFile):
        fail("rejected")
    finally:
        if "handle" in locals():
            handle.close()
    print(
        json.dumps(
            {
                "schema": "velnor.bootstrap-archive.v1",
                "format": kind,
                "members": len(entries),
                "bytes": sum(entry.size for entry in entries),
            },
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        fail("interrupted")
