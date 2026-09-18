# ref-contracts-D — OpenRouter, OpenCode Go, omp (oh-my-pi), Hermes/Nous Portal

Fetch date (UTC): 2026-09-17.
Pinned SHAs (full): CodexBar `b6e65a83dc471817b7ff7678e68e0204c9dd604f`,
openusage `56378e5765f85d38ff413036fd984afe3d4664e4`,
oh-my-pi `116190d317ca319ae17ab624cb479c76a1ca4704`,
hermes-agent `f5d192611032025d2757b07ad838921872126182`,
opencode `5a8335857b0ebec44ef6aa1d52b339cf25c329ca`,
kimi-cli `86f136422a0aae6b217ea49e7ea1d2e8a1defcd2`,
grok-build `482711333c7195dc16a272777f86086d615e2afb`,
muse-code-sdk `94e98141a8074d50d2ac418de7acb5b16d8024fa`,
minimax-cli `bfbb4cb75ec343149eaccfd668c5011aa27bcf2b`.
Evidence classes: **D** = request/handler code at pinned SHA; **S** = secondary
(scrape / header / JWT-claim derived); **R** = referenced in docs/comments/fixtures
only; **U** = unverified or absent.

## 1. OpenRouter

Base default `https://openrouter.ai/api/v1` (override `OPENROUTER_API_URL` /
`OPENROUTER_BASE_URL`). Auth: `Authorization: Bearer <key>`. Every payload wrapped
in `{data: {...}}`.

**D — GET `{base}/credits`** (account balance).
→ `data {total_credits (lifetime added), total_usage (lifetime spent)}`;
balance = `total_credits - total_usage` (real zero shown, never "No data");
Credits meter only when ceiling > 0.
**D — GET `{base}/key`** (key metadata + period spend).
→ `data {usage_daily, usage_weekly, usage_monthly (Today/Week/Month $ rows),
limit, limit_remaining (Key Limit meter: used = limit-remaining),
is_free_tier → plan "Free tier"|"Pay as you go"}`.
Each endpoint maps independently; either may fail while the other renders.
Sources: `openusage/.../OpenRouter/OpenRouterUsageClient.swift`,
`OpenRouterUsageMapper.swift`; `CodexBar/.../Resources/Plugins/openrouter.js:33,58,88`.

**D — GET `https://openrouter.ai/api/v1/auth/key`** (identity check).
200 = valid key, 401 = invalid. Canonical paste-key (`sk-or-*`) validator —
public `/api/v1/models` returns 200 unauthenticated so it can't validate.
Source: omp `packages/catalog/src/compat/rules/auth/openrouter.kdl:27`,
`docs/provider-quirks.md:1318`.

**D — GET `https://openrouter.ai/api/v1/activity`** (cost history).
Queried by the CodexBar plugin for cost-usage rows (alongside credits/key).
Source: `openrouter.js:136`.

## 2. OpenCode Go (opencode.ai Zen)

**D(server+client) — GET `https://opencode.ai/zen/go/v1/usage`.**
Auth: `Authorization: Bearer <opencode-go API key>`; omp also sends
`User-Agent` + `x-opencode-session: <install id>` (required from 09/06).
Server (`opencode/packages/console/app/src/routes/zen/go/v1/usage.ts`, `GET`):
401 `{error: {type: "AuthError", message}}` on missing/invalid key;
403 `{error: {type: "EntitlementError", message: "OpenCode Go subscription
required."}}` when no Lite row; else 200
`{usage: {rolling, weekly, monthly: {status: "ok"|"rate-limited",
percent: 0-100 floored int, resetsAt: ISO (now + resetInSec)}}}`.
Backed by `LiteTable` rolling/weekly/monthly counters +
`Subscription.analyze{Rolling,Weekly,Monthly}Usage`.
Quota: 5h rolling + weekly + monthly percent windows; `rate-limited` ⇒ exhausted;
monthly anchors the **subscription anniversary**, not a rolling 30d (no durationMs).
Clients: openusage `OpenCodeUsageClient.swift` (GET + Bearer);
`CodexBar/.../OpenCodeGo/OpenCodeGoUsageFetcher.swift:28-31,220-222`
(`fetchAPIUsage`, tolerant parser: `usagePercent|usage_percent`,
`resetInSec|reset_at|...` variants, used/limit fallback);
omp `packages/ai/src/usage/opencode-go.ts:17-18,108-140` (strict parser, cites the
server route as first-party-but-undocumented).

