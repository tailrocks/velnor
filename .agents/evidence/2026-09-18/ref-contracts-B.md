# ref-contracts-B — Kimi/Moonshot, Z.AI, MiniMax

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

## 1. Kimi / Moonshot

**D — GET `https://api.kimi.com/coding/v1/usages`** (Code quota).
Auth: `Authorization: Bearer <API key or OAuth token>`, `Accept: json`.
Base override `KIMI_CODE_BASE_URL`; path normalized so any `.../coding[/v1]` base
resolves to `.../coding/v1/usages`.
Response: `usage {limit, used|remaining (used = limit-remaining), reset_time|reset_at|
reset_time snake variants, name|title}`; `limits[] {window {duration, timeUnit
TIME_UNIT_MINUTE|HOUR|DAY}, detail {same quota shape}}`;
`usages {limit_5h, limit_7d, limit_month_total {used_ratio, reset_time}}`;
`user.membership.level` (`LEVEL_FREE/TRIAL/BASIC/INTERMEDIATE/ADVANCED` →
Adagio/Andante/Moderato/Allegretto/Allegro, only when `version` absent or
`GOODS_VERSION_V1`); `version`.
Quota: weekly summary + per-window percent meters (session 5h / weekly / monthly
pools); 401 invalid key, 404 endpoint n/a, 403 denied.
Sources: kimi-cli `src/kimi_cli/ui/shell/usage.py` (`_usage_url`, `_fetch_usage`,
`_parse_usage_payload`), `src/kimi_cli/auth/platforms.py:53-65`
(`https://api.kimi.com/coding/v1`); omp `packages/ai/src/usage/kimi.ts:19-20,43-45,265-280`;
`CodexBar/.../Kimi/KimiUsageFetcher.swift` (`fetchCodeAPIUsage`,
`codeAPIUsageEndpoint`), `KimiModels.swift`.

**D — POST `https://www.kimi.com/apiv2/kimi.gateway.billing.v1.BillingService/GetUsages`**
(web quota). Body `{scope: ["FEATURE_CODING"]}`; auth `Bearer <web authToken>` +
`Cookie: kimi-auth=<token>`, `Origin/Referer: https://www.kimi.com...`,
`connect-protocol-version: 1`, `x-language/x-msh-platform/r-timezone`,
`x-msh-device-id/session-id`, `x-traffic-id` (from JWT, see S).
Response `{usages: [{scope, detail {limit/used/remaining/resetTime}, limits[]}]}`;
only the `FEATURE_CODING` entry is kept.
**D — POST `.../membership.v2.MembershipService/GetSubscriptionStats`** (enrichment,
2s grace, best-effort). Body `{}` → `{subscriptionBalance {feature, type,
amountUsedRatio, kimiCodeUsedRatio, expireTime}, ratelimitCode7d {ratio, enabled,
resetTime}}`. Sibling `.../GetSubscription` → plan name.
Sources: `CodexBar/.../Kimi/KimiUsageFetcher.swift:10-13,340-375`.

**D — GET `https://api.moonshot.ai/v1/users/me/balance`** (platform balance).
China region: `https://api.moonshot.cn/v1/users/me/balance`. Bearer API key.
Response `{code: 0, status: true, scode, data: {available_balance, voucher_balance,
cash_balance}}` (USD intl / CNY china; negative cash = deficit).
Source: `CodexBar/.../Moonshot/MoonshotUsageFetcher.swift:114-125`,
`MoonshotRegion.swift`.

**S — JWT session decode.** `device_id`/`ssid`/`sub` claims → `x-msh-*` headers
(`KimiUsageFetcher.decodeSessionInfo`).

## 2. Z.AI / BigModel (GLM Coding Plan)

**D — GET `https://api.z.ai/api/monitor/usage/quota/limit`** (quota).
Auth: `Bearer <API key>`. Regions: global `https://api.z.ai`, BigModel CN
`https://open.bigmodel.cn` (same paths). Team scope: append `?type=2` +
`Bigmodel-Organization` / `Bigmodel-Project` headers.
Response `{success: true, code: 200, msg, data: {limits: [{type:
TOKENS_LIMIT|CREDIT_LIMIT|TIME_LIMIT, unit (1=day,3=hour,5=minute,6=week), number,
usage, currentValue, remaining, percentage, nextResetTime (ms), usageDetails:
[{modelCode, usage}]}], level ("lite"|"pro"|"max" plan label)}}`.
`success:false` on 2xx = valid key but **no GLM Coding Plan** (distinct error).
Quota: sub-daily session + multi-day token/credit percent windows
(`windowMinutes = number × unit-multiplier`); monthly `TIME_LIMIT` web-search count;
2xx + non-integer fields = parse failure, not fallback.
Peak (Mon–Fri 06:00–10:00 UTC) 1x / off-peak 0.5x credit rate is computed
client-side from the clock — no endpoint exposes it.
Sources: `openusage/.../ZAI/ZAIUsageClient.swift` (`fetchQuota`), `ZAIUsageMapper.swift`;
omp `packages/ai/src/usage/zai.ts:16-18` (+`QUOTA_PATH`, `MODEL_USAGE_PATH`);
`CodexBar/.../Zai/ZaiAPIRegion.swift`, `Resources/Plugins/zai.js:35-50`.

