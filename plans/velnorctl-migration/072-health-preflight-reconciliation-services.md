# Plan 072: Separate health, preflight, and reconciliation services

> **Executor instructions**: Extract services only. Command tasks C021–C026
> own doctor, preflight, and each reconciliation target.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/preflight.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 066–071
- **Category**: migration, correctness
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current doctor can fail-close stale jobs and cancel overdue workflow runs. Safe
inspection requires a read-only check graph; repairs require explicit typed,
dry-run-first reconciliation plans.

## Scope

- read-only host, Docker, GitHub, fleet, and storage check graph; host checks
  may report Debian package metadata, dpkg file integrity, and unit consistency
  without creating a Velnor package-version domain
- existing Docker execution preflight without runner registration
- runner, job, Docker, and storage reconciliation planners/executors
- idempotency, ownership proof, audit records, and partial failure

No CLI handlers, broad Docker prune, local kill presented as GitHub cancel, or
new repair target. No release target, package mutation, activation pointer, or
rollback service belongs in doctor or reconciliation.

## Steps

1. Extract preflight checks and typed reports from current implementation.
2. Build doctor checks behind read-only ports; add mutation spies proving zero
   write/delete/cancel/restart calls.
3. Move stale-job completion and overdue-run cancellation out of doctor into
   job reconciliation.
4. Build plan-first reconciliation for runners/jobs/Docker/storage. Execution
   requires explicit confirmation/reason at command layer and revalidates
   ownership/leases immediately before mutation.
5. Test idempotency, partial failure, active-job/lease refusal, foreign-resource
   preservation, and repeated dry runs.

**Verify**: focused nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Use pinned `tailrocks/velnor-actions-fixture`. Clean old runs/registrations,
preflight, dispatch fresh hold, prove health reads do not mutate, then seed exact
validation-owned stale state and prove reconciliation plan/execution/idempotency.
Monitor only new run ID every at most 60 seconds.

## Done criteria

- [ ] Doctor services are mutation-free.
- [ ] Four repair services are dry-run-plan-first and ownership-safe.
- [ ] C021–C026 can remain thin command adapters.

## STOP conditions

Stop when ownership is ambiguous or repair would require unrestricted pruning,
capability expansion, or killing a job without GitHub cancellation authority.
