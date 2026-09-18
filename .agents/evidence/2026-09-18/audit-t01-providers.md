# T01 Provider Capability Research — Coverage Report

Date: 2026-09-17. Machine: macOS arm64, user `$HOME=/Users/donbeave`, Homebrew + mise(node 24, bun).
Method: read `jackin-provider-research.md` + `jackin-accounts-usage-specification.md` fully;
inspected real default paths (filenames + JSON/TOML key shapes only), keychain service/account
names only, `--help`/`--version` surfaces, redacted shell routing; cross-checked first-party docs.
No secret values recorded, no authenticated usage reads executed, no code edits.

Evidence tiers (from research doc): **D** documented · **S** first-party source ·
**R** reference-derived (CodexBar/OpenUsage/omp) · **U** unverified.

## 1. Installed CLI inventory + version detection

| CLI | Path | Version (local) | Detection cmd |
|---|---|---|---|
| claude | /opt/homebrew/bin/claude | 2.1.274 | `claude --version` |
| codex | /opt/homebrew/bin/codex | 0.154.0 | `codex --version` |
| amp | /opt/homebrew/bin/amp | 0.0.1789639648-g3c529d (built 2026-09-17, auto-updates) | `amp --version` / `amp version` |
| agy | /opt/homebrew/bin/agy | 1.2.5 | `agy --version` |
| kimi | /opt/homebrew/bin/kimi | 0.43.0 | `kimi --version` |
| muse | /opt/homebrew/bin/muse | 1.3.0 (1.3.0-R3233.1) | `muse --version` |
| cursor-agent | /opt/homebrew/bin/cursor-agent | 2026.09.10-fd3934a | `cursor-agent --version` |
| grok (+`agent` symlink, same build) | Caskroom/grok-build/1.0.30 | 1.0.30 | `grok --version` |
| opencode | /opt/homebrew/bin/opencode | 1.18.30 | `opencode --version` |
| gemini | MISSING | — | `npm i -g @google/gemini-cli`, also `brew install gemini-cli` |
| omp | MISSING | — | `brew install can1357/tap/omp` or `bun install -g @oh-my-pi/pi-coding-agent` |
| hermes | MISSING | — | `curl -fsSL https://hermes-agent.nousresearch.com/install.sh \| bash` |
| minimax/mmx | MISSING | — | `npm install -g mmx-cli` (binary is `mmx`, not `minimax`) |

**Collision:** `agent` on PATH resolves to Grok Build (`/opt/homebrew/bin/agent` → grok-build
cask). Cursor's official binary is also named `agent` (installer default `~/.local/bin/agent`,
alias `cursor-agent`); locally only `cursor-agent` (brew) exists. Any resolver must use absolute
paths / version-probe, never bare `agent`.

## 2. Per-provider coverage

### 2.1 Claude / Anthropic (client: `claude`)
- Auth path/schema: `~/.claude/` dir + companion `~/.claude.json` (verified present);
  OAuth token in macOS keychain service `Claude Code-credentials` (acct=username) — verified;
  3 extra suffixed services (`-74a5d03f`, `-93aecf3d`, `-db49ea31`) = multiple profiles.
  `~/.claude.json` top keys incl `oauthAccount{accountUuid, emailAddress, organizationUuid,
  organizationName, billingType, ...}`, `cachedUsageUtilization{accountUuid, fetchedAtMs,
  utilization}`. Override: `CLAUDE_CONFIG_DIR`; sibling homes verified:
  `.claude-scentbird`, `-scentbird-ai`, `-chainargos`, `-zhokhov`, `-yolo` variants.
  `~/.config/anthropic` Console profiles: not present locally (U here).
- Identity source: keychain token → `/api/oauth/profile` (R); local `oauthAccount` cache.
- Usage source + tier: `GET api.anthropic.com/api/oauth/usage` (R); org reports
  `/v1/organizations/usage_report|/cost_report`, rate-limit + spend-limit APIs (D, scoped keys);
  native `/usage|/status` (D-interaction, version-specific parser).
