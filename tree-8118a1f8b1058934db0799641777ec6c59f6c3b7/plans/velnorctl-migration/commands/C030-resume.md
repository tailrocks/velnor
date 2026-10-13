# Command Task C030: Implement `velnorctl resume instance/<name>`

> **Executor instructions**: Implement only `velnorctl resume instance/<name>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 067, 068, 073
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Start stopped service, clear drained/cordoned intent, recreate JIT slots, and report readiness.

## Current state

Current lifecycle is signal/global-state driven and dynamic control is absent. Plan 073 provides one crash-safe per-instance lifecycle engine.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl resume instance/<name>` in `crates/velnorctl/src/commands/resume.rs` and `crates/velnorctl/tests/resume.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global mutation conventions: dry-run where specified, explicit confirmation/force/reason, timeout, authorization, audit event, warnings on stderr.

## Required behavior

- Wait for configured usable slots; report degraded partial startup.
- Avoid duplicate registrations and preserve instance config.
- Idempotent when already running/ready.
- When the daemon socket is absent because the instance is stopped, use Plan
  068's exact authorized systemd host adapter to start that instance, reconnect
  to the new Plan 073 daemon generation, then honor optional `--wait` and global
  timeout. Never use `sudo` or infer that socket absence means success.

## Steps

1. Add exact typed Clap shape for `velnorctl resume instance/<name>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c030`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c030` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
Test missing socket, systemd authorization/start failure, reconnect, partial slot
registration, and already-ready idempotency before fresh success.
Resume instance drained by fresh fixture sequence, wait Ready, dispatch success, and prove desired slot count plus registration uniqueness.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl resume --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
