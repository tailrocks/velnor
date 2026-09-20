#!/usr/bin/env python3
"""Resumable, date-windowed collection of Jackin Actions run metadata.

Uses the authenticated GitHub REST API. Date windows keep each filtered query
below the documented 1,000-result search ceiling. Responses are stored as
gzip-compressed raw HTTP exchanges so status, rate-limit headers, and JSON stay
auditable. This script writes evidence only; it does not access Git state.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import math
import re
import subprocess
import sys
from datetime import date, datetime, time, timedelta, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlencode


ROOT = Path(__file__).resolve().parent
BASE = ROOT / "jackin-retained-run-windows"
API_PATH = "repos/jackin-project/jackin/actions/runs"
PAGE_SIZE = 100
# Below the GitHub Actions workflow-run filter ceiling, leaving headroom for
# inclusive timestamp-window boundary duplicates and search-index changes.
WINDOW_LIMIT = 800
RATE_FLOOR = 100


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    tmp.replace(path)


def parse_http(raw: bytes) -> tuple[str, dict[str, str], bytes]:
    marker = b"\r\n\r\n"
    offset = raw.find(marker)
    if offset < 0:
        raise ValueError("HTTP header terminator missing")
    header_text = raw[:offset].decode("latin-1")
    lines = header_text.splitlines()
    status = lines[0] if lines else ""
    headers: dict[str, str] = {}
    for line in lines[1:]:
        if ":" in line:
            key, value = line.split(":", 1)
            headers[key.strip().lower()] = value.strip()
    return status, headers, raw[offset + len(marker) :]


def load_exchange(path: Path) -> tuple[str, dict[str, str], dict[str, Any], bytes]:
    with gzip.open(path, "rb") as stream:
        raw = stream.read()
    status, headers, body = parse_http(raw)
    payload = json.loads(body)
    return status, headers, payload, raw


def save_exchange(path: Path, raw: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with gzip.open(tmp, "wb", compresslevel=6) as stream:
        stream.write(raw)
    tmp.replace(path)


class Collector:
    def __init__(self, owner: str, repo: str) -> None:
        self.endpoint = f"repos/{owner}/{repo}/actions/runs"
        self.requests = 0
        self.lowest_remaining: int | None = None
        self.stopped_for_budget = False
        self.errors: list[dict[str, Any]] = []

    def fetch(self, created: str, page: int, output: Path) -> tuple[dict[str, Any], dict[str, str], bytes]:
        query = urlencode({"per_page": PAGE_SIZE, "created": created, "page": page})
        url = f"{self.endpoint}?{query}"
        proc = subprocess.run(
            ["rtk", "proxy", "gh", "api", "-i", "-H", "Cache-Control: no-cache", url],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=90,
        )
        self.requests += 1
        if proc.stderr:
            output.with_suffix(output.suffix + ".stderr.txt").write_bytes(proc.stderr)
        try:
            status, headers, body = parse_http(proc.stdout)
            payload = json.loads(body)
        except Exception as exc:  # preserve response/error before surfacing it
            if proc.stdout:
                save_exchange(output, proc.stdout)
            error = {
                "created": created,
                "page": page,
                "output": str(output),
                "returncode": proc.returncode,
                "error": str(exc),
            }
            self.errors.append(error)
            raise RuntimeError(f"invalid API response for {url}: {exc}") from exc
        save_exchange(output, proc.stdout)
        remaining = headers.get("x-ratelimit-remaining")
        if remaining is not None:
            current = int(remaining)
            self.lowest_remaining = current if self.lowest_remaining is None else min(self.lowest_remaining, current)
            if current <= RATE_FLOOR:
                self.stopped_for_budget = True
        if not status.startswith("HTTP/2.0 200") and not status.startswith("HTTP/1.1 200"):
            error = {
                "created": created,
                "page": page,
                "status": status,
                "remaining": remaining,
                "request_id": headers.get("x-github-request-id"),
                "output": str(output),
                "returncode": proc.returncode,
            }
            self.errors.append(error)
            raise RuntimeError(f"GitHub API returned {status} for {url}; see {output}")
        return payload, headers, proc.stdout

    @staticmethod
    def parse_created_range(created: str) -> tuple[datetime, datetime] | None:
        if ".." not in created:
            return None
        start_s, end_s = created.split("..", 1)
        fmt = "%Y-%m-%dT%H:%M:%SZ"
        return datetime.strptime(start_s, fmt).replace(tzinfo=timezone.utc), datetime.strptime(end_s, fmt).replace(tzinfo=timezone.utc)

    def collect_window(self, created: str, directory: Path, label: str) -> dict[str, Any]:
        directory.mkdir(parents=True, exist_ok=True)
        meta_path = directory / "window.json"
        if meta_path.exists():
            existing = json.loads(meta_path.read_text())
            if existing.get("created_filter") == created and existing.get("complete"):
                return existing

        first_page = directory / "page-001.http.gz"
        probe = directory / "probe.http.gz"
        existing_page = first_page if first_page.exists() else None
        existing_probe = probe if probe.exists() else None
        if existing_page is not None:
            status, headers, first_payload, raw = load_exchange(existing_page)
        elif existing_probe is not None:
            status, headers, first_payload, raw = load_exchange(existing_probe)
        else:
            target = first_page if self.parse_created_range(created) is None else probe
            first_payload, headers, raw = self.fetch(created, 1, target)
            status, _, _ = parse_http(raw)
            if target == first_page:
                existing_page = first_page
            else:
                existing_probe = probe

        total = int(first_payload.get("total_count", -1))
        runs = first_payload.get("workflow_runs", [])
        observed_at = headers.get("date")
        request_id = headers.get("x-github-request-id")
        remaining = headers.get("x-ratelimit-remaining")
        common: dict[str, Any] = {
            "created_filter": created,
            "label": label,
            "queried_at_http_date": observed_at,
            "request_id_first_page": request_id,
            "remaining_after_first_page": remaining,
            "total_count_reported": total,
            "first_page_rows": len(runs),
            "complete": False,
            "window_limit": WINDOW_LIMIT,
        }

        if total < 0 or len(runs) > PAGE_SIZE:
            write_json(meta_path, {**common, "error": "malformed workflow-runs response"})
            raise RuntimeError(f"unexpected payload for {created}: total={total}, rows={len(runs)}")

        if total > WINDOW_LIMIT:
            # Keep the oversize parent response as a probe only. Recursively
            # split its entire date/time interval; adjacent children overlap
            # at the shared second and are deduplicated during reconciliation.
            if existing_probe is None:
                if existing_page is not None:
                    first_page.rename(probe)
                    existing_probe = probe
                    existing_page = None
            span = self.parse_created_range(created)
            if span is None:
                day = date.fromisoformat(created)
                start = datetime.combine(day, time.min, tzinfo=timezone.utc)
                end = datetime.combine(day + timedelta(days=1), time.min, tzinfo=timezone.utc)
            else:
                start, end = span
            seconds = int((end - start).total_seconds())
            if seconds <= 1:
                write_json(meta_path, {**common, "error": "query still exceeds safe window at one-second span"})
                raise RuntimeError(f"cannot safely split dense timestamp window {created}: {total}")
            midpoint = start + timedelta(seconds=seconds // 2)
            fmt = "%Y-%m-%dT%H:%M:%SZ"
            mid_s = midpoint.strftime(fmt)
            if span is None:
                left_filter = f"{start.strftime(fmt)}..{mid_s}"
                right_filter = f"{mid_s}..{end.strftime(fmt)}"
            else:
                left_filter = f"{start.strftime(fmt)}..{mid_s}"
                right_filter = f"{mid_s}..{end.strftime(fmt)}"
            left_name = hashlib.sha256(left_filter.encode()).hexdigest()[:12]
            right_name = hashlib.sha256(right_filter.encode()).hexdigest()[:12]
            children = [
                self.collect_window(left_filter, directory / f"child-{left_name}", f"{label}/L"),
                self.collect_window(right_filter, directory / f"child-{right_name}", f"{label}/R"),
            ]
            common.update(
                {
                    "complete": all(c.get("complete") for c in children),
                    "split": True,
                    "children": [str((directory / f"child-{left_name}").relative_to(BASE)), str((directory / f"child-{right_name}").relative_to(BASE))],
                    "child_reported_counts": [c.get("total_count_reported") for c in children],
                }
            )
            write_json(meta_path, common)
            return common

        page_total = max(1, math.ceil(total / PAGE_SIZE))
        page_summaries: list[dict[str, Any]] = []
        for page in range(1, page_total + 1):
            if self.stopped_for_budget:
                break
            page_path = directory / f"page-{page:03d}.http.gz"
            if page == 1 and page_path.exists():
                page_status, page_headers, payload, raw_page = load_exchange(page_path)
            elif page == 1:
                # A range probe becomes page one only after the response proves
                # this is a safe leaf. Keep the raw response and headers.
                if existing_probe is not None:
                    raw_page = gzip.open(existing_probe, "rb").read()
                    page_path.parent.mkdir(parents=True, exist_ok=True)
                    save_exchange(page_path, raw_page)
                    page_status, page_headers, payload, raw_page = load_exchange(page_path)
                else:
                    payload, page_headers, raw_page = self.fetch(created, 1, page_path)
                    page_status, _, _ = parse_http(raw_page)
            elif page_path.exists():
                page_status, page_headers, payload, raw_page = load_exchange(page_path)
            else:
                payload, page_headers, raw_page = self.fetch(created, page, page_path)
                page_status, _, _ = parse_http(raw_page)
            page_runs = payload.get("workflow_runs", [])
            page_summaries.append(
                {
                    "page": page,
                    "http_status": page_status,
                    "date": page_headers.get("date"),
                    "request_id": page_headers.get("x-github-request-id"),
                    "remaining": page_headers.get("x-ratelimit-remaining"),
                    "row_count": len(page_runs),
                    "first_created_at": page_runs[0].get("created_at") if page_runs else None,
                    "last_created_at": page_runs[-1].get("created_at") if page_runs else None,
                    "sha256_raw_http": hashlib.sha256(raw_page).hexdigest(),
                }
            )
            common["page_summaries"] = page_summaries
            common["pages_collected"] = len(page_summaries)
            common["complete"] = len(page_summaries) == page_total
            write_json(meta_path, common)
        return common

    def collect_dates(self, start_day: date, end_day: date) -> None:
        day = start_day
        while day <= end_day:
            if self.stopped_for_budget:
                break
            day_dir = BASE / "days" / day.isoformat()
            result = self.collect_window(day.isoformat(), day_dir, day.isoformat())
            print(
                f"{day.isoformat()} count={result.get('total_count_reported')} "
                f"complete={result.get('complete')} split={result.get('split', False)} "
                f"requests={self.requests} remaining={self.lowest_remaining}",
                flush=True,
            )
            day += timedelta(days=1)
            self.write_checkpoint(start_day, end_day, day)

    def write_checkpoint(self, start_day: date, end_day: date, next_day: date) -> None:
        write_json(
            BASE / "collection-checkpoint.json",
            {
                "repository": "jackin-project/jackin",
                "start_day": start_day.isoformat(),
                "end_day": end_day.isoformat(),
                "next_day_to_check": next_day.isoformat(),
                "requests_this_process": self.requests,
                "lowest_rate_remaining_seen": self.lowest_remaining,
                "stopped_at_rate_floor": self.stopped_for_budget,
                "errors": self.errors,
                "updated_at_local": datetime.now().astimezone().isoformat(),
            },
        )


def leaf_directories(path: Path) -> list[Path]:
    leaves: list[Path] = []
    for meta_path in path.rglob("window.json"):
        meta = json.loads(meta_path.read_text())
        if meta.get("split"):
            continue
        if meta.get("complete"):
            leaves.append(meta_path.parent)
    return leaves


def reconcile() -> dict[str, Any]:
    seen: dict[int, dict[str, Any]] = {}
    occurrence_count: dict[int, int] = {}
    all_rows = 0
    pages = 0
    complete_days: dict[str, dict[str, Any]] = {}
    for leaf in leaf_directories(BASE / "days"):
        meta = json.loads((leaf / "window.json").read_text())
        pages += int(meta.get("pages_collected", 0))
        expected_pages = max(1, math.ceil(int(meta.get("total_count_reported", 0)) / PAGE_SIZE))
        if meta.get("pages_collected") != expected_pages:
            continue
        for page_no in range(1, expected_pages + 1):
            page_path = leaf / f"page-{page_no:03d}.http.gz"
            if not page_path.exists():
                continue
            status, headers, payload, raw = load_exchange(page_path)
            if not status.startswith("HTTP/2.0 200"):
                continue
            for run in payload.get("workflow_runs", []):
                all_rows += 1
                run_id = int(run["id"])
                occurrence_count[run_id] = occurrence_count.get(run_id, 0) + 1
                current = seen.get(run_id)
                if current is None or str(run.get("updated_at", "")) > str(current.get("updated_at", "")):
                    seen[run_id] = run
        day_key = meta.get("label", "")[:10]
        if len(day_key) == 10 and day_key[4] == "-":
            complete_days.setdefault(day_key, {"leaf_windows": 0, "reported_counts": []})
            complete_days[day_key]["leaf_windows"] += 1
            complete_days[day_key]["reported_counts"].append(meta.get("total_count_reported"))

    dates_observed: dict[str, int] = {}
    for run in seen.values():
        d = run.get("created_at", "")[:10]
        dates_observed[d] = dates_observed.get(d, 0) + 1

    output_json = BASE / "jackin-retained-runs.json.gz"
    output_csv = BASE / "jackin-retained-runs.csv"
    records = sorted(seen.values(), key=lambda x: (x.get("created_at", ""), int(x["id"])))
    with gzip.open(output_json, "wt", encoding="utf-8") as stream:
        json.dump(records, stream, separators=(",", ":"), ensure_ascii=False)
    fields = [
        "id", "run_number", "name", "workflow_id", "path", "event", "head_branch",
        "head_sha", "head_repository", "status", "conclusion", "run_attempt", "created_at",
        "run_started_at", "updated_at", "html_url", "pull_requests",
    ]
    import csv

    with output_csv.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for run in records:
            row = {field: run.get(field) for field in fields}
            row["pull_requests"] = ";".join(str(pr.get("number")) for pr in run.get("pull_requests", []))
            writer.writerow(row)

    expected_mismatches = []
    for day, entry in sorted(complete_days.items()):
        expected = None
        parent_path = BASE / "days" / day / "window.json"
        if parent_path.exists():
            parent = json.loads(parent_path.read_text())
            expected = parent.get("total_count_reported")
        observed = dates_observed.get(day, 0)
        if expected is not None and expected != observed:
            expected_mismatches.append({"date": day, "reported": expected, "observed_unique": observed})

    summary = {
        "repository": "jackin-project/jackin",
        "status": "complete" if complete_days else "incomplete",
        "requested_start_day": min(complete_days) if complete_days else None,
        "requested_end_day": max(complete_days) if complete_days else None,
        "date_windows_with_complete_leafs": len(complete_days),
        "leaf_windows": sum(v["leaf_windows"] for v in complete_days.values()),
        "api_rows_from_complete_leaf_pages": all_rows,
        "distinct_run_ids": len(seen),
        "duplicate_run_id_occurrences_across_overlapping_windows": sum(v - 1 for v in occurrence_count.values()),
        "pages_from_complete_leaf_windows": pages,
        "oldest_created_at": min((r.get("created_at", "") for r in records), default=None),
        "newest_created_at": max((r.get("created_at", "") for r in records), default=None),
        "days_with_expected_count_mismatch": expected_mismatches,
        "workflow_run_response_cap_avoided_by_split_threshold": WINDOW_LIMIT,
        "records_json_gzip": str(output_json),
        "records_csv": str(output_csv),
        "generated_at_local": datetime.now().astimezone().isoformat(),
    }
    write_json(BASE / "reconciliation-summary.json", summary)
    return summary


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--from-date", default="2026-03-31")
    parser.add_argument("--through-date", default="2026-09-20")
    parser.add_argument("--reconcile-only", action="store_true")
    args = parser.parse_args()
    if args.reconcile_only:
        print(json.dumps(reconcile(), indent=2))
        return 0
    start_day = date.fromisoformat(args.from_date)
    end_day = date.fromisoformat(args.through_date)
    if end_day < start_day:
        raise SystemExit("through date precedes start date")
    BASE.mkdir(parents=True, exist_ok=True)
    collector = Collector("jackin-project", "jackin")
    collector.collect_dates(start_day, end_day)
    collector.write_checkpoint(start_day, end_day, end_day + timedelta(days=1))
    summary = reconcile()
    summary["requests_this_process"] = collector.requests
    summary["lowest_rate_remaining_seen"] = collector.lowest_remaining
    summary["stopped_for_rate_floor"] = collector.stopped_for_budget
    summary["collection_errors"] = collector.errors
    write_json(BASE / "reconciliation-summary.json", summary)
    print(json.dumps(summary, indent=2))
    return 75 if collector.stopped_for_budget else 0


if __name__ == "__main__":
    sys.exit(main())
