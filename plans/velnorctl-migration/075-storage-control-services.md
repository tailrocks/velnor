# Plan 075: Build storage query, accounting, pressure, and GC services

> **Executor instructions**: Extract one canonical storage service. Tasks
> C043–C050 own individual `storage` commands; no `cache` alias survives.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/storage.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/capacity.rs docs/storage-and-disk-pressure-2026-07-18.md docs/cache-gc-design.md crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 066, 067
- **Category**: migration, correctness
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current storage/cache commands print directly and GC retains a lease-check
bypass. Canonical services must own logical/physical accounting, reservations,
leases, pressure, history, and safe dry-run-first reclamation.

## Scope

- storage path/class/scope/accounting/budget/pressure resources
- authoritative storage catalog and GC history
- reservation and lease queries
- pressure explanation
- GC planning/execution with leader/filesystem locks and exact ownership

No CLI handlers, cache aliases, new storage backend/class, broad Docker prune,
or manual host cleanup as product behavior.

## Steps

1. Extract typed read services from current storage/cache/capacity modules.
2. Implement or complete authoritative catalog records for objects, ownership,
   budgets, lifetime, leases, reservations, pressure, and GC outcomes.
3. Implement truthful logical/physical accounting for sparse/reflink/hardlink
   cases and explicit unknown values.
4. Implement pressure explanations and dry-run GC plans. Execution revalidates
   ownership/leases under locks; remove lease bypass.
5. Test crash recovery, concurrent GC, new/active lease races, partial deletion,
   stale accounting, unowned paths, and idempotent repeat.

**Verify**: storage nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Use isolated roots with pinned `tailrocks/velnor-actions-fixture`. Clean old
state, dispatch fresh hold, inspect active reservations/leases/pressure, prove
GC refuses active data, complete run, collect safe candidate, and inspect audit
history. Monitor only new ID every at most 60 seconds.

## Done criteria

- [ ] C043–C050 require no direct storage-file parsing.
- [ ] GC is lock-safe, lease-safe, dry-run-first, and audited.
- [ ] Fixture cache/job data remains correct.

## STOP conditions

Stop when ownership or expected reclaim is unknowable. Report unknown; do not
delete or invent a number.

