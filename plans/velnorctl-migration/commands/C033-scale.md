# Command Task C033: Implement `velnorctl scale instance/<name> --slots <count>`

> **Executor instructions**: Implement only `velnorctl scale instance/<name> --slots <count>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 067, 068, 073
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Dynamically change desired stable-slot count through control state.

## Current state

Current lifecycle is signal/global-state driven and dynamic control is absent. Plan 073 provides one crash-safe per-instance lifecycle engine.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl scale instance/<name> --slots <count>` in `crates/velnorctl/src/commands/scale.rs` and `crates/velnorctl/tests/scale.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global mutation conventions: dry-run where specified, explicit confirmation/force/reason, timeout, authorization, audit event, warnings on stderr.

## Required behavior

- Scale up preflights, creates slots/JIT registrations, reports each readiness.
- Scale down cordons highest-numbered excess slots, finishes jobs, deregisters, then commits desired count.
- Never silently rewrite env and restart; expose desired/observed divergence and support wait.
- Exact `--wait` uses global timeout. Validate manifest/configured min/max and
  host capacity before committing a new durable desired generation; represent
  each slot change as a replay-safe Plan 073 operation and recover after crash.

## Steps

1. Add exact typed Clap shape for `velnorctl scale instance/<name> --slots <count>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c033`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c033` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Use Plan 063 concurrent scenario with two simultaneous held jobs. Prove scale up
and 2→1 scale-down defer busy excess work, then converge after release without
duplicate registration or lost desired generation.
Scale dedicated fixture instance 1–2, run two concurrent jobs, scale 2–1 with one busy, and prove safe drain plus final desired/observed equality.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl scale --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
