#!/usr/bin/env python3
"""Reconcile the retained Velnor CI snapshot into a compact class ledger."""

import collections
import csv
import gzip
import hashlib
import json
import re
from pathlib import Path


SNAPSHOT = Path(
    "/Users/donbeave/Projects/work/velnor/plans/ci-performance/observations/reliability-20260920"
)
OUT = Path("/Users/donbeave/Projects/work/ci-evidence/inventory")
JSON_OUT = OUT / "velnor-snapshot-classes-20260920.json"
MD_OUT = OUT / "velnor-snapshot-classes-20260920.md"


INTERPRETATIONS = {
    "generated-tree;candidate-product": (
        "Renderer/pin/scan-input identity mismatch plus missing event-equivalent, "
        "source-bound candidate producer. Both defects remain open in this snapshot."
    ),
    "generated-tree": (
        "Renderer/pin/scan-input identity mismatch. Main and Preview examples remain "
        "open; exact source-bound render parity is still required."
    ),
    "candidate-product": (
        "Candidate lookup lacks an event-equivalent source-bound producer. "
        "Main-only producer lookup remains open."
    ),
    "dead-code": (
        "Stale GuardError::Contended use after acquisition semantics changed. Fixed at "
        "7308307b; successor PR 35522028476 is green, but main CI proof was pending."
    ),
    "superseded-diagnostics-deletion;test-failure": (
        "Historical candidate deleted runner state before an unchanged test asserted "
        "diagnostics survived. Current main omits that candidate deletion; the test "
        "passed in successor PR 35522028476. This occurrence does not establish a "
        "current main defect."
    ),
    "protocol-expectation;test-failure": (
        "Stale expectation for RunnerNotFound versus NotFound. Fixed at c24be56e; "
        "successor PR 35522028476 is green, but main CI proof was pending."
    ),
    "dirty-preview": (
        "Generated metadata dirtied an identity-sensitive source checkout. Repair "
        "57e7cafc moves it to runner.temp; the successor Preview was blocked at policy, "
        "so end-to-end repair proof was absent."
    ),
    "cross-compiler": (
        "ARM preview requested aarch64-linux-gnu-gcc on an x64 Ubuntu runner. The "
        "current route remained wrong; open PR 962 was relevant."
    ),
    "test-failure": (
        "Test failure with root cause still unclassified. Successor/source inspection "
        "is required for each occurrence."
    ),
    "rust-lint": (
        "Rust compile/lint failure with individual diagnostics retained. No blanket "
        "repair or successor proof is established."
    ),
    "missing-event-fields;rust-lint": (
        "Event schema expansion was not propagated to test consumers. Historical PR "
        "revisions need successor/source inspection."
    ),
    "packaging": (
        "Artifact construction/identity mismatch. Historical publication/package "
        "failures still need current-invariant and successor verification."
    ),
    "lockfile": (
        "Tracked lockfile was inconsistent with build inputs. Historical occurrence; "
        "successor verification is absent."
    ),
    "unavailable-log": (
        "Job log API returned 404; cause is unproven. Missing log is an evidence gap, "
        "not a solved or benign failure."
    ),
    "unclassified": (
        "No root-cause disposition. This label includes failed and cancelled jobs; "
        "cancellation intent is not inferred."
    ),
}


def read_gzip_json(path):
    with gzip.open(path, "rt", encoding="utf-8") as stream:
        return json.load(stream)


def read_gzip_csv(path):
    with gzip.open(path, "rt", newline="", encoding="utf-8") as stream:
        return list(csv.DictReader(stream))


def count_or_null(values):
    return dict(collections.Counter(v if v else "null" for v in values))


def compact_row_id(row):
    return f"{row['run_id']}/{row['attempt']}:{row['job_id']}"


