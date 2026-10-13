# Plan 2026-09-13: lifecycle unification steps 1-3 (low-risk)

Base: origin/main tip eb8d3349. Branch: r0-lifecycle-low. Worktree: /tmp/velnor-life.

## Map (verified, read-only)

The journal reducer (`journal.rs::reduce`, `ActorPhase` + record booleans) is
the real control flow. Store `JobState`/`SlotPhase` verdicts are discarded at
every runner call site (`sink.transition`/`transition_slot` return values
ignored; `DurableSlotLifecycle::transition` behind `let _`). Dead weight:

- `ActorPhase::{Starting, Retiring, Degraded, Quarantined}`: never assigned by
  the reducer, only matched (occupancy checks) and parsed.
- `Event::Assigned` + `accept_job`: sole production emitter is `accept_job`,
  which has no production callers (daemon path uses intend/resolve/confirm).
- `SideEffect::StartJob` + its `execute_effect` arm: only pushed by `JobOwned`;
  the daemon path discards `confirm_acquisition` commands and the controller
  never applies `JobOwned`, so the arm is unreachable.
- `Event::JobStarted` is emitted only by the fleet worker (`node/job.rs`); the
  daemon path (`handle_job_request`) never emits it, so daemon jobs sit in
  `Assigned` forever and `Running` is unreal for them.

## Fix (this branch, strict scope)

1. `runner.rs::handle_job_request` emits journal `JobStarted` (new
   `node::complete::record_job_started`, via
   `mark_run_service_job_started_in_journal`) right after the store `JobStarted`
   edge. Deliberately not fatal, mirroring `record_terminal_result`: the store
   edge is the execution record; a missing journal hint must not fail an
   acquired, reserved job. No-op/dry-run paths untouched.
2. Verdicts checked and logged, no behavior change: new
   `record_job_transition` wrapper routes all 6 `sink.transition` sites and
   logs `forensics.lifecycle event=job-transition-rejected` on refusal;
   `DurableSlotLifecycle::transition` logs
   `forensics.ops event=slot-transition-not-applied` when `transition_slot`
   reports not-applied (covers all 7 slot sites; return semantics unchanged).
3. Deletions: 4 dead `ActorPhase` variants (`ALL` 12->8; retired phase strings
   now fail closed in `parse_actor_phase`); `Event::Assigned` (+reducer,
   `event_generation`, `event_kind` arms); `accept_job`;
   `SideEffect::StartJob` (+`JobOwned` push, `execute_effect` arm and its
   now-unused `jobs` param); `Starting` removed from all 6 occupancy matches
   (`job_occupies_slot`, 4 controller sites, `infer_slot_id`); guardian keeps
   a Fenced-only check. Tests migrated to intend/resolve/confirm; `Assigned`
   the *phase* stays (set only by `JobAcquisitionIntended`).

NOT attempted (steps 4-6, separate work): SlotPhase2/JobPhase2 type split,
projection conversion, drain unification.

## Compatibility

Old journals containing `assigned` events or retired phase strings fail
closed on load (`journal.event.invalid` / `journal.materialized.invalid`),
same stance as the existing legacy-schema guards. No production writer has
emitted `Assigned` (no `accept_job` callers), so live fleets should not hold
such rows; daemon journals created by older binaries predate the acquisition
API and may need a fresh state dir on upgrade.

## Verification

- `cargo fmt --all`; `cargo clippy -p velnor-model -p velnor-control
  -p velnor-runner --all-targets -- -D warnings`: clean.
- New: `retired_actor_phases_fail_closed_on_materialization` (control),
  `record_job_transition_reports_rejected_edges` (runner),
  `daemon_acquisition_path_marks_job_running_at_start` (node_arch).
- Suites: model 143, control lib 238 + integration 33, node 137, node_arch 23,
  other runner integration 10: all pass.
- Full runner lib under load: 4 timing/lock flakes in untouched
  checkout/git_mirror/job-claim tests; same tests fail on pristine
  origin/main, and all pass in isolation. Pre-existing, not a regression.
