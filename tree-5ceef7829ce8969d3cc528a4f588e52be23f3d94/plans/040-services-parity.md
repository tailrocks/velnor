# Plan 040: `services:` parity — service host/port env + alias reachability (Postgres acceptance)

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/github_adapter.rs crates/velnor-runner/src/container.rs crates/velnor-runner/src/executor.rs crates/velnor-runner/src/job_message.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0 (V0.5 — schemalane's integration job gates on it)
- **Effort**: M (execution already exists — this closes parity gaps)
- **Risk**: MED
- **Depends on**: none; estate plan 055 (schemalane) consumes it; fixture
  proof lands via plan 041
- **Category**: correctness / parity
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Estate migration puts a Postgres `services:` job into schemalane's CI. The
code audit found `services:` is ALREADY parsed and executed (network,
start, health-wait, teardown) — the remaining parity gaps are the env/alias
surface GitHub-hosted jobs rely on: GitHub injects service host/port env and
network aliasing so `localhost:<mapped>` or `<alias>:<port>` connections work
from the job container. Until those match `actions/runner` behavior exactly,
the same YAML behaves differently per lane — the defining defect class.

## Current state (verified against `48b04ad` by the code audit; re-locate by symbol)

- Parse: `github_adapter.rs:357-387` `service_containers()` — from
  `job.resources.containers` (skips the job alias), builds
  `ServiceContainerSpec { image, network_alias, network, env, ports, options }`;
  helpers `:389-415`. NOTE: `job_message.rs:49-51` has an unconsumed
  `job_service_containers` field — determine whether real V2 job messages
  populate it instead of/alongside `resources.containers` (check against
  actions/runner source — AGENTS.md source-of-truth rule) and reconcile.
- Spec/args: `container.rs:658-704` — `start_args()` emits `--network`,
  `--network-alias`, `-e`, `-p`, options, image; `health_status_args()`;
  `remove_args()`.
- Execution: `executor.rs:2678-2683` — network create → per-service start +
  `wait_for_service` (`:2803-2831`, inspect-health poll, 30 s budget) → job
  container start. Cleanup `:2618-2634`, `:2761-2764`.
- Gaps (audit): no service host/port env injected into the JOB container
  (GitHub sets e.g. `<ALIAS>_PORT_*`-style vars? — VERIFY against
  actions/runner: the modern behavior is `job.services.<id>.ports` CONTEXT
  values plus network-alias reachability; container-job vs host-job port
  semantics differ); job container gets `--network <net>` but the parity of
  `localhost` port publishing for host-run steps is unverified.
- Tests: `container.rs:1448` `builds_service_container_start_args`;
  `github_adapter.rs:1072` `service_containers_use_non_job_container_resources`;
  `executor.rs:8552` `starts_and_waits_for_service_before_job_container`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Upstream truth | read `https://github.com/actions/runner` `src/Runner.Worker` service-container handling | behavior notes |

## Scope

**In scope**: `github_adapter.rs`, `container.rs`, `executor.rs`,
`job_message.rs` (the unconsumed field), tests; the `services.*` job-context
values if expression evaluation feeds them (locate the context assembly —
grep `job.services` in `crates/`).
**Out of scope**: fixture YAML (plan 041 adds the Postgres fixture job);
schemalane's workflow (estate plan 055).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits, `git commit -s`; no
push without operator instruction.

## Steps

### Step 1: Upstream behavior inventory

From actions/runner source (mandatory per AGENTS.md), write down in the PR
description: exactly what env/context a service exposes to (a) container
jobs, (b) host jobs with published ports; alias semantics; health gating.
This inventory drives steps 2–3 — do not code from memory.

**Verify**: inventory present, each claim with an upstream file reference.

### Step 2: Close the env/context gap

Implement exactly what step 1 found (expected shape: `job.services.<id>.*`
context values incl. mapped ports for expressions, and for container jobs
alias-based reachability which ALREADY works via `--network-alias` + shared
network). Reconcile `job_message.rs:49-51` `job_service_containers` per the
V2 message reality (either consume it or delete it with a comment naming why).

**Verify**: new tests — `service_context_exposes_mapped_ports`,
`container_job_reaches_service_by_alias` (RecordingRunner-level arg
assertions; pattern `executor.rs:8552`).

### Step 3: Postgres end-to-end proof (local)

A `cargo nextest` integration-style test (RecordingRunner if docker-less; a
`#[ignore]`d real-docker test if the harness supports it — check how existing
docker-dependent tests are gated) that runs the canonical GitHub Postgres
services example shape (image + health opts + env + ports) and asserts start
order + env surface.

**Verify**: test passes; `#[ignore]` variant documented for operator run.

## Test plan

≥ 3 new tests; suite green. Real acceptance = plan 041's fixture Postgres job
green on both lanes, then schemalane (055) `both` green.

## Done criteria

- [ ] Gates exit 0; step-1 inventory in PR; gaps closed per inventory
- [ ] `job_service_containers` reconciled (consumed or removed)
- [ ] No out-of-scope changes

## STOP conditions

- Upstream behavior requires runner-side port-forward proxying that Docker
  networks can't express directly → STOP with the inventory; design review
  needed.
- V2 job messages in the wiremock fixtures lack service shapes to test
  against → capture a real dump via `--dump-job-message` from a fixture run
  (operator-gated) before proceeding; if unavailable, STOP.

## Maintenance notes

- Every future estate repo with integration services inherits this surface —
  the fixture job (041) is the regression net.
- Health-wait budget (30 s) may need a knob for slow images; only add if the
  fixture proves the need.