- Version gate: none for file/keychain layout; OAuth usage headers are version-sensitive (R).
- Unknowns: exact token-refresh owner when host monitor + container sessions share one grant;
  `.config/anthropic` profile schema on this machine.

### 2.2 Codex / OpenAI (client: `codex`)
- Auth path/schema: `~/.codex/auth.json` verified:
  `{auth_mode, OPENAI_API_KEY, tokens{access_token, account_id, id_token, refresh_token},
  last_refresh}`; `codex login status` → "Logged in using ChatGPT".
  `~/.codex/config.toml` verified with `[model_providers.kimi]` (`base_url=
  https://api.kimi.com/coding/v1`, `env_key=KIMI_API_KEY`) and `[model_providers.zai]`
  (`base_url=https://api.z.ai/api/v1`, `env_key=ZAI_API_KEY`) — matches research doc bases.
  Sibling homes verified: `.codex-scentbird` (has `kimi.config.toml`, `kimi-1m.config.toml`
  with `model_provider/model_context_window/model_catalog_json`, `glm*.config.toml`),
  `.codex-chainargos`, `-chainargos2` (config only, no auth.json), `.codex-zhokhov`
  (auth.json present). `-p/--profile` layers `$CODEX_HOME/<name>.config.toml` (verified help).
  Override: `CODEX_HOME`. No Codex keychain entry → file backend in use.
- Identity source: app-server `account/read` (D); `tokens.account_id` local.
- Usage source + tier: app-server `account/rateLimits/read` + notifications,
  `account/usage/read` (D, capability-negotiated); fallback
  `GET chatgpt.com/backend-api/wham/usage` (R); org `/v1/organization/usage|/costs` (D, scoped).
- Version gate: 0.154.0 has `app-server` (+`daemon/proxy/generate-ts/generate-json-schema`),
  `login/logout/doctor`. Negotiate per-version fields; fixtures required.
- Unknowns: keyring-backend layout (not used here); headless device-code file shape.

### 2.3 Amp (client: `amp`)
- Auth path/schema: XDG data `~/.local/share/amp/secrets.json` verified:
  single key `apiKey@https://ampcode.com/` (102-char value); `device-id.json{installationID}`.
  No `~/.config/amp/settings.json` locally (help states it as default;
  `--settings-file` overrides). Sibling: `.amp-scentbird/{data,config,cache}` verified
  (data has `device-id.json`, no secrets.json → unauthenticated or env-keyed copy).
  No Amp keychain entry. `amp login/logout` verified in help.
- Identity source: bearer → `POST ampcode.com/api/internal?userDisplayBalanceInfo` (R);
  no local identity file observed.
- Usage source + tier: `amp usage [--details --start --end]` (D, verified help);
  internal balance endpoint (R: free/daily, Agent $/mo, Orb hrs/mo, workspace pools).
- Version gate: build-date versioning, auto-updates (local build 2h old) → pin + re-verify often.
- Unknowns: whether `--settings-file` + XDG roots fully isolate auth (needs live 2-profile test);
  Settings access-token vs CLI-login token forms (docs distinguish; local file holds one form).

### 2.4 Antigravity / Google (client: `agy`)
- Auth path/schema: `~/.gemini/antigravity-cli/settings.json` verified (model/toolPermission/
  trustedWorkspaces only — no credentials); OAuth in keychain `svce=gemini, acct=antigravity`
  (verified, singleton as research doc states). `~/.gemini/antigravity/` holds only `skills`.
- Identity source: keychain grant; note `/usage` JSON may omit identity (R) → bind to runtime.
- Usage source + tier: `agy -p /usage --output-format json`, `agy -p /credits
  --output-format json` (D, changelog-verified); local 127.0.0.1 LanguageServerService +
  `cloudcode-pa.googleapis.com/v1internal:*` (R, tokenless break at ≥1.2.2 per CodexBar).
