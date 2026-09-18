# Lane B report — Kimi / Z.AI / MiniMax usage parsers

Date (UTC): 2026-09-17. Branch: `feat/multi-account-support`.
Scope: ONLY `crates/jackin-usage/src/usage/{kimi,zai,minimax}.rs` (+ their tests,
kept as per-file `#[cfg(test)] mod tests` so the shared `tests.rs` needed zero
edits). No Agent-enum, catalog, or other cross-file edits. No commit.

Inputs read first: `/tmp/ref-contracts-B.md`, `/tmp/audit-t01-providers.md`,
`jackin-provider-research.md` §10–11+17, `plans/multi-account/001-t02-domain-contracts.md`.

## 1. Kimi (`usage/kimi.rs`)

- **Old/new response families.** `usages` is now an untagged `KimiUsages` enum:
  web-gateway list (`[{scope: FEATURE_CODING, detail, limits[]}]`, legacy
  behavior byte-preserved incl. `Rate Limit`/`Weekly` labels) vs Code-API pools
  object (`{limit_5h, limit_7d, limit_month_total}`). Counts accept string OR
  number (`KimiCount`); resets accept `resetTime/reset_time/reset_at/resetAt`
  as RFC 3339, epoch seconds, or epoch ms (`KimiReset`). `KimiWindow` gains
  `TIME_UNIT_SECOND`.
- **Rolling/weekly/monthly pools.** `limit_5h` → `5-hour` (Session slot),
  `limit_7d` → `Weekly` (Weekly slot), `limit_month_total` → `Monthly`
  (detail-only; `StatusSlot` has no monthly variant). Pools use explicit
  used/limit first, else `used_ratio` (≤1 fraction, >1 percent). When pools are
  present they supersede the coarser `usage` summary + `limits[]` so no window
  renders twice. Per-pool `name`/`title` overrides the default label.
- **Extra-usage wallet.** New `KimiLocalUsage` (`summary`/`limits` opaque,
  `extra_usage`, in-band `error` rejected) + `KimiExtraUsage`
  (balance/total/monthly cap/used, liberal aliases, minor units) mapping to an
  `Extra usage` Spend-slot `Money` bucket (monthly used/of cap, else consumed
  of total). Currency falls back to generic `credits`, never assumed fiat.
  `fetch_kimi_local_usage(base_url, credential)` provided; NOT wired into
  `kimi_snapshot` — local-server URL/port discovery is unverified (install-time
  gate), so default probing would be invention. Broker lane owns discovery.
- **Shared identity across clients.** `kimi_account_identity()` surfaces
  email→name→id as account/username and `user.membership.level` as plan:
  LEVEL_FREE/TRIAL/BASIC/INTERMEDIATE/ADVANCED →
  Adagio/Andante/Moderato/Allegretto/Allegro, applied only when `version` is
  absent or `GOODS_VERSION_V1` (other versions → humanized raw level). Snapshot
  now fills account/username/plan so the broker can dedup by (service +
  billing subject) across Kimi/Claude/Codex clients funding from one account.
- **Endpoint.** `KIMI_CODE_BASE_URL` override with `.../coding[/v1]` →
  `.../coding/v1/usages` normalization (`kimi_usages_url_from_base`, pure +
  tested). Fetch maps 401 invalid key / 403 denied / 404 endpoint distinctly.
- All mapping lands in existing `QuotaBucketView`/`FocusedUsageView` (+`Money`).

## 2. Z.AI (`usage/zai.rs`)

- **CREDIT_LIMIT/TOKENS_LIMIT** both slot by explicit duration (unchanged
  classifier); strict `i64` field types preserved so 2xx non-integer fields
  fail decode instead of silently defaulting.
- **Credit rate.** `zai_is_peak()` derives the Mon–Fri 06:00–10:00 UTC 1× /
  0.5× rate client-side from the clock; CREDIT_LIMIT buckets carry it in pace
  (`peak 1× rate` / `off-peak 0.5× rate`). Token buckets unaffected.
- **Period/reset metadata.** `nextResetTime` via `epoch_seconds_from_maybe_ms`
  (was blind `/1000`); `(unit, number)` documented (1=day, 3=hour, 5=minute,
  6=week; unknown codes → no window, never guessed).
- **MCP/time quotas.** `usageDetails[{modelCode, usage}]` parsed; top-two
  models appended to pace (`top glm-5 80% · glm-4.5 20%`). `TIME_LIMIT` splits
  by window: <28d → `MCP` (existing count line), ≥28d → `Web search` (monthly
  web-search count per ref §2).
- **Team selectors.** `ZaiTeamScope` (`ZAI_QUOTA_TYPE` → `?type=N`,
  `BIGMODEL_ORGANIZATION`/`ZAI_TEAM_ORG`, `BIGMODEL_PROJECT`/`ZAI_TEAM_PROJECT`
  → headers), pure `zai_team_scope_from` tested; plan label gains `· team`.
  `resolve_zai_quota_url_from` arity unchanged (shared test pins it).
