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
- authoritative `/var/lib/velnor/storage.db` catalog, schema migrations,
  reconciliation, deletion ledger, and GC history
- reservation and lease queries
- pressure explanation
- GC planning/execution with leader/filesystem locks and exact ownership

No CLI handlers, cache aliases, new storage backend/class, broad Docker prune,
or manual host cleanup as product behavior.

## Steps

1. Extract typed read services from current storage/cache/capacity modules.
   Pure observation never deletes/reaps stale lease, reservation, catalog, or
   filesystem records; explicit reconciliation owns state repair.
2. Implement/migrate authoritative catalog records for objects, ownership,
   budgets, lifetime, leases, reservations, pressure, deletion phases, and GC
   outcomes. Reconcile DB↔filesystem after crashes with explicit unknown and
   foreign states.
3. Implement truthful logical/physical accounting for sparse/reflink/hardlink
   cases and explicit unknown values.
4. Implement pressure explanations and immutable dry-run GC plans. Every active
   scope holds a kernel-backed shared lease; GC must acquire the exclusive
   lease and revalidate catalog ownership/resource version immediately before
   deletion. PID/TTL is diagnostic only, never sole safety. Delete
   `force_no_lease_check` and every equivalent bypass.
5. Test schema migration, crash reconciliation, concurrent GC, shared/exclusive
   lease behavior, new/active lease races, partial deletion,
   stale accounting, unowned paths, and idempotent repeat.

**Verify**: storage nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Use isolated roots with pinned `tailrocks/velnor-actions-fixture`. Clean old
state, dispatch fresh hold, inspect active reservations/leases/pressure, prove
GC refuses active data, complete run, collect a safe candidate, and prove actual
allocated blocks/inodes reclaimed match the bounded plan before inspecting
audit history. Monitor only new ID every at most 60 seconds.

## Done criteria

- [ ] C043–C050 require no direct storage-file parsing.
- [ ] GC is lock-safe, lease-safe, dry-run-first, and audited.
- [ ] Fixture cache/job data remains correct.

## STOP conditions

Stop when ownership or expected reclaim is unknowable. Report unknown; do not
delete or invent a number.
