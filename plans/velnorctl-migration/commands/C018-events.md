# Command Task C018: Implement `velnorctl events`

> **Executor instructions**: Implement only `velnorctl events`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/telemetry.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plans 066, 067, 071
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Query/watch normalized lifecycle and health events with specialized filters.

## Current state

Lifecycle/timing data exists in daemon logs and timing records, but no stable event, metrics, or condition-wait command exists. Plan 071 provides shared services.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl events` in `crates/velnorctl/src/commands/events.rs` and `crates/velnorctl/tests/events.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Support watch, type, reason, object target, since, and JSONL.
- Include exact reasons: InstanceReady, InstanceDegraded, DrainRequested,
  DrainCompleted, SlotConfigured, SlotReady, SlotParked, SlotRecycled,
  RegistrationMissing, RegistrationOffline, RegistrationStaleBusy, JobAcquired,
  JobWaitingForCapacity, JobStarted, JobCompleted, JobCanceled, JobRejected,
  CapacityPressure, GarbageCollectionStarted, and GarbageCollectionCompleted.
- Warnings are data records, not log-grep approximations.

## Steps

1. Add exact typed Clap shape for `velnorctl events`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c018`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c018` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Run fresh hold/cancel/recycle fixture sequence; verify reason/type/object filters and ordered watch through teardown.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl events --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
