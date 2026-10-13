# Command Task C035: Implement `velnorctl run view <run-id>`

> **Executor instructions**: Implement only `velnorctl run view <run-id>`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plans 069–071, 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Show one run with jobs, steps, Velnor placement/timing/events, logs, and artifacts.

## Current state

GitHub workflow reads/mutations currently require `gh` or maintainer helpers. Plan 074 provides GitHub-authoritative typed operations and local placement/timing correlation.

## Scope

Implement only `velnorctl run view <run-id>`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/run_view.rs` and `crates/velnorctl/tests/run_view.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Support `--jobs`, `--log`, `--log-failed`, and global machine output.
- Merge GitHub run/job/step truth with local timing, infrastructure category, events, and availability.
- Handle attempts and unavailable local history explicitly.
- `--log`/`--log-failed` are bounded projections from the same Plan 070 service
  used by C039; they do not implement a second fetch/mask/follow path.

## Steps

1. Add exact typed Clap shape for `velnorctl run view <run-id>`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c035`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c035` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
View fresh success and controlled failure before/after completion; verify job/step/log/artifact/local placement fields.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl run view --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
