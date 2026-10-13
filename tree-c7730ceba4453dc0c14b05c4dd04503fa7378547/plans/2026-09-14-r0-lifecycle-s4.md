# Plan 2026-09-14: lifecycle unification step 4 (SlotPhase2/JobPhase2 split)

Base: origin/main tip 545bab27. Branch: r0-lifecycle-s4. Worktree: /tmp/velnor-life4.

## Fix (this branch, strict scope)

Split the shared `ActorPhase` enum into two types so invalid states are
unrepresentable; wire format unchanged (same snake_case strings).

- model: new `SlotPhase2` (Absent/Provisioning/Registered/Ready/Assigned/
  Fenced) + `JobPhase2` (Assigned/Running/Completing), each with `ALL`,
  `as_str`, serde snake_case. `SlotPhase2::counts_as_ready`;
  `JobPhase2::occupies_slot` (exhaustive match, total: every job row
  occupies). `ActorPhase` deleted.
- control: `SlotRecord.phase: SlotPhase2`, `JobRecord.phase: JobPhase2`;
  `job_occupies_slot` free fn deleted, call sites use the method;
  `parse_actor_phase` split into `parse_slot_phase`/`parse_job_phase`
  (cross-type strings fail closed); schema 7->8 with `migrate_v7_to_v8`
  (validates both tables' vocabularies inside the setup tx, stamps 8;
  conforming rows migrate untouched, foreign rows fail closed pre-stamp).
- runner: controller reconcile (4 occupancy sites -> `occupies_slot`;
  Completing checks typed), complete/guardian/job bridges, runner.rs
  completion gate, node_arch tests. `infer_slot_id` keeps
  Assigned|Running matches on the job type (excludes Completing by design).
- docs: rearch-plan `ActorPhase` mention updated.

NOT attempted (steps 5-6, separate work): projection conversion, drain
unification.

## Compatibility

Journals at v7 with conforming rows upgrade silently. A v7 journal whose
slot row says `running`/`completing` (or job row a slot-only phase) fails
closed at open with `journal.materialized.invalid`, version stamp untouched.

## Verification

- `cargo fmt --all --check`; `cargo clippy -p velnor-model -p
  velnor-control -p velnor-runner --all-targets -- -D warnings`: clean.
- New: `slot_and_job_phase_vocabularies_are_separated`,
  `v7_journal_migrates_phase_vocabulary_to_v8`,
  `v7_migration_fails_closed_on_cross_type_phase_rows` (control).
  Version pin test advanced 7->8.
- Suites: model 143, control lib 240 + integration 33, node lib 137,
  node_arch 23, other runner integration 10: all pass.
- Full runner lib under load: 3 flakes in untouched
  checkout/git_mirror/cleanup-lock tests; same family fails on pristine
  origin/main (545bab27), and each passes in isolation. Pre-existing.
