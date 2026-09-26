# Rerun freshness review

The parent independently reproduced the defect and reviewed the collector
implementation against saved run `35491248265`, attempt 2. GitHub labels all
68 records attempt 2, but 17 successful jobs retain execution before the
attempt boundary. They account for 2,665 seconds of earlier work. Only three
successful jobs executed afresh, totaling 46 seconds.

The old collector reported a verified 2,711-second execution sum and a
16,184-second creation-to-completion interval. The repaired collector retains
raw records and structural completeness, marks `mixed_stale_records`, and
leaves full-attempt execution and wall time unknown. Its explicitly partial
fresh interval is 56 seconds; this is not a full validation benchmark.

Independent checks: 22 workflow collector tests passed; replay of the retained
REST inputs produced fresh sum 46,000 ms, stale sum 2,665,000 ms, three fresh
jobs, 17 stale jobs, null full execution/wall metrics and null rerun pre-start.
The implementation agent separately ran all 48 package tests and strict
all-target Clippy. Collection documentation passed Markdown validation.

Review checked skipped synthetic intervals, unknown starts, incomplete pages,
full-fresh reruns, initial trigger latency, and raw stale-row labeling. No
cache, source, coverage or artifact identity is inferred from timestamp
freshness. Missing request time remains unknown. No CI speedup follows.

Verdict: accept the collector correctness repair. Exact pushed-revision CI
remains required; this does not satisfy campaign performance acceptance.
