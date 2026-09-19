#!/usr/bin/env python3
"""Bounded ancestor-swap stress probe for the offline checker CAS path.

This is synthetic only. It does not establish a race-proof result; it records
whether accepted reports occur while a store ancestor is repeatedly swapped
with an in-root symlink to an equally-byte-valued outside directory.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from pathlib import Path


HERE = Path(__file__).resolve().parent
CASE = HERE / "fixtures" / "baseline"
BIN = Path(os.environ.get("VELNOR_TOOLS_BIN", "/private/tmp/velnor-checker-target-ce894/debug/velnor-tools"))
WORKTREE = Path(os.environ.get("VELNOR_WORKTREE", "/private/tmp/velnor-checker-review-ce894"))
ROOT = CASE / "store"
SHA = ROOT / "sha256"
REAL = CASE / "sha256-real"
OUTSIDE = CASE / "toctou-outside"


def main() -> int:
    if REAL.exists() or REAL.is_symlink():
        raise RuntimeError(f"stale probe path: {REAL}")
    if OUTSIDE.exists():
        raise RuntimeError(f"stale probe path: {OUTSIDE}")
    OUTSIDE.mkdir()
    for item in SHA.iterdir():
        (OUTSIDE / item.name).write_bytes(item.read_bytes())
    stop = threading.Event()
    swap_count = 0
    swap_lock = threading.Lock()

    def swapper() -> None:
        nonlocal swap_count
        while not stop.is_set():
            try:
                os.rename(SHA, REAL)
                os.symlink(OUTSIDE, SHA)
                with swap_lock:
                    swap_count += 1
                time.sleep(0.004)
                os.unlink(SHA)
                os.rename(REAL, SHA)
            except FileNotFoundError:
                if SHA.is_symlink():
                    SHA.unlink()
                if REAL.exists():
                    os.rename(REAL, SHA)

    thread = threading.Thread(target=swapper, daemon=True)
    thread.start()
    reports: list[dict[str, object]] = []
    try:
        for _ in range(80):
            command = [
                str(BIN),
                "evidence-check",
                "--stage",
                "G0",
                "--manifest",
                str(CASE / "manifest.json"),
                "--snapshot",
                str(CASE / "snapshot.json"),
                "--evidence",
                str(CASE / "evidence.json"),
                "--evidence-root",
                str(ROOT),
                "--json",
            ]
            completed = subprocess.run(
                command,
                cwd=WORKTREE,
                capture_output=True,
                text=True,
                check=False,
                timeout=10,
            )
            reports.append(json.loads(completed.stdout))
    finally:
        stop.set()
        thread.join(timeout=2)
        if SHA.is_symlink():
            SHA.unlink()
        if REAL.exists():
            os.rename(REAL, SHA)
        for item in OUTSIDE.iterdir():
            item.unlink()
        OUTSIDE.rmdir()
    accepted = sum(
        1
        for report in reports
        if [finding["code"] for finding in report.get("findings", [])]
        == ["g0-authoritative-proof-missing"]
    )
    with swap_lock:
        swaps = swap_count
    result = {
        "synthetic": True,
        "iterations": len(reports),
        "ancestor_swaps": swaps,
        "accepted_authority_only": accepted,
        "finding_code_sets": sorted(
            {tuple(finding["code"] for finding in report.get("findings", [])) for report in reports}
        ),
        "interpretation": "accepted reports are not proof of overlap; source still has canonicalize/metadata/read TOCTOU window",
    }
    (HERE / "toctou-results.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
