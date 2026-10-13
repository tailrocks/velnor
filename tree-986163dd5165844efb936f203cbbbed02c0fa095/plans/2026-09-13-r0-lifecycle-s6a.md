# Plan 2026-09-13: lifecycle drain unification steps 1-4 (flag-gated)

Base: origin/main tip 97281ce7. Branch: r0-lifecycle-s6a. Worktree: /tmp/velnor-s6.

## Fix (this branch, strict scope)

Unify the signal latch, journal marker, and lifecycle desired state into one
drain signal. ALL behind `VELNOR_JOURNAL_DRAIN=1` (default off); with the
flag off the static path is byte-identical (every reader short-circuits to
the latch, no drain key can exist, reducer arms are inert).

1. Journal drain state (control): `meta[drain]=requested:{version}`, NEVER a
   new `Event` variant (older binaries ignore the unknown key: forward-compat,
   no schema bump). `FleetState.drain_active` + `drain_version` loaded in
   `load_materialized_state` (absent = inactive; malformed fails closed;
   unknown sibling keys tolerated); `persist_state` re-emits the marker so
   every `apply` keeps a latched drain sticky. `Journal::set_drain`
   (idempotent immediate-txn write, version never regresses) and
   `read_drain_state` (throwaway read-only connection, zero busy timeout,
   single `SELECT` on meta, `None` on lock/error).
2. Observed writes (control): `Store::record_lifecycle_observed` with
   expected-version OCC sharing the instance's monotonic version (stale =
   converged: re-read row returned, no error, no retry loop; unknown
   instance fails closed) plus `LifecycleService::desired_fresh` (store
   read-through plus cache update; identical to `get` without a store).
3. Controller (runner): loop top `should_drain()` (latch OR journal marker
   OR fresh desired == draining; unreadable inputs are not drain orders),
   one-shot `drain_edge` (idempotent `set_drain` when newly draining plus a
   single observed write; failures are forensic, never block the exit),
   unchanged `drain_children` exit. `Store` + explicit lifecycle slug
   threaded through `supervise_from_daemon` (`ControllerLifecycle`; the
   hostname-derived ledger slug differs from the slot-prefix scope and is
   mapped explicitly; `None` = latch plus journal only). Reconcile permit
   loop and daemon `reserve_capacity_permits` gated on drain (race-window
   defense; the reducer would reject anyway). Reducer rejects
   `JobAcquisitionIntended` and `PermitReserved` when `drain_active`;
   in-flight intents still resolve/own/complete.
4. Slot/daemon readers (runner): `effective_draining()` (latch OR cached
   hint, 2s TTL, never blocks) at slot poll boundaries, broker idle/error
   boundaries, the acquire pre-check (skip + redeliver; in-flight acquires
   also cancel on journal drain), and daemon/backoff gates
   (`sleep_slot_retry_or_drain_in`, registration retry/backoff, JIT
   configure, successor prewarm). Retention lifecycle stays latch-only
   (store maintenance stops with the daemon; needs no journal path), and
   `node/slot.rs` heartbeat processes stay journal-free (the controller
   owns their lifecycle through `drain_children`).

NOT attempted (step 5, separate work): `mutate_instance` still fails closed
with "lifecycle reconciler is not installed". No drain-clear path either:
the marker is latched until the state dir is reset.

Drive-by: `execution/command_output.rs` test module gained the standard
panic-allow header; it failed `clippy -D warnings` on pristine tip.

## Compatibility

Old binaries open drained journals fine (unknown meta key ignored) and
simply do not drain. New binaries with the flag off behave exactly as
before. New binaries with the flag on honor a marker any writer latched.
Replay (`load_state`) never synthesizes drain (no event exists); the
materialized tables remain the drain source of truth, and the controller
re-latches every cycle it observes draining.

## Verification

- `cargo fmt --all --check`; `cargo clippy -p velnor-model -p velnor-control
  -p velnor-runner --all-targets -- -D warnings`: clean.
- New unit: 6 journal (round-trip, idempotent/monotonic, unknown-key
  tolerance + malformed fail-closed, none-on-missing/locked/corrupt,
  reducer gate incl. in-flight completion, stickiness), 2 store observed
  (advance + converge, unknown fails closed), 2 service (read-through +
  cache, no-store parity), 4 controller (should_drain table, edge-once,
  static edge, bind), 1 reconcile permit gate, 3 runner (latch parity,
  hint TTL cache, reserve gate).
- New integration: control `journal_drain` (a) cross-handle visibility +
  second-writer gate, (d) cross-handle observed convergence; runner
  `journal_drain_unified` (b) desired-draining drains end to end, (e)
  marker-only drain without ledger; runner `journal_drain_default_off`
  (c) flag-off ignores desired-draining (skips when the suite forces the
  flag on).
- Unchanged guards pass flag on AND off: `drain_preserves_active_jobs`,
  `drain_slot_decisions` truth table, retention lifecycle + retention
  integration (8).
- Suites: model 129+4+6, control lib 250 + integration 35, node lib 143,
  ops 34, node_arch 23: all pass. Full runner lib: 1980-1981 pass with
  3-4 flakes in the untouched checkout/git_mirror/github_adapter
  lease/env family; every one passes in isolation, the failing subset
  varies run to run, those modules reference none of the changed code,
  and the same family fails on pristine origin/main. Pre-existing.
