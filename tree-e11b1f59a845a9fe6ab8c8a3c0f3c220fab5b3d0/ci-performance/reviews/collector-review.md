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

`rtk cargo test --locked -p unit-collector --test workflow` — 18 passed.

## Source-identity defect and bounded fix (2026-09-20)

**Prior HOLD.** Before this bounded fix, `parse_run` assigned
`source_sha = pull_requests[0].head.sha` for a `pull_request` run. The run
endpoint's embedded pull-request object is a live PR projection; after a PR
advances, replaying an old run can report the new PR head. That makes saved
historical timing rows claim a source commit that did not create the run. The
current recent rows happened to have equal heads, but older rows using
`source_sha_basis=pull_requests.head.sha` require audit.

`run.head_sha` is the raw, immutable run-head field in the API response. It is
not proof of the effective checkout commit: a PR run can expose one SHA while
the checkout step records a synthetic merge commit. A direct
`refs/pull/<number>/merge` ref is retained as `merge_ref`, but the run API alone
must leave `merge_sha` unknown until immutable checkout or event evidence is
supplied. A referenced-workflow SHA is never such evidence.

For a PR run, `source_sha` stays unknown unless the run event/ref itself proves
the source context. The embedded PR head is retained separately as
`observed_pull_request_head_sha`; it is a live API projection and never becomes
historical identity. For push, dispatch, and schedule runs, a valid
`run.head_sha` is retained as `source_sha` with an event-specific basis. Invalid
or unrecognized event/SHA combinations remain explicit unknowns.

Minimal regression fixture:

```json
{
  "id": 99,
  "run_attempt": 1,
  "event": "pull_request",
  "ref": "refs/pull/7/merge",
  "head_sha": "merge-created-for-old-head",
  "pull_requests": [{"number": 7, "head": {"sha": "head-after-run"}}]
}
```

Expected normalized identity: the raw `head_sha` is retained, `merge_ref` is
retained when directly present, `merge_sha=null`, and `source_sha=null` with a
clearly marked unknown basis; `head-after-run` is retained only as current API
evidence. The regression fixture uses the actual replay where run head
`e1357dd...` was paired with live PR head `29279ab...`; output must not claim
`29279ab...` as historical source. Push, `pull_request_target`, invalid-SHA,
and unrecognized-event fixtures pin the event-aware fail-closed behavior. Do
not use `run.updated_at` or the live PR object to recover a historical source
SHA.

### Fix delivered

`parse_run` now retains the raw API `head_sha` and the embedded PR head in
separate fields. The latter is evidence only. It accepts a source SHA only for
the explicitly supported executed-ref events (`push`, `workflow_dispatch`,
and `schedule`) or an explicit `refs/pull/N/head` ref; it validates derived
SHA values as 40-character hexadecimal strings. `pull_request_target` and
unrecognized events remain unknown. A merge ref is retained, but `merge_sha`
stays unknown because this input has no immutable checkout proof.

The exact replay fixture from run `35487077663` now emits
`head_sha=e1357dd...`, `source_sha=unknown`, and
`observed_pull_request_head_sha=29279ab...`; the mutable `29279ab...` value is
not emitted as historical source. Focused fixtures cover invalid SHA, valid
push, `pull_request_target`, and unrecognized events. Regenerate affected raw
derived rows only after independent review of this boundary.

`rtk cargo fmt -p unit-collector -- --check` — passed.

`rtk cargo clippy --locked -p unit-collector --all-targets -- -D warnings` — passed.

### Independent identity verification

The parent independently reviewed the source boundary, ran all 44 collector
tests, and exercised 12 event/ref/SHA combinations through the actual CLI.
These covered matching and mismatched PR head refs, merge refs, push, schedule,
manual dispatch, PR target, missing/unknown events and malformed SHA values.
Raw run heads stayed intact; no merge SHA or mutable PR source was invented.
The explicit matching/mismatched-ref regression is retained in the test suite.
All-target Clippy passed. This is a correctness repair, not a speedup claim.

Seven existing JSONL/CSV observation pairs contain the previous derived source
basis and require regeneration from their saved raw responses. Git history
retains the original erroneous derivations; they must not be used for cohort
comparisons while that audit is pending.

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
`rtk cargo test --locked -p unit-collector --test workflow` — 18 passed.
