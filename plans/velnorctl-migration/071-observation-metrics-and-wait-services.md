# Plan 071: Build event, metrics, and wait services

> **Executor instructions**: Build shared read/watch primitives only. Tasks
> C018–C020 own `events`, `top`, and `wait`.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/telemetry.rs crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plans 066, 067, 069, 075
- **Category**: architecture, observability
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Normalized events, live resource metrics, and condition waits need shared
ordering and cancellation semantics. Separate implementations would race and
disagree about transitions.

## Scope

Build event queries/watches, bounded live CPU/memory/disk/timing snapshots, and
condition/phase waits over versioned resource streams. Preserve event reasons
from Plan 066 and return typed terminal, timeout, disconnection, and failed
condition results. Storage metrics consume Plan 075's snapshot port; this plan
never reads storage files directly.

No Prometheus replacement, historical metrics warehouse, CLI, or mutation.

## Steps

1. Add resource-version/cursor semantics and event filter contracts.
2. Collect live host/instance/slot/job/storage snapshots with declared units,
   gauge/counter kind, sample window, source time, monotonic observation time,
   and explicit stale/missing fields.
3. Implement race-free condition observation: read current state, subscribe,
   reject stale versions, and terminate on success/failure/timeout/cancel.
4. Define cursor generation/gap, bounded buffer/backpressure, terminal
   reconnect, and resnapshot rules. Test event ordering, reconnect, cursor
   expiry, metric unavailability, terminal-before-subscribe races, and bounded
   shutdown.

**Verify**: focused nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Clean `tailrocks/velnor-actions-fixture`, dispatch one fresh hold run, observe
events/metrics/waits through services, cancel through GitHub, and observe
terminal teardown. Monitor only new ID every at most 60 seconds; diagnose stasis
before two minutes.

## Done criteria

- [ ] Tasks C018–C020 use one ordered observation model.
- [ ] Fixture events, metrics, and waits converge without polling races.
- [ ] Timeout and failed-condition outcomes use Plan 065's exact exit classes.

## STOP conditions

Stop if a metric cannot be attributed safely or waits require unbounded polling.
