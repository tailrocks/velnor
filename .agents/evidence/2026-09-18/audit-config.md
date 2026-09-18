# Config/Account/Discovery Audit

No code edits made. All paths relative to `jackin/` repo root.

## 1. Exact current data model

**Registry — `crates/jackin-config/src/accounts.rs`:**

- `AiProvider` (:17–37): `Anthropic, OpenAi, Amp, Xai, Opencode, Moonshot, Zai, Minimax`. Wire form is `slug()` (:40–51): `anthropic/openai/amp/xai/opencode/moonshot/zai/minimax`. `for_agent()` (:53–62) maps each `Agent` to one native provider.
- `AccountCredential` (:89–115), serde `tag="type"`, three variants only:
  - `Profile { agent: Agent, directory: PathBuf }` — agent-managed login in an exact host dir.
  - `ApiKey { value: EnvValue, base_url: Option<String>, model: Option<String> }`.
  - `OAuthToken { agent: Agent, value: EnvValue }` — Claude-only (enforced :164–166, and `crates/jackin/src/app/account_cmd.rs:207–209`).
- `AccountConfig` (:134–149): `{ enabled: bool = true, name: String, provider: AiProvider, credential }`. No ID field inside the struct; **the map key is the ID**. No provenance, revision, billing-identity, or validation-state fields.
- `validate_account_id` (:359–375): `1–64` chars, `[a-z0-9_-]`, must start alnum. This is the only `AccountId` notion — a TOML key, not an opaque generated ID.

**Global — `crates/jackin-config/src/app_config.rs`:**

- `accounts: BTreeMap<String, AccountConfig>` (:34) → TOML `[accounts.<id>]`.
- `account_bindings: BTreeMap<Agent, String>` (:37) → TOML `[account_bindings] <agent-slug> = "<id>"`.

**Workspace — `crates/jackin-config/src/schema.rs`:**

- `WorkspaceConfig.accounts: Vec<String>` (:217) — allowlist of IDs; empty = no credentials. Duplicates rejected at validation (`accounts.rs:465–471`).
- `WorkspaceConfig.account_bindings: BTreeMap<Agent, String>` (:220).
- `WorkspaceRoleOverride.account_bindings: BTreeMap<Agent, String>` (:114) — most-specific layer.

**Agent identity — `crates/jackin-core/src/agent.rs`:**

- 6 variants only (:23–36): `Claude, Codex, Amp, Kimi, Opencode, Grok`. `ALL` order (:41–48), `slug()` (:51–60) is the serde/CLI/TOML key. No `AgentId`/`AgentInstanceId`/`ProviderServiceId`/`AccountId` types exist anywhere.

**Resolution precedence** (`accounts.rs:380–437` `resolve_account`): role binding → workspace binding → global binding (filtered by workspace allowlist) → sole eligible allowlist account → `None`; >1 candidate without binding is a hard error.

## 2. One-per-agent assumptions (file:line evidence)

1. `resolve_account(...) -> Option<&AccountConfig>` — returns a single account for one `Agent` (`accounts.rs:380–385`).
2. All three binding maps are `BTreeMap<Agent, String>` — one ID per agent by construction (`app_config.rs:37`, `schema.rs:114`, `schema.rs:220`).
3. Ambiguity is an error, not a set: `"multiple accounts support {agent}; select an account binding"` (`accounts.rs:431–435`).
4. `resolve_account_env_with` loops agents, resolves one account each, keys output by `agent.slug()` (`crates/jackin-env/src/accounts.rs:48–51,76`).
5. `AgentCredentialEnv(BTreeMap<String, BTreeMap<String,String>>)` keyed by agent name; `for_agent(&str)` (`crates/jackin-protocol/src/account_credentials.rs:12,19`).
6. `AccountConfig::credential_env(agent)` builds env for one agent (`accounts.rs:292`); `Profile`/`OAuthToken` require `owner == agent` (`accounts.rs:160–166`).
7. CLI: `Default { id, agent }` and `Select { account: Option<String>, agent }` take one account (`crates/jackin/src/cli/account.rs:40–44,98–109`).
8. `ConfigEditor::set_account_binding` inserts one `agent.slug() → id` entry (`crates/jackin-config/src/editor/accounts.rs:150–154`).
9. Discovery yields at most one `DiscoveredAccount` per agent (`accounts/discovery.rs:126–144`, `break` at :132); env scan comment: "One account per provider" (:43–45).
10. Callers assume single: `crates/jackin/src/app/load_cmd.rs:123` and `:432` bind one `selected_account` per launch.

