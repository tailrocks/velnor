# Plan 073: Build daemon and lifecycle state engine

> **Executor instructions**: Move execution behavior into one owned state
> engine. Do not add lifecycle or daemon Clap handlers; C027–C033 and C075 own
> those commands.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/main.rs crates/velnor-control crates/velnor-model systemd`

## Status

- **Priority**: P1
- **Effort**: XL
- **Risk**: HIGH
- **Depends on**: Plans 066–072
- **Category**: migration, correctness
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current daemon and slot loops own JIT configuration, broker sessions, jobs,
SIGTERM drain, recycle, and watchdog behavior. API lifecycle commands need one
per-instance state owner; final removal requires the engine to leave the old
binary without changing execution semantics.

## Scope

- instance desired/observed state and stable slot lifecycle
- typed cordon, uncordon, drain, resume, restart, recycle, and scale requests
- signal and control-API requests through the same owner
- daemon startup, preflight, watchdog, JIT, broker, job, teardown, and `--once`
- crash recovery, durable intent, progress streams, audit events

No CLI handlers, installed systemd/package cutover, remote host transport, or
normal-path job kill.

## Steps

1. Characterize current daemon/slot/SIGTERM/JIT behavior with state-machine and
   process tests.
2. Replace global drain flags with owned per-instance desired/observed state.
3. Implement service operations for cordon/uncordon/drain/resume/restart,
   stable-slot recycle, and dynamic desired slot count.
4. Move worker startup and `--once` execution into `velnor-control` while old
   binary temporarily delegates to the same engine.
5. Preserve strict admission before checkout/cache/service/container side
   effects, fail-close completion, graceful SIGTERM, watchdog, and JIT recycle.
6. Test every phase, crash/restart, concurrent commands, partial JIT failure,
   scale up/down, deferred recycle, and normal drain with busy jobs.

**Verify**: focused process/state tests and `rtk mise run check` pass.

## Mandatory fixture integration

Against pinned `tailrocks/velnor-actions-fixture`, clean before every dispatch
and exercise success, hold/cordon, queued/un-cordon, drain/resume, idle recycle,
scale 1–2–1, draining restart, and `--once`. Monitor only fresh run IDs every at
most 60 seconds. No normal operation may kill an active job.

## Done criteria

- [ ] C027–C033 and C075 call one lifecycle engine.
- [ ] Stable slot IDs survive ephemeral registration recycle.
- [ ] Source-built new engine passes full fixture lifecycle sequence.

## STOP conditions

Stop if desired state is not crash-safe, scale requires env rewrite/restart, or
normal drain/restart/recycle must kill active work.
