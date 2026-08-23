# Command Task C058: Implement `velnorctl context set <name> --endpoint <uri>`

> **Executor instructions**: Implement only `velnorctl context set <name> --endpoint <uri>`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 065, 068
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Create or update context metadata and endpoint reference.

## Current state

No stable `velnorctl` context file or current-context selection exists. Plan 068 defines context records and protected credential references.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl context set <name> --endpoint <uri>` in `crates/velnorctl/src/commands/context_set.rs` and `crates/velnorctl/tests/context_set.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Mutation follows plan-first/idempotent rules, explicit authorization/confirmation/reason where destructive, atomic writes, audit event, and safe rollback.

## Required behavior

- Through Plan 079 accept only a canonical absolute Unix instance-directory
  endpoint such as `unix:///run/velnor/<instance>`. Reject
  HTTPS, userinfo, query, fragment, dot components, symlink-unsafe
  normalization, and non-absolute paths with a stable unsupported/usage error.
  Plan 080 extends this same command only after authenticated remote transport
  and explicit security approval exist.
- Store atomically at `~/.config/velnor/config.toml`; never accept inline PAT/private-key contents.
- Do not switch current context unless explicitly requested by existing command contract.

## Steps

1. Add exact typed Clap syntax for `velnorctl context set <name> --endpoint <uri>`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c058`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c058` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Set a dedicated Unix fixture context and run fresh success through it. Prove an
HTTPS endpoint is rejected and leaves the context file byte-identical with no
credential value in file/output.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl context set --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
