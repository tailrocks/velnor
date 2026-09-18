# Lane T10 — canonical metric groups (report)

Branch `feat/multi-account-support`, shared workspace. No commit (per instructions).
`/tmp/audit-usage.md` absent; designed from `plans/multi-account/001-t02-domain-contracts.md` §6,
`jackin-accounts-usage-specification.md` §6, and full reads of `usage_broker.rs` + `projection.rs`.

## Owned files changed (only these 4; `control.rs` untouched — no `FocusedUsageView` change needed)

- `crates/jackin-protocol/src/usage_broker.rs` (+tests)
- `crates/jackin-usage/src/host/projection.rs` (+tests)

## What was implemented

1. **Typed metric groups** — `UsageAccountV1.metric_groups: Vec<UsageMetricGroupV1>`
   (`#[serde(default, skip_serializing_if)]`, wire-compatible with old JSON).
   Kinds: window / balance / spend_cap / token_totals / rate_limit / plan.
   Each group: stable `group_id`, `rank`, `label`, `UsageMetricScopeV1`
   (service/model/pool/key_id), `observed/fetched/last_success` epochs, freshness
   `phase` + `is_stale`, `quota_state`, typed `UsageMetricValueV1` payload, `reset_at_epoch`,
   `renews_at_epoch`, per-group `issues` (scope must be new `UsageIssueScopeV1::Group`).
   Money reuses `control::Money` (currency+exponent); periods are typed
   (`Rolling`/`Calendar`/`ProviderDefined`/`Unknown`). `windows` keeps principal-window
   semantics unchanged (documented on the type).
2. **Raw over-100% preservation** — `UsagePercent::clamp_raw/split_raw/meter_fill`;
   `remaining_raw_percent`/`used_raw_percent` (i32) beside clamped percents on windows and
   window-group payloads. Checked/saturating math only; raw requires its clamped pair and
   must equal its clamp, else `validate()` fails. Geometry uses clamped values only.
3. **Distinct states, no collapsing** — new `UsageQuotaStateV1::{NoPermission, Unknown,
   NotApplicable}` (wire: `no_permission`/`unknown`/`not_applicable`; all 10 variants
   serialization-tested). Projection un-collapsed `NeedsLogin`/`NeedsSecret` (were
   `Unsupported`, now `NoPermission`); fresh quantity-less buckets are `Unknown` (never `0%`
   or `Available`); money over-cap reads `Exhausted`; plan/token/uncapped-spend groups are
   `NotApplicable`. `UsageLifecycleV1` intentionally NOT extended: console `usage.rs` has an
   exhaustive match on it (other lane) — lifecycle coverage is via precise mapping instead.
4. **reset vs credential-expiry vs renewal** — window/group `reset_at_epoch` (allowed only on
   window/spend_cap/rate_limit kinds, enforced), account `credential_expires_at_epoch`, plan
   group `renews_at_epoch` (plan-only, enforced). Projection leaves credential-expiry and
   renewal `None`: no signal exists in current views, nothing borrowed. Tested independent.
5. **Projection builder** — `project_groups()` from existing views only (no collector
   changes): one window group per bucket (mirrors percents/raw/reset/quota), one spend-cap
   group per monetary bucket (structured cap/spent/remaining, ratio-derived state with
   checked math), one plan group per plan label. Stable ids via
   `account_key_hash(.., "canonical-group-v1:{rank}")`; per-group epochs (observed=fetch,
   last-success iff bucket usable); `validate()` extended (ranks, unique ids, kind/value
   match, spend single-denomination, reset/renewal kind rules, group issue scope).
6. **Shared-quota dedup** — `UsageQuotaScopeKey { service, billing_subject, scope,
   model?, key? }` + length-prefixed `dedup_key()` + `shares_allowance()` (full equality).
   Never-merge-independent-caps rule documented on the type and tested (key-a vs key-b,
   scoped vs unscoped, `("ab","c")` vs `("a","bc")`, `None` vs `Some("")`).

## Verification (observed)

- `cargo test -p jackin-protocol usage` → 23 passed, 0 failed
- `cargo test -p jackin-protocol` (full) → 105 + 6 passed, 0 failed
- `cargo test -p jackin-usage projection` → 17 passed, 0 failed
- `cargo test -p jackin-usage` (full) → 412 passed, 0 failed
- `cargo clippy -p jackin-protocol --all-targets` → zero errors/warnings
- `cargo clippy -p jackin-usage --all-targets` → zero hits in owned files
- `rustfmt --check` clean on all 4 owned files; frozen fixture round-trip test still passes

## Known cross-lane fallout (for orchestrator/T11, not fixable from this lane)

- New struct fields break exhaustive literals in
  `crates/jackin-console/src/tui/screens/usage/tests.rs` (3× `UsageAccountV1`, 2×
  `UsageLimitWindowV1` need `metric_groups: vec![]`, `credential_expires_at_epoch: None`,
  `remaining_raw_percent: None`, `used_raw_percent: None`). Wire JSON is backward/forward
  compatible. Console lib code itself (`usage.rs`) only reads fields and is unaffected.
- `cargo clippy -p jackin-usage --all-targets` currently fails on pre-existing errors in
  other lanes' files (`usage/cursor.rs`, `usage/antigravity.rs`, `usage/opencode.rs`,
  `usage/openrouter.rs`, … — shifting between runs as lanes edit); none are in owned files
  and none were touched.

## Deferred (needs provider-collector lanes, not this one)

- Group scope labels (model/pool/key), balance/token-totals/rate-limit groups, renewal and
  credential-expiry signals: wire shape exists and validates; projection leaves them unset
  until collectors supply them (likely additive `control.rs` fields by provider lanes).
