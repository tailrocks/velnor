# Plan 069: Build resource query and description services

> **Executor instructions**: Build projections and joins only. Do not add any
> `get` or `describe` command; tasks C006–C016 own those commands.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/config.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plans 065–068, 074–075
- **Category**: architecture, migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Resource views currently require manual correlation across config, slot loops,
`in-flight-job.json`, storage files, logs, systemd, Docker, and GitHub. Commands
must consume one typed projection layer without exposing those layouts.

## Scope

Build finite read-only query and description snapshots for host, instance,
stable slot, ephemeral runner, job, run, queue entry, event, reservation, and
lease. Plan 077 owns capability/adapter views; Plan 071 owns cursors, watch, and
wait. Implement source tags `LOCAL`, `GITHUB`, and `MERGED`, selectors, field
selectors, pagination, time bounds, provenance, freshness, and partial-source
state.

No terminal rendering, CLI parser, mutation, scheduler, or queue ownership.

## Steps

1. Define query/filter/page contracts and stable identities for every resource.
2. Project local daemon/store/systemd/Docker/storage state through explicit
   adapters; storage reads consume Plan 075's pure snapshot port and never parse
   terminal output or reap state.
3. Consume GitHub state only through Plan 074. Join only immutable repository,
   run, attempt, job-database, plan, job, and runner IDs—never names or
   timestamps. Every merged field carries source, observed-at, and
   fresh/stale/unavailable state. Record missing/ambiguous correlations and
   never override GitHub conclusions.
4. Build human-description sections as typed data: identity, placement, state,
   timing, resources, diagnostics, and source evidence.
5. Test selector semantics, split-source outages, renamed runners, attempt
   rollover, stale page tokens, missing data, stable-slot/JIT-runner identity,
   pagination, and zero-side-effect reads.

**Verify each step**: relevant `rtk cargo nextest run` filters pass. Final gate:
`rtk mise run check` exits 0.

## Mandatory fixture integration

Use pinned `tailrocks/velnor-actions-fixture`. Clean old runs/registrations,
dispatch fresh hold/success/cancel scenarios, and query every resource through
the service while active and terminal. Monitor only new IDs every at most 60
seconds. Prove source labels and correlations match GitHub and local state.

## Done criteria

- [ ] Tasks C006–C016 need no direct filesystem/systemd/Docker parsing;
      capability/adapter descriptions delegate to Plan 077.
- [ ] GitHub authority and stable-slot semantics are preserved.
- [ ] All fixture resource projections are complete, redacted, and read-only.

## STOP conditions

Stop if a requested field cannot be sourced authoritatively. Mark it unavailable
instead of inferring or inventing it.
