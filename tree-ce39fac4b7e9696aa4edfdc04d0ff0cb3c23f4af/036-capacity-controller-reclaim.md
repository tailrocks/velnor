# Plan 036: Filesystem capacity controller — leases, reservations, reclaim-before-accept

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cache.rs crates/velnor-runner/src/storage.rs`
> (storage.rs is created by plan 035.) Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0 (V0.12 + V0.13; storage doc "Filesystem-wide pressure
  controller" + "Reclaim before accept")
- **Effort**: L
- **Risk**: HIGH (admission control for the whole fleet)
- **Depends on**: plans/035-canonical-storage-contract.md (path authority),
  plans/037-cache-gc-and-accounting.md (the reclaim executor it drives —
  037 can land in parallel but 036's reclaim path needs 037's GC engine)
- **Category**: correctness / stability
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

The storage doc's accepted design: one filesystem coordinator leases active
scopes, serializes daemons, reserves peak job space BEFORE advertising a
slot, automatically reclaims toward the reservation, and — once a job is
acquired — its reservation holds through result upload; Velnor never
silently refuses an acquired job, and never runs broad Docker prune. Today
the ONLY capacity signal is a 2 GiB `df` floor that parks the slot
(`DISK_MIN_FREE_BYTES`, `runner.rs:834`) — on a shared host that is
park-and-wait, the opposite of reclaim-before-accept.

## Current state (verified against `48b04ad`)

- `runner.rs:834` `DISK_MIN_FREE_BYTES = 2 GiB`; `:839-857`
  `disk_space_problem`; `:859-875` `free_space_bytes` shells `df -Pk`.
  Consumed in the slot loop `run_daemon_slot` (`runner.rs:913+`, disk check
  before `run()`), and doctor (`runner.rs:936`).
- Slot lifecycle (map): `daemon_pass` `runner.rs:674` → `run_daemon_slot`
  `:913` → `run_v2` `:1527` (broker session `:1555`, poll loop `:1603`,
  message → `handle_v2_message` `:2105`, `acquire_job` `:2203`,
  `handle_job_request` `:2288`).
- No leases/locks exist anywhere (grep-confirmed); GC eviction policy exists
  but `in_use_scopes` is always empty (`cache.rs:66`).
- `/run` lease dir is reserved by plan 035's layout (`run_root`).

Design constraints from the doc (binding): per-class budgets; active-scope
leases that GC/reclaim must honor; hysteresis (don't thrash at the
threshold); hard emergency reserve (never allocate the last N GiB);
pre-assignment backpressure = stop advertising (don't create the broker
session / don't poll) rather than accept-and-fail; acquired jobs keep their
reservation until completion posted.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner capacity` | new tests pass |

## Scope

**In scope**: new `crates/velnor-runner/src/capacity.rs`; wiring in
`runner.rs` (slot loop + acquire path + completion path); lease files under
`<run_root>` via `storage.rs`; `cache.rs` (`in_use_scopes` fed from leases);
doctor output additions; config knobs (`cli.rs`).
**Out of scope**: the GC engine internals (plan 037); storage layout (035);
BuildKit pruning policy beyond calling 037's owned-prune hook; broker
protocol changes (none needed — backpressure = not polling).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits, `git commit -s`;
no push without operator instruction.

## Steps

### Step 1: Lease primitives

`capacity.rs`: `ScopeLease` — a file under `<run_root>/leases/<class>/<scope-key>`
created with O_EXCL semantics + PID + timestamp JSON, held for the duration
of a job's use of a scope (target bucket, caches scope, cargo/mise stores are
shared-read: lease only WRITER scopes — target buckets and `_velnor_caches`
scopes). Stale-lease detection: PID gone or age > configurable max → reapable.
API: `acquire(scope) -> Result<ScopeLease>`, `Drop` releases,
`active_scopes() -> BTreeSet<String>`.

**Verify**: unit tests with tempdir (`lease_excludes_second_acquirer`,
`stale_lease_detected`); suite green.

### Step 2: Reservation + admission

