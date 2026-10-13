# Plan 043: Job lifecycle latency — async finalization, container pre-create, JIT overlap

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/executor.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P1 (V1.9 + V1.10 + V1.12; measured basis
  `docs/p3-performance-design-2026-06-11.md` ranks 6, 7, 9)
- **Effort**: L
- **Risk**: HIGH (completion ordering touches the never-abandon-a-job
  invariant from plan 011)
- **Depends on**: none hard; AFTER 036 if both in flight (completion path
  contention)
- **Category**: perf
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Measured (p3 doc): 2–8 s typical, 30–34 s contended trailing gap between the
last step and job completion because ALL teardown happens before completion
is posted; 2–4 s/job container boot serialized after checkout; 1–3 s/job JIT
re-registration serialized after completion. Combined these dominate the
per-job overhead on warm runs — exactly what the §2.11 budgets
(pickup→first-step ≤ 5 s, teardown-visible ≤ 2 s) target.

## Current state (verified by the lifecycle audit; re-locate by symbol)

- Ordering today (`handle_job_request`, `runner.rs:2288+`): `spawn_blocking
  (execute_script_job)` at `:2388`; INSIDE it — steps run, then
  `cleanup(container)` (`executor.rs:2618`: docker rm container → rm
  services → network rm) via `execute_ordered_steps_with_completion`
  (`executor.rs:941`, cleanup at `:953`), then workdir
  `fs::remove_dir_all(&job_dir)` (`runner.rs:3495`). AFTER the blocking call
  returns: log/timeline publishers drained (`:2426-2433`), completion posted
  `complete_run_service_job_refreshing` (`:2480` call), renewal aborted
  (`:2494` — comment `:2422`: renewal MUST live until completion accepted).
- Completion body: `complete_run_service_job` `runner.rs:5159` — timeline
  logs (best-effort) → Results-Service job-log blob (`:5178`) → job-log.txt
  artifact fallback (`:5179`) → `complete_job` POST (`:5246`).
- Container create/start: `start_job_environment_once` `executor.rs:2648`
  (network create `:2678`, services, job container `:2683`), invoked lazily
  from the step executor; spec built `runner.rs:3673` AFTER host-side eager
  checkout (`runner.rs:3592-3640`).
- Per-job network: `velnor-net-<job_id>` (`github_adapter.rs:318-319`);
  startup pruner `prune_stale_velnor_docker_resources` (`runner.rs:1318`).
- Slot recycle: `recycle_daemon_slot` `runner.rs:1050` — delete JIT config →
  `configure()` fresh (`:1073`) — serialized after job completion.
- Invariants that MUST survive (from landed plans 010/011): hung-step
  timeout kills still complete the job; panic/setup failure still completes
  the job; renewal lives until completion accepted.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner lifecycle` | new tests pass |

## Scope

**In scope**: `runner.rs` (`handle_job_request`, `execute_script_job`,
recycle path), `executor.rs` (cleanup split, start seam), tests.
**Out of scope**: git mirror + reflink (plan 044), timing/SLO surfacing
(plan 045), log pipeline internals (p3 #10 — landed as plan 020), protocol
changes.

## Git workflow

Branch `velnor-estate-standard`; one commit per sub-feature (`perf(runner):
async finalization`, `... container pre-create`, `... jit overlap`),
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: Async finalization (V1.9) — complete BEFORE teardown

Split `cleanup` out of the blocking job path: `execute_script_job` returns
with the container/network/workdir STILL PRESENT plus a `TeardownHandle`
(names/paths to remove). `handle_job_request` then: drain publishers → post
completion (unchanged) → abort renewal → THEN run teardown (docker rm,
network rm, workdir removal) — either inline after completion or on a
detached tokio task whose failure only logs (the startup pruner
`runner.rs:1318` is the safety net for crashes mid-teardown).
The never-abandon invariant is strengthened, not weakened: completion no
longer waits on Docker teardown latency.

**Verify**: reorder-sensitive tests — extend the RecordingRunner executor
harness to assert call order: `complete` recorded BEFORE `docker rm`
(model `starts_and_waits_for_service_before_job_container`,
`executor.rs:8552`); plan-011's panic-completion tests still green; plan-019
drain tests still green.

### Step 2: Container pre-create at acquire (V1.10)

At `handle_v2_message` after `acquire_job` succeeds (`runner.rs:2203+`):
spawn the network-create + job-container create/start (`start_job_environment`
seam, `executor.rs:2636/2648`) concurrently with the host-side eager checkout
(`runner.rs:3592`). The executor's lazy `start_job_environment_once` then
finds it started (make `_once` join the pre-created handle instead of
re-running). Failure of the pre-create falls back to the lazy path (log,
don't fail the job early). Service containers stay in the lazy path
(they may depend on job env not yet known? — verify: `service_containers`
builds from the job message only (`github_adapter.rs:357`), so services CAN
pre-create too; do it if the spec is available, else leave lazy and note).

**Verify**: harness test `container_boot_overlaps_checkout` (order:
network/create recorded before checkout completes — simulate slow checkout
via the recording layer); fallback test (pre-create fails → lazy path runs →
job green).

### Step 3: JIT overlap (V1.12)

In `run_daemon_slot`/`recycle_daemon_slot`: kick the next JIT
`configure()` concurrently with the tail of completion (after `complete_job`
POST accepted, during teardown) instead of strictly after. Guard: the fresh
JIT must not register while the old runner identity is still working the job
(GitHub sees one runner name) — overlap only the teardown window, not the
completion window; keep delete-old-config → create-new ordering
(`runner.rs:1062-1073`).

**Verify**: daemon test (harness `tests/daemon_cli.rs` or unit-level on the
recycle fn) asserting configure starts without waiting for teardown-complete;
zombie-prevention tests (registry reconciliation, `runner.rs:1837+`) green.

### Step 4: Measure

Add `forensics.lifecycle` timestamps around: last-step-end,
completion-posted, teardown-done, next-jit-ready (these feed plan 045's
SLOs). Run the full suite.

**Verify**: log lines present in harness runs; suite green.

## Test plan

≥ 5 new order-assertion tests; ALL existing lifecycle/drain/panic tests
green (they are the invariant net). Operator acceptance: warm fixture run —
trailing gap (last step → completed on GitHub UI) ≤ 2 s median, teardown no
longer visible in wall time; record before/after in the PR.

## Done criteria

- [ ] Gates exit 0; new tests pass; plans 010/011/019 test sets green
- [ ] Completion provably posted before docker/network/workdir teardown
- [ ] Pre-create overlap works with lazy fallback
- [ ] JIT overlap bounded to the teardown window
- [ ] No out-of-scope changes

## STOP conditions

- Any existing never-abandon/drain test fails and the fix isn't obvious →
  STOP (this is the exact regression class the plan must not introduce).
- Pre-created container leaks when the job message is subsequently rejected
  by capability validation (033 runs BEFORE acquire? verify — 033 wires at
  `runner.rs:2377` which is AFTER acquire at `:2203`) → pre-create must
  happen only after validation passes; re-order your hook to post-validation
  or STOP if the seams conflict.
- Docker API contention makes teardown-after-completion race the NEXT job's
  pre-create on the same slot → serialize per-slot teardown-then-precreate;
  if that erases the win, STOP with measurements.

## Maintenance notes

- The teardown pruner (`runner.rs:1318`) is now load-bearing for crash
  windows — never remove it.
- Plan 045 consumes the step-4 timestamps; keep field names stable.
