# Plan 066: Persist sanitized operational history and lifecycle events

> **Executor instructions**: Add durable state without making SQLite or file
> layout part of CLI/API contracts. Preserve current job execution and fail-close
> semantics. Never place credentials, masks, raw job messages, or job logs in the
> operational database.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/job_message.rs crates/velnor-runner/src/github_adapter.rs crates/velnor-runner/src/capacity.rs crates/velnor-runner/src/slot_log.rs crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plan 065
- **Category**: architecture, migration
- **Planned at**: commit `35d5bb7`, 2026-08-24
- **Progress** (2026-08-25): steps 1–5 implemented on
  `velnor-estate-standard` — store schema v3 with retention state,
  sanitized admission persistence wired at the daemon acquisition boundary,
  idempotent transitions/events emitted from real boundaries, required-write
  fail-close semantics, bounded retention (age/row/byte budgets,
  incremental vacuum, WAL accounting), and the Plan 064 dependency-boundary
  test amended to allow the transitional runner→store edge that Plan 079
  deletes. Step 6 (fixture hold/cancel proof) runs against the deployed apt
  build.

## Why this matters

Active job identity lives in a four-field `in-flight-job.json`; timing history
is reconstructed from lifecycle log text. This cannot reliably power queries,
events, waits, reconciliation audits, or completed-job descriptions. A durable,
sanitized operational store gives the daemon one query model while GitHub stays
authoritative for complete workflow history and logs.

## Current state

- `runner.rs:145-172` persists only plan ID, job ID, run-service URL, and billing
  owner.
- `runner.rs:288-299` records timing fields; `runner.rs:7612-7643` reparses log
  lines to recover them.
- `github_adapter.rs:176-190` already derives sanitized job identity including
  workflow, repository, run ID, and attempt.
- `capacity.rs:12-24` stores filesystem lease/reservation records separately.
- Direction document `docs/storage-and-disk-pressure-2026-07-18.md:140-170`
  already expects an authoritative SQLite lifecycle for storage; do not merge
  secret state into it accidentally.

## Scope

- `crates/velnor-control/src/store/**`, schema migrations, repository tests
- `crates/velnor-model` event/transition types
- narrow event-sink calls at daemon, slot, acquisition, capacity, execution,
  completion, teardown, recycle, registration, and GC boundaries
- default operational path `/var/lib/velnor/state.db`; injectable temp path

**Out of scope**: CLI queries, HTTP, remote access, full GitHub history, raw logs,
storage catalog migration.

## Steps

### 1. Define store contract and migrations

Create idempotent SQLite migrations for `instances`, `slots`,
`runner_registrations`, `jobs`, `job_transitions`, `events`, and
`reconciliations`. Use WAL and bounded busy timeout.
Transactions must atomically update current resource state and append its event.
Use one host-shared `/var/lib/velnor/state.db`: every key is instance-namespaced,
schema migration is serialized by one explicit migration lock/owner, and all
daemon instances may write concurrently. Never let an instance-local process
silently create a divergent database.

**Verify**: migration tests cover empty DB, reopen, repeated migration, rollback
on failure, and five concurrent daemon writers/readers across crash/restart.

### 2. Persist sanitized job summaries

At successful acquisition, persist repository, workflow/job names, run ID and
attempt, ref/SHA, event, queue/acquisition times, instance/slot/runner identity,
trust/resource policy, current phase, conclusion, and infrastructure category.
Derive only from already normalized fields. Keep existing in-flight record until
reconciliation has switched; do not place run-service URL or billing owner in
the query DTO.

**Verify**: unit tests feed a job containing secret variables/endpoints and prove
database pages/query DTOs contain none of their values.

### 3. Emit normalized transitions and events

Publish the retained event reasons: readiness/degradation, drain, slot state,
registration missing/offline/stale-busy, job acquired/waiting/started/completed/
canceled/rejected, capacity pressure, and GC start/completion. Every transition
is idempotent under retry and carries a correlation ID, reason, message, and
transition time.

**Verify**: state-machine tests reject impossible transitions and prove replay
does not duplicate terminal events.

### 4. Make store failure observable without corrupting job outcomes

Classify startup/open/migration failure as daemon readiness failure. During an
active job, a failed state write emits a forensic error and marks control state
degraded; it must not change a successful workflow step into success/failure by
accident. Define which writes are required before accepting a job.

**Verify**: injected disk/full/locked failures cover startup and mid-job paths;
no test leaves GitHub completion absent or reports false success.

### 5. Enforce bounded retention and database accounting

Define row, age, and database-byte budgets per event/history class. Pruning runs
only in a terminal transaction and never removes active/nonterminal jobs,
current instance/slot state, protected reconciliation operations, or their
required ancestry. Publish current rows/bytes, oldest retained timestamp, last
prune result, and WAL bytes. Bound WAL checkpoint/compaction work and prove it
cannot block job completion or violate the host disk reserve.

**Verify**: boundary, crash-during-prune, protected-row, byte-budget, WAL growth,
and reopen tests prove deterministic retention and valid referential history.

### 6. Mandatory fixture integration

Use `tailrocks/velnor-actions-fixture` and the clean-run sequence: cancel old
runs, delete only stale validation runner
registrations, prove clean, start validation daemon with a temporary state DB,
dispatch one new `control-plane` hold run, and inspect DB through the store API
while active. Cancel through GitHub, monitor only that run ID at intervals of at
most 60s, then inspect terminal state.

**Verify**: transitions include acquired, running, canceled, teardown, and slot
recycle in order; no secret marker appears; fixture reports canceled; no orphan
registration remains.

## Done criteria

- [ ] Schema and migrations are idempotent and transactional.
- [ ] Active and completed fixture jobs have sanitized summaries.
- [ ] Required event reasons are emitted from real boundaries.
- [ ] Store failure behavior is explicit and tested.
- [ ] Row/time/byte retention and WAL accounting are bounded and preserve all
      active/protected history.
- [ ] `rtk mise run check` and fresh fixture hold/cancel run pass.

## STOP conditions

- Persisted metadata requires raw endpoint authorization or secret variables.
- State writes can silently prevent GitHub terminal completion.
- SQLite choice breaks supported cross-build/release targets and no compatible
  configuration is proven.

## Maintenance notes

Keep operational state separate from full workflow logs and GitHub history.
Retention policy must delete old sanitized rows transactionally without touching
GitHub-owned resources.