`capacity.rs`: `CapacityController::reserve_for_slot() -> Result<Reservation>`
computed BEFORE `run_v2` creates a broker session (`runner.rs:1555`):
free bytes (reuse `free_space_bytes`) minus emergency reserve (default 10 GiB,
`--emergency-reserve-bytes`) must cover the per-job peak estimate (default
30 GiB, `--job-peak-bytes`; both env-backed like `VELNOR_JOB_CPUS`,
`cli.rs:210-215` pattern). Shortfall → invoke reclaim (step 3) → re-check →
still short → slot enters explicit backpressure: log
`forensics.lifecycle("capacity backpressure: ...")`, do NOT create the
session, retry with hysteresis backoff (e.g. re-check every 60 s, resume only
when free ≥ threshold + 20% band). On job acquisition (`runner.rs:2203`
`acquire_job` success) the reservation is pinned to the job and released only
after completion posting (`complete_run_service_job_refreshing`,
`runner.rs:2480` call site).

**Verify**: tests `reservation_blocks_session_when_short`
(inject a fake free-bytes fn — refactor `free_space_bytes` behind a trait or
fn pointer for testability), `reservation_survives_until_completion`.

### Step 3: Reclaim toward the reservation

On shortfall, call plan 037's GC engine programmatically (library call, not
subprocess): evict per policy (TTL/LRU/byte-ceiling) EXCLUDING
`active_scopes()` from step 1's leases, targeting `needed_bytes`, oldest
first, per-class budget order (regenerable classes first: caches → targets →
kache/sccache over-budget portions; never durable classes; never broad
`docker system prune` — BuildKit reclaim only through 037's owned-builder
prune hook). Every deletion logged with class/scope/bytes (forensics).

**Verify**: test `reclaim_skips_leased_scopes` (tempdir stores + a held
lease); test `reclaim_stops_at_target_bytes`.

### Step 4: Wire `in_use_scopes` + doctor

`cache.rs` GC callers populate `EvictionPolicy.in_use_scopes` from
`capacity::active_scopes()` (kills the always-empty set at `cache.rs:66`).
Doctor gains: free bytes, emergency reserve, current reservations, active
leases, backpressure state (extend the doctor path near `runner.rs:5762`).

**Verify**: existing test `cache_gc_skips_in_use_scopes` (`cache.rs:551`) now
exercised via the lease-fed path (adapt it); doctor prints the new block
(assert via the doctor unit tests if present, else add one for the formatter).

### Step 5: Storage-doc status

Annotate `(closed: plan 036)` on controller/lease/reserve lines.

**Verify**: grep.

## Test plan

≥ 6 new tests as listed; suite green. Operator soak (post-merge): on a
loaded host, fill disk toward the threshold and observe: backpressure log +
no new sessions + reclaim firing + resume above band — record in the PR.

## Done criteria

- [ ] Gates exit 0; new tests pass
- [ ] Slot never creates a broker session without a satisfiable reservation
- [ ] Acquired job's reservation provably held to completion (test)
- [ ] `in_use_scopes` live; no broad docker prune anywhere
      (`grep -rn "system prune" crates/` → none)
- [ ] No out-of-scope changes

## STOP conditions

- The peak-estimate model proves unworkable (jobs vary 1–100 GiB) → STOP
  and propose per-pool overrides ONLY (env), not dynamic guessing.
- Reclaim needs 037's engine and 037 is not landed → implement steps 1–2 +
  backpressure WITHOUT reclaim (park with explicit reason), mark the plan
  partially done, note the 037 dependency in the status row.
- Refactoring `free_space_bytes` for testability ripples into doctor
  signatures → keep the ripple ≤ 2 call sites or STOP.

## Maintenance notes

- The emergency reserve + peak estimate are the two tunables operators will
  touch; document both in `docs/runner-usage.md` (follow-up allowed inline
  here if ≤ 10 lines).
- Multi-daemon hosts: leases are filesystem-wide by design; the leader-lock
  for competing reapers is 037's concern.