**S — Zen balance web scrape.**
`https://opencode.ai/workspace/{id}[/go]` cookie fetch → `zenBalanceUSD`
("Zen balance" cost row); balance-only fallback when usage fields missing.
Legacy OpenCode (non-Go): `https://opencode.ai/_server` RPC + workspace/billing
pages. Sources: `OpenCodeGoZenBalanceFetcher.swift`,
`CodexBar/.../OpenCode/OpenCodeUsageFetcher.swift:28-29`.

**R — Related routes.** `https://opencode.ai/auth`; inference
`/zen/go/v1/{responses,messages,chat/completions}`, `/zen/v1/*`
(`opencode/packages/console/app/src/routes/zen/...`).

## 3. omp = oh-my-pi (the aggregator itself)

**U(n/a) — no single omp identity/usage endpoint.** oh-my-pi is a local CLI that
fans out to per-provider quota APIs and renders `/usage` + `/subscription` bars:
providers in `packages/ai/src/usage/{claude,openai-codex,openai-codex-reset,
cursor,zai,kimi,minimax-code,opencode-go,google-antigravity,gemini,xai-oauth,
muse-code,...}.ts`, auth in `packages/ai/src/registry/oauth/*`, credential
ranking/rotation in `packages/ai/src/usage.ts`, contract notes in
`docs/provider-quirks.md`. Per-provider contracts: files A (Claude/Codex),
B (Kimi/ZAI/MiniMax), C (Google/Cursor), E (Grok/Muse).

## 4. Hermes agent / Nous Portal

Portal base default `https://portal.nousresearch.com`
(env `HERMES_PORTAL_BASE_URL` / `NOUS_PORTAL_BASE_URL`, then stored state).
Auth: `Bearer <Nous token>` (30s token cache; 401 = invalid/session_revoked →
re-login; 403 `insufficient_scope` = missing `billing:manage` → device step-up,
`remote_spending_revoked` → reconnect; 429/503 carry `retry_after`, fail closed).
Money is decimal **strings**, never float.

**D — GET `/api/billing/state`** (role-tiered overview, no scope).
→ `{cliBillingEnabled, card {...}|null, balance_usd, min/max_usd, charge_presets,
portalUrl (relative by design), org fields, is_admin/can_change_plan...}`.
**D — GET `/api/billing/subscription`** (plan + tiers + usage, no scope).
→ `{current: {tierId|id, tier_name, monthly_credits, credits_remaining,
cycle_ends_at, pending_downgrade_*, cancel_at_period_end...}|null,
tiers: [{tier_id, name, tier_order, dollars_per_month, monthly_credits,
is_current, is_enabled}]}`.
**D — POST `/api/billing/subscription/preview** `{subscriptionTypeId}` →
`{effect: charge_now|scheduled|no_op|blocked, amount_due_now_cents,
monthly_credits_delta, effective_at, reason...}` (chargeless quote).
**D — PUT/DELETE `/api/billing/subscription/pending-change`** (set/clear single
end-of-period intent). **D — POST `/api/billing/subscription/upgrade`**
`{subscriptionTypeId}` + idempotency key (the single money route).
**D — POST `/api/billing/charge`** `{amountUsd}` + idempotency key;
**D — GET `/api/billing/charge/{id}`** (poll); **D — PATCH `/api/billing/auto-top-up`**
(auto-reload config).
Sources: `hermes_cli/nous_billing.py` (`DEFAULT_PORTAL_BASE_URL`, `_request`,
`get_billing_state`, `get_subscription_state`, `post_subscription_preview`,
`put/delete_subscription_pending_change`, `post_subscription_upgrade`,
`post_charge`, `get_charge_status`, `patch_auto_top_up`),
`agent/billing_view.py`, `agent/subscription_view.py`.
Portal deep-links: `/billing?topup=open`, `/manage-subscription`.

**S — Local Hermes usage.** `/usage` + `/subscription` bars from rate-limit tracker,
turn usage/pricing (`agent/turn_usage.py`, `usage_pricing.py`,
`rate_limit_tracker.py`); price catalogs `https://models.dev/api.json`
(`agent/models_dev.py:27`) + LiteLLM.
