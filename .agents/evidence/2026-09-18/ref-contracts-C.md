# ref-contracts-C — Antigravity/Gemini (Google Cloud Code), Cursor

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

## 1. Antigravity / Gemini (Google Cloud Code Private API)

All calls: **POST** `Bearer <Google OAuth access>`, JSON bodies, `Accept/Content-Type:
application/json`. Base failover: `https://cloudcode-pa.googleapis.com` ↔
`https://daily-cloudcode-pa.googleapis.com` (401/403 short-circuits as auth failure,
other errors try next base); omp Antigravity also lists
`https://daily-cloudcode-pa.sandbox.googleapis.com`.

**D — POST `{base}/v1internal:loadCodeAssist`** (plan + project identity).
Body `{project|cloudaicompanionProject...}` (small JSON; CodexBar posts
`{}`-ish metadata). Response `{currentTier {id, name}, paidTier {id, name}
(preferred), cloudaicompanionProject}`; consumer-unsupported flags parsed for
shutdown detection. Prefer Google `userTier.name` over Windsurf-inherited
`planInfo.planName` (always "Pro" when paid).
**D — POST `{base}/v1internal:fetchAvailableModels`** (legacy per-model quota).
Response `{models: {<id>: {displayName, label, model, quotaInfo
{remainingFraction, resetTime...}, isInternal}}}`; drop `isInternal`/empty-label.
**D — POST `{base}/v1internal:retrieveUserQuota`** (legacy buckets).
Response `{buckets: [{modelId, remainingFraction 0-1 (absent→0/depleted),
resetTime ISO}]}`.
**D — POST `{base}/v1internal:retrieveUserQuotaSummary`** (authoritative pools).
Response `{groups: [{buckets: [{bucketId, remainingFraction, resetTime}]}]}`
(LS wraps in `{response: {groups}}`, remote is bare).
Exact `bucketId` match only: `gemini-5h`→Session, `gemini-weekly`→Weekly,
`3p-5h`→Claude, `3p-weekly`→Claude Weekly (3p = every non-Gemini model incl.
GPT-OSS). Parsed summary (even empty) wins over legacy endpoints.
**D — POST `{base}/v1internal:onboardUser`** (onboarding; CodexBar).
Quota: 5h + weekly percent pools per family; legacy per-model rows collapse to the
worst (lowest) remaining fraction per pool and are 5h-only (weekly reads "No data").
Blacklisted/internal model IDs never surface as meters.
Sources: `openusage/.../Antigravity/AntigravityUsageClient.swift` (bases, paths,
`cloudCode()`), `AntigravityUsageMapper.swift` (summaryBuckets, parsers);
`CodexBar/.../Antigravity/AntigravityRemoteUsageFetcher.swift:36-40,397-399`;
`CodexBar/.../Gemini/GeminiStatusProbe.swift:189-190,454-497`;
omp `packages/ai/src/usage/gemini.ts:16,107,144`,
`packages/ai/src/usage/google-antigravity.ts:85-87,424-499`.

**D — POST `https://oauth2.googleapis.com/token`** (Google refresh).
Form `{client_id, client_secret, refresh_token, grant_type=refresh_token}` →
`{access_token, expires_in}`. Installed-app credentials ship in the client
(openusage documents this as intentional):
[REDACTED: public openusage example client_id/client_secret — pattern trips push protection; values were documented examples, not live secrets]. 4xx (`invalid_grant`) = dead token;
5xx/network/429/408 = transient.
Source: `AntigravityUsageClient.refreshGoogleToken`, `GeminiStatusProbe.swift:194`.

**D(local) — Antigravity language-server loopback RPC.**
`POST {http|https}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/
{method}` body `{metadata: {ideName: "antigravity", ...}}`, headers
`Connect-Protocol-Version: 1`, `x-codeium-csrf-token: <csrf>`; self-signed cert
(insecure loopback session). Methods incl. `GetUserStatus` (plan + model configs),
`GetCommandModelConfigs` (fallback), quota-summary RPC.
Source: `AntigravityUsageClient.callLS`.

