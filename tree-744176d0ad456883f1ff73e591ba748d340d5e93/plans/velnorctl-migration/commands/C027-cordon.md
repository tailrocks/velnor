# Command Task C027: Implement `velnorctl cordon instance/<name>`

> **Executor instructions**: Implement only `velnorctl cordon instance/<name>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 067, 073
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Keep daemon/API alive while stopping new acquisition after slots become idle.

## Current state

Current lifecycle is signal/global-state driven and dynamic control is absent. Plan 073 provides one crash-safe per-instance lifecycle engine.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl cordon instance/<name>` in `crates/velnorctl/src/commands/cordon.rs` and `crates/velnorctl/tests/cordon.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global mutation conventions: dry-run where specified, explicit confirmation/force/reason, timeout, authorization, audit event, warnings on stderr.

## Required behavior

- Preserve active jobs and logs; persist explicit Cordoned state across daemon restart.
- Idle slots stop acquiring without pretending drained/stopped.
- Mutation requires authorization, timeout/reason conventions, and audit event.
- Exact mutation flag is `--reason <text>`; repeated cordon returns the same
  persisted desired generation without duplicate events/effects.

## Steps

1. Add exact typed Clap shape for `velnorctl cordon instance/<name>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c027`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c027` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
Use Plan 063's dedicated queue scenario/instance and prove no alternate runner
can claim the queued job; otherwise STOP. Cordon preserves active work, survives
restart, and keeps the queued job GitHub-owned.
Dispatch fresh hold plus queued fixture work, cordon instance, prove active finishes and queued stays queued, restart daemon, verify cordon persists.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl cordon --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
