# Plan 065: Define versioned resources, rendering, and CLI conventions

> **Executor instructions**: Build stable types first. Tables are views of typed
> resources, never the source of truth. Do not include secrets or raw endpoint
> authorization data in any serializable resource.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/src/storage.rs crates/velnorctl crates/velnor-model crates/velnor-render`

## Status

**DONE** (2026-08-24)

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
to stderr. `name` is an explicitly unversioned newline-delimited canonical
identity projection; table/wide are human formats. Durations are human-readable
only in tables. Machine durations use unsigned fields named `*_ms`; unavailable
is `null`, never zero, and overflow is a typed serialization error rather than
silent wrapping. Color respects `--no-color` and non-TTY output.

**Verify**: golden tests cover every resource/output combination and verify
stdout/stderr separation.

### 4. Implement global CLI conventions and command metadata interfaces

Add global `--context`, `-o/--output`, `--instance`, `--repo`, `--selector`,
`--field-selector`, `--since`, `--timeout`, `--no-color`, and verbosity. Add
typed `ValueEnum`s for closed choices. Define schema/help/completion/man metadata
interfaces only. C002, C003, C004, and C005 own those leaf commands.

**Verify**: parser matrix tests cover placement before/after subcommands,
invalid values, stdout/stderr, and non-zero usage exit.

Define one public `ExitClass` contract and numeric process mapping used by every
leaf command and transport:

| Code | Class | Meaning |
|---:|---|---|
| 0 | `Success` | Requested operation completed, or an idempotent target already matched. |
| 1 | `Condition` | Inspection completed and authoritatively found a degraded/failed condition. |
| 2 | `Usage` | CLI syntax, selector, field, or local input is invalid. |
| 3 | `Authorization` | Authentication failed or the identity lacks required permission. |
| 4 | `Unavailable` | An authoritative resource is absent, unavailable, or not found. |
| 5 | `Timeout` | The requested deadline elapsed before a terminal result. |
| 6 | `Conflict` | Version, state, plan, or safety precondition no longer matches. |
| 7 | `Transport` | Connection, rate-limit, or ambiguous upstream transport outcome. |
| 8 | `Operation` | An accepted domain operation reached a definite failure. |
| 130 | `Interrupted` | Local user interruption (`SIGINT`) stopped observation. |

Machine error envelopes carry the class, numeric code, stable reason, request
ID, and safe remediation. Commands may refine reasons, never invent another
numeric mapping. Workflow conclusions remain data unless an explicit
`--exit-status` contract says otherwise.

**Verify**: exhaustive tests prove every error variant maps once, all commands
use the shared mapping, and transport/domain errors cannot collapse into usage
or success.

### 5. Mandatory fixture integration

Against `tailrocks/velnor-actions-fixture`, cancel old fixture runs, remove only
validation-prefix stale registrations,
prove clean state, dispatch a new control-plane success run, and monitor only
its run ID at intervals no longer than 60 seconds. Serialize a sanitized model
of that run from test data or current maintainer tooling and render all formats.

**Verify**: run succeeds; rendered output contains repository/run/job identity
but no token, endpoint authorization, or raw secret variable.

## Done criteria

- [x] All approved resource nouns and slot phases are modeled.
- [x] JSON, YAML, and JSONL are versioned and deterministic; `name` is the
      documented unversioned identity projection.
- [x] Redaction tests pass.
- [x] C002–C005 can consume schema/help/completion/man metadata without domain
      duplication.
- [x] Every command and API error uses the shared `ExitClass` mapping.
- [x] `rtk mise run check` and fresh fixture run pass.

## Evidence (2026-08-24)

- **Convergence of record**: `ce98a27` adopted the canonical peer surface from
  `a9f017f`/`34a6dad` after the writer conflict
  (`.velnor-compare/2026-08-24-065-writer-conflict/`); gates at landing:
  943/943 exit 0.
- **Independent review #1** (orchestrating session): verdict FIX-FIRST(2).
  Both majors repaired in `8734b4b`: `SanitizedUrl::try_from(String)`
  fail-closed sanitize; `--since` checked resolution with typed
  `SinceResolveError`; five new tests cover the repairs. Repairs
  independently re-verified.
- **Independent review #2**
  (`.velnor-compare/2026-08-24-065-seam-review/feedback-to-converging-session.md`):
  DBG-leak finding closed by `04321e9`, including the stderr-silence
  subprocess contract test.
- **Criteria evidence**:
  - 12 approved nouns `Host` … `Adapter` modeled (`crates/velnor-model/src/resources.rs`).
  - All 11 `SlotPhase` variants in plan order, covered by test.
  - Versioned envelope `schemaVersion = 1` plus six deterministic goldens with
    a fail-closed `UPDATE_GOLDENS` guard.
  - Redaction corpus: PAT / OAuth / Basic / Bearer / credential-URL /
    secret-value × JSON/YAML × stdout/stderr.
  - C002–C005 consume metadata interfaces only (`compose()` returns empty).
  - `ExitClass` full 10-class mapping incl. `Interrupted = 130`, exhaustiveness +
    inverse-uniqueness tests.
  - Live smoke: `version` → 2, `--help` → 0.
  - Fixture run 32724621332 success via unchanged daemon
    (`crates/velnor-render/tests/fixture_run_model.rs:51`).
  - Scoped gates green at `8734b4b`: fmt/clippy exit 0; model+render 64/64;
    velnorctl 23/23.
  - Workspace `mise run check` temporarily RED solely from foreign velnor-tools
    WIP per COORDINATION.md; rerun when fleet WIP lands.
  - Accepted review notes deferred to C002+: `condition.rs` schema_version wire
    validation, `metadata.rs` stale doc link `crate::globals::Cli`, undocumented
    public tuple fields on `DurationMs`/`RepositoryRef`/`SecretRef`/`IdentityRef`.

## STOP conditions

- A resource requires persisting secret-bearing source data.
- Renderer needs to parse human output from another command.
- A new resource noun outside the approved research becomes necessary.

## Maintenance notes

Wire compatibility begins when these types land. Later field additions must be
optional or schema-versioned; table columns may evolve without changing JSON.
