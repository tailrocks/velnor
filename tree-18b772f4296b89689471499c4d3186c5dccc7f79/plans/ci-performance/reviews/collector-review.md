# Workflow collector review

Independent postfix review of `tools/unit-collector` found one timing
correctness defect and one evidence-retention defect. Both are fixed in the
current working tree; no source or timing claim is accepted without the
scoped checks below.

## Required-gate timing

`required_gate` now returns an unknown state unless the selected jobs have a
matching run ID and attempt, the collection has no duplicate-job or page
identity conflict, every non-skipped selected job is terminal, and every
selected start/end pair parses in non-inverted order. A selected in-progress
or inverted job can no longer produce an observed gate timestamp while the
run-level completion is censored.

Selectors are an all-of requirement: every distinct requested job name must
have exactly one matching job with a known ID and valid terminal timing. A
missing name, ambiguous duplicate name, skipped required job, or partial
selector set remains explicitly unknown; completion from only the names that
happen to be present is never labeled as the full gate.

## Referenced-workflow evidence

Non-object or malformed-field members of `referenced_workflows` are retained
as structured evidence issues with member index, reason, and compact raw JSON.
Valid members remain available as configuration metadata. No referenced
workflow SHA is promoted to a top-level merge SHA.

## Invariants reviewed

- Missing counts, page identity, attempts, IDs, timestamps, and terminal state
  keep derived completion and complete execution sums unknown.
- Duplicate or conflicting job IDs remain visible but cannot verify aggregate
  timing; partial sums exclude conflicting IDs.
- Skipped jobs do not invalidate valid executed timing, including GitHub's
  inverted synthetic skipped timestamps.
- Run `completed_at` and `updated_at` remain metadata; derived completion is
  the maximum verified non-skipped job completion timestamp.
- Critical path remains unknown without a dependency graph.

## Scoped verification

`rtk cargo test -p unit-collector --test workflow` — 16 passed.

`rtk cargo fmt -p unit-collector -- --check` — passed.

`rtk cargo clippy -p unit-collector --all-targets -- -D warnings` — passed.

## Independent bounded review (2026-09-20)

**PASS for the requested scope.** I independently traced the current
`required_gate` path and ran the focused suite. Every distinct requested job
name is all-of: exactly one matching record is required, with a known job/run/
attempt identity, terminal non-skipped status, and parseable non-inverted start
and end timestamps. Missing or duplicate names, missing IDs, mismatched
attempts, duplicate IDs, duplicate or incomplete pages, mixed attempts, and
page-identity conflicts fail closed before an observed gate timestamp can be
returned. Malformed `referenced_workflows` members remain attached to every
emitted run/job row with member index, reason, and raw JSON; valid members are
kept separately. A malformed top-level non-array field returns an input error,
which is fail-closed rather than silently discarded. Independent command:
`rtk cargo test -p unit-collector --test workflow` — 16 passed.
