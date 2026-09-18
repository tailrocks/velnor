# Lane report: broker cadence (coordinator.rs)

Branch `feat/multi-account-support`. Scope held: **only
`crates/jackin-usage/src/coordinator.rs` modified** (+540/-2, uncommitted).
`coordinator/` subdir, `Cargo.toml`, all other files untouched.

## What was built (spec §7)

Per-account in-memory cadence (`AccountCadence`: activity tier, low-power
flag, next due epoch) hung off the existing `AccountEntry`; no schema or
persistence change, no new threads. New accounts start due immediately so
screen-open freshness flows through the broker.

- `set_activity(cap, UsageActivity, low_power, now)` — select tier
  (DirectInteraction/Recent/Idle/LongIdle reuse `policy::cadence`: 2/5/15/30
  min, low-power forces 30 min). Due accounts stay due; otherwise due time
  only moves earlier. Dispatches no provider work.
- `poll_due(now) -> Vec<UsageGenerationView>` — one ambient (non-force)
  refresh per due account. Rides the existing `request_refresh` single-flight
  + shared deadline checks, so a second requester joins in-flight work and
  Retry-After/rate-limit/success deadlines always win. After each poll the due
  time advances to the max future shared deadline, else one jittered cadence
  interval — one call can never emit a burst.
- `next_due_epoch() -> Option<i64>` — earliest due time for scheduler sleep.
- `note_wake(now) -> usize` — sleep/wake/reconnect: every missed due time
  becomes one deterministic jittered cadence deadline from now; future due
  times untouched; dispatches no provider work. Returns recalculated count.
- `cadence_deadline()` — tier cadence plus deterministic `[0, cadence/4]`
  FNV-1a skew seeded by capability+generation, so accounts de-sync and joined
  callers agree.

Single-flight/join semantics needed no change: winner/joiner, stale-observed
adoption, and forced-manual-join already hold in `request_refresh`; the new
test pins the fresh-observed force + repeated-`r` + ambient join case.

## Tests (new `cadence_tests` module inside coordinator.rs)

Self-contained fixtures (existing `coordinator/tests.rs` is out of lane, and
`jackin-test-support` is not a dev-dep, so plain epoch math, no time
harness):

1. Tier intervals match spec with bounded jitter + determinism.
2. `poll_due` fires once per interval; shared success cooldown wins over the
   shorter active cadence.
3. `note_wake` after 10 h sleep: 1 recalculation, 0 dispatches, then exactly
   1 poll at the jittered due time (no missed-poll burst).
4. Provider `Retry-After` wins over periodic due; fires exactly at deadline.
5. Two-client single-flight: periodic winner + fresh-observed forced manual +
   repeated manual + ambient joiner → exactly 1 provider call.

## Verification

- Isolated tree (`git archive HEAD` + this file, strict lints, no
  downgrade): `cargo test -p jackin-usage coordinator` → **27 passed,
  0 failed**; `cargo test -p jackin-usage cadence` → 6 passed (5 new + policy
  matrix); `cargo clippy -p jackin-usage --all-targets` → 0 warnings;
  `rustfmt --check` clean.
- Working tree: `cargo check -p jackin-usage` clean. Full `cargo test` is
  currently broken by a **concurrent lane's** edit to
  `crates/jackin-usage/src/host/broker/tests.rs:411,625`
  (`Arc::clone(&executor)` pinning `Arc<CountingExecutor>` against
  `Arc<dyn UsageProviderExecutor>` — same coercion trap) plus pre-existing
  `-D dead-code` failures in `host/broker/view.rs`. Both are outside this
  lane; left untouched. Earlier in the session, before that edit landed, the
  working tree ran the same 27/27 green with `--cap-lints warn`.

## Notes for integrator

- `poll_due` covers known (loaded) accounts only; screen-open still calls
  `request_refresh` per account per spec, which also loads them.
- Default tier for untouched accounts is `Idle`; the driver should call
  `set_activity` (e.g. `DirectInteraction` while a monitoring screen is
  visible).
- Not committed, per instructions.