**D — GET `https://cloudresourcemanager.googleapis.com/v1/projects`** (Gemini
project listing; CodexBar `GeminiStatusProbe.swift:191`).

## 2. Cursor

**D — POST `https://api2.cursor.sh/aiserver.v1.DashboardService/{method}`**
(Connect protocol: body `{}`, headers `Authorization: Bearer <Cursor access>`,
`Content-Type: application/json`, `Connect-Protocol-Version: 1`).
- `GetCurrentPeriodUsage` → `usage {enabled, planUsage {limit, totalPercentUsed,
totalSpend...}, spendLimitUsage {limitType ("team"|...), pooledLimit}}`.
  Quota: Total-usage percent meter over the billing cycle; `planUsage` present but
  limit-less → request-based/REST fallback; team shape inferred from
  `limitType=team || pooledLimit>0`.
- `GetPlanInfo` → `{planName}` (identity/plan label).
- `GetCreditGrantsBalance` → credit-grant cents buckets (combined with Stripe
  balance into one Credits row; `grantTotal + stripeBalanceCents`).
- `GetSandUsageStatus` ("Grok Bot") → `{usagePercent, nextResetTimestampUtc,
  currentPeriodStart, usesPooledEnterpriseAllowance, hasNonZeroIncludedLimit,
  includedLimitZero}`; weekly meter; pooled/zero-allowance accounts get no meter.
Source: `openusage/.../Cursor/CursorUsageClient.swift:9-13` (`connectPost`),
`CursorUsageMapper.swift` (`CursorPlanUsageFacts`, `mapUsage`, `mapGrokBotUsage`).

**D — POST `https://api2.cursor.sh/oauth/token`** (refresh).
JSON `{grant_type: "refresh_token", client_id: "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB",
refresh_token}`. Source: `CursorUsageClient.refreshToken`.

**D — REST cookie endpoints** (`Cookie: WorkosCursorSessionToken={userId}%3A%3A
{accessToken}`; userId = JWT `sub` part after `|`).
- `GET https://cursor.com/api/usage?user={userId}` → `{gpt-4 {maxRequestUsage,
  numRequests|numRequestsTotal}, startOfMonth...}` request allowance ("Total usage"
  + "Requests" count meters, monthly).
- `GET https://cursor.com/api/usage-summary` → `{billingCycleStart/End,
  membershipType, limitType, individualUsage {plan {totalPercentUsed,
  autoPercentUsed, apiPercentUsed}, onDemand, overall}, teamUsage {onDemand,
  pooled}}` structured % + dollar buckets + exact cycle bounds.
- `GET https://cursor.com/api/auth/stripe` → Stripe balance cents.
- `GET https://cursor.com/api/auth/me` → `{sub, email}` identity (validated
  `sub == userId`).
- `GET https://cursor.com/api/dashboard/export-usage-events-csv?startDate=&endDate=
  (epoch ms)&strategy=tokens` → CSV (`Accept: text/csv`).
Sources: `CursorUsageClient.swift:14-17` + `session(from:)`, `CursorUsageSummaryMapper.swift`,
omp `packages/ai/src/usage/cursor.ts:394-430`, `registry/oauth/cursor.ts`
(`extractCursorAccessTokenUserId`).

**D(omp-legacy) — GET `https://api2.cursor.sh/auth/usage`.**
omp default `DEFAULT_CURSOR_BASE_URL=https://api2.cursor.sh` + `/auth/usage`,
Bearer token (OAuth or API key); cents buckets `{enabled, used, limit, remaining}`
parsed per plan meter; usage-summary/auth-me enrichment only when OAuth + default base.
Note: openusage/CodexBar use the `DashboardService/*` Connect methods instead —
treat as a parallel legacy shape, not the same endpoint.
Source: omp `packages/ai/src/usage/cursor.ts:21-24,374-380`.
