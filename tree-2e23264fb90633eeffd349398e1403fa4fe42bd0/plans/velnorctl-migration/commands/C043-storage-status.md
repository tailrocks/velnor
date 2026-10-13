# Command Task C043: Implement `velnorctl storage status`

> **Executor instructions**: Implement only `velnorctl storage status`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/storage.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plans 067, 075
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Show canonical storage classes, logical/physical bytes, budgets, pressure, free reserve, reservations, and leases.

## Current state

Old `storage` and `cache` namespaces print directly; GC includes a lease-check bypass. Plan 075 provides canonical typed storage services.

## Scope

Implement only `velnorctl storage status`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/storage_status.rs` and `crates/velnorctl/tests/storage_status.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Render path/class/logical/physical/budget/pressure plus host bytes/inodes,
  percent and absolute thresholds, emergency reserve, active reservations and
  leases, oldest use, last GC/history, unowned paths, source time, and staleness.
- Distinguish unavailable/unknown accounting from zero.
- Read-only and safe during active GC/jobs; observation never reaps state.

## Steps

1. Add exact typed Clap shape for `velnorctl storage status`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c043`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c043` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
During fresh cache-using hold job, verify status matches isolated filesystem/accounting and active reservation/lease counts.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl storage status --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
