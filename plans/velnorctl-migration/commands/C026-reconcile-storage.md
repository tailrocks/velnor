# Command Task C026: Implement `velnorctl reconcile storage`

> **Executor instructions**: Implement only `velnorctl reconcile storage`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/preflight.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 066, 067, 072, 075
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Plan and repair stale storage reservations, leases, ownership, accounting, and pressure state.

## Current state

Old doctor mixes diagnosis and mutation; preflight and repair logic are runner CLI internals. Plan 072 separates read-only checks, preflight, and typed reconciliation.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl reconcile storage` in `crates/velnorctl/src/commands/reconcile_storage.rs` and `crates/velnorctl/tests/reconcile_storage.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global mutation conventions: dry-run where specified, explicit confirmation/force/reason, timeout, authorization, audit event, warnings on stderr.

## Required behavior

- Default dry-run; execution requires confirmation/reason.
- Remove/expire only proven stale records, detect unowned paths, rebuild accounting, clear pressure only after recovery.
- Never delete ambiguous data or bypass active leases.

## Steps

1. Add exact typed Clap shape for `velnorctl reconcile storage`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c026`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c026` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Seed isolated stale reservation/lease/accounting beside fresh fixture cache use; prove active data preserved, stale state repaired, pressure truth restored.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl reconcile storage --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.

