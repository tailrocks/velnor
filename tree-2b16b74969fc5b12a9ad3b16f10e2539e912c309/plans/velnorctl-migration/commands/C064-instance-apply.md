# Command Task C064: Implement `velnorctl instance apply <name>`

> **Executor instructions**: Implement only `velnorctl instance apply <name>`. Do not combine
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

Reconcile desired instance configuration into live daemon, systemd, and GitHub registration state.

## Current state

Old `configure` and `remove` own low-level runner files and registrations; packaged systemd owns daemon startup. Plan 068 extracts idempotent desired-instance operations.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl instance apply <name>` in `crates/velnorctl/src/commands/instance_apply.rs` and `crates/velnorctl/tests/instance_apply.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Mutation follows plan-first/idempotent rules, explicit authorization/confirmation/reason where destructive, atomic writes, audit event, and safe rollback.

## Required behavior

- Compute/show plan first; perform safe drain when live changes require restart; preserve active jobs.
- Default is immutable dry-run plan; mutation requires `--yes --plan-id <id>
  --reason <text>`. Persist phase journal and preconditions, enforce drain
  deadline without killing active work, reject drift, and define recovery or
  rollback for every config/unit/GitHub partial-failure boundary.
- Apply config/unit/live state idempotently, create desired JIT slots, wait readiness, and report partial failure/drift.
- Never silently widen trusted admission, labels, secrets, or capabilities.

## Steps

1. Add exact typed Clap syntax for `velnorctl instance apply <name>`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c064`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c064` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
Apply new fixture instance, run fresh success, reapply with empty plan, then
change slots 1→2→1 during concurrent hold and prove held work survives,
phase-journal recovery works, and readiness/drift converge.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl instance apply --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
