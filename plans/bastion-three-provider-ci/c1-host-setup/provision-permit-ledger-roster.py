#!/usr/bin/env python3
"""Validate and install the Velnor host permit-source roster.

The caller must resolve effective daemon paths from systemd/env/config and
hold the package transaction lock with all Velnor admission units drained.
This helper serializes roster writers; it does not stop or fence services.
"""

from __future__ import annotations

import argparse
import fcntl
import os
import pathlib
import re
import secrets
import stat
import subprocess
import sys
import tomllib
from collections import defaultdict
from contextlib import contextmanager
from typing import Iterator


ROSTER_DEFAULT = pathlib.Path("/etc/velnor/permit-ledger.sources")
MAX_ROSTER_BYTES = 1024 * 1024
MAX_SOURCE_ENTRIES = 8192
DIRECTORY_MODE = 0o750
DATABASE_MODE = 0o600
ROSTER_MODE = 0o644
LOCK_MODE = 0o600
DEFAULT_STATE_DB = pathlib.Path("/var/lib/velnor/state.db")
DEFAULT_WORKING_DIRECTORY = pathlib.Path("/var/lib/velnor")
DEFAULT_SLOT_COUNT = 4

ALL_KINDS = ("permit-ledger", "state-db", "demand-db", "native-slot")


class RosterError(ValueError):
    """Unsafe, incomplete, or malformed roster input."""