- Version gate: local 1.2.5 ≥ 1.1.11 → official commands available; gate `<1.1.11` off
  (unknown slash command would run as prompt). ≥1.2.2: prefer official commands over local API.
- Unknowns: multi-account keyring namespaces (singleton blocks naive per-HOME isolation);
  exact local `/usage` JSON pool schema on 1.2.5 (needs one authenticated run).

### 2.5 Kimi (clients: `kimi` + Claude/Codex configs)
- Auth path/schema (new family): `~/.kimi-code/` verified: `config.toml`
  (`[providers."managed:kimi-code"]{type,api_key,base_url,oauth{storage,key,oauth_host}}`,
  `[models."kimi-code/k3|k3-256k|kimi-for-coding..."]`, `[services.moonshot_*]`);
  `credentials/kimi-code.json` + `credentials/kimi-code-env-<hex>.json`, both
  `{access_token, refresh_token, token_type, expires_at, expires_in, scope}`;
  `oauth/` holds 0-byte marker files. Old `~/.kimi` family: absent. Override:
  `KIMI_CODE_HOME` (D). `kimi login --region mainland-cn|global`, `kimi provider
  add|remove|list|catalog`, `kimi doctor`, `kimi web|server` (local server API) verified.
- Claude config: shell `_claude_provider` wrapper sets `ANTHROPIC_AUTH_TOKEN` (1Password ref)
  + `ANTHROPIC_BASE_URL=https://api.kimi.com/coding/` + model `k3` (verified redacted .zshrc;
  base host verified). MiniMax-analogous wrapper uses `MiniMax-M3`; Z.AI uses `glm-5.3[X]`.
- Codex config: `[model_providers.kimi]` + per-model `kimi.config.toml`/`kimi-1m.config.toml`
  (context 262144/1048576) verified.
- Identity source: OAuth userinfo via local server `/api/v1/oauth/userinfo` (D).
- Usage source + tier: `GET api.kimi.com/coding/v1/usages` (S: legacy first-party CLI + R
  collectors); local server `/api/v1/oauth/usage` `{summary, limits, extra_usage}` (D);
  web membership enrichment (R, optional).
- Version gate: 0.43.0 new family; old `.kimi`/`KIMI_SHARE_DIR` only if legacy binary present.
- Unknowns: `KIMI_MODEL_*` vs provider-config precedence on 0.43.0; region routing of keys.

### 2.6 Z.AI / GLM (provider; no own TUI)
- Auth path/schema: no vendor home; keys via env (`ZAI_API_KEY` in codex `env_key`, verified)
  and 1Password refs in shell (`ZAI_OP_REF`, verified names-only); `~/.codex/glm*.config.toml`
  + sibling copies verified.
- Endpoints verified locally: Codex Responses `https://api.z.ai/api/v1` (matches doc);
  shell comment cites Anthropic base `api.z.ai/api/anthropic` and paas/v4 (D per docs).
- Identity source: quota response scope (global vs CN, personal vs team) (R).
- Usage source + tier: `GET api.z.ai/api/monitor/usage/quota/limit` + `model-usage` (R);
  CN team selectors (R); no verified global PAYG-balance endpoint (U).
- Version gate: none (server-side); client eligibility per Z.AI tool-support matrix (D).
- Unknowns: live `limits[]` schema drift (`CREDIT_LIMIT` vs `TOKENS_LIMIT`); `nextResetTime` units.

### 2.7 Muse Code (client: `muse`)
- Auth path/schema: **resolved locally** — `~/.config/muse/auth.json` verified:
  `{schema_version: 2, providers: {meta: {mechanism, storage, obtained_via, api_base_url,
  user_full_name, user_email}}}`; secret itself in keychain `svce=ai.meta.dev.credentials,
  acct=meta` (verified). `settings.json{schema_version:1, provider, model,
  reasoning_effort, tui{...}}` verified. No `~/.muse`. `META_API_KEY` overrides login
  (verified `muse login --help`). Subcommands verified: `login/logout/auth/mcp/serve`(MSP
  over stdio)/`schema`/`config`/`sandbox`. No `MUSE_HOME` observed → per-child HOME isolation.
