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
import json
import os
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


class Entry(NamedTuple):
    name: str
    directory: bool
    size: int
    source: object


class CheckError(Exception):
    pass


def fail(code: str, exit_code: int = 1) -> None:
    print(f"trusted-archive-check:{code}", file=sys.stderr)
    raise SystemExit(exit_code)


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


def check_tree(root: Path) -> tuple[str, int, int]:
    if not root.is_dir() or root.is_symlink():
        raise CheckError("tree-root")
    entries = 0
    bytes_total = 0
    inodes: set[tuple[int, int]] = set()
    for current, directories, files in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        for name in directories + files:
            relative = (current_path / name).relative_to(root).as_posix()
            normalize_name(relative)
            target = current_path / name
            info = target.lstat()
            entries += 1
            if entries > MAX_OUTPUT_ENTRIES:
                raise CheckError("tree-entry-limit")
            if stat.S_ISLNK(info.st_mode) or not (
                stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)
            ):
                raise CheckError("tree-file-type")
            if stat.S_ISREG(info.st_mode):
                bytes_total += info.st_size
                if bytes_total > MAX_OUTPUT_BYTES:
                    raise CheckError("tree-byte-limit")
                inode = (info.st_dev, info.st_ino)
                if info.st_nlink > 1 or inode in inodes:
                    raise CheckError("tree-hardlink")
                inodes.add(inode)
        directories[:] = [name for name in directories if not (current_path / name).is_symlink()]
    return "accepted", entries, bytes_total


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--extract", metavar="DEST")
    parser.add_argument("--check-tree", metavar="ROOT")
    parser.add_argument("archive", nargs="?")
    args = parser.parse_args()
    if args.extract and args.check_tree or (args.check_tree and args.archive):
        fail("arguments")
    if args.check_tree:
        try:
            status, entries, total = check_tree(Path(args.check_tree))
        except (CheckError, OSError, ValueError):
            print(
                json.dumps(
                    {
                        "schema": "velnor.bootstrap-hostile-tree.v1",
                        "status": "rejected",
                    },
                    separators=(",", ":"),
                )
            )
            return 2
        print(
            json.dumps(
                {
                    "schema": "velnor.bootstrap-hostile-tree.v1",
                    "status": status,
                    "entries": entries,
                    "bytes": total,
                },
                separators=(",", ":"),
            )
        )
        return 0
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
