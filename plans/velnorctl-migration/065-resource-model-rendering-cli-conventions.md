# Plan 065: Define versioned resources, rendering, and CLI conventions

> **Executor instructions**: Build stable types first. Tables are views of typed
> resources, never the source of truth. Do not include secrets or raw endpoint
> authorization data in any serializable resource.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/src/storage.rs crates/velnorctl crates/velnor-model crates/velnor-render`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plan 064
- **Category**: architecture, dx
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current status and doctor paths print ad hoc lines. No stable instance, slot,
runner, job, event, reservation, lease, capability, or adapter resource exists.
Every later API and command needs one versioned, redacted model.

## Current state

- `runner.rs:836-848` has only a private three-value drain decision, not an
  operator-visible slot phase.
- `runner.rs:7887-7946` prints config status directly.
- `runner.rs:288-299` stores timing fields without resource metadata.
- `storage.rs:24-37` prints tab-separated rows directly from command dispatch.

## Scope

- `crates/velnor-model/src/**`
- `crates/velnor-render/src/**`
- global CLI/output modules in `crates/velnorctl/src/**`
- golden fixtures under those crates' test directories

**Out of scope**: filesystem discovery, database writes, control API, live
GitHub calls, lifecycle mutation.

## Steps

### 1. Define resource envelope and exact nouns

Create schema-versioned resource types for `Host`, `Instance`, `Slot`,
`RunnerRegistration`, `Job`, `Run`, `QueueEntry`, `Event`, `Reservation`,
`Lease`, `Capability`, and `Adapter`. Every object has stable
identity, source (`LOCAL|GITHUB|MERGED` where relevant), conditions, reason,
message, and RFC 3339 `lastTransitionTime`.

Define slot phases exactly: `Configuring`, `Idle`, `Acquiring`,
`WaitingForCapacity`, `Running`, `Finalizing`, `Teardown`, `Recycling`,
`Parked`, `Draining`, and `Error`. Preserve the stable-slot/ephemeral-runner
distinction in types.

**Verify**: serde golden tests prove schema field names, unknown enum handling is
fail-closed, timestamps are RFC 3339, and a fixture resource round-trips.

### 2. Enforce redaction by construction

Use dedicated sanitized DTOs. Do not serialize PATs, OAuth keys/tokens,
authorization URLs with credentials, raw job variables marked secret, endpoint
authorization maps, or raw message bodies. Add a deny-list regression corpus.

**Verify**: tests serialize representative resources containing secret-bearing
source inputs and assert secret markers/values are absent.

### 3. Implement renderers

Support `table`, `wide`, `json`, `yaml`, `jsonl`, and `name`. JSON/YAML emit
versioned resources. JSONL emits exactly one object/event per line. Warnings go
to stderr. Durations are human-readable only in tables and numeric in machine
output. Color respects `--no-color` and non-TTY output.

**Verify**: golden tests cover every resource/output combination and verify
stdout/stderr separation.

### 4. Implement global CLI conventions and command metadata interfaces

Add global `--context`, `-o/--output`, `--instance`, `--repo`, `--selector`,
`--field-selector`, `--since`, `--timeout`, `--no-color`, and verbosity. Add
typed `ValueEnum`s for closed choices. Define schema/help/completion/man metadata
interfaces only. C002, C003, C004, and C005 own those leaf commands.

**Verify**: parser matrix tests cover placement before/after subcommands,
invalid values, stdout/stderr, and non-zero usage exit.

### 5. Mandatory fixture integration

Against `tailrocks/velnor-actions-fixture`, cancel old fixture runs, remove only
validation-prefix stale registrations,
prove clean state, dispatch a new control-plane success run, and monitor only
its run ID at intervals no longer than 60 seconds. Serialize a sanitized model
of that run from test data or current maintainer tooling and render all formats.

**Verify**: run succeeds; rendered output contains repository/run/job identity
but no token, endpoint authorization, or raw secret variable.

## Done criteria

- [ ] All approved resource nouns and slot phases are modeled.
- [ ] Every machine format is versioned and deterministic.
- [ ] Redaction tests pass.
- [ ] C002–C005 can consume schema/help/completion/man metadata without domain
      duplication.
- [ ] `rtk mise run check` and fresh fixture run pass.

## STOP conditions

- A resource requires persisting secret-bearing source data.
- Renderer needs to parse human output from another command.
- A new resource noun outside the approved research becomes necessary.

## Maintenance notes

Wire compatibility begins when these types land. Later field additions must be
optional or schema-versioned; table columns may evolve without changing JSON.