- Identity source: auth.json `user_email/user_full_name` + MSP session (D/SDK).
- Usage source + tier: MSP `usage/read` + `usage/changed` (S: SDK schema; D within SDK);
  `/usage` refresh route (changelog; verify structured form); omp's
  `POST api.meta.ai/muse-code/key` (R, **conditional** — possible credential exchange).
- Version gate: local 1.3.0; use `muse schema` export to pin MSP contract per version.
- Unknowns: Linux install artifact path; keychain→file fallback on Linux; whether key-exchange
  POST mints/rotates credentials (must test before any polling use).

### 2.8 Cursor Agent (client: `cursor-agent`; official alias `agent`)
- Auth path/schema: `~/.cursor/auth.json{accessToken, refreshToken}` (424-char values,
  verified) + `~/.cursor/cli-config.json` (verified, incl
  `authInfo{authId, displayName, email, userId}`, model `claude-fable-5-1/composer-2.5/
  grok-4.6` refs). Keychain also holds `cursor-access-token`/`cursor-refresh-token`
  (acct `cursor-user`) — second lineage (editor vs CLI or legacy). Env: `CURSOR_API_KEY`,
  `CURSOR_API_ENDPOINT` (default `https://api2.cursor.sh`) verified in help.
  Subcommands verified: `login/logout/status|whoami/bedrock`. `CURSOR_CONFIG_DIR` (D per docs).
- Identity source: `status|whoami`, `authInfo` (D); dashboard RPC identity (R).
- Usage source + tier: Enterprise Admin `POST /teams/spend`, `/teams/filtered-usage-events`
  (D, admin auth); `DashboardService/GetCurrentPeriodUsage|GetPlanInfo|
  GetCreditGrantsBalance|GetSandUsageStatus` (R); cursor.com session APIs (R).
- Version gate: local 2026.09.10; binary-name gate — resolve `cursor-agent` first, never bare
  `agent` (Grok collision on this machine).
- Unknowns: which lineage (file vs keychain) the CLI actually uses; `CURSOR_CONFIG_DIR`
  coverage of auth+history (needs 2-profile test).

### 2.9 Grok Build (client: `grok`/`agent`)
- Auth path/schema: `~/.grok/auth.json` verified: single key
  `https://auth.x.ai::<uuid>` → `{auth_mode, key, refresh_token, principal_id,
  principal_type, team_id, email, first_name, last_name, oidc_issuer, oidc_client_id,
  expires_at, create_time, ...}` — identity embedded. `~/.grok/config.toml` (mcp/ui/
  marketplace only). Override: `GROK_HOME` (D); `grok du` references `~/.grok`.
  Subcommands verified: `login/logout/usage/models/sessions/inspect/du`, `--oauth` flag.
  Stored subscription auth outranks ambient `XAI_API_KEY` (D per source).
- Identity source: auth.json principal/email (S); `https://auth.x.ai` OIDC (S).
- Usage source + tier: `GET cli-chat-proxy.grok.com/v1/billing?format=credits` (S:
  first-party billing.rs; fields `creditUsagePercent`, weekly/monthly, prepaid, caps);
  `grok agent stdio` billing RPC (R); `grok usage` = local persisted session tokens/cost
  (D-help, not quota). xAI Management API for dev/API billing (D, separate credential).
- Version gate: local 1.0.30; device login suitable for containers (S).
- Unknowns: exact live billing JSON on current server; auto-top-up display fields.

### 2.10 OpenRouter (provider; via OpenCode/omp/Hermes/etc.)
- Auth path/schema: provider entries in client stores + env keys; no vendor home.
  Locally: no OpenRouter entry found in codex/opencode stores (opencode auth empty).
  Client ID forms: `openrouter/<org>/<model>` (OpenCode/omp), Hermes
  `--provider openrouter --model <org>/<model>` (D per client docs).
