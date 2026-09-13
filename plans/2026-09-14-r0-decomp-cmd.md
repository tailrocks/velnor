# Plan 2026-09-14: decomposition slice 2 — step-output workflow commands (GOAL 49/58)

Base: origin/main tip 545bab27. Branch: r0-decomp-cmd. Worktree: /tmp/velnor-dec2.

## Problem

`crates/velnor-runner/src/executor.rs` is a 31.8k-line
responsibility-mixed module (GOAL 58: decompose giant modules where
structure enables defects). The step-output half of the
workflow-command surface — parse `::directives` from captured
stdout/stderr, fold a failed command result into the step exit code,
render the step log under the same directive policy, rewrite
command-file env paths for action containers — sat in `executor.rs`
as five free functions even though the directive parser
(`workflow_command`: echo/debug/mask/stop-commands) and the
command-file plan (`script_step::ScriptStepPlan::prepare`/
`collect_state` for ENV/OUTPUT/PATH/STATE) already live in their own
modules.

## Fix (this branch, strict scope)

New module `execution/command_output.rs` (existing `execution/`
layout, private mod + targeted `pub(crate) use`, mirroring the
slice-1 `composite_scopes` precedent from PR #741). Moved verbatim,
bodies byte-identical, `pub(crate)` visibility only:

- `parse_workflow_commands_from_output` (stdout+stderr directive
  parse + merge);
- `apply_command_result` (upstream `RunStepAsync` merge:
  `command_failed` with exit 0 becomes exit 1);
- `step_log_lines` + `skipped_step_log_lines` (directive-aware step
  log rendering; the skip marker stays visible at the call site
  through the same function);
- `rewrite_command_file_env_for_action_container` (`/__t/` to
  `/github/file_commands/` rewrite for the five command-file vars).

All eight engine call sites are byte-unmodified (name resolution now
via the `execution::{...}` import). Eight unit tests move with their
code, bodies byte-identical
(`command_result_merge_fails_step_on_command_failure`,
`unsecure_command_opt_in_flows_from_job_env_to_step_parsing`, six
`step_log_lines_*`); the moved test module carries a local `temp_dir`
helper (per-module-helper precedent: `preflight.rs`), since the
executor test helper stays with its 205 remaining users.

Explicitly NOT extracted (see next candidates): the `apply`-pipeline
application of `StepCommandState` into job state
(outputs/state/env/path/masks), `secret_masks`, `step_debug` —
interleaved with outcome/conclusion tracking, needs its own slice.
No semantic change anywhere: no redesign, no behavior delta.

## Tests

- `cargo fmt -p velnor-runner -- --check`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- Moved tests `cargo test -p velnor-runner --lib --
  execution::command_output`: 8 passed, 0 failed.
- `cargo test -p velnor-runner --lib -- executor::`: 300 passed,
  0 failed (slice-1 baseline 308 minus the 8 moved tests).
- Full `cargo test -p velnor-runner --no-fail-fast`: lib 1965
  passed with only the known parallel lock/lease flakes (this run:
  2 checkout + 1 node::cleanup; the failing set churns run-to-run);
  all other targets green except one timing-sensitive
  `idle_scaling` miss that also reproduces on base (see below).

## Pre-existing flakes (not caused by, not fixed by this branch)

`checkout::tests::checkout_emits_the_four_bench_phase_spans`,
`checkout_releases_mirror_reader_lease_after_hydration`,
`git_mirror::tests::checkout_reader_lease_blocks_mirror_repair`,
`node::cleanup::tests::stale_outbox_quarantine_defers_while_removal_lock_is_held`,
`runner::tests::configure_lock_excludes_concurrent_transactions` —
lock/lease contention under full-suite parallelism; the unmodified
base (`git stash -u` + same full suite) fails 5 in the same family.
`idle_scaling::idle_resource_scaling_from_one_to_sixteen_slots_is_bounded`
panics at `tests/idle_scaling.rs:729` identically on base when run
with the same single-test filter. None touch command output.

## Next candidate domains (untouched, observed during this work)

1. Command-state application half (`executor.rs`:
   `JobExecutionState::apply` outputs/state/env/path/masks block,
   `secret_masks`, `step_debug`) — the direct sibling of this
   extraction; interleaved with outcome/conclusion recording, so it
   needs a careful application-vs-tracking split as slice 3.
2. `CompositeFrame` execution-frame domain (struct +
   `umbrella_result`/`merge_nested`/`absorb`/`into_step_log`, the
   `composite_frames` stack, `CompositeStart`/`CompositeEnd`
   handling, defensive flush) — still the largest coherent
   timeline/log candidate; named by the pilot, untouched here.
