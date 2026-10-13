#!/usr/bin/env python3
"""Split captured GitHub Actions log HTTP responses and extract their ZIPs."""

from pathlib import Path
from zipfile import ZipFile


ROOT = Path(__file__).resolve().parent
CAPTURES = (
    "jackin-run-35521080097-attempt-1-logs.http.txt",
    "jackin-run-35515575859-attempt-1-logs.http.txt",
)


def main() -> None:
    for capture in CAPTURES:
        raw = (ROOT / capture).read_bytes()
        marker = b"\r\n\r\n"
        split_at = raw.find(marker)
        if split_at < 0:
            raise SystemExit(f"missing HTTP header terminator: {capture}")
        headers = raw[:split_at].decode("latin-1")
        body = raw[split_at + len(marker) :]
        if not headers.startswith("HTTP/2.0 200") or not body.startswith(b"PK"):
            raise SystemExit(f"unexpected HTTP response/archive: {capture}")
        archive = ROOT / capture.replace(".http.txt", ".zip")
        archive.write_bytes(body)
        dest = ROOT / archive.stem
        dest.mkdir(exist_ok=True)
        with ZipFile(archive) as zf:
            for item in zf.infolist():
                # These archives are supplied by the authenticated GitHub API.
                # Extract only ordinary files; reject traversal paths.
                target = (dest / item.filename).resolve()
                if dest.resolve() not in target.parents:
                    raise SystemExit(f"unsafe path in {archive.name}: {item.filename}")
                if not item.is_dir():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(zf.read(item))
                print(f"{archive.name}\t{item.file_size}\t{item.filename}")


if __name__ == "__main__":
    main()
