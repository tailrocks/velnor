# Plan 2026-09-14: decomposition slice 4 — post-action drain (GOAL 49/58)

Base: origin/main tip 2f8af349. Branch: r0-decomp-post. Worktree: /tmp/velnor-dec4.

## Problem

`crates/velnor-runner/src/executor.rs` is a 31.3k-line
responsibility-mixed module (GOAL 58: decompose giant modules where
structure enables defects). The post-action drain machinery — the
unified LIFO post stack (registration records for JavaScript, native,
and Docker posts), the registration gates (post-entrypoint presence,
native adapter conditions), the upstream-`TryPop` drain ordering with
per-entry post-condition selection, and the `Post <name>` record
helpers (display naming, timeline order reservation, log preludes) —
sat in `executor.rs` as five types plus seven free helpers, even
though the drain loop that consumes them is engine dispatch.

## Fix (this branch, strict scope)

New module `execution/post_drain.rs` (existing `execution/`
layout, private mod + targeted `pub(crate) use`, mirroring the
slice-1 `composite_scopes` / slice-2 `command_output` / slice-3
`step_conditions` precedent). Moved verbatim, bodies byte-identical:

- record types: `PostJavaScriptAction` + `new` (the post-entrypoint
  registration gate), `PostNativeAction`, `PostDockerAction`,
  `PostAction` + `condition` / `display_name` / `step_id` (test-only),
  `PostDrainItem` (`Run` / `ConditionFailed`);
- drain ordering: `drain_post_stack` (the `rev` + post-condition
  selection + `ConditionFailed`-in-position block); the one engine
  call site becomes
  `let post_actions = drain_post_stack(post_actions, &state);`
  (the block's inline comment becomes the function's doc comment,
  with one stale `above` pointer dropped);
- record helpers: `post_step_display_name`,
  `native_post_condition`, `reserve_github_post_step_orders`,
  `native_post_log_prelude` + `javascript_post_log_prelude` +
  `docker_post_log_prelude`.

Mechanical enablers only: `action_log_prelude` widens private →
`pub(crate)` for the moved preludes; moved fields/methods widen
private → `pub(crate)` for their staying engine/test callers; two
targeted `#[allow(dead_code)]` on record-provenance fields that are
write-only outside tests (`PostJavaScriptAction::umbrella_display`,
`PostDockerAction::step_id` — silent before only because
`executor.rs` carries a module-level `allow(dead_code)`).
Registration push sites, the drain execution loop, and the prelude
callers are byte-unmodified apart from the shared import. One unit
test moves with its code, body byte-identical
(`unified_post_stack_drain_benchmark`).

Explicitly NOT extracted: registration dispatch interleaving (push
sites, `post_registered` flags, pre-ran gates, `composite_frames`
umbrella plumbing — decided inline with main-step dispatch), the
drain execution loop (JS/Docker/native execution, `ConditionFailed`
record emission, consecutive-native grouping + combined merge,
cancellation swap, timeline/emission plumbing),
`execute_native_post_action`, `trailing_post_action_count`,
`JobExecutionState::apply` / `apply_cancelled` + the
outcomes/conclusions maps where post results merge into job status
(the generic outcome-tracking domain, i.e. the command-state
application candidate below), `runner.rs`'s separate
`post_step_display_name`. No semantic change anywhere: no redesign,
no behavior delta.

## Tests

- `cargo fmt -p velnor-runner -- --check`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- Moved test `cargo test -p velnor-runner --lib --
  execution::post_drain`: 1 passed, 0 failed.
- `cargo test -p velnor-runner --lib -- executor::`: 297 passed,
  0 failed (slice-3 baseline 298 minus the 1 moved test).
- Post-targeted filters: `post` 22 passed, `drain` 15 passed,
  0 failed.
- Full `cargo test -p velnor-runner --no-fail-fast`: lib 1990–1991
  passed with only the known parallel lock/lease flakes (first
  branch run: 3 checkout/git_mirror + 1 runner job-claim; second
  branch run: the identical 3 as the unmodified base, see below);
  all other targets green (bins, every integration suite
  including `idle_scaling` 4/4, doc).

## Pre-existing flakes (not caused by, not fixed by this branch)

`checkout::tests::checkout_emits_the_four_bench_phase_spans`,
`checkout_releases_mirror_reader_lease_after_hydration`,
`git_mirror::tests::checkout_reader_lease_blocks_mirror_repair` —
lock/lease contention under full-suite parallelism; the unmodified
base (`git stash -u` + same full lib suite) fails the identical 3
with identical counts (1991 passed / 3 failed / 2 ignored).
`runner::tests::job_claim_excludes_duplicate_slots_until_owner_drops`
failed once on the branch (same lock-contention family as slice 2's
`configure_lock_excludes_concurrent_transactions`), passes in
isolation 2/2, and passes on the branch's second full run —
run-to-run churn, not this change (untouched file, untouched
domain). None touch post drain.

## Next candidate domains (untouched, observed during this work)

1. Command-state application half (`executor.rs`:
   `JobExecutionState::apply` outputs/state/env/path/masks block,
   `apply_cancelled`, `secret_masks`, `step_debug`, the
   outcomes/conclusions maps where post results merge into job
   status) — named by slices 2 and 3; this slice's drain loop is
   its biggest remaining caller, so the application-vs-tracking
   split is now the natural slice 5.
2. `CompositeFrame` execution-frame domain (struct +
   `umbrella_result`/`merge_nested`/`absorb`/`into_step_log`, the
   `composite_frames` stack, `CompositeStart`/`CompositeEnd`
   handling, defensive flush) — still the largest coherent
   timeline/log candidate; named by the pilot, untouched here.