## 3. `ConfigEditor::open` / bootstrap

`crates/jackin-config/src/editor.rs:80–156`:

1. Acquires the config write lock, ensures base dirs.
2. **Bootstrap trigger is file existence only** (`if !paths.config_file.exists()`, :83). No versioned init sentinel/state machine.
3. Fresh-install seed: `AppConfig::default()` + `sync_builtin_agents()` → `discover_default_accounts(home)` → `default-{agent}` Profile accounts (:86–100) → live `std::env` via `discover_environment_accounts` → `{provider}-api-key` with `EnvValue "${VAR}"` (:101–120) → `discover_environment_oauth_accounts` → `{agent}-oauth-token` (:121–134) → `validate_accounts()` → `atomic_write` pretty TOML.
4. Then migrates, reads, runs `load_split_config` (migrating legacy embedded workspaces), re-reads, parses `DocumentMut`, loads split workspace docs.

`AppConfig::load_or_init` (`app_config/persist.rs:709–753`) does **not** seed accounts itself — missing file yields `AppConfig::default()` with empty accounts (`load_split_config` returns default on `None`, :435). It only picks up bootstrapped accounts indirectly: on fresh installs `sync_builtin_agents()` always reports change, so it opens a `ConfigEditor` (which creates the seeded file) and takes `editor.save()`'s re-parse (:737–745).

## 4. CLI scan flow

`crates/jackin/src/cli/account.rs:17–19` (`Scan`, no args) → `crates/jackin/src/app/account_cmd.rs:80–175` `scan()`:

1. `discover_default_accounts(home)` → for each hit, skip if any registered `Profile` has same `(agent, directory)` (:86); else ID `default-{agent}` with `-N` suffix on collision (:87–93); insert Profile with `provider = for_agent(agent)`; `editor.upsert_account`.
2. Live process env → `discover_environment_accounts` → skip if same `(provider, ApiKey value "${VAR}")` exists (:113); else `{provider}-api-key[-N]` (:114–120).
3. `discover_environment_oauth_accounts` → skip if same `(owner, value)` (:138); else `{agent}-oauth-token[-N]`.
4. `editor.save()` only if `added > 0` (:160–162); issues to stderr (:163–170); always prints summary + "Assign access with `jackin workspace account assign ...`" (:171–173). **Scan never assigns to workspaces.**
5. Dedup is by credential source, not ID — re-running is idempotent for unchanged sources. Identity *change* at a registered path is invisible (no revalidation, no provenance to compare).

