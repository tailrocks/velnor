#!/usr/bin/env python3
"""Summarize a complete gh api --paginate --slurp run-list capture."""

from __future__ import annotations

import argparse
import csv
import json
from collections import Counter
from pathlib import Path
from typing import Any


PR_EVENTS = {"pull_request", "pull_request_target", "merge_group"}


def iter_top_level_pages(path: Path):
    """Decode one page object at a time from gh's outer JSON array."""
    decoder = json.JSONDecoder()
    chunk_size = 4 * 1024 * 1024
    with path.open("r", encoding="utf-8") as stream:
        buffer = stream.read(chunk_size)
        position = 0
        eof = False

        def refill() -> None:
            nonlocal buffer, position, eof
            if position:
                buffer = buffer[position:]
                position = 0
            next_chunk = stream.read(chunk_size)
            if next_chunk:
                buffer += next_chunk
            else:
                eof = True

        while not buffer and not eof:
            refill()
        while position < len(buffer) and buffer[position].isspace():
            position += 1
        if position >= len(buffer) or buffer[position] != "[":
            raise SystemExit("capture is not a gh --slurp JSON array")
        position += 1

        while True:
            while True:
                while position < len(buffer) and (
                    buffer[position].isspace() or buffer[position] == ","
                ):
                    position += 1
                if position < len(buffer) or eof:
                    break
                refill()
            if position >= len(buffer):
                raise SystemExit("capture ended before the closing array bracket")
            if buffer[position] == "]":
                return
            try:
                page, end = decoder.raw_decode(buffer, position)
            except json.JSONDecodeError:
                if eof:
                    raise
                refill()
                continue
            position = end
            yield page


def scope_for(run: dict[str, Any]) -> str | None:
    branch = run.get("head_branch") or ""
    event = run.get("event") or ""
    if (
        event in PR_EVENTS
        or bool(run.get("pull_requests"))
        or branch.startswith("refs/pull/")
    ):
        return "pr_or_integration_candidate"
    if branch == "main" or branch == "refs/heads/main":
        return "main_ref"
    return None


def pull_request_numbers(run: dict[str, Any]) -> str:
    numbers = []
    for pr in run.get("pull_requests") or []:
        number = pr.get("number")
        if number is not None:
            numbers.append(str(number))
    return ";".join(numbers)


