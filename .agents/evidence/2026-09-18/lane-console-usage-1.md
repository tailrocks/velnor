# Lane report: Console Usage phase-1

Branch: `feat/multi-account-support`. No commit. Files touched (only):
`crates/jackin-console/src/tui/screens/usage.rs`,
`crates/jackin-console/src/tui/screens/usage/tests.rs`,
`crates/jackin-console/src/tui/input/list.rs` (usage-open block),
`crates/jackin/src/console/adapter/run.rs` (usage-refresh fns).

## What changed

**Canonical IDs kept (no protocol changes).** `UsageAccount` now carries
`provider_id`, `canonical_account_id` (capability id while `unresolved`),
`lifecycle`, `freshness_phase`, `last_good_at_epoch`, `is_stale`;
`UsageWindow` carries `window_id`, `used_percent`, `reset_at_epoch`.
`from_projection` fills all of them. `stable_id()` =
`{provider_id}:{canonical_account_id}`.

**Stable-ID selection.** `selected_id` re-anchored in `move_selection`;
`apply_refresh` re-anchors by id across rename/reorder; removed selection →
Overview + inline notice (appended to, not clobbering, the unresolved notice).
`set_accounts` removed; all callers owned and migrated.

**Async refresh, no UI-thread broker work.** `UsageScreenState` holds
`refresh_rx: Option<BlockingSubscription<UsageRefreshOutcome>>` (same shape as
ManagerState's `instances_refresh_rx`), plus `refresh_due` /
`last_refresh_at`. `refresh_console_usage_on_key` now only polls one outcome,
applies it, and spawns `load_console_usage_state(paths, true)` via
`spawn_blocking_subscription` when due and none in flight. `r` sets
`refresh_due` in the route and joins in-flight work (flag consumed, no dup
spawn). Errors advance the timer (heartbeat-cadence retry, never per-key
hot loop). The 10s join still exists but runs on the worker thread.

**Heartbeat.** `USAGE_HEARTBEAT_INTERVAL = 2min` (matches the spec's existing
direct-interaction cadence). Open always marks due, so the first broker read
lands right after open. Completions reset the timer: no sleep catch-up burst,
max one in flight.

**Render.** Detail/overview show all windows + per-account freshness ages
(`updated just now/5m ago/2h ago/2d ago`, `stale · updated …`,
`never updated`, `refreshing…`), used% mirrored to meter geometry, and a
`Refreshing usage…` indicator while in flight. `meter_line` returns `None`
for unknown percents: no fabricated empty bars. List rows also show the age.

## Verification (clean worktree + my 4 files)

Shared tree currently does NOT compile: sibling S1 work breaks
`jackin-config` (69 dead-code/unreachable-pub denies) and `jackin-console`
(`auth_impls.rs` non-exhaustive `AiProvider` match). Verified instead in
`/tmp/jackin-usage-verify` (worktree at HEAD abc2ca20 + my files):
- `cargo test -p jackin-console usage`: 20 passed, 0 failed (incl. new
  narrow 40x20 / wide 120x30 render tests, stable-id, freshness, poll tests)
- `cargo test -p jackin-console --lib`: 1270 passed, 0 failed
- `cargo test -p jackin --lib console::`: 106 passed, 0 failed
- `cargo clippy -p jackin-console -p jackin --all-targets`: zero warnings
  in touched code; `cargo fmt --check`: clean

## Known seams / deferred

1. Poll/apply is keypress-driven (`refresh_console_usage_on_key` runs per key
   event): an idle open screen refreshes on the next keypress, not on tick.
   Tick-driven apply needs the loop-owned `drain_background_messages` /
   `poll_background_messages` (other lanes' files). Screen API is already
   shaped for it: a tick caller just needs `poll_refresh` +
   `heartbeat_due` + `begin_refresh`. Spec asks for updates without
   keypresses — that last hop is phase-2, loop-owner work.
2. Startup's current-only snapshot read stays synchronous (call site not
   owned); it performs no refresh/join.
3. `run/tests.rs` untouched (not owned): run.rs glue covered indirectly via
   screen-method tests; no direct broker test (would spawn broker threads).
