# Command Task C009: Implement `velnorctl get runners`

> **Executor instructions**: Implement only `velnorctl get runners`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/config.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 065–069
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Merge local config, live slot, and GitHub registry views for ephemeral JIT registrations.

## Current state

Required state is split across runner config, slot loops, in-flight records, systemd, Docker, storage records, logs, and GitHub. Plan 069 provides typed read projections.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl get runners` in `crates/velnorctl/src/commands/get_runners.rs` and `crates/velnorctl/tests/get_runners.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Show name, stable slot, GitHub ID, online/offline status, busy, local/registry presence, and age.
- Detect missing remote/local identity, orphan, offline+busy, duplicate name, stale registration, credential exchange failure, label mismatch, and group mismatch.
- Read-only command reports reconciliation reason but performs no repair.

## Steps

1. Add exact typed Clap shape for `velnorctl get runners`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c009`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c009` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Seed validation-owned stale/missing/offline cases around fresh hold run; prove every condition appears and no registry mutation occurs.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl get runners --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.

