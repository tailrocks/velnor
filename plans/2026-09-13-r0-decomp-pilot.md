# Plan 2026-09-13: decomposition pilot — composite conclusion scopes (GOAL 49/58)

Base: origin/main tip bfefb5c7. Branch: r0-decomp-pilot. Worktree: /tmp/velnor-decomp.

## Problem

`crates/velnor-runner/src/executor.rs` is a 31,923-line
responsibility-mixed module (GOAL 58: decompose giant modules where
structure enables defects). The composite conclusion-scope machinery —
frame stack, converted-id set, descendant propagation, converted-aware
status scans — sat inside `JobExecutionState` as three bare fields with
the push/pop/record/scan invariants spread across seven methods.

## Fix (this branch, strict scope)

New module `execution/composite_scopes.rs` (existing `execution/`
layout, private mod + targeted `pub(crate) use`, mirroring the
`docker` precedent). Moved verbatim, bodies byte-identical, field
renames only:

- `CompositeConclusionFrame` (stays private to the new module —
  nothing outside names it after the move);
- `StepOutcome` + `as_str` (domain vocabulary; the frame map, the
  job maps, and the scan predicates all share it, so it moves with
  the domain instead of widening `executor.rs` visibility);
- new owner `CompositeConclusionScopes` holding the three fields
  (`stack`, `converted`, `frames`) with narrow API:
  `push`, `pop`, `convert`, `convert_open_scopes`,
  `record` (the scope half of umbrella result application, extracted
  from the `apply`/`apply_cancelled` frame writes),
  `top_level_has_failure` (job-status scan),
  `scope_has_failure` (composite-status scan).

`JobExecutionState` keeps one field (`composite_scopes`) and thin
delegates (`push/pop/convert/convert_open`, `record` calls in
`apply`/`apply_cancelled`, scan calls in `job_status`/
`status_scope_has_failure`), so all four engine call sites and every
test call site are byte-unmodified.

Interpretation: "umbrella result application" = recording (umbrella)
results into the open conclusion scope. The `CompositeFrame`
timeline/log frame (`umbrella_result`, `merge_nested`, `into_step_log`,
the `composite_frames` stack and Start/End handling) is a separate
domain — explicitly NOT extracted (see next candidates).

Zero tests moved: no test references the moved items directly; all
scope tests go through the preserved `JobExecutionState` API.

## Tests

- `cargo fmt -p velnor-runner`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- `cargo test -p velnor-runner --lib -- executor::`: 308 passed,
  0 failed (all executor tests unmodified).
- Scope-targeted filters (composite/conclusion/umbrella/action_status/
  job_status/failure_ignored/continue_on_error): 54 passed, 0 failed.
- Full `cargo test -p velnor-runner --no-fail-fast`: lib 1928 passed
  with only parallel-run lock/lease flakes outside the touched code
  (checkout/git_mirror spans and leases; the failing set churns
  run-to-run — base tip shows the same flake family, 5 failed on one
  full base run); all other targets (bins, integration, doc) green.

## Pre-existing flakes (not caused by, not fixed by this branch)

`checkout::tests::checkout_emits_the_four_bench_phase_spans`,
`checkout_releases_mirror_reader_lease_after_hydration`,
`git_mirror::tests::checkout_reader_lease_blocks_mirror_repair`,
`runner::tests::job_claim_excludes_duplicate_slots_until_owner_drops`,
`node::cleanup::tests::stale_outbox_quarantine_defers_while_removal_lock_is_held`,
`runner::tests::configure_lock_excludes_concurrent_transactions` —
all lock/lease contention under full-suite parallelism; reproduce on
the unmodified base (`git stash -u` + same filters), none touch
conclusion scopes.

## Next candidate domains (untouched, observed during this work)

1. `CompositeFrame` execution-frame domain (`executor.rs`: struct +
   `umbrella_result`/`merge_nested`/`absorb`/`into_step_log`, the
   `composite_frames` stack, `CompositeStart`/`CompositeEnd` handling
   in the step loop, defensive flush) — the timeline sibling of this
   extraction; deeply interleaved with step dispatch, needs its own
   work package.
2. `JobExecutionState` remainder: expression context
   (`steps_context`, `job_context`), status functions
   (`success/failure/cancelled`), and the `apply` pipeline — the
   semantic expression/runtime state boundary GOAL 49 names.
3. Full inventory pass still needed before further extractions; no
   other domain was mapped in this pilot.
