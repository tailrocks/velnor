# Command Task C057: Implement `velnorctl context use <name>`

> **Executor instructions**: Implement only `velnorctl context use <name>`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: MED
- **Depends on**: Plan 068
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Atomically select an existing context as current.

## Current state

No stable `velnorctl` context file or current-context selection exists. Plan 068 defines context records and protected credential references.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl context use <name>` in `crates/velnorctl/src/commands/context_use.rs` and `crates/velnorctl/tests/context_use.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Mutation follows plan-first/idempotent rules, explicit authorization/confirmation/reason where destructive, atomic writes, audit event, and safe rollback.

## Required behavior

- Validate target exists and context file permissions/ownership are safe.
- Write atomically with lock; preserve unrelated contexts and credential references.
- Idempotent for already-current context; no daemon/GitHub mutation.

## Steps

1. Add exact typed Clap syntax for `velnorctl context use <name>`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c057`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c057` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Create two local fixture contexts, switch before fresh runs, and prove each run/inspection targets selected instance with no config loss.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl context use --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
