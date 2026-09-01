# Command Task C063: Implement `velnorctl instance install <name>`

> **Executor instructions**: Implement only `velnorctl instance install <name>`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/Cargo.toml crates/velnor-runner/debian crates/velnor-control crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 068, 076
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Materialize packaged systemd instance configuration from desired state.

## Current state

Old `configure` and `remove` own low-level runner files and registrations; packaged systemd owns daemon startup. Plan 068 extracts idempotent desired-instance operations.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl instance install <name>` in `crates/velnorctl/src/commands/instance_install.rs` and `crates/velnorctl/tests/instance_install.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Mutation follows plan-first/idempotent rules, explicit authorization/confirmation/reason where destructive, atomic writes, audit event, and safe rollback.

## Required behavior

- Architecture correction: installs instance unit/environment links from already signed-apt-installed package; never installs Velnor package/binary.
- Plan then mutate with authorization/confirmation; preserve unit hardening, user/group, watchdog, stop timeout, and protected credential paths.
- Do not start/acquire work; `instance apply` owns activation.
- Materialize exact `/etc/velnor/<name>.env` and the packaged instance-unit
  enablement link, then daemon-reload without starting. Never invoke `sudo`;
  use the authorized host adapter and fail `Authorization` when privilege is
  absent.

## Steps

1. Add exact typed Clap syntax for `velnorctl instance install <name>`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c063`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c063` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
In disposable systemd/package environment, install fixture instance, inspect unit bytes/permissions with no registration, then apply and run fresh success.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl instance install --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
