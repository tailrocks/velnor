# Lane prov-e report — Muse / omp / Hermes usage adapters

Branch: `feat/multi-account-support` (shared workspace). No commit made.

## Files created (NEW only)

- `crates/jackin-usage/src/usage/muse.rs` — MSP `usage/read` + `usage/changed`
  cached observation: `observedAtMs`, tier, rolling window
  (`usedPercent`/`resetsAtMs`/`windowDurationMins`), weekly window.
- `crates/jackin-usage/src/usage/omp.rs` — attribution adapter: underlying
  provider buckets pass through unchanged; native pool filters/counters map to
  zero buckets; broker pool file documented as routing, not authorization.
- `crates/jackin-usage/src/usage/hermes.rs` — attribution adapter: underlying
  provider buckets + Nous Portal subscription (decimal strings, never float);
  tracker counters map to zero buckets; exclusive profile ownership.

## `usage.rs` wiring (exactly 3 lines, nothing else)

- `mod hermes;` after `mod grok;`
- `mod muse;` + `mod omp;` after `mod minimax;` (alphabetical: muse, omp, opencode)

Other `usage.rs` hunks in the working tree belong to concurrent lanes
(antigravity/cursor/gemini); not touched by this lane.

## Contract coverage

- Muse: omitted `usage` → `Ok(None)` (honest no-observation, never error);
  over-100% preserved raw in `used_label` (`142.5% used`), remaining clamps to
  0 per Bug-11 convention; window label from `windowDurationMins` via
  `window_minutes_label`; `muse_freshness_epoch` keeps cached
  `fetched_at_epoch` when re-read `observedAtMs` is unchanged; view maps to
  `FocusedUsageView` with source `Cache`, confidence `Authoritative`,
  plan = tier, `Session`/`Weekly` slots.
- Key exchange: NO fetch function exists by design. `MuseKeyExchangePolicy`
  documents URL, request shape, 7-field non-secret whitelist, 5-field secret
  denylist, and `polling_enabled() == false` (tested, incl.
  whitelist/disjointness).
- omp: `omp_attribute` preserves buckets verbatim (tested identity);
  `omp_pool_routing_buckets` always empty; broker-pool-file const +
  `omp_broker_pool_is_authorization() == false`; view attributes underlying
  provider/account with origin `omp provider entry`.
- Hermes: `HermesRuntime::for_profile` always exclusive; Portal `current: null`
  → `Ok(None)`; decimals verbatim (`173.50 left` / `200.00 total`, no float
  round-trip, no derived percents); 401/403 → `NeedsLogin`, else `Error`
  (fail closed); tracker counters always empty; view appends portal bucket to
  underlying buckets, plan = tier, origin names the profile.
- Mapping uses EXISTING `QuotaBucketView`/`FocusedUsageView` types only (struct
  literals + `timed_bucket`/`with_status_slot`/`status_bar_quota_labels`;
  no `UsageSurface` additions since `usage.rs` was otherwise frozen).
- Fixtures sanitized (`operator@example.com`, `example-model-*`); no secrets.

## Verification (observed)

- `cargo test -p jackin-usage --lib usage::` → 231 passed, 0 failed;
  lane-E tests: 13 muse + 5 omp + 8 hermes = 26, all ok.
- `cargo build -p jackin-usage` → success, zero warnings.
- `rustfmt --edition 2024 --check` on the 3 new files → clean.
- `cargo clippy -p jackin-usage` → BLOCKED by other lanes: `jackin-config`
  (e.g. `accounts/stores/{hermes,omp}.rs`, `discovery.rs`) currently fails
  clippy; no clippy diagnostic names any lane-E file. Re-run after the tree
  settles.
- `cargo fmt --check` (workspace) → red in other lanes' files only.

## Notes for integrator

- Adapters are intentionally unwired from dispatch (no `UsageSurface`
  variants, no re-exports): broker/dispatch wiring is a follow-up once the
  surface enum is unfrozen.
- `#[expect(clippy::cast_sign_loss)]` on the Muse remaining-percent cast
  follows the `amp.rs`/`antigravity.rs` precedent exactly.
