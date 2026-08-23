# Plan 063: Record the velnorctl direction and fixture control contract

> **Executor instructions**: Complete this plan before product implementation.
> Update both repositories through normal reviewed changes. Do not weaken any
> existing fixture. If the fixture baseline cannot pass unchanged, diagnose and
> fix Velnor first; do not hide the failure in the new workflow.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- AGENTS.md docs/vision.md docs/roadmap.md docs/prompt.md plans/README.md crates/velnor-tools/src/main.rs scripts`
> Compare live text with the evidence below before editing.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: direction, tests
- **Planned at**: Velnor `35d5bb7`; fixture `dc4204ca055c3138cf78d666b4dd1c5adfddc963`; 2026-08-24

## Why this matters

Repository policy requires direction documents, `AGENTS.md`, plan index, and
active prompt to agree before execution. Existing fixture proves workflow
compatibility but does not deliberately hold, fail, or cancel a job for control
plane inspection. Every later task needs one stable integration corpus.

## Current state

- `docs/vision.md:48-66` names performance, native adapters, and UX as active
  direction but does not name `velnorctl` or removal of `velnor-runner`.
- `docs/prompt.md:19-21` says only the tracked unified-CI graph is active.
- `plans/README.md` previously declared every numbered plan historical.
- `crates/velnor-tools/src/main.rs:18` still defaults fixture tooling to
  `donbeave/velnor-actions-fixture`; authoritative fixture is now
  `tailrocks/velnor-actions-fixture`.
- Fixture `compat.yml` covers real execution semantics. It lacks deterministic
  hold/fail/cancel phases needed by `logs`, `wait`, lifecycle, queue, and event
  validation.

## Scope

**Velnor repository**:

- `docs/vision.md`, `docs/roadmap.md`, `docs/prompt.md`, `AGENTS.md`
- `plans/README.md`
- `crates/velnor-tools/src/main.rs` and its fixture-default tests
- fixture validation scripts only when needed to enforce the cleanup sequence

**Fixture repository**:

- `.github/workflows/control-plane.yml` (create)
- `README.md`
- fixture-local tests/audits that enumerate workflow surface

**Out of scope**: runner behavior, capability manifest changes, systemd,
packaging, removal of any existing fixture job.

## Steps

### 1. Make the direction authoritative

Update `docs/vision.md` first, then `docs/roadmap.md`. State final crate layout,
the service-only `velnorctl daemon` entrypoint, command-by-command migration,
intentional lack of backward aliases, and final deletion criteria. Add a dated
direction-log entry to `AGENTS.md`. Reconcile `docs/prompt.md` and
`plans/README.md` to point at this plan set.

Record the package boundary explicitly: `velnorctl` has no release command
family or release resource. Debian package metadata is authoritative for the
installed version; signed apt is the only installation, upgrade, downgrade,
rollback, and recovery mechanism. Velnor must not maintain an active-version
symlink, previous-version pointer, duplicate version history, or package
activation API. Package build/sign/publish remains CI/maintainer work, not an
operator CLI surface.

**Verify**: `rtk rg -n "velnorctl|velnor-runner" docs/vision.md docs/roadmap.md docs/prompt.md AGENTS.md plans/README.md` shows the new direction and no contradiction claiming `velnor-runner` remains final architecture.

### 2. Correct fixture ownership in maintainer tooling

Change the default slug and fixture-status tests from `donbeave/...` to
`tailrocks/velnor-actions-fixture`. Keep explicit `--repo` overrides.

**Verify**: `rtk cargo nextest run -p velnor-tools --locked` passes; no default
fixture reference points to `donbeave`.

### 3. Add a control-plane fixture without weakening compatibility coverage

Add a manual workflow using only already approved actions and bash. Required
inputs: `scenario=success|failure|hold`, `hold_seconds` bounded to `0..300`, and
`lane=velnor|github|both`. Jobs must expose distinct named steps, write a
sanitized result artifact, emit output/summary lines, deliberately fail only for
`failure`, and remain active long enough for `hold` inspection. Add a hosted
aggregator that reports the requested terminal state. Do not alter `compat.yml`.

**Verify**: fixture `mise run check` and `actionlint` pass; workflow audit proves
full SHA pins, concurrency, timeouts, `cargo nextest` policy, and canonical lane
selection.

### 4. Mandatory fixture integration: establish clean live baseline

Before dispatch, cancel all non-completed fixture runs. Delete only stale GitHub
runner registrations with the dedicated validation-name prefix; confirm no such
registration remains. Dispatch existing `compat.yml` with `lanes=both`, capture
the new run ID, and inspect only it every at most 60 seconds. If queued or
unchanged for two minutes, inspect runner group/label, daemon registration, and
broker/registry logs immediately.

Then dispatch `control-plane.yml` for `success`, `failure`, and `hold`; cancel the
hold run through GitHub and prove GitHub reports `cancelled`. Record run URLs and
sanitized JSON only.

**Verify**: `compat` lane comparison passes; control success succeeds, controlled
failure fails at its named step with logs, and hold cancellation terminates with
no orphaned runner registration.

## Test plan

- Unit tests for corrected default fixture slug and dispatch input validation.
- Fixture static tests for every scenario and lane.
- Live both-lane compatibility run plus three control scenarios.

## Done criteria

- [ ] Direction files and prompt agree on complete `velnor-runner` removal.
- [ ] Direction files agree that apt/dpkg exclusively own installed-version
      management and no Velnor release command/resource/API remains.
- [ ] Fixture tooling defaults to `tailrocks/velnor-actions-fixture`.
- [ ] Control workflow adds coverage without changing existing fixtures.
- [ ] `rtk mise run check` passes in both repositories.
- [ ] Fresh fixture run IDs and conclusions are recorded.

## STOP conditions

- Existing fixture is not green before the new workflow is added.
- New fixture needs an unapproved capability or weakens an existing assertion.
- Direction documents disagree about final binary/package ownership.

## Maintenance notes

Later tasks may extend control scenarios, but must preserve success, failure,
hold, and cancellation semantics. Workflow dispatch cleanup rules remain
mandatory for every migration task.
