# Command Task C037: Implement `velnorctl run cancel <run-id>`

> **Executor instructions**: Implement only `velnorctl run cancel <run-id>`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 070, 073, 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Request cancellation from GitHub and observe broker-driven local termination.

## Current state

GitHub workflow reads/mutations currently require `gh` or maintainer helpers. Plan 074 provides GitHub-authoritative typed operations and local placement/timing correlation.

## Scope

Implement only `velnorctl run cancel <run-id>`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/run_cancel.rs` and `crates/velnorctl/tests/run_cancel.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply mutation rules: explicit authority, timeout/reason, dry-run/confirmation where specified, audit record, warnings on stderr.

## Required behavior

- Send cancellation only to GitHub; never treat local Docker kill as successful cancellation.
- Wait/report GitHub terminal state and correlated local teardown within timeout.
- Repeat cancel is idempotent only when GitHub authoritatively reports
  `cancelled`; already success/failure is Conflict, not cancel success. Never
  blindly retry POST after ambiguous transport loss. No undocumented
  force-cancel surface is authorized.

## Steps

1. Add exact typed Clap shape for `velnorctl run cancel <run-id>`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c037`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c037` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Dispatch fresh hold, cancel through command, prove GitHub `cancelled`, local broker cancellation/teardown, no orphan registration/container.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl run cancel --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
