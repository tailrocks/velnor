# Lane report: broker subscription lane

Branch `feat/multi-account-support`, shared workspace. Owned files only:
`crates/jackin-usage/src/host/broker.rs`, `broker/{probe,publish,view,waits,tests}.rs`.
No other file touched. No commit.

## What was built (spec §7, existing types only)

1. **View subscribe/unsubscribe** (`broker/view.rs`, methods on the existing
   `UsageBrokerClient`): `subscribe_all` (due-on-open), `subscribe`,
   `unsubscribe`, `unsubscribe_all`, `refresh_due`, `subscriptions`,
   `observed_generation`. Placement note: `host.rs` (outside this lane) cannot
   gain a re-export here, and the crate denies `unreachable_pub` + `dead_code`,
   so a new exported view type is unshippable from this lane. The subscription
   set rides on the already-exported client instead; `Clone` forks the set so
   two screens never share subscriptions. Unsubscribe performs zero broker I/O,
   so it can never cancel a generation another client owns or awaits; broker
   ownership always runs to terminal (joins only observe).
2. **Due-on-open** (`subscribe_all` + `refresh_projection` dispatch): per-account
   `current` then `refresh(observed, force=false)`; duplicates deduped via
   `BTreeSet`/`request_refresh_all`. Still-fresh data reused through the
   coordinator success cooldown; active work joined via single-flight; retry and
   rate-limit deadlines always win. Server-side `RequestRefresh` now runs real
   due checks over observed accounts instead of returning a static projection.
3. **Incremental publication** (`broker/publish.rs`): `ProjectionPublisher`
   merges each traffic-observed account independently into `UsageProjectionV1`
   (settled order, canonical ranks, `validate()`d; invalid output keeps the
   last-good publication). `broker_generation`/`projection_id` advance per
   change; `discovery_revision` is fixed for the process lifetime. Per-account
   published `(generation, phase)` only moves forward, so older generations
   never regress a publication and unknown accounts are never fabricated.
   Publishing happens on every per-account dispatch, in a 200 ms ticker while
   non-idle, and inside `JoinPublication` waits. `JoinPublication` returns
   superseded/settled publications immediately and reports `WaitTimeout` without
   touching ownership.
4. **Probe budgets** (`broker/probe.rs` + `DiscoveryProviderExecutor`): the
   blocking provider call (child CLI/RPC, secret resolution, rediscovery) runs
   on a worker thread under `config.coordinator.provider_timeout` (default 30 s).
   This matters because the coordinator only classifies elapsed time *after* a
   probe returns — previously a hung child probe held a coordinator worker
   forever. Expiry returns a typed `ProviderTimeout` failure through the normal
   path (last-good preserved, retry lifecycle intact); the late worker result is
   dropped; worker panics propagate so the coordinator still classifies
   `OwnerLost`. No adapter declares a budget over 30 s
   (`PROVIDER_CLI_TIMEOUT` 10 s, `GROK_RPC_REQUEST_TIMEOUT` 12 s), so the single
   shared knob cannot invalidate a documented adapter budget.

## Verification (observed)

- `cargo test -p jackin-usage broker`: **23 passed, 0 failed** (18 in
  `host::broker`: 10 pre-existing + 8 new; rest are pre-existing matches elsewhere).
- New tests: open dedup/fresh-reuse/force-once; unsubscribe-never-cancels;
  clone-forks-subscriptions; healthy-publishes-while-one-stalls (same catalog
  revision, `validate()` clean); projection refresh due-checks + join settles;
  publication-join timeout preserves ownership; budget fast/expire/panic.
- `cargo clippy -p jackin-usage --all-targets`: zero findings in `host/broker`.
- `cargo fmt -p jackin-usage -- --check`: clean.
- One self-caught flake fixed during development: probe-count asserts now join
  forced generations first instead of racing async dispatch.
- Full-crate runs were intermittently blocked by concurrent lanes (broken
  `usage/kimi.rs`, then `jackin-config` mid-refactor); both outside this lane,
  resolved without touching them. Final lane verification ran green after the
  tree settled.

## Notes for integration

- Console/screen code consumes `UsageBrokerClient::{subscribe_all, refresh_due,
  unsubscribe, unsubscribe_all}` — already reachable via the existing `host.rs`
  re-export; no other-file change needed for this API.
- First-ever `RequestRefresh` on a fresh broker publishes only traffic-observed
  accounts; the documented open path (`subscribe_all` first) seeds the set.
  Seeding from discovery bindings would require changing the
  `run_usage_broker_service_with_executor` signature, which external callers
  (ffi bridge, runtime relay, e2e) share — deliberately not done.
- True child-process *kill* on cancellation lives below this lane (discovery /
  adapter own the child handles); this lane bounds the broker-side wait and
  drops late results. Adapter timeouts (≤ ~12 s) bound the detached worker.
