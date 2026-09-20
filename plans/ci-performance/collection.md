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
explicit. The raw API `head_sha` is run metadata, not checkout proof. An
embedded PR head may advance after the run and is retained separately as
`observed_pull_request_head_sha`. PR source identity stays unknown unless an
explicit matching `refs/pull/N/head` ref proves it. Push, dispatch and scheduled
run heads have event-specific source bases; derived SHAs must be full hex.
The direct merge ref is retained, but merge SHA stays unknown without immutable
checkout evidence. A run head and a checkout merge commit can differ.
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

Run completion is the maximum observed non-skipped job `completed_at`; run
`updated_at` is never used. Collection completeness and execution freshness
are separate facts. `run_attempt` plus a new job ID does not prove that a
rerun executed the job: GitHub can expose a reused successful job under the
new attempt while retaining its earlier timestamps.

For every non-skipped job, the collector classifies freshness against the
run's `run_started_at`: `fresh` when the job starts at or after that boundary,
`stale_before_attempt` when it starts earlier, and an explicit unknown or
invalid state when the boundary/start/order cannot be proven. Skipped jobs
remain in the collection count and do not create freshness obligations; their
synthetic inverted intervals remain visible as skipped evidence.

`attempt_freshness_state` must be `complete_fresh` before a full execution sum
or `attempt_wall_ms` is emitted. Mixed or stale attempts retain raw rows,
`fresh_execution_partial_sum_ms`, `stale_execution_partial_sum_ms`, fresh and
stale counts, and a `fresh_attempt_wall_ms` value only when that observed
subset has complete timing. Per-subset unknown counters expose missing or
overflowed durations; a partial sum never hides those failures. Those fields
are explicitly partial evidence and must not be used as full-workflow
benchmarks. `run_lifetime_ms` measures
`created_at` to completion only for attempt 1, where `created_at` is the
trigger boundary. Reruns report
`run_lifetime_state=unknown_rerun_created_at_is_original`; their attempt wall
measurement uses `run_started_at` and remains null unless all executed records
are fresh and valid. `pre_start_ms` remains unclassified and is never queue
time.

This timestamp boundary is execution evidence, not source or coverage proof.
For stronger rerun attribution, a transport may bind current and prior attempt
snapshots, or a generated workflow may emit a per-attempt marker. Missing
snapshot or marker evidence remains unknown; it cannot upgrade a stale or
mixed timestamp set. `complete_fresh` proves only the observed timing
boundary; it does not prove source identity, effective checkout, required
coverage, or artifact validity. Job attempts must exactly match the run
attempt; mismatches remain separate synthetic rows.

CSV is a flat job ranking with raw timestamp strings, duration, run metadata,
referenced-workflow JSON, cache/report fields when present, and completeness
flags. Markdown ranks jobs
only by known raw timestamp duration and reports censored/unknown counts. It
explicitly says no critical path is inferred without a dependency graph.

Fixtures cover parallel jobs, skipped jobs, missing timestamps, incomplete page
counts, duplicate IDs, malformed entries, page gaps, retries/attempts, direct
and wrapped API responses, reusable-workflow SHA provenance, and invalid time
ordering. Tests assert unknown/censored outputs rather than filling gaps.
