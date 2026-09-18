# ref-contracts-A — Claude/Anthropic, Codex/OpenAI, Amp

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
Evidence classes: **D** = request/handler code at pinned SHA (URL+method+auth observed);
**S** = secondary (scrape / header / JWT-claim / log derived); **R** = referenced in
docs/comments/fixtures only; **U** = unverified or absent.

## 1. Claude / Anthropic (OAuth)

**D — GET `{base}/api/oauth/usage`** (live quota).
Base default `https://api.anthropic.com` (`openusage/.../Claude/ClaudeAuthStore.swift:65`;
omp `packages/ai/src/usage/claude.ts:26` = `https://api.anthropic.com/api/oauth`).
Auth: `Authorization: Bearer <OAuth access>`, `anthropic-beta: oauth-2025-04-20`
(omp sends a longer beta list), `Accept/Content-Type: application/json`,
`User-Agent: claude-code/2.1.69` (openusage) / `claude-cli/{ver} (external, cli)` (omp).
Custom `baseUrl` is probed first, canonical endpoint is fallback (omp).
Response: `five_hour` / `seven_day` / `seven_day_sonnet` (`seven_day_opus` legacy) each
`{utilization 0-100, resets_at ISO}`; `limits[]` `{kind: "weekly_scoped",
scope.model.display_name e.g. "Fable", percent 0-100, resets_at}`;
`extra_usage {is_enabled, used_credits (cents), monthly_limit, decimal_places, currency}`;
`spend {used/limit {amount_minor, currency, exponent}, enabled}`.
Quota: Session (5h) + Weekly (7d) percent windows gate everything; per-model weekly
scoped rows; extra-usage dollar spend; HTTP 429 + `Retry-After` (don't manual-refresh).
Sources: `openusage/.../ClaudeUsageClient.swift` (`fetchUsage`), `ClaudeUsageMapper.swift`,
omp `usage/claude.ts`, `docs/provider-quirks.md:168`
(`anthropic-ratelimit-unified-*` headers feed rotation).

**D — GET `{base}/api/oauth/profile`** (identity).
Same base/auth as above (usage URL minus last path + `profile`).
Response: `account {uuid}`, `organization {uuid, organization_type
("claude_max"/"claude_pro"→Max/Pro), rate_limit_tier ("...Nx")}`.
Used to verify token↔account/org and to refresh the plan label after upgrades.
Source: `openusage/.../ClaudeUsageClient.swift` (`fetchProfile`/`verifyAccount`).

**D — POST `https://platform.claude.com/v1/oauth/token`** (refresh).
JSON body `{grant_type: "refresh_token", refresh_token, client_id:
"9d1c250a-e61b-44d9-88ed-5944d1962f5e", scope: "user:profile user:inference
user:sessions:claude_code user:mcp_servers user:file_upload"}`.
Non-prod: staging refresh `https://platform.staging.ant.dev/v1/oauth/token`,
client `22422756-60c9-4084-8eb7-27705fd5cf9a`; custom/local overrides via
`CLAUDE_CODE_CUSTOM_OAUTH_URL` / `CLAUDE_LOCAL_OAUTH_API_BASE`.
Source: `ClaudeAuthStore.swift:65-68,280-334`.

**D — Claude Web cookie API** (fallback when OAuth lacks `user:profile`).
`GET https://claude.ai/api/organizations` → `[{uuid, name...}]`;
`GET .../organizations/{org}/usage` → same 5h/7d/sonnet + `extra_usage` shape;
`GET .../organizations/{org}/prepaid/credits` → remaining Extra-usage balance.
Auth: browser session cookie. Source:
`CodexBar/.../Claude/ClaudeWeb/ClaudeWebAPIFetcher.swift:92-96,569,597`.

## 2. Codex / OpenAI (ChatGPT OAuth)

**D — POST `https://auth.openai.com/oauth/token`** (refresh).
Form body `grant_type=refresh_token&client_id=app_EMoamEEZ73f0CkXaXp7hrann&refresh_token=...`.
200 → `{access_token, refresh_token?, id_token?}`; 400/401 `error.code` ∈
`{refresh_token_expired → sessionExpired, refresh_token_reused → tokenConflict,
refresh_token_invalidated → tokenRevoked}`; other 4xx/5xx = request failure, not expiry.
Source: `openusage/.../Codex/CodexUsageClient.swift` (`refreshToken`).

**D — GET `https://chatgpt.com/backend-api/wham/usage`** (quota).
Auth: `Bearer <Codex OAuth>`, optional `ChatGPT-Account-Id: <account>`, `Accept: json`.
Base constant `CODEX_BASE_URL=https://chatgpt.com/backend-api` (omp
`packages/catalog/src/wire/codex.ts:5`); only `chatgpt.com`/`chat.openai.com` origins
accepted, extra proxy paths normalized away (`usage/openai-codex-base-url.ts`).
Response: `plan_type`; `rate_limit {primary_window, secondary_window}` each
`{used_percent, reset_at (epoch s) | reset_after_seconds}`; `additional_rate_limits[]
{limit_name|metered_feature ("GPT-5.3-Codex-Spark"→Spark/Spark Weekly),
rate_limit{...same window shape}}`; `rate_limit_reset_credits` (count only);
credits-remaining (body or header).
Fallback: response headers `x-codex-primary-used-percent` /
`x-codex-secondary-used-percent`.
Quota: 5h session + weekly percent windows; classify by explicit duration, slot
(primary=session, secondary=weekly) only as fallback; flex credits ≈ $0.04 each.
Sources: `CodexUsageClient.swift` (`fetchUsage`), `CodexUsageMapper.swift`, omp
`usage/openai-codex.ts:22,252,497-512`.

**D — GET `.../wham/rate-limit-reset-credits`** (per-credit expiries).
Same auth + `OpenAI-Beta: codex-1`, `originator: Codex Desktop`.
Best-effort enrichment of the usage count.
**D — POST `.../wham/rate-limit-reset-credits/consume`** (claim one credit).
JSON `{redeem_request_id: <UUID idempotency key>, credit_id}`; 200 `code` ∈
`{reset, already_redeemed, nothing_to_reset, no_credit}`.
Source: `CodexUsageClient.swift` (`fetchResetCredits`, `consumeResetCredit`),
`CodexResetClaimService`, `docs/research/codex-reset-credit-claim.md`.

**S — JWT identity claims.** `https://api.openai.com/auth → {chatgpt_account_id}`
(hermes `agent/codex_headers.py:57`, `CODEX_AUX_BASE_URL=https://chatgpt.com/backend-api/codex`);
`https://api.openai.com/profile` email claim (omp `usage/openai-codex.ts:24`).
Account id feeds the `ChatGPT-Account-Id` header.

**R — OpenAI web dashboard** `https://chatgpt.com/backend-api/accounts/...`
(`CodexBar/.../OpenAIWeb/OpenAIDashboardFetcher.swift`) — referenced only.

## 3. Amp

**D — POST `https://ampcode.com/api/internal?userDisplayBalanceInfo`** (quota).
JSON body `{method: "userDisplayBalanceInfo", params: {}}`;
`Authorization: Bearer <Amp API token>`, `accept/content-type: application/json`.
Response `{ok, result.displayText, error{code: "auth-required"...}}`;
`displayText` parsed into free quota/used, hourly replenishment, window hours,
individual credits, per-workspace remaining balances.
Source: `CodexBar/.../Amp/AmpUsageFetcher.swift:103-104,265-270` (`makeUsageAPIRequest`,
`parseUsageAPIResponse`).

**S — GET `https://ampcode.com/settings`** (cookie HTML scrape).
`Cookie: <ampcode.com session>`, browser UA/`origin: https://ampcode.com`;
HTML parsed by `AmpUsageParser`; 401/403 or login redirect = invalid credentials.
Dashboard: `https://ampcode.com/settings/usage`.
Source: `AmpUsageFetcher.swift` (`fetchLegacyHTMLWithDiagnostics`), `AmpUsageParser.swift`.