- **HTTP-success error envelopes.** `success:false` → distinct `Z.AI key has no
  GLM Coding Plan (...)`; `code!=200` keeps prior wording; 2xx with zero
  windows → explicit error (team hint when team scope active, entitlement hint
  otherwise) instead of empty-but-fresh quota.

## 3. MiniMax (`usage/minimax.rs`)

- **Token Plan vs PAYG.** `minimax_key_product()`: `sk-api-*` → PAYG
  (`GET {base}/account/query_balance`), else Token Plan. New
  `MiniMaxBalanceResponse` (+validate) maps decimal-string amounts to a
  `Balance` bucket: available in used-label, `USD · cash … · voucher … ·
  credit … · owed …` pace line with region currency. Amounts stay in labels
  (funds ≠ used-of-limit, so no mislabeled `Money`); `minimax_decimal_minor()`
  parses to minor units exactly (truncate, no float). PAYG plan label `PAYG`;
  fetch-failure placeholder is `Balance` vs `Coding plan` by key shape.
- **Boosts/unlimited.** `interval/weekly_boost_permille` parsed under both
  spellings (±`current_` prefix). Rendered remaining = base × boost/1000, can
  exceed 100 (raw kept per T02 F-rules; only the `u8` carrier bounds it);
  `+N% boost` pace note. Status 3 → `Unlimited` bucket (no percent);
  status 2 → 0% + `Exhausted`; unknown codes still skipped. General-model slot
  rules and weekly gating unchanged.
- **Region/currency.** `MiniMaxRegion` (global/USD, CN/CNY); explicit
  `MINIMAX_REGION` (cn/china/minimaxi) or CN host override selects CN, else
  global — the previous global+CN fan-out violated research §17 (never send a
  credential cross-region on failure) and is replaced by a region-pinned
  `MiniMaxFetchPlan` (pure `minimax_fetch_plan_from`, tested). Pinned legacy
  resolver kept byte-identical (shared tests). Credential origin gains the
  serving host (`· api.minimax.io`); telemetry path adds
  `/account/query_balance`.
- **`remains_time` fix.** Live values are millisecond durations (14_400_000 =
  4h; seconds reading gave +166d/+11y resets). `minimax_duration_seconds()`
  normalizes >1e6 as ms; explicit `end_time` still wins. No shared test pinned
  the old reading.

## 4. Tests

New per-file `#[cfg(test)] mod tests` (inline sanitized `json!` fixtures, no
secrets, no separate fixture files needed): 6 Kimi (pools/identity/version-gate/
numeric-detail/wallet/URL), 5 Z.AI (credit rate+models/peak window/MCP-vs-search/
team/model-note), 8 MiniMax (product/plan/boost/exhausted+unlimited/balance/
decimal/duration/operation-path).

## 5. Verification (observed)

- `cargo test -p jackin-usage --lib kimi` → 14/14 ok (clean, uncapped).
- `cargo test -p jackin-usage --lib zai` → 9/9 ok (clean, uncapped).
- `cargo test -p jackin-usage --lib minimax` → 15/15 ok (clean, uncapped).
- Full `-p jackin-usage --lib`: 382 pass, 3 fail — all 3 are
  `host::tests::*` (`credential_matrix…`, `host_surfaces…`,
  `canonical_projection…`, e.g. `matrix missing google`), owned by the
  catalog/host lane and untouched by this diff (my files export no symbols
  they consume; failures reproduce on lane-A state).
- `cargo clippy -p jackin-usage --lib --tests`: zero findings in the three
  owned files. `rustfmt --check` clean on all three.
- Mid-task the build was blocked twice by S1-lane `jackin-config` breakage
  (dead_code deny, then E0432/E0599 mid-refactor); verified interim with
  capped lints, then re-verified clean once it settled. No foreign files
  touched at any point (`git status` on owned paths: only the 3 files).
- Not committed, per instructions.

## 6. Judgments / deferred items for orchestrator

- Kimi local-server wallet has mapping + fetch helper but no snapshot wiring
  (needs verified local-server URL/credential discovery).
- Kimi pools supersede (not append to) the coarser summary/`limits[]` to avoid
  duplicate meters; CodexBar shows both — a product call if parity is wanted.
- Z.AI monthly `TIME_LIMIT` labeled `Web search` (vs `MCP`) purely by the ≥28d
  window threshold from ref §2; no sharper discriminator exists in the quota
  response.
- MiniMax PAYG currency follows region convention (global→USD, CN→CNY); the
  balance endpoint returns no currency code.
- `MiniMaxFetched` changes `fetch_minimax_usage`'s return type (internal
  `pub(crate)`, sole caller is `minimax_snapshot`); `minimax_bucket` gained a
  boost parameter (no external callers).
