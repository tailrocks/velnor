# ref-contracts-E — Grok/xAI (SuperGrok CLI + Management API), Muse

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

## 1. Grok / xAI OAuth (SuperGrok CLI billing proxy)

Proxy base default `https://cli-chat-proxy.grok.com/v1` (override `GROK_PROXY_URL`
/ `--cli-chat-proxy-base-url`). Quota auth: `Authorization: Bearer <xAI OAuth
access>` + `X-XAI-Token-Auth: xai-grok-cli` (same product gate as chat inference),
`Accept: application/json`.

**D — GET `{proxy}/billing?format=credits`** (weekly shared-pool quota).
This is the exact call the Grok CLI makes (`billing.rs` appends
`/billing?format=credits`). Proto-JSON response
`{config: {creditUsagePercent 0-100 (**omitted when 0**, not a schema change),
currentPeriod: {type: "USAGE_PERIOD_TYPE_WEEKLY", start, end (ISO)},
onDemandCap: {val} (0/absent = PAYG disabled), onDemandUsed?,
isUnifiedBillingUser, productUsage: [{product, usagePercent}]...}}`.
Quota: weekly percent meter (`resetsAt = currentPeriod.end`); only rendered when
`type` is weekly — monthly-only accounts honestly blank.
**D — GET `{proxy}/billing`** (no `format`; unified-billing monthly quota).
`isUnifiedBillingUser` accounts omit `creditUsagePercent` above and expose
`monthlyLimit/used + periodStart/End` here ("SuperGrok Monthly Included" meter,
unit unknown — matches dashboard quota points).
Sources: `openusage/.../Grok/GrokUsageClient.swift` (`creditsConfigURL`,
`authHeaders`), `GrokCreditsConfigDecoder.swift` (live shape 2026-07-06),
`GrokUsageMapper.swift`; `CodexBar/.../Grok/GrokCreditsProxyFetcher.swift:11`;
omp `packages/ai/src/usage/xai-oauth.ts` (weekly+monthly parsers),
`packages/ai/src/registry/oauth/xai-oauth.ts:11-13,229-244`
(`buildXAICliBillingUrl`, `getXAICliBillingHeaders`).

**D — GET `{proxy}/settings`** (plan label + remote settings).
Same auth. Response includes `subscription_tier_display` (plan name) plus feature
flags (`oauth2_issuer`, `oauth2_client_id`, `release_channel`,
`subscription_watch_interval_secs`, memory/pruning/flush knobs...).
Sources: `GrokUsageClient.fetchSettings`, `GrokUsageMapper.planName`,
`CodexBar/.../Grok/GrokCLISettingsFetcher.swift:9,19-60`,
grok-build `crates/codegen/xai-grok-config-types/src/lib.rs:279` (`RemoteSettings`).

**D — GET `{proxy}/user[?include=subscription]`** (identity).
`UserInfo` (camelCase): `user_id`, `email?`, `first/last_name?`,
`team_{id,name,role}?`, `organization_{id,name,role}?`, `principal_{type,id}?`,
`subscription_tier?` (only with `?include=subscription`).
Source: grok-build `crates/codegen/xai-grok-login/src/model.rs` (`UserInfo`).

**D — POST `https://auth.x.ai/oauth2/token`** (refresh).
Form `{grant_type=refresh_token, client_id, refresh_token}` →
`{access_token, refresh_token?, id_token?, expires_in?}`.
**D — GET `https://auth.x.ai/oauth2/userinfo`** (OIDC identity).
Bearer → `{sub → accountId, email, name}` (best-effort enrichment).
Sources: `GrokUsageClient.refreshURL` (+`tokenAuthHeader = "xai-grok-cli"`),
omp `registry/oauth/xai-oauth.ts:8-10,191-227`.

**S — Web gRPC fallback.**
`POST https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig`
(`Origin/Referer: https://grok.com`, cookie/Bearer); CodexBar notes the grok.com
gRPC-web endpoint hardened, so the CLI proxy above is primary.
Source: `CodexBar/.../Grok/GrokWebBillingFetcher.swift:128,221-231`.

## 2. xAI Management API (console teams, API-key auth)

**D — GET `https://management-api.x.ai/v1/billing/teams/{teamId}/prepaid/balance`.**
Auth: `Bearer <MANAGEMENT key>` — inference API keys rejected (401/403).
Env `XAI_MANAGEMENT_API_KEY` + `XAI_TEAM_ID` (path-safe single segment).
Response `total.val` = **cents string** (signed); balance USD = `-val/100`.
404 = wrong team/key-team mismatch; 429 = rate-limited.
**D — POST `.../usage`.**
Body `{analyticsRequest: {timeRange: {startTime, endTime ("YYYY-MM-DD HH:MM:SS"),
timezone: "Etc/GMT"}, timeUnit: "TIME_UNIT_DAY", values: [{name: "usd",
aggregation: "AGGREGATION_SUM"}], groupBy: [], filters: []}}` (last 30d).
Response `{timeSeries: [{dataPoints: [{timestamp, values: [usd]}]}],
limitReached?}` → daily $ history (partial when `limitReached`).
Prepaid balance is remaining credit, never spend — shown as fallback cost row only.
Source: `CodexBar/.../Resources/Plugins/xai.js:4-90`,
`Providers/XAI/{XAISettingsReader,XAIProviderDescriptor,XAICostUsageMapping}.swift`.
Dashboards: `https://console.x.ai`, `https://status.x.ai`.

## 3. Muse (Meta)

**D — POST `https://api.meta.ai/muse-code/key`** (identity + quota in one call).
Headers `Authorization: Bearer <Meta OAuth access>`, `Accept/Content-Type:
application/json`, `x-api-version: 1.0.0`; body `{}` (interactive login may send
`{onboard: true}`); 20s timeout; non-2xx → `OAuthError(kind: token-exchange,
status)`; invalid JSON/shape → `OAuthError(kind: validation)`.
Response `{api_key?, require_payment?, require_payment_action_url?|action_url?,
user_email?, user_id?, is_subs_active?, subs_tier_id?|subs_tier_name? (tier label),
subs_usage?: {window?: {used_percent, resets_at (ISO|epoch), window_duration_mins},
weekly?: {...}}}`.
Quota: rolling-window percent meter (label from `window_duration_mins`, id
`{N}m`) + weekly percent meter; reset accepts ISO string or epoch number.
Credential stored as JSON `{oauthAccessToken, apiKey}` (`parseMuseCodeCredential`).
Also validates credentials (`validatesCredentials: true`, 5-min failure backoff).
Sources: omp `packages/ai/src/registry/oauth/muse-code.ts`
(`MUSE_KEY_URL`, `requestMuseCodeKey`, schemas),
`packages/ai/src/usage/muse-code.ts` (`SOURCE = "api.meta.ai/muse-code/key"`,
`buildLimits`).

**U(absent) — meta-models/muse-code-sdk@94e98141 exposes NO remote billing/usage
endpoint.** Grep over the pinned tree finds no metacode/Muse API host: usage there
is local per-session `TokenUsage {inputTokens, outputTokens, cachedTokens,
reasoningTokens}` streamed over the MSP stdio JSON-RPC protocol
(`schema/msp/*`, `turn/completed.usage`, `usage/changed`). Remote quota for Muse
comes only from the `api.meta.ai/muse-code/key` contract above (omp).
