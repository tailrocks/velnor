# Plan 070: Build active and completed log access services

> **Executor instructions**: Build one log service. Do not add `logs` or
> `run logs` handlers; command tasks C017 and C039 own those surfaces.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/slot_log.rs crates/velnor-runner/src/telemetry.rs crates/velnor-runner/src/protocol.rs docs/log-format-contract.md crates/velnor-control crates/velnor-client`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 065–069, 074
- **Category**: migration, correctness
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Active masked output is local; completed run/attempt/job logs use Plan 074's
current GitHub REST redirect endpoints, with `job-log.txt` explicitly labelled
as artifact fallback. Execution-time Results Service URLs/credentials never
enter durable state or operator CLI. Lifecycle, broker, registry, daemon, trace, and systemd
streams have distinct owners. One service must preserve masking and RAW/blob
timestamp contracts across every command.

## Scope

- active job streaming through daemon
- completed GitHub REST log retrieval and labelled `job-log.txt` fallback
- step/failed/tail/since/timestamp/source filters
- lifecycle, broker, registry, daemon, trace, and systemd streams
- monotonic cursors with retention generation, reconnect, redaction, and source
  metadata; expired/gapped cursors return a typed resnapshot-required result

No new log format, raw secret data, rendered GitHub HTML, or duplicate command
implementation.

## Steps

1. Define typed log request, source, record, cursor, and terminal error models.
2. Extract local masked console streaming without exposing workspace paths.
3. Consume Plan 074's attempt-aware GitHub completed-log and artifact calls;
   never persist or reuse job-scoped Results Service credentials.
4. Add an explicit stream authorization matrix: ordinary sanitized job logs
   require read role; slot forensic, daemon journal, systemd, and trace streams
   require admin/root. Denial reveals no path, existence, or content metadata.
5. Characterize RAW live output and seven-digit uploaded timestamp behavior;
   add reconnect/gap/expiry/resnapshot, source-selection, masking,
   authorization, and teardown-race tests. Never silently omit or duplicate a
   record across cursor generation changes.

**Verify each step**: focused nextest tests pass. Final gate includes
`rtk mise run check` and visual log-contract comparison required by `AGENTS.md`.

## Mandatory fixture integration

Use pinned `tailrocks/velnor-actions-fixture`. Clean old state, dispatch fresh
hold, success, and controlled-failure runs, stream active logs, then fetch
completed native/fallback logs. Monitor only new IDs every at most 60 seconds.
Prove step/failed filters, masking, source switches, and timestamp parity.

## Done criteria

- [ ] C017 and C039 share this service.
- [ ] Active and completed fixture logs match GitHub-visible content.
- [ ] No secret leakage or doubled/missing timestamps occurs.

## STOP conditions

Stop if behavior diverges from current GitHub Results Service or the repository
log-format contract. Consult current `actions/runner` before protocol edits.
