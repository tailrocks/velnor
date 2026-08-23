# Command Task C065: Implement `velnorctl instance delete <name>`

> **Executor instructions**: Implement only `velnorctl instance delete <name>`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/Cargo.toml crates/velnor-runner/debian crates/velnor-control crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 068, 072, 073
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Drain and remove one exact instance plus owned registrations/state while preserving credentials and shared data.

## Current state

Old `configure` and `remove` own low-level runner files and registrations; packaged systemd owns daemon startup. Plan 068 extracts idempotent desired-instance operations.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl instance delete <name>` in `crates/velnorctl/src/commands/instance_delete.rs` and `crates/velnorctl/tests/instance_delete.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Mutation follows plan-first/idempotent rules, explicit authorization/confirmation/reason where destructive, atomic writes, audit event, and safe rollback.

## Required behavior

- Default plan/dry-run; execution requires confirmation/reason and completed drain.
- Remove exact unit/config/runtime state and idle registrations owned by instance; refuse active/ambiguous ownership.
- Preserve credential files, shared caches, the installed Debian package, other
  instances, and GitHub runs.
- Retain a sanitized instance tombstone and operation audit under Plan 066
  retention. Refuse active jobs, active leases, foreign/ambiguous ownership, or
  incomplete registration identity; repeated completed delete is idempotent.

## Steps

1. Add exact typed Clap syntax for `velnorctl instance delete <name>`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c065`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c065` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
After fresh fixture success, prove active-job and ambiguous-ownership refusal,
then delete the drained dedicated instance. Prove exact registrations/unit/live
state gone, tombstone retained, credentials/shared caches/other instance
untouched, and cleanup failure is recoverable/idempotent.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl instance delete --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