Related: `account add` (`account_cmd.rs:177–227`) validates profile dirs via `discover_account_directory` (must contain credential evidence, :198–201) and rejects empty dirs; secrets via masked prompt/stdin/`--secret-ref` (`$VAR`/`${VAR}`/`op://` only, :265–284). `add` checks ID uniqueness against the pre-open `config` snapshot (:38) — concurrent adds can race (editor itself doesn't recheck ID collision, only source duplication at `editor/accounts.rs:18–24`).

## 5. Discovery sources (current)

`crates/jackin-config/src/accounts/discovery.rs`:

- Per-agent default dir = `home / agent.runtime().state_paths().credential_dir`, plus `.kimi` fallback for Kimi (:124–145).
- Evidence files (:167–172): Claude `.credentials.json` (`claudeAiOauth.accessToken`), Codex/Opencode/Grok `auth.json`, Amp `secrets.json` (+ `data/amp/secrets.json` nested alias, :174–179), Kimi `credentials/kimi-code.json`. Matchers :232–264. Opencode's matcher accepts **any** `api`/`oauth` entry — but the importer still registers one account named after the client, not per-provider entries.
- Claude macOS Keychain existence check (metadata only, no secret read, :266–276) scoped via `claude_keychain_scope` (:191–200).
- Bounded 1 MiB reads; `DiscoveryError { Unreadable, Malformed, TooLarge }` per source, other agents still scanned (:88–120). Values never leave the boundary (var names only).

## 6. Precise gaps vs `jackin-accounts-usage-specification.md`

**§3 Domain model / identity rules:**
- No `AgentInstanceId`, `ProviderServiceId`, `AccountId` (opaque), `CredentialSource`, `BillingIdentity`, `QuotaScope`, `AgentConfiguration`, `LaunchManifest` types. Account "ID" is a user-chosen TOML slug with no stability beyond rename-=`remove+add` (rename changes bindings — violates "renaming does not change bindings").
- No provenance, revision, validation state, or identity-verification fields on `AccountConfig`; no "unresolved account" state — unknown/incompatible references are validation errors.
- No `AgentConfiguration { agent, account, endpoint, exact model }` entity: `model`/`base_url` live on the *credential* (`ApiKey`), so one account cannot have multiple named model configurations (§4 OpenRouter requirement: "same account can have multiple named configurations" — impossible).
- Multi-provider stores (§3 "register all actual API keys", §5 omp/Hermes/OpenCode): scan collapses to one account per provider/client; OpenRouter/Z.AI-as-routing-provider has no representation (Zai exists as `AiProvider` but only as a Claude/Codex-compatible API-key target, `accounts.rs:167–183`).
- The 13-entry client catalog (§1: Antigravity, Gemini CLI, Muse, Cursor, OpenRouter, omp, Hermes, etc.) vs 6 `Agent` variants; no catalog-to-support ledger.

**§4 Settings → Accounts:** (console/UI surface, out of audited files, but config-layer blockers) — no draft/candidate model in `ConfigEditor` (every mutation validates + saves immediately); no conflict/re-merge handling beyond the write lock; disable prunes bindings immediately (`editor/accounts.rs:30–36`) rather than "deny new launches, keep recorded labels on running instances"; removal deletes assignments with no "affected configurations" flagging.

**§5 Discovery / bootstrap:**
- No versioned init sentinel (§5: "rather than only testing whether config.toml exists") — bootstrap is exactly the existence test `editor.rs:83`. No fresh-install marker; no pre-existing-config migration path; interrupted bootstrap retries by re-testing existence (atomic write makes this safe, but there is no "mark complete only after transaction" state).
- No sibling-pattern candidates (`.claude-*`, `.codex-*`, Amp XDG roots enumeration), no per-source summary counts (added/registered/needs-auth/unreadable/unsupported), no unchanged/changed-identity reporting, no `.zshrc` static importer.
- Path-family gaps: Amp assumes one selected dir (+ one nested alias), not three explicit XDG roots; no `CLAUDE_CONFIG_DIR`/`CODEX_HOME` override awareness in scan (deliberately catalog-defaults only, :122); no Antigravity/Gemini/`.kimi-code`/Cursor/Muse stores.
- Concurrent bootstrap/scan recheck under lock: `upsert_account` checks source duplication against a TOML re-parse of its own doc (`editor/accounts.rs:17–24`) — two racing editors can both pass and last-writer-wins (violates §5 "recheck source identity under the configuration lock").

**§9 Workspace/launch policy:**
- No `allowed_accounts` vs `default_launch` separation: `accounts` is the allowlist and `account_bindings` is per-agent single default; no launch-set (multi-instance) concept, no explicit per-launch selection plumbing, no precedence levels 1–2 (one-launch selection, role default *launch set* — role scope holds only per-agent bindings).
- No replacement-vs-inherit semantics decision point, no explicit-empty-set handling, no atomic multi-entry validation for a launch selection.
- `[[agent_configurations]]` example (`claude-work` + `claude-personal` + `codex-work` in one container) is unrepresentable: two Claude accounts cannot coexist in bindings, resolution, `AgentCredentialEnv`, or staging.

**§10 Container/session:** keyed-by-`Agent` transport (`AgentCredentialEnv::for_agent(&str)`) cannot carry per-instance bindings; no `AgentInstanceId` in launch/session/restore records (per spec §2 anchor, confirmed at `account_credentials.rs:12–21`); no per-instance HOME/state or usage-only-credential separation in the audited config/env layers.

**Smallest structural facts for implementers:** the one-per-agent constraint is load-bearing in exactly three shapes — `BTreeMap<Agent, String>` bindings (3 sites), `resolve_account -> Option<single>` (+ its multi-candidate error), and `AgentCredentialEnv` keyed by agent slug. Everything else (validation, editor, CLI, discovery dedup) follows from those three.
