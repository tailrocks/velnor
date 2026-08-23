# Command Task C029: Implement `velnorctl drain instance/<name>`

> **Executor instructions**: Implement only `velnorctl drain instance/<name>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 067, 073
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Stop new work, finish active jobs, deregister idle/completed slots, and leave instance drained/stopped.

## Current state

Current lifecycle is signal/global-state driven and dynamic control is absent. Plan 073 provides one crash-safe per-instance lifecycle engine.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl drain instance/<name>` in `crates/velnorctl/src/commands/drain.rs` and `crates/velnorctl/tests/drain.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global mutation conventions: dry-run where specified, explicit confirmation/force/reason, timeout, authorization, audit event, warnings on stderr.

## Required behavior

- Support timeout, wait, progress, and explicit emergency `--force-after-timeout --reason`.
- Normal drain never kills job container; systemd stop timeout remains outer bound.
- On timeout, durable drain intent remains active and the command returns timeout
  without killing work. `--force-after-timeout` requests GitHub cancellation and
  observes broker/terminal transition before teardown; unobserved local
  termination is recorded infrastructure failure, never successful drain.
- Persist DrainRequested/DrainCompleted and per-slot progress.

## Steps

1. Add exact typed Clap shape for `velnorctl drain instance/<name>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c029`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c029` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Drain during fresh fixture hold, prove job completes normally and slots deregister; test explicit timeout without force and separate audited emergency case.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl drain --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
