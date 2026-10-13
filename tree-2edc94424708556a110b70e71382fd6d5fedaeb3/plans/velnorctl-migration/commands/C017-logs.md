# Command Task C017: Implement `velnorctl logs <resource>/<name>`

> **Executor instructions**: Implement only `velnorctl logs <resource>/<name>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/config.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 067, 069, 070
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Provide first-class job, slot, and instance logs with active/local and completed/GitHub source selection.

## Current state

Required state is split across runner config, slot loops, in-flight records, systemd, Docker, storage records, logs, and GitHub. Plan 069 provides typed read projections.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl logs <resource>/<name>` in `crates/velnorctl/src/commands/logs.rs` and `crates/velnorctl/tests/logs.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Support follow, tail, since, timestamps, step, failed, source auto/local/github, text/jsonl, and stream selection.
- Active job streams masked local console; completed job uses native GitHub log then artifact fallback.
- Support job/lifecycle/broker/registry/daemon/trace/systemd streams and preserve log-format contract.

## Steps

1. Add exact typed Clap shape for `velnorctl logs <resource>/<name>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c017`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c017` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Use fresh hold/success/failure fixture runs; stream active output, select step/failed streams, fetch completed logs, compare both lanes, and scan for secrets/timestamp errors.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl logs --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
