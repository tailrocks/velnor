# Plan 2026-09-14: lifecycle unification step 5 (store projections + admission gate)

Base: origin/main tip 2e320bd5. Branch: r0-lifecycle-s5. Worktree: /tmp/velnor-life5.

## Fix (this branch, strict scope)

Convert the store `JobState`/`SlotPhase` transition writes to
projections; the tables stay as materialized views on those paths
(`record_job` still seeds `phase` verbatim out of band).

- control/store: new read functions `project_job_state` (phase cell ->
  `JobState`, fail-closed `store.job.state.unknown`),
  `project_job_transition` (`(from, reason) -> target`, else
  `IllegalJobEdge`), `project_slot_phase` (`&SlotRow -> SlotPhase`),
  `project_slot_transition` (`(from, target) -> target`, else
  `IllegalSlotEdge`). The job/slot *transition* write paths materialize
  exactly what the projections return; `decode_summary_row` projects
  through `project_job_state` too, so readers and transition writers
  share one seam. `record_job` is the out-of-band seeding seam: it
  writes `phase` verbatim with no edge check (pre-existing wart,
  preserved not introduced).
- model: `transition_target` / `slot_transition_allowed` kept as the
  projection asserts (cheap: pure, total); doc comments recast, no
  signature or behavior change.
- No silent illegal-edge drops: a refused edge bumps the new
  `Store::illegal_transition_edges` counter and still fails closed with
  the same `CONFLICT` envelopes (`store.job.transition.illegal`,
  `store.slot.transition.illegal`, same remediations, nothing written).
  Runner forensics lines from steps 1-3 remain the log half.
- Admission gate (admitted-yet-undecodable stuck-row fix): `record_job`
  rejects rows missing `run_id`/`attempt`
  (`store.job.summary.unidentified`, like `insert_summary`), carrying
  negatives, or carrying an attempt above `u32::MAX`
  (`store.job.summary.range`). Such rows stay readable via
  `job_summaries` and drivable via `record_job_transition`; only the
  `decode_summary_row` paths (`fetch_summary*`) break on them. The gate
  covers new admissions and refreshes (an update cannot NULL out a
  healthy row's identity). No backfill sweep for pre-existing rows: a
  sweep is unsafe — their external identity cannot be invented
  (invented values corrupt external (GitHub) correlation, and
  colliding triples yield store.job.summary.ambiguous in
  fetch_summary), and deleting them would destroy
  legitimate records that remain readable and drivable.

NOT attempted (step 6, separate work): drain unification.

## Compatibility

Same observable states and error envelopes. Single-step-from-materialized
semantics preserved (the concurrent-daemon test re-admits `queued` before
every edge and still applies). No schema change.

## Verification

- `cargo fmt --all --check`: clean.
- `cargo clippy -p velnor-model -p velnor-control --all-targets -- -D
  warnings`: clean. Runner `--all-targets` has 6 pre-existing errors in
  `execution/command_output.rs` test code (new file from #772, untouched
  here); identical 6 on pristine base 2e320bd5. Left untouched.
- New: `projections_match_transition_tables_exhaustively` (7x17 job +
  11x11 slot matrices vs the tables), `..._materialized_columns_equal_
  projections_and_illegal_edges_count`, `admission_gate_rejects_rows_
  without_usable_identity`, `admission_gate_rejects_attempt_above_u32_
  max` (oversized-attempt rejection + `u32::MAX` decode round-trip;
  control).
- Suites: model 129+4+6+4, control lib 244 + integration 33
  (retention 8, store_integration 18, summary_corpus 7), runner
  node_arch 23 + other integration 10, runner ops 34, node 138: all pass.
- Full runner lib: 1965 pass, 5 flake in untouched
  checkout/executor/git_mirror/conformance tests; same family fails on
  pristine base (verified identical 5-test set), passes in isolation.
  Pre-existing, not a regression.
