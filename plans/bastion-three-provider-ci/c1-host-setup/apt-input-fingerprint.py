#!/usr/bin/env python3
"""Fingerprint the exact APT config, source inputs, and Docker key used by C1."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import sys
from pathlib import Path


DEBIAN_ARCHIVE_KEYRING_GPG = Path(
    "/usr/share/keyrings/debian-archive-keyring.gpg"
)
DEBIAN_ARCHIVE_KEYRING_PGP = Path(
    "/usr/share/keyrings/debian-archive-keyring.pgp"
)


def file_record(path: Path, *, required: bool) -> dict[str, object]:
    try:
        info = path.lstat()
    except FileNotFoundError:
        if required:
            raise ValueError(f"required APT input is missing: {path}")
        return {"path": str(path), "state": "missing"}

    if stat.S_ISLNK(info.st_mode):
        if path != DEBIAN_ARCHIVE_KEYRING_GPG:
            raise ValueError(f"APT input is a symlink: {path}")
        link_target = os.readlink(path)
        if link_target not in {
            DEBIAN_ARCHIVE_KEYRING_PGP.name,
            str(DEBIAN_ARCHIVE_KEYRING_PGP),
        }:
            raise ValueError(f"APT input has an unapproved keyring alias: {path}")
        target_record = file_record(DEBIAN_ARCHIVE_KEYRING_PGP, required=True)
        if target_record.get("kind") != "file":
            raise ValueError(f"APT keyring alias target is not a regular file: {path}")
        return {
            "path": str(path),
            "kind": "symlink-alias",
            "uid": info.st_uid,
            "gid": info.st_gid,
            "mode": stat.S_IMODE(info.st_mode),
            "links": info.st_nlink,
            "target": str(DEBIAN_ARCHIVE_KEYRING_PGP),
            "target_record": target_record,
        }
    record: dict[str, object] = {
        "path": str(path),
        "uid": info.st_uid,
        "gid": info.st_gid,
        "mode": stat.S_IMODE(info.st_mode),
        "links": info.st_nlink,
    }
    if stat.S_ISREG(info.st_mode):
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        record.update(kind="file", sha256=digest.hexdigest())
    elif stat.S_ISDIR(info.st_mode):
        record["kind"] = "directory"
    else:
        raise ValueError(f"APT input is not a regular file or directory: {path}")
    return record


def fingerprint(config: Path, source_list: Path, source_parts: Path, keyrings: list[Path]) -> str:
    if not config.is_absolute() or not source_list.is_absolute() \
            or not source_parts.is_absolute() or not keyrings \
            or any(not keyring.is_absolute() for keyring in keyrings):
        raise ValueError("all APT evidence paths must be absolute")

    records: list[dict[str, object]] = [file_record(config, required=True)]
    records.append(file_record(source_list, required=False))
    parts_record = file_record(source_parts, required=False)
    records.append(parts_record)
    if parts_record.get("kind") == "directory":
        entries = []
        for path in sorted(source_parts.iterdir(), key=lambda item: os.fsencode(item.name)):
            if not (path.name.endswith(".list") or path.name.endswith(".sources")):
                continue
            entries.append(file_record(path, required=True))
        records.extend(entries)
    records.extend(file_record(keyring, required=False) for keyring in keyrings)
    payload = json.dumps(records, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def main(argv: list[str]) -> int:
    if len(argv) < 5:
        print("usage: apt-input-fingerprint.py APT_CONFIG SOURCE_LIST SOURCE_PARTS KEYRING...", file=sys.stderr)
        return 2
    try:
        print(fingerprint(Path(argv[1]), Path(argv[2]), Path(argv[3]), [Path(value) for value in argv[4:]]))
    except (OSError, ValueError) as error:
        print(f"APT input fingerprint failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