- Identity source: `/key` response (key identity + cap) (D).
- Usage source + tier: `GET /key` (D, ordinary key); `GET /credits`, `/activity`,
  `/analytics/*` (D, Management key); `/generation?id=` (D); `/models` catalog (D).
- Version gate: none (server API); validate exact model ID per client.
- Unknowns: user's key scopes (needs `GET /key` with user consent); BYOK attribution shape.

### 2.11 omp / oh-my-pi (client; NOT installed)
- Auth path/schema (D/S, unverified locally): `~/.omp/agent/agent.db` (SQLite),
  `PI_CODING_AGENT_DIR` / `OMP_PROFILE` overrides (S: dirs.ts); named profiles; many
  provider adapters (S: providers.md). Corroborated by third-party connectors
  (`~/.omp/agent` path, `auth-broker-gateway.md` trusted-client warning).
- Identity source: per-provider entries in agent.db (S).
- Usage source + tier: per-provider collectors incl. Muse/Go (S); broker file
  `OMP_AUTH_BROKER_ACCOUNT_POOL_FILE` is trusted-client routing, NOT authz (S) →
  must not be used for selected-accounts-only admission.
- Version gate: install pinned release first; parse actual source resolver on install.
- Unknowns: everything local — install, `omp --version`, live DB schema, profile semantics.

### 2.12 Hermes TUI (client; NOT installed)
- Auth path/schema (D/S, unverified locally): `~/.hermes/` (local dir holds only `skills/`
  scaffold); `config.yaml`, `.env`, `auth.json`, `.hermes/profiles`, `HERMES_HOME`,
  `hermes -p` profile (docs). Recent upstream commits confirm HERMES_HOME plumbing +
  per-profile secret scoping and "clone drops rotating OAuth" behavior.
- Identity source: active profile + provider entries (S).
- Usage source + tier: underlying provider collectors; Nous Portal route (S); no
  Hermes-native quota API established (U).
- Version gate: TUI needs Node ≥20 + Python agent runtime + PTY; `HERMES_TUI=1` or
  `--tui`; `HERMES_TUI_DIR` prebuild select (D).
- Unknowns: everything local; Anthropic-OAuth-via-Hermes entitlement (U per research).

### 2.13 OpenCode (client: `opencode`)
- Auth path/schema: docs still state `~/.local/share/opencode/auth.json` via `/connect`
  (D, re-verified today on opencode.ai/docs/providers). **Locally absent**: no auth.json;
  SQLite `credential`/`account`/`account_state`/`control_account` tables all 0 rows
  (credential cols: id/integration_id/label/value/connector_id/method_id/active/...;
  account cols: id/email/url/access_token/refresh_token/...). `~/.config/opencode/
  opencode.json` holds only $schema/command/mcp/plugin. `opencode auth list|login|logout`
  = `providers` alias (verified). Conclusion: user runs opencode with env keys, no
  `auth login` performed. XDG config/data/state/cache are distinct (S: global.ts).
