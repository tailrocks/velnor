# Command Task C055: Implement `velnorctl context list`

> **Executor instructions**: Implement only `velnorctl context list`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 064, 065, 068
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

List saved contexts and current/default selection.

## Current state

No stable `velnorctl` context file or current-context selection exists. Plan 068 defines context records and protected credential references.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl context list` in `crates/velnorctl/src/commands/context_list.rs` and `crates/velnorctl/tests/context_list.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Inspection/check behavior is read-only, versioned, redacted, and uses standard output/exit conventions.

## Required behavior

- Show name, endpoint, default instance/repo/scope, output, timeout, credential reference, and current marker.
- Never resolve or print credential contents.
- Work offline from context file; report invalid entries distinctly.

## Steps

1. Add exact typed Clap syntax for `velnorctl context list`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c055`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c055` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Create local fixture context, dispatch fresh success through it, and prove list marks exact current context without secret data.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl context list --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.

