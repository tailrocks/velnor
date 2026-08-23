# Command Task C047: Implement `velnorctl storage history`

> **Executor instructions**: Implement only `velnorctl storage history`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/storage.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 066, 067, 075
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

List pressure and GC plan/result history.

## Current state

Old `storage` and `cache` namespaces print directly; GC includes a lease-check bypass. Plan 075 provides canonical typed storage services.

## Scope

Implement only `velnorctl storage history`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/storage_history.rs` and `crates/velnorctl/tests/storage_history.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Support limit, since, class, result, and standard output.
- Show operator reason, selected/removed/refused objects, expected/actual physical bytes, and partial errors.
- Read-only and ordered from authoritative catalog.

## Steps

1. Add exact typed Clap shape for `velnorctl storage history`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c047`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c047` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Execute isolated dry-run and successful GC after fixture job; prove history contains exact correlated records and no secret paths.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl storage history --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.

