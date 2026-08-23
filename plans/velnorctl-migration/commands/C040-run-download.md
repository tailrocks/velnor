# Command Task C040: Implement `velnorctl run download <run-id>`

> **Executor instructions**: Implement only `velnorctl run download <run-id>`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plan 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Download selected GitHub run artifacts safely.

## Current state

GitHub workflow reads/mutations currently require `gh` or maintainer helpers. Plan 074 provides GitHub-authoritative typed operations and local placement/timing correlation.

## Scope

Implement only `velnorctl run download <run-id>`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/run_download.rs` and `crates/velnorctl/tests/run_download.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- List/select artifacts, preserve artifact boundaries, and validate destination/overwrite policy.
- Reject traversal, absolute paths, unsafe symlinks, duplicate collisions, corrupt archives, and expired URLs.
- Download is GitHub-sourced; local fallback is not product authority.

## Steps

1. Add exact typed Clap shape for `velnorctl run download <run-id>`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c040`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c040` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all pending/in-progress old fixture runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Produce multiple fixture artifacts, download exact new run, verify bytes/names/boundaries, and run hostile archive test through mock service.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl run download --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
