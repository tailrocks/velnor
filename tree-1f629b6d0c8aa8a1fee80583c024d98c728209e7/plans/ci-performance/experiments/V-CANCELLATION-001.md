# V-CANCELLATION-001: obsolete PR run cancellation

Status: independent source review and isolated deterministic checks passed;
real-CI cancellation replay and consumer runtime rollout pending.
Date: 2026-09-20.

## Finding

The pull-request workflow declares `concurrency.cancel-in-progress: true`,
but generated caller and aggregate jobs use job-level `always()` conditions.
GitHub documents that `always()` remains true after cancellation and warns that
it can prevent a job from stopping. The current generated `ci-pr.yml` has 37
textual `always()` references across reusable callers and control gates. This
can keep an obsolete run alive while the replacement waits for the same group.

Primary references:

- [GitHub expressions: status check functions](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions)
- [GitHub workflow concurrency](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [GitHub cancellation troubleshooting](https://docs.github.com/en/actions/how-tos/troubleshoot-workflows)

## Raw observation

Run [35485817832](https://github.com/tailrocks/velnor/actions/runs/35485817832),
attempt 1, was created and started at `03:08:55Z` and reports conclusion
`cancelled`. Its API `updated_at` is `03:18:54Z`; that is not an authoritative
completion or cancellation timestamp. Its 68-job response shows executed
planning, unit, and gate jobs completing successfully through `03:18:53Z`;
there is no executed failure explaining the cancellation.

Replacement run
[35485891400](https://github.com/tailrocks/velnor/actions/runs/35485891400),
attempt 1, was created and started at `03:10:34Z` but planning did not start
until `03:18:56Z`, three seconds after the old run’s last executed job completed. That is
502 seconds from replacement creation to planning start. Its planning job ran
`03:18:56–03:19:16Z`; `ci-required` ran `03:28:06–03:28:08Z`; `Control /
Required` ran `03:28:10–03:28:13Z`. The raw run/job files are
`observations/velnor-35485817832-*` and `observations/velnor-35485891400-*`.

This timing establishes a cancellation/concurrency symptom. It does not by
itself prove every second was queue time or establish a critical path; the
collector leaves pre-start intervals unclassified.

## Root cause hypothesis

The enabling structure is a cancellation guard at workflow scope combined with
unconditional job-level `always()`. The `needs` result expressions are useful
for deciding failure and skip obligations, but `always()` also asks GitHub to
continue scheduling work after the run is cancelled. The replacement cannot
start until the old group member relinquishes the concurrency slot.

The root fix is to put `!cancelled()` before the existing obligation
expressions on reusable callers, preparation jobs, and aggregate gates. Keep
step-level `always()` only where cleanup/artifact publication deliberately
runs during a failed but non-cancelled job. A cancelled run must not publish a
passing required gate.

## Alternatives

1. **Preferred:** render `if: ${{ !cancelled() && <existing condition> }}` for
   every job-level caller and aggregate gate in this PR group. Preserve the
   existing explicit `needs.*.result` and admission/skip clauses. The required
   aggregate still evaluates ordinary failed/skipped dependencies because
   `!cancelled()` is true in those cases, but it stops promptly on cancellation.
2. Guard only reusable callers and leave `ci-required`/`required` on
   `always()`. This may reduce runner work but the aggregate can still hold the
   group until its dependencies drain. Reject until a measured replay proves
   the required check remains correct and the old run releases promptly.
3. Queue runs (`cancel-in-progress: false`) or add a first-step cancellation
   check. Queueing preserves obsolete work and worsens the wait; a first-step
   guard still schedules every job and can alter required-check behavior.
   Reject both as fixes to this root cause.

## Exact implementation boundary for review

Keep the first source change to the two IR renderers and their focused tests:

- `crates/velnor-workflow/src/primitives/ir.rs`: pass the aggregate's
  `cancel_in_progress` decision from `render_nested` into
  `render_prepare_cargo_caller`, `render_unit_lane_caller`,
  `render_node_callers`, `render_nodes_required`, and the `required` mirror
  they emit. Also cover the direct `render_required` path, which emits the
  same `ci-required`/`required` jobs for the legacy direct renderer.
- `crates/velnor-workflow/src/s2/primitives/ir.rs`: make the same change for
  `render_prepare_cargo_caller`, `render_unit_provider_caller`,
  `render_node_callers`, `render_nodes_required`, and its required mirror.
- Add tests beside those IR modules (or their existing generator contract
  suites) that render PR, main, and nightly workflows from the same minimal
  one-unit graph.

The renderer should prepend `!cancelled() &&` only when the aggregate's
`cancel_in_progress` is `true`. This covers PR callers and required aggregates
without changing main's producer cadence (`cancel-in-progress: false`) or
nightly/release/publishing workflows. Preserve the existing `needs.*.result`,
selection, admission, and dependency clauses byte-for-byte after the prefix.
Keep step-level cleanup/artifact `always()` expressions outside this change.
Do not edit generated YAML, release renderers, nightly alert publication, or
the policy workflow in this experiment.

Required fixture assertions:

1. PR output has the guard on every reusable caller, `ci-required`, and the
   `Control / Required` mirror; the old result/admission predicates remain.
2. Main output keeps `cancel-in-progress: false` and has no new cancellation
   guard from this change. Nightly output keeps its existing alert and
   publishing conditions.
3. A synthetic failed or skipped `needs` result still reaches the required
   shell validator in the rendered contract and fails according to its
   existing verdict logic; `!cancelled()` must not become a success shortcut.
4. A rendered job with no `needs` result is still rejected by the existing
   required gate fixture. This guards against replacing explicit obligations
   with a cancellation-only condition.

Run source-1 and source-2 focused IR tests, generator contract tests,
actionlint, and a real two-push cancellation replay before accepting the
change. Jackin's independent review should compare generated job counts,
needs, required status names, and publication conditions against the baseline.

## Acceptance experiments

- Generate source-1 and source-2 PR workflows and classify every job-level
  `if`: no unconditional `always()` remains in the cancellation group; cleanup
  steps may retain a reviewed step-level status guard.
- Fixture a failed unit and a skipped/admitted unit. Verify the required
  aggregate still runs for ordinary failure/skip results and fails closed.
- Trigger two same-PR runs close together. Record run/job API timestamps and
  verify the old run becomes cancelled before the new planning job starts;
  record the required-check conclusion for both. Use raw timestamps, not
  `updated_at`, for wait attribution.
- Trigger cancellation during candidate acquisition and during a required
  aggregate. Verify no candidate artifact publication or successful required
  status survives the cancelled run.

No 10x or any performance claim follows from the one 502-second wait. The
measurement target is obsolete work and queue delay first.

## Independent challenge

The `!cancelled()` prefix is semantically safe for ordinary failures and
skips: `cancelled()` is false, the existing explicit `needs.*.result`
conditions still select the job, and the shell verdict still rejects a
selected failure, missing result, or forbidden skip. On cancellation the
required aggregate and its mirror become cancelled/skipped; neither can emit
a passing required result. Main, nightly, release, and publishing workflows
keep `cancel-in-progress: false` and are outside this prefix.

The timing evidence does not yet prove that job-level `always()` caused the
502-second delay. In run 35485817832, the replacement was created at
03:10:34Z, but the old run's longest reusable child, `Rust · velnor-workflow ·
github-hosted`, ran 03:09:19–03:18:43Z and completed successfully. The old
`ci-required` and `Control / Required` jobs then ran 03:18:46–03:18:53Z; the
run was marked cancelled at 03:18:54Z. The replacement planning job started
03:18:56Z. This proves stale work occupied the path, but it does not identify
the cancellation request time or show that an `always()` gate held the run.
The child log reaches successful checks and cleanup at 03:18:40Z.

The proposed aggregate caller guard cannot stop a reusable child that already
started. Child workflows have ordinary job conditions, but several cleanup,
cache, marker, and report **steps** still use `always()`; those can continue
after cancellation. Measure this separately before claiming prompt release.
The first implementation should therefore be accepted only as a guard for
not-yet-started PR callers and control gates. A cancellation injected during a
long child and another during `ci-required` must verify actual release and
absence of a passing required check.

The exact source boundary is otherwise correct: schema 1 covers
`render_prepare_cargo_caller`, `render_unit_lane_caller`,
`render_node_callers`, `render_nodes_required`, and the direct
`render_required` path; schema 2 covers `render_prepare_cargo_caller`,
`render_unit_provider_caller`, `render_node_callers`, and
`render_nodes_required` with its required mirror. Do not add the guard to
plan jobs, reusable child steps, nightly alerting, or publishing cleanup in
this experiment. Prefer a typed boolean/enum derived from the aggregate
trigger instead of passing the rendered string `"true"` through the call
graph.

## Candidate implementation

The candidate threads a typed `bool` returned by `aggregate_triggers` through
both IR renderers. Only pull-request aggregates replace the first job-level
`always()` term with `!cancelled()` on reusable callers, the required aggregate,
and its `required` mirror. The existing plan, policy, dependency, selection,
admission, and shell verdict expressions remain in their original order. Main
and nightly pass `false` and retain `always()`; child-workflow steps, nightly
alerts, and publishing cleanup are unchanged.

Focused fixture coverage now renders both schemas for PR, main, and nightly
graphs. It checks the plan-success and dependency-skipped clauses, the PR
concurrency/required guards, and the unchanged stable-event guard. Schema 1
also executes the rendered required shell for selected `success`, `failure`,
`skipped`, and `cancelled` caller results; schema 2 retains the equivalent
existing result loop and explicit status cases. Local source checks: focused
IR tests passed before the unrelated policy test fixture drift appeared;
library `cargo check` and `cargo clippy --lib -D warnings` pass; formatting and
diff checks pass. The clean two-file candidate diff is retained at
`/tmp/velnor-cancellation-20260920.patch` during review.

Acceptance remains open until the generated PR workflows pass actionlint and a
paired CI replay injects cancellation during a long child and during the
required aggregate. The replay must verify old-run release, no passing required
status from the cancelled run, and no change to main/nightly/release/publish
paths. The local test cannot establish those GitHub scheduler properties.

## Independent integration checks

`/root/parallax_inventory` independently confirmed PR-only guards and preserved
required-result predicates. It explicitly rejected attributing the full
502-second delay to these guards: already-running reusable children and their
cleanup may continue. Real cancellation replay remains necessary.

Parent applied only the cancellation unit to an isolated checkout based on
`64cef5ef93a90660983b3f2ec9dc42cfebda73c3` (locked-install source tree).
The first full test run found two old PR assertions expecting `always()` and
expected checked-in YAML drift. The assertions now require `!cancelled()` while
retaining dependency-failure evaluation; the generator regenerated the YAML.
All 1,790 library tests then passed, as did all-target Clippy and actionlint.
Only `ci-pr.yml` changes workflow behavior: 37 job guards change, with existing
selection, trust, and verdict expressions retained. Main/nightly/release output
is byte-identical in this isolated comparison. Generator revisions are schema
1 = 59 and schema 2 = 61. No elapsed-time improvement is accepted from these
local checks.
