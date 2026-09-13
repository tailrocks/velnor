# Plan 2026-09-13: lifecycle drain unification steps 1-4 (flag-gated)

Base: origin/main tip 97281ce7. Branch: r0-lifecycle-s6a. Worktree: /tmp/velnor-s6.

## Fix (this branch, strict scope)

Unify the signal latch, journal marker, and lifecycle desired state into one
drain signal. ALL behind `VELNOR_JOURNAL_DRAIN=1` (default off); with the
flag off the static path is behaviorally identical on marker-free journals
(every reader short-circuits to the latch). A latched marker drains even
with the flag off (fail-closed): the reducer arms are deliberately
unconditional and the controller loop honors the marker flag-off.

1. Journal drain state (control): `meta[drain]=requested:{version}`, NEVER a
   new `Event` variant (older binaries ignore the unknown key on reads —
   no schema bump — but any write by an older binary drops it: forward
   tolerance is read-only, see Compatibility). `FleetState.drain_active` +
   `drain_version` loaded in
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
   OR fresh desired == draining; unreadable inputs are not drain orders;
   the unreadable-desired diagnosis logs once per process), one-shot
   infallible `drain_edge` (idempotent `set_drain` when newly draining
   plus a single observed write; failures are forensic, never block the
   exit or the `drain_children` call that follows; the marker-changed
   bool is logged), unchanged `drain_children` exit. `Store` + explicit
   lifecycle slug threaded through `supervise_from_daemon`
   (`ControllerLifecycle`; the hostname-derived ledger slug differs from
   the slot-prefix scope and is mapped explicitly; `None` = latch plus
   journal only). Reconcile permit loop and daemon
   `reserve_capacity_permits` gated on drain (race-window defense; the
   reducer would reject anyway). Reducer rejects
   `JobAcquisitionIntended` and `PermitReserved` when `drain_active`
   (unconditional arms: fail-closed even with the flag off); in-flight
   intents still resolve/own/complete.
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
the marker is latched until the state dir is reset — except that an
old-binary write drops it (see Compatibility).

Drive-by: `execution/command_output.rs` test module gained the standard
panic-allow header; it failed `clippy -D warnings` on pristine tip.

## Compatibility

Old binaries open drained journals fine (unknown meta key ignored on
reads) and simply do not drain — BUT any write by an old binary clears a
latched marker: pre-drain `persist_state` rewrites `meta` without the
drain key (verified against base 97281ce7), and every `persist_state`
drops unknown keys. Mixed-version fleets must not share one journal
across an upgrade; a cleared marker re-latches on the next flag-on drain
edge that observes draining. Rollback to an old binary therefore
unlatches the drain rather than preserving it. New binaries with the flag
off behave as before on marker-free journals; a latched marker drains
even flag-off (fail-closed, honoring the reducer's unconditional arms).
New binaries with the flag on honor a marker any writer latched. Replay
(`load_state`) never synthesizes drain (no event exists); the
materialized tables remain the drain source of truth, and the controller
latches on the drain edge before exiting.

## Race windows closed (R1-R7)

Each race names the code that closes it and the test that pins it.

- R1 marker latched after the loop-top check, before permit
  reconciliation: `reconcile_once` re-checks `permits_gated`
  (`node/controller.rs`) and the reducer rejects `PermitReserved`
  (`control/journal.rs`). Tests:
  `reconcile_skips_new_permits_while_journal_drain_is_latched`,
  `reducer_rejects_new_permits_and_acquisitions_when_draining`, (a).
- R2 acquisition starting (or in flight) while a drain latches:
  `handle_v2_message` skips on `journal_drain_hint` and in-flight
  acquires cancel via `wait_for_drain_signal_in` (`runner.rs`); the
  broker redelivers to a live runner. Test:
  `journal_drain_hint_caches_per_path_with_ttl` (hint mechanism; no
  dedicated acquire test).
- R3 stale cached desired missing another process's drain write:
  `LifecycleService::desired_fresh` reads through the store and refreshes
  the cache (`control/lifecycle.rs`). Tests:
  `desired_fresh_reads_through_a_stale_cache_and_updates_it`, (b).
- R4 two writers latching concurrently: `set_drain` runs under an
  immediate transaction and the version never regresses (`max`) with an
  idempotent no-op on equality (`control/journal.rs`). Tests:
  `set_drain_is_idempotent_and_monotonic`, (a).
- R5 observed write racing another observed/desired writer:
  `record_lifecycle_observed` uses expected-version OCC on the shared
  monotonic version; stale converges (re-read row, no error, no retry)
  (`control/store/records.rs`). Tests:
  `observed_write_advances_and_stale_version_converges_without_retry`, (d).
- R6 slot/daemon reader blocking on a writer's lock at a poll boundary:
  `read_drain_state` uses a throwaway read-only connection with a zero
  busy timeout (`control/journal.rs`); the runner's 2s-TTL hint cache
  uses `try_lock` exclusively so contention degrades to a fresh read
  (`runner.rs`). Tests:
  `read_drain_state_returns_none_on_missing_locked_or_corrupt`,
  `journal_drain_hint_caches_per_path_with_ttl`.
- R7 fresh capacity admitted while draining: daemon
  `reserve_capacity_permits` and the controller permit loop check the
  marker first, and the reducer rejects either event anyway
  (`runner.rs`, `control/journal.rs`). Tests:
  `reserve_capacity_permits_reserves_nothing_while_draining`,
  `reconcile_skips_new_permits_while_journal_drain_is_latched`, (a).

## Verification

- `cargo fmt --all --check`; `cargo clippy -p velnor-model -p velnor-control
  -p velnor-runner --all-targets -- -D warnings`: clean.
- New unit: 8 journal (round-trip, idempotent/monotonic, unknown-key
  tolerance + malformed fail-closed, none-on-missing/locked/corrupt,
  reducer gate incl. in-flight completion, stickiness, persist drops
  unknown keys, old-writer unlatch), 2 store observed (advance +
  converge, unknown fails closed), 2 service (read-through + cache,
  no-store parity), 6 controller (should_drain table, edge-once, static
  edge, write-failure still drains children, unreadable journal
  tolerated, bind), 1 reconcile permit gate, 3 runner (latch parity,
  hint TTL cache, reserve gate).
- New integration: control `journal_drain` (a) cross-handle visibility +
  second-writer gate, (d) cross-handle observed convergence; runner
  `journal_drain_unified` (b) desired-draining drains end to end, (e)
  marker-only drain without ledger; runner `journal_drain_default_off`
  (c) flag-off ignores desired-draining (skips when the suite forces the
  flag on) plus a non-skippable subprocess test (stale marker drains
  with the flag off; the child re-executes with the flag removed).
- Unchanged guards pass flag on AND off: `drain_preserves_active_jobs`,
  `drain_slot_decisions` truth table, retention lifecycle + retention
  integration (8).
- Suites: model 129+4+6, control lib 250 + integration 35, node lib 143,
  ops 34, node_arch 23: all pass. Full runner lib: 1980-1981 pass with
  3-4 flakes in the untouched checkout/git_mirror/github_adapter
  lease/env family; every one passes in isolation, the failing subset
  varies run to run, those modules reference none of the changed code,
  and the same family fails on pristine origin/main. Pre-existing.