**D — GET `.../api/monitor/usage/model-usage?startTime=&endTime=[&type=3]`** (history).
`startTime/endTime` format `YYYY-MM-DD HH:MM:SS`; team adds `type=3`.
Response `{success, code, data: {x_time[], modelDataList: [{modelName,
tokensUsage[]}]}}` → per-model token totals + time series (≤120 points).
Source: `zai.js` (`modelUsage()`), `ZaiAPIRegion.modelUsageURL`.

**D — GET `https://api.z.ai/api/biz/subscription/list`** (plan name, best-effort).
Bearer; failure must not blank quota meters.
Source: `openusage/.../ZAI/ZAIUsageClient.swift` (`fetchSubscription`).

**D — GET `https://www.bigmodel.cn/api/biz/account/query-customer-account-report`**
(BigModel CN PAYG balance only; no z.ai-global equivalent).
Accepts `Bearer <key>` or raw key. Verified 2026-08 per code comment.
Source: `ZaiAPIRegion.balanceURL`.

## 3. MiniMax (Token Plan / Coding Plan)

**D — GET `{apiBase}/v1/token_plan/remains`** (quota; canonical).
`apiBase`: global `https://api.minimax.io`, CN `https://api.minimaxi.com`.
Auth: `Bearer <token-plan key>` (minimax-cli region probe also tries `x-api-key`;
both accepted); CodexBar adds `MM-API-Source: CodexBar`.
Response `{base_resp: {status_code: 0}, model_remains: [{model_name,
current_interval_total_count, current_interval_usage_count, start_time, end_time,
remains_time, interval_boost_permill(e), current_interval_remaining_percent,
current_interval_status (1 normal / 2 exhausted / 3 unlimited),
current_weekly_total_count, current_weekly_usage_count, weekly_start/end/remains_time,
weekly_boost_permill(e), current_weekly_remaining_percent, current_weekly_status}]}`.
Quota: per-model rolling-interval + weekly count windows; rendered weekly % =
base × (boost_permille/1000), can exceed 100%; `*_usage_count` ambiguity resolved
client-side (`minimax-cli/src/utils/quota.ts`).
Sources: minimax-cli `src/client/endpoints.ts:45-46`, `src/types/api.ts:286-320`,
`src/sdk/quota/index.ts`, `src/config/schema.ts:2-3`; omp
`packages/ai/src/usage/minimax-code.ts:7-8,192-232`; `CodexBar/.../MiniMax/
MiniMaxAPIRegion.tokenPlanRemainsURL`, `MiniMaxModelRemains.swift`.

**D — GET `{base}/v1/api/openplatform/coding_plan/remains`** (legacy fallback).
Tried after token-plan 404/405/auth-reject; same Bearer auth, same bucket shape.
Platform base `https://platform.minimax.io` (global) /
`https://platform.minimaxi.com` (CN); API-base twin also probed.
Source: `MiniMaxAPIRegion.remainsURL/apiRemainsURL`, `MiniMaxUsageFetcher.fetchUsageOnce`.

**D — GET `{base}/account/query_balance`** (secret `sk-api-*` keys only).
Selected when key starts with `sk-api-` (`selectUsageEndpoint`).
Response `{available_amount, cash_balance, voucher_balance, credit_balance,
owed_amount, balance_alert_switch/threshold, base_resp}` (decimal strings).
Source: minimax-cli `src/client/endpoints.ts:50-84`, `src/types/api.ts:290-300`.

**S — Web coding-plan page + billing history.**
`GET https://platform.minimax.io|platform.minimaxi.com/user-center/payment/
coding-plan?cycle_type=3` (Cookie + optional Bearer) → HTML service quotas;
`GET .../account/amount?page&limit&aggregate=false` → billing records.
Source: `MiniMaxAPIRegion.codingPlanURL/billingHistoryURL`, `MiniMaxUsageFetcher`.
