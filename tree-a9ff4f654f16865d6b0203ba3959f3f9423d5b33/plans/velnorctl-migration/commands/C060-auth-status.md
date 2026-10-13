# Command Task C060: Implement `velnorctl auth status`

> **Executor instructions**: Implement only `velnorctl auth status`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/protocol.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: HIGH
- **Depends on**: Plans 065, 066, 068
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Show credential source and cached/known authentication state without active mutation.

## Current state

GitHub credentials and endpoint access are validated indirectly during configuration/daemon operation. Plan 068 provides read-only credential-source and permission probes.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl auth status` in `crates/velnorctl/src/commands/auth_status.rs` and `crates/velnorctl/tests/auth_status.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Inspection/check behavior is read-only, versioned, redacted, and uses standard output/exit conventions.

## Required behavior

- Show source reference/type, last check, scopes/capabilities known, rate-limit snapshot, and remediation state.
- Read a daemon-owned sanitized auth-status record with credential-reference
  identity, checked-at/freshness, known scopes/capabilities, and rate-limit
  snapshot. No record is `NOT_CHECKED`, never inferred valid/invalid. Bound its
  retention under Plan 066 and never persist credential/token/header contents.
- Never print credential value, private key, signed URL, or authorization header.
- Distinguish unknown/not-checked from invalid.

## Steps

1. Add exact typed Clap syntax for `velnorctl auth status`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c060`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c060` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Use fixture context credential reference, inspect status before/after fresh success, and scan all outputs for injected canaries.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl auth status --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