- Provider catalog scope (D, today's provider directory): 302.AI, Bedrock, Anthropic,
  Azure OpenAI, DeepSeek, MiniMax, Moonshot, OpenAI, OpenRouter, xAI, Z.AI, ZenMux,
  GitHub Copilot, Google Vertex, HuggingFace, Ollama, +custom providers — incl.
  ChatGPT OAuth with residency forwarding. Catalog-to-support ledger still required.
- Identity source: per-provider auth entry (S); Zen/Go account (S).
- Usage source + tier: `GET opencode.ai/zen/go/v1/usage` Bearer (S, undocumented;
  rolling/weekly/monthly `{status, percent, reset}`, 1 = 1%); `opencode stats`
  = local session cost (D-help, not quota); no verified Zen PAYG-balance API (U).
- Version gate: local 1.18.30; interactive `opencode` vs `run` (D).
- Unknowns: live Go `/usage` per-model fields (needs redacted live response); 2-account
  same-provider mechanism (separate XDG data roots unverified).

### 2.14 Gemini CLI (client; NOT installed)
- Auth path/schema (D, unverified locally): `~/.gemini/settings.json`,
  `~/.gemini/oauth_creds.json` (local settings.json holds only hooks; oauth_creds absent);
  `GEMINI_CLI_HOME` = parent to which `.gemini` is appended. Install:
  `npm i -g @google/gemini-cli` / `brew install gemini-cli` (D).
- Identity source: OAuth entitlement response (D).
- Usage source + tier: Code Assist Standard/Enterprise (D); consumer OAuth deprecated
  2026-06-18 (D notice) — old logins need migration action, not blind 403 mapping.
- Version gate: install then pin; verify entitlement response shape.
- Unknowns: everything local; project-scoped quota mapping for API-key route.

### 2.15 MiniMax (provider + `mmx` CLI; NOT installed)
- Auth path/schema: Token-Plan Subscription Keys vs PAYG keys are distinct products (D:
  platform.minimax.io Token Plan docs); Claude route via `ANTHROPIC_BASE_URL` +
  `MiniMax-M3` model, 1Password ref (verified shell pattern, host redacted);
  Codex route Responses `https://api.minimax.io/v1` (D). `~/.minimax/` holds only skills.
  CLI: `npm i -g mmx-cli`, dual region `api.minimax.io` / `api.minimaxi.com` (S: README).
- Identity source: token-plan account / team scope (S).
- Usage source + tier: `GET api.minimax.io/v1/token_plan/remains` (S: endpoints.ts;
  per-model interval/weekly used/reset/boosts/unlimited); PAYG
  `GET api.minimax.io/account/query_balance` (S: cash/voucher/credit/debt strings).
- Version gate: install `mmx-cli`, pin; never cross regions on failure.
- Unknowns: local key product (Subscription vs PAYG); live `remains` schema on current server.

## 3. Coverage matrix

| # | Provider | Auth path (default) | Schema | Identity source | Usage source | Usage tier | Version gate | Top unknown |
|---|---|---|---|---|---|---|---|---|
| 1 | Claude/Anthropic | `~/.claude/` + `~/.claude.json` + keychain `Claude Code-credentials*` | verified local | oauth profile + local cache | oauth/usage; org reports | R / D | headers drift | refresh ownership |
| 2 | Codex/OpenAI | `~/.codex/auth.json` + `config.toml` | verified local | app-server account/read | rateLimits/read, usage/read; wham | D / R | 0.154.0 negotiate | keyring layout |
| 3 | Amp | `~/.local/share/amp/secrets.json` | verified local | internal balance API | `amp usage`; internal API | D / R | auto-update pin | auth isolation proof |
| 4 | Antigravity | `~/.gemini/antigravity-cli/` + keychain `gemini/antigravity` | verified local | keychain grant (cmd may omit) | `agy -p /usage|/credits -o json` | D | ≥1.1.11 (local 1.2.5 ✓) | multi-acct keyring |
| 5 | Kimi | `~/.kimi-code/{config.toml,credentials/,oauth/}` | verified local | oauth userinfo | `/coding/v1/usages`; local server | S+D / D | new-family 0.43.0 | model/key precedence |
| 6 | Z.AI | env + 1Password refs; codex `model_providers.zai` | verified local | quota scope | monitor/quota/limit | R | server-side | schema drift; PAYG endpoint U |
| 7 | Muse Code | `~/.config/muse/auth.json` + keychain `ai.meta.dev.credentials` | verified local (new) | auth.json user_* + MSP | MSP usage/read|changed | S | 1.3.0; `muse schema` | Linux layout; key-POST safety |
| 8 | Cursor Agent | `~/.cursor/{auth.json,cli-config.json}` + keychain `cursor-*-token` | verified local | status/whoami + authInfo | DashboardService RPC; Admin API | R / D | binary-name gate | file-vs-keychain lineage |
| 9 | Grok Build | `~/.grok/auth.json` (`auth.x.ai::<uuid>` key) | verified local | embedded principal/email | billing?format=credits; stdio RPC | S / R | 1.0.30 | live JSON shape |
| 10 | OpenRouter | client stores + env | verified pattern | GET /key | /key /credits /activity /analytics | D | model-ID validation | user key scopes |
| 11 | omp | `~/.omp/agent/agent.db` | NOT installed (S) | agent.db entries | per-provider collectors | S | install+pin first | all local |
| 12 | Hermes | `~/.hermes/` + profiles | NOT installed (D/S) | active profile | underlying providers | S/U | Node20+Py+PTY | all local; entitlements |
| 13 | OpenCode | `~/.local/share/opencode/auth.json` (docs) — locally absent/empty | verified absent | provider entries; Zen/Go | zen/go/v1/usage; stats(local) | S / D-local | 1.18.30 | live Go model fields |
| + | Gemini CLI | `~/.gemini/` (oauth_creds absent) | NOT installed (D) | entitlement response | Code Assist Std/Ent | D | install+pin | all local |
| + | MiniMax | 1Password/env; `mmx-cli` | NOT installed (S/D) | token-plan scope | token_plan/remains; query_balance | S | install mmx-cli | key product; live schema |

## 4. Keychain summary (services, names only)

| svce | acct | Owner |
|---|---|---|
| `Claude Code-credentials` + 3 suffixed | username | Claude Code (multi-profile) |
| `gemini` | `antigravity` | Antigravity singleton |
| `ai.meta.dev.credentials` | `meta` | Muse Code |
| `cursor-access-token` / `cursor-refresh-token` | `cursor-user` | Cursor (editor-or-legacy lineage) |
| `Codex MCP Credentials` | `firecrawl\|…` | Codex MCP OAuth |
| `com.steipete.codexbar.cache` | `cookie.claude`, `cookie.codex`, `oauth.claude.profile.*` | CodexBar artifacts (reference tool, ignore) |
| — (none) | — | Codex file-backend, Amp file, Kimi file, Grok file, Cursor-CLI file |

## 5. Cross-cutting findings

1. Bare `agent` = Grok on this machine; Cursor docs name their binary `agent` too.
   Resolvers must probe absolute paths + `--version` stdout, never the basename.
2. Muse native layout resolved (was U in research doc):
   `~/.config/muse/auth.json` (schema_version 2, identity fields) + keychain secret.
   Linux layout still U (likely same XDG path).
3. OpenCode `auth.json` documented but absent; SQLite `credential`/`account` tables exist
   but are empty (integration/account-shaped, not provider-auth-shaped). Env-key operation.
4. Kimi + Z.AI Codex bases verified byte-equal to research doc; Claude-side routing via
   `ANTHROPIC_BASE_URL` + per-provider model map + 1Password refs verified in shell.
5. `~/.codex` siblings without auth.json (scentbird/chainargos*) are config-only profiles —
   scanner must report them as configuration candidates, not authenticated accounts.
6. `~/.minimax/`, `~/.hermes/` contain only skills scaffolds (no auth); `~/.amp-scentbird`
   lacks secrets.json (no login there).
7. `op` (1Password CLI) installed — env/1Password reference resolution path exists.
8. Amp version string embeds build timestamp and was built 2h before inspection:
   treat Amp as continuously deployed; re-verify on each run.

## 6. Remaining unknowns requiring live/authenticated checks

- omp/Hermes/Gemini-CLI/mmx install + first-run auth + `--version` (4 installs).
- Antigravity + Cursor + Amp two-profile concurrency/isolation proofs.
- Muse `POST .../muse-code/key` passive-read safety; Linux artifact.
- Redacted live responses: agy `/usage`, Kimi local `/oauth/usage`, Grok billing,
  OpenCode Go `/usage`, Z.AI quota, MiniMax `remains`, Cursor DashboardService.
- Codex keyring-backend and Muse file-fallback layouts (not exercised on this Mac).
