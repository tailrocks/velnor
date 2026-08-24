# Plan 064: Scaffold velnorctl and shared workspace seams

> **Executor instructions**: Add new crates without copying runner business
> logic. Keep the old binary operational only as a temporary migration source.
> New `velnorctl` must not expose legacy command aliases.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- Cargo.toml Cargo.lock crates/velnor-runner crates/velnor-tools Dockerfile`

## Status

- **Status**: DONE (2026-08-24)
- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plan 063
- **Category**: migration, architecture
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current binary owns parsing, telemetry initialization, and direct dispatch into
private modules. `crates/velnor-runner/src/lib.rs` exports only `protocol`, so a
new CLI cannot reuse behavior without spawning or duplicating the old binary.
This plan establishes inward dependencies and a real `velnorctl` executable.

## Current state

- `Cargo.toml:2` has only `crates/velnor-runner` and `crates/velnor-tools`.
- `crates/velnor-runner/src/main.rs:1-31` declares every runtime module in the
  binary; `main.rs:80-92` directly matches old commands.
- `crates/velnor-runner/src/lib.rs:1-3` exports only `protocol`.
- Local baseline is 871/871 nextest tests passing at `35d5bb7`.

## Target shape

Create `velnor-model`, `velnor-control`, `velnor-client`, `velnor-render`, and
`velnorctl`. Dependency direction is:

```text
velnor-model <- velnor-control <- velnorctl
velnor-model <- velnor-client  <- velnorctl
velnor-model <- velnor-render  <- velnorctl
```

`velnor-client` never depends on `velnor-control`, Axum, daemon internals, or
server application ports; both sides meet only through versioned model DTOs and
an explicit transport contract. No shared crate depends on Clap. Core model and
application modules do not depend on Axum; Plan 067 isolates Axum in the
control crate's transport-adapter module. During the
migration only, `velnorctl` may depend on a narrow public facade from
`velnor-runner`; later tasks remove that dependency before Plan 079.

## Scope

- workspace `Cargo.toml`, `Cargo.lock`
- new crate manifests and source roots under `crates/`
- minimal public service facade in `crates/velnor-runner/src/lib.rs`
- `crates/velnorctl/src/main.rs`, CLI smoke tests

**Out of scope**: command behavior beyond `version`; systemd, Docker, Debian,
release workflow, internal runner module moves.

## Steps

### 1. Add crate topology

Create all five crates with workspace metadata and exact compatible dependency
pins. Keep model/render/client/control as libraries. Make `velnorctl` a binary
and library so integration tests can parse/dispatch without subprocess-only
tests.

**Verify**: `rtk cargo metadata --no-deps --format-version 1` lists seven
workspace packages and the dependency graph has no cycle. A boundary test proves
`velnor-client` does not depend on `velnor-control`, Axum, or runner internals.

### 2. Extract binary bootstrapping from old main

Move reusable runtime setup and old command dispatch behind a narrow library
facade without making internal structs public. Old `main.rs` becomes a thin
adapter. Do not copy modules or spawn `velnor-runner` from `velnorctl`.

**Verify**: old CLI test still passes and a dependency scan finds no
`std::process::Command` invocation of `velnor-runner` in new crates.

### 3. Add parser and dispatch seams without leaf commands

Create global parser/dispatch composition points and reserve no successful
placeholder. Command Task C001 owns `velnorctl version`; every other leaf command
has its own C-task. Legacy names such as `cache`, `capabilities`, `configure`,
`remove`, `status`, and worker `run` must remain rejected.

**Verify**: parser construction tests prove zero unimplemented command can exit
success and later command modules can register without depending on domain
internals.

### 4. Mandatory fixture integration: preserve runtime behavior

Run repository gates against `tailrocks/velnor-actions-fixture`. Use the exact
fixture cleanup sequence from Plan 063:
cancel all non-completed runs, delete only stale validation-prefix runners,
confirm clean state, dispatch one new `control-plane` success run, capture its
ID, and check only that run every at most 60 seconds. Use the unchanged old
daemon for this refactor-only task.

**Verify**: new binary parser/help starts without a leaf command implementation;
fixture run succeeds through unchanged old daemon; no execution or GitHub
conclusion changes.

## Test plan

- Parser-composition tests for rejected unimplemented/legacy commands.
- Dependency-boundary test or `cargo metadata` assertion.
- All drift-snapshot baseline tests plus new crate tests.
- One fresh fixture success run.

## Done criteria

- [x] New crates exist with acyclic dependencies.
- [x] CLI parser/dispatch seams exist without owning a leaf command.
- [x] New code neither spawns nor parses output from old binary.
- [x] `rtk mise run check` passes.
- [x] Fresh fixture success run passes.

## Evidence (2026-08-24)

1. **Five crates, acyclic**: `crates/velnor-client/tests/dependency_boundaries.rs`
   @5ab7479 (exactly-seven-packages assertion, DFS acyclicity check, direction
   assertions) — 8 tests green. Workspace resolves exactly seven packages
   (`velnor-runner`, `velnor-tools`, `velnor-model`, `velnor-control`,
   `velnor-client`, `velnor-render`, `velnorctl`).
2. **Seams with zero successful commands**: `lib.rs` legacy/unimplemented
   rejection tests plus `cli_smoke` subprocess assertions (`--help`=0,
   bare=2, `cache` du=3, `version`=2). Live binary smoke table verified
   2026-08-24: `version`/`--version`=2, `help`=0, no-args=2,
   `cache`/`cache du`/`status`/`capabilities`/`run --once`=3.
3. **No spawn/parse of old binary**: the only `Command` uses are the
   `CARGO_BIN_EXE_velnorctl` smoke test and the `cargo metadata` boundary
   probe; zero in new-crate source.
4. **Gates**: fmt/lint/test-focused (velnorctl 12/12, velnor-runner
   732/732)/test (877/877)/check all exit 0 @5ab7479; re-ran
   `test-focused -p velnorctl` + `check` after the dev-deps cleanup
   (commit `7a63d72`) — both exit 0.
5. **Fixture success through unchanged old daemon**: sanitized evidence in
   `.velnor-compare/2026-08-24-control-plane/summary.md` shows fresh
   successes including dual-lane compat run 32703106587 and the cold→warm
   cache pair 32704574052→32704719858; daemon is apt-installed and
   unaffected by velnorctl-only commits fb4fdbc→5ab7479→7a63d72.

Provenance notes:

- Implementation base of record is twin commit `5ab7479`, superseding
  `fb4fdbc`. Review FIX-FIRST(4) items — missing serde_json dev-dep,
  phantom runner-edge assertion, exit-code mismatches, dual seam systems —
  are all resolved in `5ab7479` per
  `.velnor-compare/2026-08-24-064-seam-review/feedback-to-twin.md`; the
  residual empty `[dev-dependencies]` header was removed in `7a63d72`.
- Executor run id 32714016121 was untraceable in the tree and is superseded by
  the `summary.md` evidence above.

### Execution evidence 2026-08-24 @ 5ab7479 (+nit/closeout commit)

- Gates: `mise run test-focused -- -p velnorctl -p velnor-client -p
  velnor-model -p velnor-control -p velnor-render` exit 0 (23/23); `mise run
  check` exit 0 (877/877 pre-nit baseline). Nits re-verified with both gates
  exit 0.
- Fixture: fresh `control-plane` run 32714994603 success in 28s through the
  unchanged old daemon; no execution or conclusion change.
- Verification: PASS (gates green, criteria mapped). Review: APPROVE
  (`dependency_boundaries.rs:189-200` direction pin deliberate, left as is).
- Message correction: commit `fb4fdbc` claims `velnorctl` depends on the
  `velnor-runner` facade; the facade was extracted but never consumed. The
  dependency exists only in the migration-scaffold allowance, and no code path
  uses it. This note supersedes that message text.

## STOP conditions

- Reuse requires exposing credentials or raw job messages publicly.
- New crate cycle appears.
- Existing runner behavior changes to make extraction easier.

## Maintenance notes

Temporary dependency on the old library is an implementation scaffold, not a
compatibility promise. Plan 079 must prove it is gone.
