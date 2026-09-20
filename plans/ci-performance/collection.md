# Raw workflow collection design

The `tools/unit-collector` package keeps its existing Cargo JSON collector and
adds a separate `workflow-collector` binary. The binary reads saved GitHub REST
run JSON and jobs-page JSON; network transport and connector pagination remain
outside Rust so raw responses can be retained and replayed.

Each JSONL row is one observed job, with run identity and run-level aggregates
repeated on the row. A run with no jobs still emits a run row carrying unknown
job fields. Rows retain run ID, attempt, event, status/conclusion, head/source/
merge/workflow SHAs, referenced workflow SHAs, raw job timestamps, runner
identity, and nested raw step timestamps/statuses. Source SHA provenance is
explicit. A merge SHA is emitted only when the direct run response is a pull
request, its direct `ref` is `refs/pull/N/merge` for the reported pull number,
and its direct `head_sha` differs from the source SHA.
`referenced_workflows[].sha` identifies workflow configuration only; it never
proves a merge SHA. No timestamp is synthesized from `updated_at` or log
markers.

Job pages accept direct GitHub REST objects and connector-style wrappers. The
collector recognizes `workflow_runs` and `jobs` envelopes, nested `content` or
`structuredContent` JSON text, and direct objects. A run is complete only when
the terminal run status is explicit, the jobs-page `total_count` is known and
matches the observed entries, every job and run has a known matching ID and
attempt, no duplicate job ID or page identity is present, and every expected
non-skipped job has a valid completion timestamp. A single raw page may establish the
count without page metadata when its total equals its observed count; multiple
pages require page identity and contiguous pagination. Malformed entries and
missing IDs are counted and censor completeness. Otherwise derived completion
is null with an explicit unknown reason. Any API `completed_at` field is kept
as metadata and never substitutes for derived job completion.

Run completion is the maximum verified non-skipped job `completed_at`; run
`updated_at` is never used. Job execution durations and their sum are separate
from run wall latency. The sum is null when the verified job set is incomplete
or any non-skipped job duration is missing, while a partial observed sum and
unobserved/unknown counts are retained. Pre-start (`run_started_at -
created_at`) is reported separately and labeled unclassified. Skipped jobs stay
in completeness counts, including GitHub's inverted synthetic timestamps, and
are not silently treated as executed work. Job attempts must exactly match the
run attempt; mismatches remain separate synthetic rows.

CSV is a flat job ranking with raw timestamp strings, duration, run metadata,
referenced-workflow JSON, cache/report fields when present, and completeness
flags. Markdown ranks jobs
only by known raw timestamp duration and reports censored/unknown counts. It
explicitly says no critical path is inferred without a dependency graph.

Fixtures cover parallel jobs, skipped jobs, missing timestamps, incomplete page
counts, duplicate IDs, malformed entries, page gaps, retries/attempts, direct
and wrapped API responses, reusable-workflow SHA provenance, and invalid time
ordering. Tests assert unknown/censored outputs rather than filling gaps.
