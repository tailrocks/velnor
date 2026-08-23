# Command Task C042: Implement `velnorctl run open <run-id>`

> **Executor instructions**: Implement only `velnorctl run open <run-id>`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Open or print canonical GitHub run URL.

## Current state

GitHub workflow reads/mutations currently require `gh` or maintainer helpers. Plan 074 provides GitHub-authoritative typed operations and local placement/timing correlation.

## Scope

Implement only `velnorctl run open <run-id>`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/run_open.rs` and `crates/velnorctl/tests/run_open.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Resolve exact run/repository/context and canonical HTTPS URL.
- Attempt browser only when environment supports it; otherwise print URL.
- Browser launch failure must be explicit but never change GitHub run conclusion.

## Steps

1. Add exact typed Clap shape for `velnorctl run open <run-id>`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c042`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c042` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Dispatch fresh success, run `open` with browser disabled, verify printed URL resolves exact run and contains no authorization query.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl run open --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
