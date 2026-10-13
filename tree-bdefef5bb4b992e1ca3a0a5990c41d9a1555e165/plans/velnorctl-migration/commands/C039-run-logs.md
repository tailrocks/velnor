# Command Task C039: Implement `velnorctl run logs <run-id>`

> **Executor instructions**: Implement only `velnorctl run logs <run-id>`. Keep sibling
> commands separate. Run every gate and update command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-control crates/velnorctl`
> Stop on incompatible drift; do not improvise around changed authority.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 070, 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Read logs for jobs in one workflow run through shared log service.

## Current state

GitHub workflow reads/mutations currently require `gh` or maintainer helpers. Plan 074 provides GitHub-authoritative typed operations and local placement/timing correlation.

## Scope

Implement only `velnorctl run logs <run-id>`: typed parser/arguments, thin handler, versioned output, errors, help/completion metadata, and tests in `crates/velnorctl/src/commands/run_logs.rs` and `crates/velnorctl/tests/run_logs.rs`.
Use shared services; never spawn old binary, parse sibling human output, or duplicate domain logic.
Apply inspection rules: standard output formats/filters where relevant, resource stdout, warnings stderr, useful non-zero exits.

## Required behavior

- Aggregate job logs by GitHub job/attempt while preserving boundaries and source.
- Support follow/failed/step/tail/since/source options. Structured records use
  global `-o jsonl`; `--raw` is the sole raw-text mode and is mutually exclusive
  with machine output. Preserve run/attempt/job/step/source and fallback labels,
  plus bounded reconnect/cursor gap/dedup semantics.
- Delegate all masking, active-local, completed-GitHub, and artifact fallback to Plan 070.

## Steps

1. Add exact typed Clap shape for `velnorctl run logs <run-id>`; reject unknown and sibling arguments.
2. Call shared typed service/client and map invalid/auth/connectivity/rate-limit/timeout/domain errors to documented exits.
3. Render versioned machine output and stable human output; redact authorization/credential data.
4. Add parser, mock-service, transport, output, exit, and redaction tests under filter `command_c039`.
5. Update command reference, completion/man metadata, and old-command migration mapping without aliases.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c039` passes; `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
Use fresh multi-job hold/success/failure run; follow active logs, filter failed, fetch completed, and prove no duplicates/secrets.
Monitor only new run IDs every at most 60 seconds; diagnose unchanged/queued state before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl run logs --help` exits 0 with exact syntax.
- [ ] Focused and repository gates pass.
- [ ] Fresh fixture proof covers command behavior and authority.
- [ ] No sibling command, alias, secret, or direct internal-layout dependency was added.

## STOP conditions

- Shared service lacks authoritative required behavior.
- Work needs capability/trust expansion, protocol guessing, fixture weakening, or destructive scope beyond command.
- Two-minute fixture stasis cannot be diagnosed.