def row_for(run: dict[str, Any], scope: str | None) -> dict[str, Any]:
    head_repository = run.get("head_repository") or {}
    conclusion = run.get("conclusion")
    status = run.get("status")
    attempts = run.get("run_attempt") or 1
    return {
        "scope": scope or "other",
        "run_id": run.get("id"),
        "run_number": run.get("run_number"),
        "workflow_id": run.get("workflow_id"),
        "workflow_name": run.get("name"),
        "workflow_path": run.get("path"),
        "event": run.get("event"),
        "head_branch": run.get("head_branch"),
        "head_sha": run.get("head_sha"),
        "head_repository": head_repository.get("full_name"),
        "status": status,
        "latest_conclusion": conclusion,
        "latest_attempt": attempts,
        "created_at": run.get("created_at"),
        "run_started_at": run.get("run_started_at"),
        "updated_at": run.get("updated_at"),
        "html_url": run.get("html_url"),
        "pr_numbers": pull_request_numbers(run),
        "latest_attempt_non_green": bool(
            status != "completed" or conclusion != "success"
        ),
    }


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    if path.name.endswith("duplicate-ids.csv"):
        fields = [
            "run_id",
            "first_page",
            "duplicate_page",
            "first_updated_at",
            "duplicate_updated_at",
        ]
    elif rows:
        fields = list(dict.fromkeys(key for row in rows for key in row))
    else:
        fields = [
            "scope",
            "run_id",
            "run_number",
            "workflow_id",
            "workflow_name",
            "workflow_path",
            "event",
            "head_branch",
            "head_sha",
            "head_repository",
            "status",
            "latest_conclusion",
            "latest_attempt",
            "created_at",
            "run_started_at",
            "updated_at",
            "html_url",
            "pr_numbers",
            "latest_attempt_non_green",
        ]
    with path.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("capture", type=Path)
    parser.add_argument("output_prefix", type=Path)
    args = parser.parse_args()

    total_counts: set[int] = set()
    page_lengths: list[int] = []
    observed_runs = 0
    by_id: dict[int, tuple[int, dict[str, Any]]] = {}
    duplicate_rows: list[dict[str, Any]] = []
    page_count = 0
    for page_count, page in enumerate(iter_top_level_pages(args.capture), start=1):
        if not isinstance(page, dict):
            raise SystemExit(f"page {page_count} is not an API object")
        page_total = page.get("total_count")
        if isinstance(page_total, int):
            total_counts.add(page_total)
        runs = page.get("workflow_runs")
        if not isinstance(runs, list):
            raise SystemExit(f"page {page_count} has no workflow_runs array")
        page_lengths.append(len(runs))
        observed_runs += len(runs)
        for run in runs:
            run_id = run.get("id")
            if run_id is None:
                continue
            row = row_for(run, scope_for(run))
            if run_id in by_id:
                prior_page, prior = by_id[run_id]
                duplicate_rows.append(
                    {
                        "run_id": run_id,
                        "first_page": prior_page,
                        "duplicate_page": page_count,
                        "first_updated_at": prior.get("updated_at"),
                        "duplicate_updated_at": row.get("updated_at"),
                    }
                )
                if (row.get("updated_at") or "") > (prior.get("updated_at") or ""):
                    by_id[run_id] = (page_count, row)
            else:
                by_id[run_id] = (page_count, row)

    distinct_rows = [item[1] for item in by_id.values()]
    distinct_rows.sort(key=lambda row: (row.get("created_at") or "", row.get("run_id") or 0))
    scoped_rows = [row for row in distinct_rows if row["scope"] != "other"]
    non_green_rows = [row for row in scoped_rows if row["latest_attempt_non_green"]]
    rerun_rows = [row for row in scoped_rows if int(row["latest_attempt"] or 1) > 1]

    conclusions = Counter(
        "<null>" if row.get("latest_conclusion") is None else row.get("latest_conclusion")
        for row in distinct_rows
    )
    events = Counter(row.get("event") or "<null>" for row in distinct_rows)
    statuses = Counter(row.get("status") or "<null>" for row in distinct_rows)
    scopes = Counter(row.get("scope") or "other" for row in distinct_rows)
    attempt_numbers = Counter(int(row.get("latest_attempt") or 1) for row in distinct_rows)
    main_push = [
        row
        for row in distinct_rows
        if row.get("event") == "push" and row.get("head_branch") == "main"
    ]
    terminal_main_push = [row for row in main_push if row.get("status") == "completed"]
    unchanged_attempt_main_push = [
        row for row in terminal_main_push if int(row.get("latest_attempt") or 1) == 1
    ]

    timestamps = [row.get("created_at") for row in distinct_rows if row.get("created_at")]
    api_total_unique = sorted(total_counts)
    page_count_reconciles = (
        observed_runs == len(distinct_rows)
        and len(duplicate_rows) == 0
        and len(api_total_unique) == 1
        and len(distinct_rows) == api_total_unique[0]
    )

    summary = {
        "repository": "jackin-project/jackin",
        "capture_file": str(args.capture),
        "pages_received": page_count,
        "rows_received": observed_runs,
        "distinct_run_ids": len(distinct_rows),
        "duplicate_run_ids": len(duplicate_rows),
        "page_lengths": page_lengths,
        "api_total_count_values": api_total_unique,
        "api_total_count_stable": len(api_total_unique) == 1,
        "count_reconciles_without_pagination_duplicates": page_count_reconciles,
        "oldest_created_at": min(timestamps) if timestamps else None,
        "newest_created_at": max(timestamps) if timestamps else None,
        "latest_conclusions": dict(sorted(conclusions.items())),
        "latest_statuses": dict(sorted(statuses.items())),
        "events": dict(sorted(events.items())),
        "scopes": dict(sorted(scopes.items())),
        "latest_run_attempt_numbers": dict(sorted(attempt_numbers.items())),
        "runs_with_reruns": sum(
            1 for row in distinct_rows if int(row.get("latest_attempt") or 1) > 1
        ),
        "earlier_attempts_indicated_by_latest_run_records": sum(
            max(int(row.get("latest_attempt") or 1) - 1, 0) for row in distinct_rows
        ),
        "main_push_runs": len(main_push),
        "terminal_main_push_latest_attempts": len(terminal_main_push),
        "terminal_main_push_latest_green": sum(
            1 for run in terminal_main_push if run.get("latest_conclusion") == "success"
        ),
        "terminal_main_push_without_reruns": len(unchanged_attempt_main_push),
        "terminal_main_push_without_reruns_green": sum(
            1
            for run in unchanged_attempt_main_push
            if run.get("latest_conclusion") == "success"
        ),
        "scoped_run_rows": len(scoped_rows),
        "all_latest_non_green_rows": sum(
            1 for row in distinct_rows if row["latest_attempt_non_green"]
        ),
        "scoped_latest_non_green_rows": len(non_green_rows),
        "scoped_latest_non_green_by_scope": dict(
            sorted(Counter(row["scope"] for row in non_green_rows).items())
        ),
        "scoped_runs_with_reruns": len(rerun_rows),
        "limitations": [
            "Run-list records expose only latest attempt conclusion; earlier attempt outcomes require the jobs API with filter=all.",
            "A current run-list capture is not a transactional snapshot; compare page total_count values, duplicate IDs, and a fresh frontier before claiming exact completeness.",
            "The no-rerun main-push percentage is not an all-run first-attempt reliability estimate; it excludes rerun outcomes.",
            "Run metadata does not classify job root causes or substitute for retained job logs.",
        ],
    }

    prefix = args.output_prefix
    prefix.parent.mkdir(parents=True, exist_ok=True)
    prefix.with_suffix(".summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    write_csv(prefix.with_suffix(".all-runs.csv"), distinct_rows)
    write_csv(prefix.with_suffix(".scoped-runs.csv"), scoped_rows)
    write_csv(prefix.with_suffix(".non-green-latest.csv"), non_green_rows)
    write_csv(prefix.with_suffix(".rerun-runs.csv"), rerun_rows)
    write_csv(prefix.with_suffix(".duplicate-ids.csv"), duplicate_rows)
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