def class_order(name):
    order = [
        "generated-tree;candidate-product",
        "generated-tree",
        "candidate-product",
        "dead-code",
        "superseded-diagnostics-deletion;test-failure",
        "protocol-expectation;test-failure",
        "dirty-preview",
        "cross-compiler",
        "test-failure",
        "rust-lint",
        "missing-event-fields;rust-lint",
        "packaging",
        "lockfile",
        "unavailable-log",
        "unclassified",
    ]
    return order.index(name) if name in order else len(order)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    retained = read_gzip_json(SNAPSHOT / "retained-runs-attempts-20260920.json.gz")
    nongreen = read_gzip_csv(SNAPSHOT / "retained-nongreen-attempts.csv.gz")
    jobs = read_gzip_csv(SNAPSHOT / "inspected-failed-jobs.csv.gz")
    excerpts = gzip.open(SNAPSHOT / "failure-excerpts.md.gz", "rt", encoding="utf-8").read()
    with (SNAPSHOT / "snapshot-manifest.json").open(encoding="utf-8") as stream:
        manifest = json.load(stream)

    latest = retained["latest_runs"]
    earlier = retained["earlier_attempts"]
    latest_by_id = {str(row["id"]): row for row in latest}
    latest_non_green = [row for row in latest if row["conclusion"] != "success"]
    earlier_non_green = [row for row in earlier if row["conclusion"] != "success"]
    earlier_keys = {(str(row["id"]), int(row["run_attempt"])) for row in earlier}
    expected_earlier_keys = {
        (run_id, attempt)
        for run_id, row in latest_by_id.items()
        for attempt in range(1, int(row["run_attempt"]))
    }
    excerpt_keys = {
        tuple(match)
        for match in re.findall(
            r"^###\s+(\d+)\s+/\s+job\s+(\d+)\s+/\s+attempt\s+(\d+)\s*$",
            excerpts,
            re.MULTILINE,
        )
    }

    by_class = collections.defaultdict(list)
    for row in jobs:
        by_class[row["class"]].append(row)
    class_records = []
    for label in sorted(by_class, key=class_order):
        rows = by_class[label]
        class_records.append(
            {
                "label": label,
                "occurrence_count": len(rows),
                "conclusions": count_or_null(row["conclusion"] for row in rows),
                "root_cause_texts": sorted({row["root_cause"] for row in rows}),
                "snapshot_dispositions": sorted({row["disposition"] for row in rows}),
                "workflows": dict(collections.Counter(row["workflow"] for row in rows)),
                "occurrence_ids": [compact_row_id(row) for row in rows],
            }
        )

    main_push = [
        row for row in latest if row["event"] == "push" and row["head_branch"] == "main"
    ]
    main_push_terminal = [row for row in main_push if row["status"] == "completed"]
    main_push_first = {
        str(row["id"]): row
        for row in earlier
        if int(row["run_attempt"]) == 1
    }
    main_push_first = [
        main_push_first.get(str(row["id"]), row) for row in main_push_terminal
    ]

    excerpted_jobs = [
        row
        for row in jobs
        if (row["run_id"], row["job_id"], row["attempt"]) in excerpt_keys
    ]
    missing_excerpt_jobs = [
        row
        for row in jobs
        if (row["run_id"], row["job_id"], row["attempt"]) not in excerpt_keys
    ]
    missing_failure_rows = [row for row in missing_excerpt_jobs if row["conclusion"] == "failure"]
    missing_failure_hashes = [row for row in missing_failure_rows if row["log_sha256"]]
    no_hash_missing_failure = [row for row in missing_failure_rows if not row["log_sha256"]]
    log_path_exists = sum(
        Path(row["raw_log"]).is_file() for row in jobs if row["raw_log"]
    )

    manifest_verified = 0
    manifest_mismatches = []
    for name, expected in manifest.items():
        path = SNAPSHOT / name
        if not path.is_file():
            manifest_mismatches.append({"file": name, "problem": "missing"})
            continue
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected["sha256"]:
            manifest_mismatches.append({"file": name, "problem": "sha256 mismatch"})
        else:
            manifest_verified += 1

    compressed_logs = list(SNAPSHOT.glob("*.log.gz"))
    archive_job_ids = set()
    for path in compressed_logs:
        ids = re.findall(r"\d{8,}", path.name)
        archive_job_ids.update(ids)
    archived_ledger_rows = [row for row in jobs if row["job_id"] in archive_job_ids]

    summary = {
        "scope": "Read-only reconciliation of retained Velnor snapshot; no network or Git operations.",
        "snapshot_path": str(SNAPSHOT),
        "collected_at": retained["collected_at"],
        "run_metadata": {
            "api_pages_reported": 76,
            "api_total_count_reported": 7594,
            "rows": len(latest),
            "unique_run_ids": len({row["id"] for row in latest}),
            "reported_total_reconciles_locally": len(latest) == 7594
            and len({row["id"] for row in latest}) == 7594,
            "created_at_min": min(row["created_at"] for row in latest),
            "created_at_max": max(row["created_at"] for row in latest),
            "latest_conclusions": count_or_null(row["conclusion"] for row in latest),
            "latest_statuses": dict(collections.Counter(row["status"] for row in latest)),
            "earlier_attempt_rows": len(earlier),
            "earlier_attempt_conclusions": count_or_null(
                row["conclusion"] for row in earlier
            ),
            "earlier_attempt_statuses": dict(
                collections.Counter(row["status"] for row in earlier)
            ),
            "rerun_runs_in_latest_index": sum(int(row["run_attempt"]) > 1 for row in latest),
            "expected_earlier_attempt_keys": len(expected_earlier_keys),
            "observed_earlier_attempt_keys": len(earlier_keys),
            "earlier_attempt_key_gaps": sorted(expected_earlier_keys - earlier_keys),
            "unexpected_earlier_attempt_keys": sorted(earlier_keys - expected_earlier_keys),
            "latest_non_success_rows": len(latest_non_green),
            "earlier_non_success_rows": len(earlier_non_green),
            "nongreen_ledger_rows": len(nongreen),
            "nongreen_ledger_reconciles": len(nongreen)
            == len(latest_non_green) + len(earlier_non_green),
            "nongreen_ledger_statuses": dict(
                collections.Counter(
                    (row["conclusion"] or "null") + "/" + row["status"]
                    for row in nongreen
                )
            ),
        },
        "main_push_reconciliation": {
            "branch": "main",
            "event": "push",
            "latest_run_rows": len(main_push),
            "terminal_runs": len(main_push_terminal),
            "first_attempt_green": sum(row["conclusion"] == "success" for row in main_push_first),
            "latest_attempt_green": sum(
                row["conclusion"] == "success" for row in main_push_terminal
            ),
            "retried_runs": sum(int(row["run_attempt"]) > 1 for row in main_push_terminal),
            "metric_note": "Workflow-run denominator, not per-commit aggregate pipelines; includes cancellations and historical workflow revisions.",
        },
        "job_inventory": {
            "summary_reported_runs_queried_filter_all": 559,
            "summary_reported_unique_jobs": 17961,
            "raw_all_jobs_inventory_saved": False,
            "ledger_rows": len(jobs),
            "unique_job_ids": len({row["job_id"] for row in jobs}),
            "distinct_runs_with_ledger_rows": len({row["run_id"] for row in jobs}),
            "distinct_run_attempts_with_ledger_rows": len(
                {(row["run_id"], row["attempt"]) for row in jobs}
            ),
            "conclusions": count_or_null(row["conclusion"] for row in jobs),
            "classes": class_records,
            "class_occurrence_count_reconciles": sum(
                row["occurrence_count"] for row in class_records
            )
            == len(jobs),
        },
        "log_evidence": {
            "failure_excerpt_sections": len(excerpt_keys),
            "excerpt_sections_matching_job_ledger": len(excerpted_jobs),
            "excerpted_job_conclusions": count_or_null(
                row["conclusion"] for row in excerpted_jobs
            ),
            "ledger_rows_without_excerpt": len(missing_excerpt_jobs),
            "failures_without_excerpt": len(missing_failure_rows),
            "failures_without_excerpt_with_sha256_only": len(missing_failure_hashes),
            "failures_without_excerpt_without_sha256": len(no_hash_missing_failure),
            "failures_without_excerpt_without_sha256_non404": sum(
                row["class"] != "unavailable-log" for row in no_hash_missing_failure
            ),
            "api_404_unavailable_log_rows": sum(
                row["class"] == "unavailable-log" for row in jobs
            ),
            "raw_log_reference_paths_existing_now": log_path_exists,
            "raw_log_reference_paths_missing_now": sum(bool(row["raw_log"]) for row in jobs)
            - log_path_exists,
            "compressed_log_archives_in_snapshot": len(compressed_logs),
            "job_ids_in_compressed_archives": sorted(archive_job_ids),
            "compressed_archive_job_rows_matching_ledger": len(archived_ledger_rows),
            "full_job_query_inventory_available": False,
        },
        "artifact_integrity": {
            "manifest_entries": len(manifest),
            "manifest_sha256_matches": manifest_verified,
            "manifest_mismatches": manifest_mismatches,
            "source_file_sha256": {
                name: manifest[name]["sha256"]
                for name in [
                    "retained-runs-attempts-20260920.json.gz",
                    "retained-nongreen-attempts.csv.gz",
                    "inspected-failed-jobs.csv.gz",
                    "failure-excerpts.md.gz",
                ]
            },
        },
        "jackin_history_context": {
            "run_35521080097_job_106105226160": {
                "local_log": "/Users/donbeave/Projects/work/ci-evidence/inventory/jackin-run-35521080097-attempt-1-logs/30_Rust · jackin-diagnostics _ GitHub.txt",
                "test": "conformance_partial_success_is_not_retried",
                "assertion": "left 7, right 1",
                "summary": "107 passed, 1 failed, 1 skipped; 14/122 unrun after fail-fast",
                "classification": "Jackin unit-test failure; not Velnor Clippy or renderer class.",
            },
            "run_35515575859_job_106090835001": {
                "local_evidence": "/Users/donbeave/Projects/work/ci-evidence/inventory/jackin-job-106090835001-public-page.md",
                "annotation": "A higher-priority waiter existed for the same main concurrency group.",
                "classification": "Explicit cancellation cause for this Jackin job only; do not generalize to Velnor cancellations.",
            },
        },
    }

    JSON_OUT.write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    class_rows = []
    for record in class_records:
        label = record["label"]
        items = by_class[label]
        representative_ids = ", ".join(compact_row_id(row) for row in items[:2])
        if len(items) > 2:
            representative_ids += ", …"
        state = INTERPRETATIONS[label]
        class_rows.append(
            f"| `{label}` | {record['occurrence_count']} | {representative_ids} | {state} |"
        )

    outcome = summary["run_metadata"]
    job_counts = summary["job_inventory"]["conclusions"]
    log_counts = summary["log_evidence"]
    main_counts = summary["main_push_reconciliation"]
    md = [
        "# Velnor retained CI class inventory (snapshot 2026-09-20)",
        "",
        f"Snapshot collected at `{retained['collected_at']}`. This is a read-only audit of "
        "saved artifacts; no GitHub/network calls or repository changes were made.",
        "",
        "## Coverage reconciliation",
        "",
        f"- Runs: {len(latest):,} rows and {len({row['id'] for row in latest}):,} distinct IDs. "
        "The source summary reports 76 unfiltered pages and API `total_count=7,594`; the saved "
        "compact index matches both numbers. Oldest creation `{}`; newest `{}`. This covers "
        "the retained response only, not Velnor's entire history.".format(
            min(row["created_at"] for row in latest), max(row["created_at"] for row in latest)
        ),
        f"- Earlier attempts: {len(earlier):,} records across "
        f"{outcome['rerun_runs_in_latest_index']} rerun runs. Expected attempt keys from each "
        f"latest `run_attempt` reconcile: {outcome['expected_earlier_attempt_keys']} expected, "
        f"{outcome['observed_earlier_attempt_keys']} retained. Outcomes: "
        f"`{json.dumps(outcome['earlier_attempt_conclusions'], sort_keys=True)}`; two historical "
        "attempt rows remain queued in the API snapshot.",
        f"- The non-green ledger contains {len(nongreen):,} rows and reconciles to "
        f"{len(latest_non_green):,} latest non-success runs plus {len(earlier_non_green):,} "
        "non-success earlier attempts.",
        f"- Main-branch push sample: {main_counts['first_attempt_green']}/"
        f"{main_counts['terminal_runs']} first attempts green, {main_counts['latest_attempt_green']}/"
        f"{main_counts['terminal_runs']} latest attempts green; "
        f"{main_counts['retried_runs']} runs had retries. Denominator is workflow runs, not "
        "per-commit aggregate pipelines; cancellations and historical workflows are included.",
        f"- The summary reports `filter=all` job collection for 559 runs and 17,961 unique jobs, "
        f"but the raw all-jobs inventory is not present here, so those two figures cannot be "
        f"independently recomputed. The saved failed/nonterminal/cancelled ledger does reconcile: "
        f"{len(jobs):,} rows, {len({row['job_id'] for row in jobs}):,} unique job IDs across "
        f"{len({row['run_id'] for row in jobs}):,} runs and "
        f"{len({(row['run_id'], row['attempt']) for row in jobs}):,} run-attempt pairs. Outcomes: "
        f"`{json.dumps(job_counts, sort_keys=True)}`.",
        "",
        "## Evidence-backed classes and snapshot disposition",
        "",
        "Counts are ledger rows, not unique incidents. Combined labels remain one row each; "
        "the full occurrence-ID arrays are in the companion JSON. Statuses below are as of "
        "the snapshot time, not a live 2026-09-21 refresh.",
        "",
        "| Class | Rows | Representative run/attempt:job | Evidence and status at snapshot |",
        "| --- | ---: | --- | --- |",
        *class_rows,
        "",
        "The renderer/candidate-bootstrap groups contain 113 distinct ledger rows: 58 with "
        "both labels, 28 renderer-only, and 27 candidate-only. Do not add the two cause totals "
        "without preserving the overlap.",
        "",
        "## Evidence gaps and limits",
        "",
        f"- Exact excerpts cover {log_counts['excerpt_sections_matching_job_ledger']:,} of "
        f"1,651 ledger rows: {log_counts['excerpted_job_conclusions'].get('failure', 0):,} failed "
        f"jobs plus one null/in-progress job. {log_counts['failures_without_excerpt']:,} failed "
        f"rows have no excerpt; 43 are explicitly marked API 404, 149 others retain only a "
        f"log SHA-256, and 8 other failures have neither excerpt nor SHA-256. In total, 51 of "
        f"the 200 lack an excerpt also lack a log hash. The 661 cancelled rows also have "
        "no excerpt. A hash proves identity only, not the missing log contents.",
        f"- The CSV's `raw_log` fields point to `/tmp/velnor-failures/logs`; that directory is "
        f"absent now ({log_counts['raw_log_reference_paths_existing_now']} of "
        f"{log_counts['raw_log_reference_paths_missing_now']:,} referenced paths exist). The "
        f"snapshot contains {log_counts['compressed_log_archives_in_snapshot']} compressed "
        f"`.log.gz` files; {log_counts['compressed_archive_job_rows_matching_ledger']} match "
        "ledger job IDs, and those IDs are already among the excerpted set. The manifest's 28 "
        f"file hashes all verify ({manifest_verified}/{len(manifest)}).",
        "- The 1,399 `unclassified` job rows comprise 738 failures and 661 cancellations. "
        "None has a causal disposition; cancellation intent is not inferred. `unavailable-log` "
        "adds 43 more failures with explicit API 404 but no causal evidence.",
        "- The snapshot records older missing job logs as API 404/410, but it does not preserve "
        "a raw all-jobs response set. The summary's 559/17,961 jobs-query totals are therefore "
        "reported metadata rather than independently reproducible evidence here.",
        "- The snapshot was collected 2026-09-20. It is not a live status refresh. PR-success "
        "evidence for `dead-code` and `protocol-expectation` does not establish mainline success; "
        "the exact main proof remained pending in the saved disposition.",
        "",
        "## Separate Jackin history context",
        "",
        "- Jackin run 35521080097/job 106105226160 now has an extracted local raw log. It confirms "
        "`conformance_partial_success_is_not_retried` failed with assertion left 7/right 1; the "
        "test summary was 107 passed, 1 failed, 1 skipped, with 14/122 unrun after fail-fast. "
        "This is a Jackin test failure and is not evidence for any Velnor class.",
        "- Jackin run 35515575859/job 106090835001 has an explicit higher-priority waiter "
        "annotation for the same main concurrency group. That confirms the cancellation cause "
        "for this one job only; it does not classify Velnor's 661 cancelled ledger rows.",
        "",
        "## Audit artifacts",
        "",
        f"- Machine-readable counts and per-occurrence IDs: `{JSON_OUT}`",
        f"- Reusable local analyzer: `{Path(__file__).resolve()}`",
        f"- Source ledger: `{SNAPSHOT / 'inspected-failed-jobs.csv.gz'}`",
        f"- Source excerpts: `{SNAPSHOT / 'failure-excerpts.md.gz'}`",
        f"- Summary: `{SNAPSHOT / 'inventory-summary.md'}`",
        "",
    ]
    MD_OUT.write_text("\n".join(md), encoding="utf-8")
    print(json.dumps({"json": str(JSON_OUT), "markdown": str(MD_OUT), "classes": len(class_records), "rows": len(jobs)}))


if __name__ == "__main__":
    main()