def _rooted_path(root: pathlib.Path, value: str | pathlib.Path, base: pathlib.Path) -> pathlib.Path:
    text = _path_text(value, "configured")
    path = pathlib.Path(text)
    if not path.is_absolute():
        path = base / path
    if root == pathlib.Path("/"):
        return _normalize_absolute_path(path, "configured")
    if not path.is_absolute():
        raise RosterError(f"configured path must be absolute: {path}")

    # For an alternate --root, interpret the configured absolute path inside
    # that tree. Checking the same spelling against the host root would follow
    # host-only links such as macOS `/var -> /private/var` and inspect the wrong
    # filesystem. Keep the guest path's `..` rules while checking components
    # under the fixture root.
    guest = pathlib.Path("/")
    actual = root
    missing_component = False
    for component in path.parts[1:]:
        if component in ("", "."):
            continue
        if component == "..":
            if missing_component:
                raise RosterError(
                    f"configured path traverses `..` after a missing component: {path}"
                )
            try:
                metadata = actual.lstat()
            except OSError as error:
                raise RosterError(
                    f"cannot inspect configured path component {actual}: {error}"
                ) from error
            if not stat.S_ISDIR(metadata.st_mode):
                raise RosterError(
                    f"configured path traverses `..` after a non-directory component: {actual}"
                )
            if guest != pathlib.Path("/"):
                guest = guest.parent
                actual = actual.parent
            continue
        guest = guest / component
        actual = actual / component
        try:
            metadata = actual.lstat()
        except FileNotFoundError:
            missing_component = True
            continue
        except OSError as error:
            raise RosterError(f"cannot inspect configured path component {actual}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"refusing symlink in configured path: {actual}")
    return actual


def _read_environment_file(path: pathlib.Path, *, owner_uid: int) -> dict[str, str]:
    _validate_existing_parents(path, owner_uid=owner_uid, label="daemon environment file")
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return {}
    except OSError as error:
        raise RosterError(f"cannot inspect daemon environment file {path}: {error}") from error
    if stat.S_ISLNK(metadata.st_mode):
        raise RosterError(f"daemon environment file is a symlink: {path}")
    _validate_regular_file(path, metadata, label="daemon environment file", owner_uid=owner_uid, writable=False)
    if metadata.st_size > MAX_ROSTER_BYTES:
        raise RosterError(f"daemon environment file is too large: {path}")
    try:
        contents = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise RosterError(f"cannot read daemon environment file {path}: {error}") from error
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(_environment_logical_lines(contents), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#") or line.startswith(";"):
            continue
        if "=" not in line:
            raise RosterError(f"unsupported daemon environment syntax at {path}:{line_number}")
        name, raw_value = line.split("=", 1)
        name = name.strip()
        if not name:
            raise RosterError(f"empty daemon environment key at {path}:{line_number}")
        values[name] = _environment_value(raw_value)
    return values


def _environment_logical_lines(contents: str) -> Iterator[str]:
    """Match daemon_instance.rs logical_lines for EnvironmentFile parsing."""
    current = ""
    for line in _rust_lines(contents):
        if line.endswith("\\"):
            current += line[:-1]
            current += " "
            continue
        current += line
        yield current
        current = ""
    if current:
        yield current


def _environment_value(value: str) -> str:
    """Match daemon_instance.rs quote removal and double-quoted unescaping."""
    value = value.strip()
    if len(value) >= 2 and value[0] in ('"', "'") and value.endswith(value[0]):
        quote = value[0]
        inner = value[1:-1]
        if quote != '"':
            return inner
        output: list[str] = []
        index = 0
        while index < len(inner):
            character = inner[index]
            if character == "\\" and index + 1 < len(inner):
                escaped = inner[index + 1]
                if escaped == "n":
                    output.append("\n")
                elif escaped == "t":
                    output.append("\t")
                else:
                    output.append(escaped)
                index += 2
            else:
                output.append(character)
                index += 1
        return "".join(output)
    return value


def _daemon_paths_from_environment(
    *,
    root: pathlib.Path,
    instance: str | None,
    owner_uid: int,
) -> tuple[pathlib.Path, list[pathlib.Path], list[pathlib.Path], list[pathlib.Path]]:
    if instance is None:
        environment_files = ("velnor.env", "secrets.env")
        state_directory = pathlib.Path("/var/lib/velnor")
        working_directory = DEFAULT_WORKING_DIRECTORY
    else:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", instance):
            raise RosterError(f"unsupported systemd daemon instance name: {instance}")
        environment_files = (f"{instance}.env", f"{instance}.secrets.env")
        state_directory = pathlib.Path(f"/var/lib/velnor-{instance}")
        working_directory = state_directory

    environment: dict[str, str] = {}
    for filename in environment_files:
        environment.update(
            _read_environment_file(
                root / "etc" / "velnor" / filename,
                owner_uid=owner_uid,
            )
        )

    raw_state_db = environment.get("VELNOR_STATE_DB", str(DEFAULT_STATE_DB))
    if not raw_state_db:
        raise RosterError("VELNOR_STATE_DB is empty")
    state_db = _rooted_path(root, raw_state_db, working_directory)

    raw_ledger = environment.get("VELNOR_PERMIT_LEDGER", "")
    if raw_ledger:
        ledger = _rooted_path(root, raw_ledger, working_directory)
    else:
        ledger = state_db.parent / "permit-ledger.db"

    raw_config_dir = environment.get("VELNOR_CONFIG_DIR", "")
    config_base = (
        _rooted_path(root, raw_config_dir, working_directory)
        if raw_config_dir
        else root / state_directory.relative_to("/") / "runner"
    )
    name = environment.get("VELNOR_NAME", "")
    if not name:
        name = environment.get("VELNOR_WORK_DIR", "") or environment.get("VELNOR_URL", "")
    if not name:
        raise RosterError("daemon identity is unresolved; set VELNOR_NAME, work-dir, or URL")
    sanitized = "".join(
        character if character.isascii() and (character.isalnum() or character in ".-_") else "-"
        for character in name
    ).strip("-") or "default"
    daemon_config_dir = config_base / "daemons" / sanitized

    slots_text = environment.get("VELNOR_SLOTS", str(DEFAULT_SLOT_COUNT))
    if not re.fullmatch(r"[1-9][0-9]*", slots_text):
        raise RosterError(f"VELNOR_SLOTS is not a positive integer: {slots_text!r}")
    slots = int(slots_text)
    if slots > MAX_SOURCE_ENTRIES:
        raise RosterError(f"VELNOR_SLOTS exceeds roster capacity: {slots}")
    if slots == 1:
        native_slots = [daemon_config_dir]
    else:
        native_slots = [
            daemon_config_dir / "slots" / f"slot-{slot_index}"
            for slot_index in range(1, slots + 1)
        ]

    state_dbs = [state_db]
    demand_dbs: list[pathlib.Path] = []
    raw_scale_config = environment.get("VELNOR_SCALE_SET_CONFIG")
    if raw_scale_config is not None:
        if not raw_scale_config:
            raise RosterError("VELNOR_SCALE_SET_CONFIG is empty")
        scale_config = _rooted_path(root, raw_scale_config, working_directory)
        _validate_existing_parents(
            scale_config,
            owner_uid=owner_uid,
            label="Scale Set config",
        )
        try:
            metadata = scale_config.lstat()
        except OSError as error:
            raise RosterError(f"cannot inspect Scale Set config {scale_config}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"Scale Set config is a symlink: {scale_config}")
        _validate_regular_file(
            scale_config,
            metadata,
            label="Scale Set config",
            owner_uid=owner_uid,
            writable=False,
        )
        try:
            scale_values = tomllib.loads(scale_config.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
            raise RosterError(f"cannot parse Scale Set config {scale_config}: {error}") from error
        raw_scale_db = scale_values.get("state_db")
        if raw_scale_db is None:
            scale_db = state_db
        elif isinstance(raw_scale_db, str) and raw_scale_db:
            scale_db = _rooted_path(root, raw_scale_db, working_directory)
        else:
            raise RosterError("Scale Set state_db must be a nonempty string when present")
        state_dbs.append(scale_db)
        demand_dbs.append(scale_db)

    return ledger, state_dbs, demand_dbs, native_slots


def _path_text(value: str | os.PathLike[str], label: str) -> str:
    text = os.fspath(value)
    if not isinstance(text, str) or not text:
        raise RosterError(f"{label} path is empty or not text")
    if "\x00" in text or "\n" in text or "\r" in text:
        raise RosterError(f"{label} path cannot contain NUL or newline")
    if text != text.strip():
        raise RosterError(f"{label} path cannot start or end with whitespace")
    try:
        text.encode("utf-8")
    except UnicodeEncodeError as error:
        raise RosterError(f"{label} path is not valid UTF-8") from error
    return text


def _paths_overlap(left: pathlib.Path, right: pathlib.Path) -> bool:
    return left == right or left in right.parents or right in left.parents


def _reject_reserved_aliases(
    entries: list[tuple[str, pathlib.Path]],
    *,
    requested: dict[str, list[pathlib.Path]],
    roster_path: pathlib.Path,
    lock_path: pathlib.Path,
) -> None:
    reserved = (roster_path, lock_path)
    for kind, paths in requested.items():
        for path in paths:
            if kind == "native-slot":
                if any(_paths_overlap(path, item) for item in reserved):
                    raise RosterError(
                        f"native-slot path cannot contain or alias the roster or its lock: {path}"
                    )
            elif path in reserved:
                raise RosterError(
                    f"{kind} path cannot alias the permit-source roster or its lock: {path}"
                )
    for kind, path in entries:
        if kind == "native-slot":
            if any(_paths_overlap(path, item) for item in reserved):
                raise RosterError(
                    f"existing native-slot path overlaps the permit-source roster or its lock: {path}"
                )
        elif path in reserved:
            raise RosterError(
                f"existing roster {kind} path aliases the roster or its lock: {path}"
            )


def _absolute_path(value: str | os.PathLike[str], label: str) -> pathlib.Path:
    text = _path_text(value, label)
    path = pathlib.Path(text)
    if not path.is_absolute():
        raise RosterError(f"{label} path must be absolute: {text}")
    return _normalize_absolute_path(path, label)


def _rust_lines(contents: str) -> Iterator[str]:
    """Yield lines using Rust `str::lines()` terminators, not Python splitlines()."""
    pieces = contents.split("\n")
    if contents.endswith("\n"):
        pieces.pop()
    for line in pieces:
        yield line[:-1] if line.endswith("\r") else line


def _normalize_absolute_path(path: pathlib.Path, label: str) -> pathlib.Path:
    """Normalize lexically while matching filesystem traversal around `..`."""
    if not path.is_absolute():
        raise RosterError(f"{label} path must be absolute: {path}")
    current = pathlib.Path("/")
    missing_component = False
    for component in path.parts[1:]:
        if component in ("", "."):
            continue
        if component == "..":
            if missing_component:
                raise RosterError(
                    f"{label} path traverses `..` after a missing component: {path}"
                )
            try:
                metadata = current.lstat()
            except OSError as error:
                raise RosterError(
                    f"cannot inspect {label} path component {current}: {error}"
                ) from error
            if not stat.S_ISDIR(metadata.st_mode):
                raise RosterError(
                    f"{label} path traverses `..` after a non-directory component: {current}"
                )
            current = current.parent
            continue
        current = current / component
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            missing_component = True
            continue
        except OSError as error:
            raise RosterError(
                f"cannot inspect {label} path component {current}: {error}"
            ) from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"refusing symlink in {label} path: {current}")
    return current


def _reject_symlink_components(path: pathlib.Path, label: str) -> None:
    """Reject path redirection before resolving any component."""
    _normalize_absolute_path(path, label)


def _mode(metadata: os.stat_result) -> int:
    return stat.S_IMODE(metadata.st_mode)


def _fsync_directory(path: pathlib.Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
    fd = os.open(path, flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _validate_directory(
    path: pathlib.Path,
    metadata: os.stat_result,
    *,
    label: str,
    owner_uid: int,
    is_target: bool,
) -> None:
    if not stat.S_ISDIR(metadata.st_mode):
        raise RosterError(f"{label} is not a directory: {path}")
    if is_target and metadata.st_uid != owner_uid:
        raise RosterError(f"{label} directory must be owned by uid {owner_uid}: {path}")
    if not is_target and metadata.st_uid not in {0, owner_uid}:
        raise RosterError(f"{label} parent directory has unexpected owner: {path}")
    mode = _mode(metadata)
    sticky_root_tmp = (
        not is_target
        and metadata.st_uid == 0
        and bool(mode & stat.S_ISVTX)
    )
    if mode & 0o022 and not sticky_root_tmp:
        raise RosterError(f"{label} directory is group/world writable: {path}")
    if mode & (stat.S_ISUID | stat.S_ISGID):
        raise RosterError(f"{label} directory has special permission bits: {path}")


def _ensure_directory_tree(
    path: pathlib.Path,
    *,
    create: bool,
    owner_uid: int,
    owner_gid: int,
    label: str,
) -> None:
    """Walk and validate each directory; create missing components safely."""
    path = _normalize_absolute_path(path, label)
    current = pathlib.Path("/")
    components = path.parts[1:]
    if not components:
        root_stat = current.lstat()
        _validate_directory(
            current,
            root_stat,
            label=label,
            owner_uid=owner_uid,
            is_target=True,
        )
        return

    for index, component in enumerate(components):
        current = current / component
        is_target = index + 1 == len(components)
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            if not create:
                raise RosterError(f"{label} directory is missing: {current}")
            created = False
            try:
                current.mkdir(mode=DIRECTORY_MODE)
                created = True
            except FileExistsError:
                pass
            except OSError as error:
                raise RosterError(f"cannot create {label} directory {current}: {error}") from error
            try:
                # A concurrent creator may win between lstat() and mkdir().
                # Validate that entry as-is; never silently take ownership of
                # an object we did not create.
                if created:
                    os.chown(current, owner_uid, owner_gid, follow_symlinks=False)
                    os.chmod(current, DIRECTORY_MODE, follow_symlinks=False)
                    _fsync_directory(current.parent)
                    _fsync_directory(current)
                metadata = current.lstat()
            except OSError as error:
                raise RosterError(f"cannot secure {label} directory {current}: {error}") from error
        except OSError as error:
            raise RosterError(f"cannot inspect {label} directory {current}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"refusing symlink in {label} directory path: {current}")
        _validate_directory(
            current,
            metadata,
            label=label,
            owner_uid=owner_uid,
            is_target=is_target,
        )
        _fsync_directory(current.parent)


def _validate_regular_file(
    path: pathlib.Path,
    metadata: os.stat_result,
    *,
    label: str,
    owner_uid: int,
    writable: bool,
    private_database: bool = False,
) -> None:
    if not stat.S_ISREG(metadata.st_mode):
        raise RosterError(f"{label} must be a regular file: {path}")
    if metadata.st_uid != owner_uid:
        raise RosterError(f"{label} must be owned by uid {owner_uid}: {path}")
    if metadata.st_nlink != 1:
        raise RosterError(f"{label} must have exactly one hard link: {path}")
    mode = _mode(metadata)
    if private_database and mode != DATABASE_MODE:
        raise RosterError(
            f"{label} must have mode {DATABASE_MODE:04o}, got {mode:04o}: {path}"
        )
    if mode & 0o022:
        raise RosterError(f"{label} is group/world writable: {path}")
    if not mode & 0o400:
        raise RosterError(f"{label} is not owner-readable: {path}")
    if writable and mode & 0o200 == 0:
        raise RosterError(f"{label} is not owner-writable: {path}")
    if mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
        raise RosterError(f"{label} has special permission bits: {path}")


def _ensure_file(
    path: pathlib.Path,
    *,
    create: bool,
    owner_uid: int,
    owner_gid: int,
    label: str,
) -> pathlib.Path:
    _reject_symlink_components(path, label)
    path = _normalize_absolute_path(path, label)
    _ensure_directory_tree(
        path.parent,
        create=create,
        owner_uid=owner_uid,
        owner_gid=owner_gid,
        label=f"{label} parent",
    )
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        if not create:
            raise RosterError(f"{label} file is missing: {path}")
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        try:
            fd = os.open(path, flags, DATABASE_MODE)
            try:
                os.fchown(fd, owner_uid, owner_gid)
                os.fchmod(fd, DATABASE_MODE)
                os.fsync(fd)
                metadata = os.fstat(fd)
            finally:
                os.close(fd)
        except FileExistsError:
            metadata = path.lstat()
        except OSError as error:
            raise RosterError(f"cannot create {label} file {path}: {error}") from error
    except OSError as error:
        raise RosterError(f"cannot inspect {label} file {path}: {error}") from error
    _fsync_directory(path.parent)
    if stat.S_ISLNK(metadata.st_mode):
        raise RosterError(f"refusing symlink for {label}: {path}")
    _validate_regular_file(
        path,
        metadata,
        label=label,
        owner_uid=owner_uid,
        writable=True,
        private_database=True,
    )
    return path.resolve(strict=True)


def _ensure_native_directory(
    path: pathlib.Path,
    *,
    create: bool,
    owner_uid: int,
    owner_gid: int,
) -> pathlib.Path:
    _reject_symlink_components(path, "native-slot")
    _ensure_directory_tree(
        path,
        create=create,
        owner_uid=owner_uid,
        owner_gid=owner_gid,
        label="native-slot",
    )
    return path.resolve(strict=True)


def _lexical_roster_path(raw_path: str, roster_parent: pathlib.Path) -> pathlib.Path:
    text = _path_text(raw_path, "roster entry")
    path = pathlib.Path(text)
    if not path.is_absolute():
        path = roster_parent / path
    return _normalize_absolute_path(path, "roster entry")


def _split_entry(line: str, line_number: int) -> tuple[str, str] | None:
    entry = line.strip()
    if not entry or entry.startswith("#"):
        return None
    # Match Rust's parser order and accepted spellings.
    for kind in ALL_KINDS:
        if entry.startswith(f"{kind} "):
            raw_path = entry[len(kind) + 1 :].strip()
            break
        if entry.startswith(f"{kind}="):
            raw_path = entry[len(kind) + 1 :].strip()
            break
    else:
        raise RosterError(f"unknown roster entry on line {line_number}")
    if not raw_path:
        raise RosterError(f"empty roster path on line {line_number}")
    _path_text(raw_path, f"roster line {line_number}")
    return kind, raw_path


def _parse_roster(
    contents: str,
    roster_parent: pathlib.Path,
) -> list[tuple[str, pathlib.Path]]:
    entries: list[tuple[str, pathlib.Path]] = []
    for line_number, line in enumerate(_rust_lines(contents), start=1):
        parsed = _split_entry(line, line_number)
        if parsed is None:
            continue
        kind, raw_path = parsed
        entries.append((kind, _lexical_roster_path(raw_path, roster_parent)))
    source_count = sum(kind != "permit-ledger" for kind, _path in entries)
    if source_count > MAX_SOURCE_ENTRIES:
        raise RosterError(f"roster has more than {MAX_SOURCE_ENTRIES} source entries")
    ledgers = [path for kind, path in entries if kind == "permit-ledger"]
    if len(ledgers) > 1:
        raise RosterError("roster declares permit-ledger more than once")
    return entries


def _read_roster(path: pathlib.Path, *, owner_uid: int) -> str | None:
    try:
        named = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise RosterError(f"cannot inspect roster {path}: {error}") from error
    if stat.S_ISLNK(named.st_mode):
        raise RosterError(f"refusing symlinked roster: {path}")
    _validate_regular_file(path, named, label="roster", owner_uid=owner_uid, writable=False)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        fd = os.open(path, flags)
    except OSError as error:
        raise RosterError(f"cannot open roster {path}: {error}") from error
    try:
        opened = os.fstat(fd)
        current = path.lstat()
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise RosterError(f"roster changed while being opened: {path}")
        _validate_regular_file(path, opened, label="roster", owner_uid=owner_uid, writable=False)
        if opened.st_size > MAX_ROSTER_BYTES:
            raise RosterError(f"roster exceeds {MAX_ROSTER_BYTES} bytes: {path}")
        chunks: list[bytes] = []
        remaining = MAX_ROSTER_BYTES + 1
        while remaining:
            chunk = os.read(fd, min(65536, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        raw = b"".join(chunks)
        if len(raw) > MAX_ROSTER_BYTES:
            raise RosterError(f"roster exceeds {MAX_ROSTER_BYTES} bytes: {path}")
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise RosterError(f"roster is not valid UTF-8: {path}") from error
    finally:
        os.close(fd)


def _validate_lock_fd(fd: int, path: pathlib.Path, *, owner_uid: int) -> None:
    opened = os.fstat(fd)
    try:
        named = path.lstat()
    except OSError as error:
        raise RosterError(f"cannot recheck roster lock {path}: {error}") from error
    if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(named.st_mode):
        raise RosterError(f"roster lock must be a regular non-symlink file: {path}")
    if (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino):
        raise RosterError(f"roster lock changed while being opened: {path}")
    if opened.st_uid != owner_uid or opened.st_nlink != 1:
        raise RosterError(f"roster lock must be singly linked and owned by uid {owner_uid}: {path}")
    mode = _mode(opened)
    if mode & 0o022 or mode & 0o600 != 0o600:
        raise RosterError(f"roster lock must be owner-readable/writable and not group/world writable: {path}")
    if mode != LOCK_MODE:
        raise RosterError(f"roster lock must have mode {LOCK_MODE:04o}: {path}")
    if mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
        raise RosterError(f"roster lock has special permission bits: {path}")


@contextmanager
def _roster_lock(
    path: pathlib.Path,
    *,
    create: bool,
    exclusive: bool,
    owner_uid: int,
    owner_gid: int,
) -> Iterator[None]:
    if create:
        _ensure_directory_tree(
            path.parent,
            create=True,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
            label="roster parent",
        )
    else:
        _ensure_directory_tree(
            path.parent,
            create=False,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
            label="roster parent",
        )
    flags = (os.O_RDWR if exclusive else os.O_RDONLY) | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    created = False
    try:
        if create:
            try:
                fd = os.open(path, flags | os.O_CREAT | os.O_EXCL, LOCK_MODE)
                created = True
            except FileExistsError:
                fd = os.open(path, flags)
        else:
            fd = os.open(path, flags)
    except OSError as error:
        raise RosterError(f"cannot open roster lock {path}: {error}") from error
    try:
        if created:
            os.fchown(fd, owner_uid, owner_gid)
            os.fchmod(fd, LOCK_MODE)
        _fsync_directory(path.parent)
        _validate_lock_fd(fd, path, owner_uid=owner_uid)
        fcntl.flock(fd, fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)
        _validate_lock_fd(fd, path, owner_uid=owner_uid)
        yield
    finally:
        os.close(fd)


def _validate_existing_parents(
    path: pathlib.Path,
    *,
    owner_uid: int,
    label: str,
) -> None:
    """Validate existing parent components before any create operation."""
    current = pathlib.Path("/")
    for component in path.parent.parts[1:]:
        current = current / component
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            return
        except OSError as error:
            raise RosterError(f"cannot inspect {label} parent {current}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"refusing symlink in {label} parent path: {current}")
        _validate_directory(
            current,
            metadata,
            label=f"{label} parent",
            owner_uid=owner_uid,
            is_target=False,
        )


def _preflight_requested_paths(
    ledger_path: pathlib.Path,
    state_db_paths: list[pathlib.Path],
    demand_db_paths: list[pathlib.Path],
    native_slot_paths: list[pathlib.Path],
    *,
    owner_uid: int,
) -> None:
    if not state_db_paths:
        raise RosterError("at least one --state-db is required")
    if not native_slot_paths:
        raise RosterError("at least one --native-slot is required")

    file_paths: dict[str, list[pathlib.Path]] = {
        "permit-ledger": [ledger_path],
        "state-db": list(dict.fromkeys(state_db_paths)),
        "demand-db": list(dict.fromkeys(demand_db_paths)),
    }
    native_paths = list(native_slot_paths)
    if len(set(native_paths)) != len(native_paths):
        raise RosterError("duplicate requested native-slot path")
    file_paths_flat = {path for paths in file_paths.values() for path in paths}
    if file_paths["permit-ledger"][0] in file_paths["state-db"]:
        raise RosterError("permit-ledger cannot also be a state-db")
    if file_paths["permit-ledger"][0] in file_paths["demand-db"]:
        raise RosterError("permit-ledger cannot also be a demand-db")
    if file_paths_flat.intersection(native_paths):
        raise RosterError("a native-slot directory cannot also be a database file")
    source_count = sum(len(file_paths[kind]) for kind in ("state-db", "demand-db")) + len(native_paths)
    if source_count > MAX_SOURCE_ENTRIES:
        raise RosterError(f"requested roster exceeds {MAX_SOURCE_ENTRIES} source entries")

    for kind, paths in file_paths.items():
        for path in paths:
            _validate_existing_parents(path, owner_uid=owner_uid, label=kind)
            try:
                metadata = path.lstat()
            except FileNotFoundError:
                continue
            except OSError as error:
                raise RosterError(f"cannot inspect {kind} path {path}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                raise RosterError(f"refusing symlink for {kind}: {path}")
            _validate_regular_file(
                path,
                metadata,
                label=kind,
                owner_uid=owner_uid,
                writable=True,
                private_database=True,
            )
    for path in native_paths:
        _validate_existing_parents(path, owner_uid=owner_uid, label="native-slot")
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise RosterError(f"cannot inspect native-slot path {path}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise RosterError(f"refusing symlink for native-slot: {path}")
        _validate_directory(
            path,
            metadata,
            label="native-slot",
            owner_uid=owner_uid,
            is_target=True,
        )


def _ensure_requested_paths(
    ledger_path: pathlib.Path,
    state_db_paths: list[pathlib.Path],
    demand_db_paths: list[pathlib.Path],
    native_slot_paths: list[pathlib.Path],
    *,
    create: bool,
    owner_uid: int,
    owner_gid: int,
) -> dict[str, list[pathlib.Path]]:
    if not state_db_paths:
        raise RosterError("at least one --state-db is required")
    if not native_slot_paths:
        raise RosterError("at least one --native-slot is required")

    requested: dict[str, list[pathlib.Path]] = defaultdict(list)
    requested["permit-ledger"].append(
        _ensure_file(
            ledger_path,
            create=create,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
            label="permit-ledger",
        )
    )
    for kind, paths in (
        ("state-db", state_db_paths),
        ("demand-db", demand_db_paths),
    ):
        for path in paths:
            canonical = _ensure_file(
                path,
                create=create,
                owner_uid=owner_uid,
                owner_gid=owner_gid,
                label=kind,
            )
            if canonical not in requested[kind]:
                requested[kind].append(canonical)
    for path in native_slot_paths:
        canonical = _ensure_native_directory(
            path,
            create=create,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
        )
        if canonical in requested["native-slot"]:
            raise RosterError(f"duplicate native-slot directory: {canonical}")
        requested["native-slot"].append(canonical)

    ledger = requested["permit-ledger"][0]
    for kind in ("state-db", "demand-db"):
        if ledger in requested[kind]:
            raise RosterError(f"permit-ledger cannot also be a {kind}: {ledger}")
    return requested


def _validate_existing_entries(
    entries: list[tuple[str, pathlib.Path]],
    requested: dict[str, list[pathlib.Path]],
    *,
    owner_uid: int,
) -> dict[str, list[pathlib.Path]]:
    canonical: dict[str, list[pathlib.Path]] = defaultdict(list)
    requested_by_kind = {
        kind: set(paths) for kind, paths in requested.items()
    }
    for kind, path in entries:
        candidate = pathlib.Path(os.path.abspath(path))
        try:
            metadata = candidate.lstat()
        except FileNotFoundError:
            if candidate not in requested_by_kind.get(kind, set()):
                raise RosterError(f"existing roster {kind} path is missing: {candidate}")
            # A matching requested path may be new; its required type and
            # ownership are checked after the requested paths are prepared.
            canonical[kind].append(candidate)
            continue
        except OSError as error:
            raise RosterError(f"cannot inspect existing roster {kind} path {candidate}: {error}") from error
        if kind == "native-slot":
            _reject_symlink_components(candidate, kind)
            _validate_directory(
                candidate,
                metadata,
                label=kind,
                owner_uid=owner_uid,
                is_target=True,
            )
            if metadata.st_nlink < 1:
                raise RosterError(f"invalid native-slot directory link count: {candidate}")
            resolved = candidate.resolve(strict=True)
        else:
            _reject_symlink_components(candidate, kind)
            _validate_regular_file(
                candidate,
                metadata,
                label=kind,
                owner_uid=owner_uid,
                writable=True,
                private_database=True,
            )
            resolved = candidate.resolve(strict=True)
        canonical[kind].append(resolved)

    for kind, paths in canonical.items():
        seen: set[pathlib.Path] = set()
        for path in paths:
            if path in seen:
                raise RosterError(f"roster contains duplicate {kind} path: {path}")
            seen.add(path)
    ledgers = canonical.get("permit-ledger", [])
    if len(ledgers) > 1:
        raise RosterError("roster declares permit-ledger more than once")
    return canonical


def _atomic_write_roster(
    path: pathlib.Path,
    contents: str,
    *,
    owner_uid: int,
    owner_gid: int,
    mode: int,
) -> None:
    parent = path.parent
    temp_path = parent / f".{path.name}.tmp.{os.getpid()}.{secrets.token_hex(6)}"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    fd: int | None = None
    try:
        fd = os.open(temp_path, flags, 0o600)
        os.fchown(fd, owner_uid, owner_gid)
        os.fchmod(fd, mode)
        data = contents.encode("utf-8")
        offset = 0
        while offset < len(data):
            written = os.write(fd, data[offset:])
            if written <= 0:
                raise OSError("roster write made no progress")
            offset += written
        os.fsync(fd)
        os.close(fd)
        fd = None
        os.replace(temp_path, path)
        directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
        directory_fd = os.open(parent, directory_flags)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except OSError as error:
        raise RosterError(f"cannot atomically write roster {path}: {error}") from error
    finally:
        if fd is not None:
            os.close(fd)
        try:
            temp_path.unlink()
        except FileNotFoundError:
            pass


def _render_roster(
    current: str,
    existing: dict[str, list[pathlib.Path]],
    requested: dict[str, list[pathlib.Path]],
) -> tuple[str, list[str]]:
    merged_source_count = sum(
        len(set(existing.get(kind, [])) | set(requested.get(kind, [])))
        for kind in ("state-db", "demand-db", "native-slot")
    )
    if merged_source_count > MAX_SOURCE_ENTRIES:
        raise RosterError(f"merged roster exceeds {MAX_SOURCE_ENTRIES} source entries")
    missing: list[tuple[str, pathlib.Path]] = []
    for kind in ALL_KINDS:
        have = set(existing.get(kind, []))
        for path in requested.get(kind, []):
            if path not in have:
                missing.append((kind, path))
    missing.sort(key=lambda item: (ALL_KINDS.index(item[0]), str(item[1])))
    if not missing:
        return current, []
    suffix = "" if not current or current.endswith("\n") else "\n"
    lines = [f"{kind} {path}" for kind, path in missing]
    return current + suffix + "\n".join(lines) + "\n", lines


def provision_roster(
    *,
    roster_path: pathlib.Path,
    ledger_path: pathlib.Path,
    state_db_paths: list[pathlib.Path],
    demand_db_paths: list[pathlib.Path],
    native_slot_paths: list[pathlib.Path],
    check_only: bool = False,
    owner_uid: int = 0,
    owner_gid: int = 0,
) -> list[str]:
    """Validate requested sources, merge the roster, and return added lines."""
    roster_path = _absolute_path(roster_path, "roster")
    ledger_path = _absolute_path(ledger_path, "permit-ledger")
    state_db_paths = [_absolute_path(path, "state-db") for path in state_db_paths]
    demand_db_paths = [_absolute_path(path, "demand-db") for path in demand_db_paths]
    native_slot_paths = [_absolute_path(path, "native-slot") for path in native_slot_paths]
    lock_path = _absolute_path(f"{roster_path}.lock", "roster lock")
    requested_paths: dict[str, list[pathlib.Path]] = {
        "permit-ledger": [ledger_path],
        "state-db": state_db_paths,
        "demand-db": demand_db_paths,
        "native-slot": native_slot_paths,
    }
    _reject_reserved_aliases(
        [],
        requested=requested_paths,
        roster_path=roster_path,
        lock_path=lock_path,
    )

    with _roster_lock(
        lock_path,
        create=not check_only,
        exclusive=not check_only,
        owner_uid=owner_uid,
        owner_gid=owner_gid,
    ):
        current = _read_roster(roster_path, owner_uid=owner_uid)
        if check_only and current is None:
            raise RosterError(f"roster is missing: {roster_path}")
        contents = current or ""
        entries = _parse_roster(contents, roster_path.parent)
        _reject_reserved_aliases(
            entries,
            requested=requested_paths,
            roster_path=roster_path,
            lock_path=lock_path,
        )

        # Existing entries may be absent only when this call is responsible
        # for creating that exact path. Unlisted paths are never resurrected.
        requested_lexical: dict[str, set[pathlib.Path]] = defaultdict(set)
        for kind, paths in (
            ("permit-ledger", [ledger_path]),
            ("state-db", state_db_paths),
            ("demand-db", demand_db_paths),
            ("native-slot", native_slot_paths),
        ):
            for path in paths:
                requested_lexical[kind].add(path)
        for kind, path in entries:
            candidate = path
            if not candidate.exists() and candidate not in requested_lexical.get(kind, set()):
                raise RosterError(f"existing roster {kind} path is missing: {candidate}")

        # Validate the old roster before creating any requested state. In
        # particular, a ledger mismatch must leave all database paths alone.
        requested_before_create: dict[str, list[pathlib.Path]] = defaultdict(list)
        for kind, paths in (
            ("permit-ledger", [ledger_path]),
            ("state-db", state_db_paths),
            ("demand-db", demand_db_paths),
            ("native-slot", native_slot_paths),
        ):
            requested_before_create[kind] = list(paths)
        old_entries = _validate_existing_entries(
            entries,
            requested_before_create,
            owner_uid=owner_uid,
        )
        old_ledger = old_entries.get("permit-ledger", [])
        expected_ledger = pathlib.Path(os.path.abspath(ledger_path))
        for kind in ("state-db", "demand-db"):
            if expected_ledger in old_entries.get(kind, []):
                raise RosterError(f"permit-ledger cannot also be a {kind}: {expected_ledger}")
        if old_ledger and old_ledger[0] != expected_ledger:
            raise RosterError(
                "existing permit-ledger entry resolves to a different ledger: "
                f"{old_ledger[0]} (expected {expected_ledger})"
            )

        _preflight_requested_paths(
            ledger_path,
            state_db_paths,
            demand_db_paths,
            native_slot_paths,
            owner_uid=owner_uid,
        )

        requested = _ensure_requested_paths(
            ledger_path,
            state_db_paths,
            demand_db_paths,
            native_slot_paths,
            create=not check_only,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
        )
        existing = _validate_existing_entries(entries, requested, owner_uid=owner_uid)
        old_ledger = existing.get("permit-ledger", [])
        if old_ledger and old_ledger[0] != requested["permit-ledger"][0]:
            raise RosterError(
                "existing permit-ledger entry resolves to a different ledger: "
                f"{old_ledger[0]} (expected {requested['permit-ledger'][0]})"
            )
        updated, added = _render_roster(contents, existing, requested)
        if check_only and added:
            raise RosterError("roster is missing required entries: " + ", ".join(added))
        if not check_only and updated != contents:
            roster_mode = ROSTER_MODE
            if current is not None:
                roster_mode = _mode(roster_path.lstat())
            _atomic_write_roster(
                roster_path,
                updated,
                owner_uid=owner_uid,
                owner_gid=owner_gid,
                mode=roster_mode,
            )
        return added


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Validate or provision /etc/velnor/permit-ledger.sources from the stock daemon "
            "environment, or pass effective absolute paths explicitly."
        )
    )
    parser.add_argument("--stock-daemon", action="store_true")
    parser.add_argument("--instance", action="append", default=[])
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("/"))
    parser.add_argument("--ledger", type=pathlib.Path)
    parser.add_argument("--state-db", type=pathlib.Path, action="append")
    parser.add_argument("--demand-db", type=pathlib.Path, action="append", default=[])
    parser.add_argument("--native-slot", type=pathlib.Path, action="append")
    parser.add_argument("--check", action="store_true", help="validate only; do not create or rewrite")
    parser.add_argument(
        "--package-lock-fd",
        type=int,
        help=argparse.SUPPRESS,
    )
    return parser


def _verify_apply_barrier(package_lock_fd: int, *, root: pathlib.Path, owner_uid: int) -> None:
    lock_path = root / "run" / "velnor" / "package-transaction.lock"
    try:
        descriptor_metadata = os.fstat(package_lock_fd)
        lock_metadata = lock_path.lstat()
    except OSError as error:
        raise RosterError(f"locked provisioner package-lock proof is unavailable: {error}") from error
    # Compare the open descriptor with the managed pathname by inode below.
    # `/proc/self/fd` is Linux-specific and unnecessary for this proof.
    if not stat.S_ISREG(descriptor_metadata.st_mode):
        raise RosterError("apply is supported only through the locked host provisioner")
    if not stat.S_ISREG(lock_metadata.st_mode) or stat.S_ISLNK(lock_metadata.st_mode):
        raise RosterError("package transaction lock must be a regular non-symlink file")
    if (
        descriptor_metadata.st_dev != lock_metadata.st_dev
        or descriptor_metadata.st_ino != lock_metadata.st_ino
        or descriptor_metadata.st_uid != owner_uid
        or descriptor_metadata.st_gid != os.getegid()
        or stat.S_IMODE(descriptor_metadata.st_mode) != LOCK_MODE
        or descriptor_metadata.st_nlink != 1
    ):
        raise RosterError("package transaction lock metadata does not match the managed lock")
    try:
        fcntl.flock(package_lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        raise RosterError("exclusive package transaction lock is not held") from error

    systemctl = root / "usr" / "bin" / "systemctl"
    systemd_runtime = root / "run" / "systemd" / "system"
    if not systemd_runtime.is_dir() or not systemctl.is_file() or not os.access(systemctl, os.X_OK):
        raise RosterError("apply requires a running systemd manager")
    try:
        result = subprocess.run(
            [
                str(systemctl),
                "list-units",
                "--all",
                "--plain",
                "--no-legend",
                "velnor*",
            ],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RosterError(f"cannot verify stopped Velnor units: {error}") from error
    if result.returncode != 0:
        raise RosterError("cannot verify stopped Velnor units")
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) < 4:
            continue
        unit, active_state = fields[0], fields[2]
        if unit.startswith("velnor") and unit.endswith((".service", ".socket", ".timer", ".path")):
            if active_state not in ("inactive", "failed", "dead"):
                raise RosterError(f"Velnor admission unit is not stopped: {unit} ({active_state})")


def _main_paths(args: argparse.Namespace) -> tuple[pathlib.Path, pathlib.Path, list[pathlib.Path], list[pathlib.Path], list[pathlib.Path], int, int]:
    if args.stock_daemon:
        if args.ledger or args.state_db or args.demand_db or args.native_slot:
            raise RosterError("--stock-daemon cannot be combined with explicit source paths")
        if len(set(args.instance)) != len(args.instance):
            raise RosterError("duplicate --instance")
        raw_root = args.root
        try:
            root = raw_root.resolve(strict=True)
            root_metadata = root.lstat()
        except OSError as error:
            raise RosterError(f"cannot resolve root fixture directory {raw_root}: {error}") from error
        if not stat.S_ISDIR(root_metadata.st_mode):
            raise RosterError(f"root fixture path is not a directory: {root}")
        if root == pathlib.Path("/"):
            if os.geteuid() != 0:
                raise RosterError("stock daemon provisioning on / requires uid 0")
            owner_uid, owner_gid = 0, 0
        else:
            if root_metadata.st_uid != os.geteuid() or root_metadata.st_mode & 0o022:
                raise RosterError(
                    "alternate --root must be owned by the caller and not group/world writable"
                )
            owner_uid, owner_gid = os.geteuid(), os.getegid()

        resolved = [
            _daemon_paths_from_environment(
                root=root,
                instance=instance,
                owner_uid=owner_uid,
            )
            for instance in (None, *args.instance)
        ]
        ledgers = {ledger for ledger, _states, _demands, _slots in resolved}
        if len(ledgers) != 1:
            raise RosterError(
                "configured daemon instances resolve different permit ledgers; set one shared VELNOR_PERMIT_LEDGER"
            )
        ledger = next(iter(ledgers))
        state_dbs = list(dict.fromkeys(path for _ledger, states, _demands, _slots in resolved for path in states))
        demand_dbs = list(dict.fromkeys(path for _ledger, _states, demands, _slots in resolved for path in demands))
        native_slots = [path for _ledger, _states, _demands, slots in resolved for path in slots]
        roster_path = root / ROSTER_DEFAULT.relative_to("/")
        return roster_path, ledger, state_dbs, demand_dbs, native_slots, owner_uid, owner_gid

    if args.instance or args.root != pathlib.Path("/"):
        raise RosterError("--instance and alternate --root require --stock-daemon")
    if args.ledger is None or not args.state_db or not args.native_slot:
        raise RosterError(
            "provide --stock-daemon or explicit --ledger, at least one --state-db, and at least one --native-slot"
        )
    return (
        ROSTER_DEFAULT,
        args.ledger,
        args.state_db,
        args.demand_db,
        args.native_slot,
        0,
        0,
    )


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.check and args.package_lock_fd is not None:
            raise RosterError("--package-lock-fd is valid only for provisioner apply")
        if not args.check:
            if not args.stock_daemon:
                raise RosterError("apply is supported only for stock daemons through the host provisioner")
            if args.package_lock_fd is None:
                raise RosterError("apply requires the host provisioner's exclusive package lock")
        roster_path, ledger, state_dbs, demand_dbs, native_slots, owner_uid, owner_gid = _main_paths(args)
        if not args.check and roster_path == ROSTER_DEFAULT and os.geteuid() != 0:
            raise RosterError("roster apply requires uid 0")
        if not args.check:
            root = args.root.resolve(strict=True)
            _verify_apply_barrier(args.package_lock_fd, root=root, owner_uid=owner_uid)
        added = provision_roster(
            roster_path=roster_path,
            ledger_path=ledger,
            state_db_paths=state_dbs,
            demand_db_paths=demand_dbs,
            native_slot_paths=native_slots,
            check_only=args.check,
            owner_uid=owner_uid,
            owner_gid=owner_gid,
        )
    except (OSError, RosterError) as error:
        print(f"permit-source roster preflight failed: {error}", file=sys.stderr)
        return 2
    if args.check:
        print(f"permit-source roster valid: {roster_path}")
    elif added:
        print(f"permit-source roster updated: {roster_path} ({len(added)} entries added)")
    else:
        print(f"permit-source roster unchanged: {roster_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
