# Command Task C049: Implement `velnorctl storage leases`

> **Executor instructions**: Implement only `velnorctl storage leases`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/storage.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: MED
- **Depends on**: Plans 067, 075
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Show detailed storage/cache leases and GC protection.

## Current state

Old `storage` and `cache` namespaces print directly; GC includes a lease-check bypass. Plan 075 provides canonical typed storage services.

## Scope

Implement only `velnorctl storage leases`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/storage_leases.rs` and `crates/velnorctl/tests/storage_leases.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Show object/class/scope/owner, acquired/renewed/expiry, state, and GC protection.
- Support active, class, job, since, and standard output filters.
- Read-only; expiry/reap belongs to reconcile storage.

## Steps

1. Add exact typed Clap shape for `velnorctl storage leases`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c049`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c049` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
During fresh cache job inspect active renewal, prove GC protection, then terminal release; check no renewal gap over allowed threshold.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl storage leases --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.

