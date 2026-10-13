# Slice 5: step outcome/conclusion application — plan note

Date: 2026-09-14. Base: origin/main tip `f5c515fc` at fetch time.
Branch: `r0-decomp-outcome`. Follows slices 1–4 (composite scopes #741,
command output #772, step conditions #797, post drain unmerged
`origin/r0-decomp-post`).

## Moved surface (executor.rs → execution/step_outcome.rs)

- `JobExecutionState::apply` — outcome-vs-conclusion application:
  `outcome` from `skipped`/`exit_code`, `conclusion` converting
  `failure`→`success` only when `failure_ignored` (upstream
  `ApplyContinueOnError`), plus the outputs/state/env/path/masks merge
  block (one method body; splitting it would redesign semantics).
- `JobExecutionState::apply_cancelled` — cancelled override recording
  `cancelled`/`cancelled`, never converted by `continue-on-error`.
- `JobExpressionContext::steps_context` — `steps.<id>.{outputs,outcome,
  conclusion}` exposure built from runtime step state. Widened
  `private` → `pub(crate)` for the staying `named_value` caller.
- Two unit tests move with their code, bodies byte-identical:
  `job_state_flows_env_and_path_to_later_steps`,
  `resolves_step_outputs_in_later_action_env`.

Mechanical enablers only: `JobExecutionState` fields `env`,
`dynamic_env`, `outputs`, `action_states`, `outcomes`, `path`, `masks`
and `JobExpressionContext::state` widen `private` → `pub(crate)`.
No call-site updates needed (same-crate method calls). `mod.rs` gains
`mod step_outcome;` only — no new free functions to re-export.

Staying (explicitly out of scope): composite-scope delegates
(`push/pop/convert`), `secret_masks`, `step_debug`, `action_state_env`,
`prelude_env`, expression rendering, engine `failure_ignored` dispatch
sites, the job-conclusion scan over `results`.

## Evidence

- `cargo check -p velnor-runner --all-targets`: clean.
- `cargo fmt -p velnor-runner -- --check`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- Moved tests `execution::step_outcome`: 2/2 pass.
- `cargo test -p velnor-runner --lib -- executor::`: 296 passed,
  0 failed (298 slice-3 baseline minus the 2 moved tests).
- Outcome-targeted filters (`outcome conclusion continue_on_error
  steps_context`): 20 passed, 0 failed.
- Full `cargo test -p velnor-runner --no-fail-fast`: all targets green
  except the known parallel lock/lease flakes in the lib suite
  (branch run 1: 2003 passed / 4 failed; run 2: 2005 passed /
  2 failed — run-to-run churn). Unmodified base (`git stash -u` +
  same lib suite): 2004 passed / 3 failed, the identical
  `checkout_emits_the_four_bench_phase_spans`,
  `checkout_releases_mirror_reader_lease_after_hydration`,
  `checkout_reader_lease_blocks_mirror_repair` flakes. The fourth
  branch-run-1 failure
  (`node::cleanup::stale_outbox_quarantine_defers_while_removal_lock_is_held`)
  passes in isolation 4/4 and on the branch's second full run —
  same lock-contention family, untouched file, untouched domain.

## Next candidate domain (untouched, observed during this work)

`CompositeFrame` execution-frame domain (`executor.rs`: struct +
`absorb`/`umbrella_result`/`merge_nested`, the `composite_frames`
stack, `CompositeStart`/`CompositeEnd` handling, defensive flush) —
still the largest coherent timeline/log candidate; named by the pilot
and slice 4, untouched here. With the outcome-application surface now
behind `step_outcome`, the frame's `umbrella_result` /
`failure_ignored` propagation is the natural slice 6.
