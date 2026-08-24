# Command Task C046: Implement `velnorctl storage gc`

> **Executor instructions**: Implement only `velnorctl storage gc`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/storage.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 067, 075
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Plan safe garbage collection and execute only with explicit confirmation.

## Current state

Old `storage` and `cache` namespaces print directly; GC includes a lease-check bypass. Plan 075 provides canonical typed storage services.

## Scope

Implement only `velnorctl storage gc`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/storage_gc.rs` and `crates/velnorctl/tests/storage_gc.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply mutation rules: explicit authority, timeout/reason, dry-run/confirmation where specified, audit record, warnings on stderr.

## Required behavior

- Exact mutation surface is dry-run by default, `--target-free <bytes>`,
  `--class <class>`, global `-o json`, and
  `--yes --plan-id <id> --reason <text>` for execution.
- Acquire GC leader/filesystem locks, honor active leases, revalidate ownership/
  resource versions, show expected physical reclaim, and audit outcome. After
  each deletion remeasure actual free blocks/inodes; owned builder cleanup uses
  its dedicated builder path. Record history atomically.
- Never expose force/lease bypass or call broad Docker prune.

## Steps

1. Add exact typed Clap shape for `velnorctl storage gc`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c046`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c046` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
During fresh hold prove active candidate refused; after teardown collect seeded owned candidate, verify bytes/history/idempotency and foreign preservation.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl storage gc --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
