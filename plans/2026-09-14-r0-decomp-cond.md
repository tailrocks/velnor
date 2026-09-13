# Plan 2026-09-14: decomposition slice 3 — step-condition evaluation (GOAL 49/58)

Base: origin/main tip 5b4dfa54. Branch: r0-decomp-cond. Worktree: /tmp/velnor-dec3.

## Problem

`crates/velnor-runner/src/executor.rs` is a 31.6k-line
responsibility-mixed module (GOAL 58: decompose giant modules where
structure enables defects). The step-gating surface — the default
`success()` condition, the implicit `success() && (...)` prefix, the
`success`/`failure`/`cancelled`/`always` status functions, the
`job.status` / `github.action_status` derivations, the live-token
cancelled read that replaces upstream's re-evaluation pass, and the
immutable-`github` static-false preflight — sat in `executor.rs` as ten
`JobExecutionState` methods plus five free helpers, even though the
expression parser/evaluator (`expression`: parse, `evaluate_node`,
truthiness) already lives in its own module.

## Fix (this branch, strict scope)

New module `execution/step_conditions.rs` (existing `execution/`
layout, private mod + targeted `pub(crate) use`, mirroring the
slice-1 `composite_scopes` / slice-2 `command_output` precedent).
Moved verbatim, bodies byte-identical:

- `impl JobExecutionState`: `is_cancelled` + `set_cancellation`
  (cancelled re-evaluation logic: the live-token read), `success_status` +
  `failure_status` + `job_status` + `action_status` (status-function
  evaluation), `status_scope_has_failure` (stays module-private: all
  callers moved with it), `evaluate_condition` +
  `evaluate_post_condition` + `evaluate_condition_expression` (the
  latter stays module-private: both callers moved with it);
- free functions: `condition_is_statically_false` (`pub(crate)` API,
  re-exported for the planner) + module-private `strip_expression`,
  `node_references_status_function`, `immutable_expression_is_false`,
  `reads_only_immutable_github`.

Mechanical enablers only: three `JobExecutionState` fields
(`conclusions`, `composite_scopes`, `cancellation`),
`expression_context()`, and `JobExpressionContext` widen private →
`pub(crate)` so the relocated `impl` can read them; five moved methods
widen private → `pub(crate)` for their staying callers
(`JobExpressionContext::call_function`, `github_context`/`job_context`,
engine dispatch, tests). All engine call sites byte-unmodified
(methods still resolve on the same type); the one free-function caller
(`runner.rs` planner) updates its import mechanically. Two unit tests
move with their code, bodies byte-identical
(`condition_evaluation_failure_is_reported`,
`immutable_github_condition_can_prove_local_action_is_skipped`).

Explicitly NOT extracted: `JobExpressionContext` itself (github / env /
runner / steps / job contexts, `hashFiles` — the whole expression
context, far broader than gating), `node_reads_runtime_context` +
`render_template` (plan-time template deferral, not condition
evaluation), `step_debug`. No semantic change anywhere: no redesign, no
behavior delta.

## Tests

- `cargo fmt -p velnor-runner -- --check`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- Moved tests `cargo test -p velnor-runner --lib --
  execution::step_conditions`: 2 passed, 0 failed.
- `cargo test -p velnor-runner --lib -- executor::`: 298 passed,
  0 failed (slice-2 baseline 300 minus the 2 moved tests).
- Gating-targeted filters: `condition` 24 passed, `status_function`
  3 passed, `cancelled` 12 passed, 0 failed.
- Full `cargo test -p velnor-runner --no-fail-fast`: lib 1989
  passed with only the known parallel lock/lease flakes (this run:
  2 checkout + 1 git_mirror; identical set on the unmodified base,
  see below); all other targets green (bins, every integration suite
  including `idle_scaling` 4/4, doc).

## Pre-existing flakes (not caused by, not fixed by this branch)

`checkout::tests::checkout_emits_the_four_bench_phase_spans`,
`checkout_releases_mirror_reader_lease_after_hydration`,
`git_mirror::tests::checkout_reader_lease_blocks_mirror_repair` —
lock/lease contention under full-suite parallelism; the unmodified
base (`git stash -u` + same full lib suite) fails the identical 3
with identical counts (1989 passed / 3 failed / 2 ignored). None touch
step conditions.

## Next candidate domains (untouched, observed during this work)

1. Command-state application half (`executor.rs`:
   `JobExecutionState::apply` outputs/state/env/path/masks block,
   `secret_masks`, `step_debug`) — the direct sibling named by slice 2
   as slice 3; this slice took conditions instead per delegation, so
   the application-vs-tracking split is now the natural slice 4.
2. `CompositeFrame` execution-frame domain (struct +
   `umbrella_result`/`merge_nested`/`absorb`/`into_step_log`, the
   `composite_frames` stack, `CompositeStart`/`CompositeEnd`
   handling, defensive flush) — still the largest coherent
   timeline/log candidate; named by the pilot, untouched here.
