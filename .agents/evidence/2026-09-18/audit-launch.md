# Launch / Provisioning / Capsule audit: one-per-agent assumptions

Scope: `crates/jackin-runtime/src/runtime/launch/` (`account_config.rs`, `account_identity.rs`,
`launch_pipeline/*`, `programmatic.rs`, `mounts.rs`, `capsule_setup.rs`, `usage_relay.rs`),
`crates/jackin-instance/{lib,auth}.rs`, `crates/jackin-core/{container_paths,agent}.rs`,
`crates/jackin-protocol/{account_credentials.rs,lib.rs}`,
`crates/jackin-capsule/{session.rs,runtime_setup.rs,config.rs,daemon.rs,main.rs}` plus
directly adjacent launch files read for flow completeness. No code edits.

## 1. Exact one-per-agent assumptions (with file:line evidence)

### 1.1 Config model: one account ID per agent, one account per (agent, scope)

- Bindings are `BTreeMap<Agent, String>` at all three scopes (global, workspace, workspace-role);
  `resolve_account` returns a single `Option<&AccountConfig>`:
  `crates/jackin-config/src/accounts.rs:380-437`. Precedence is role→workspace→global
  (:393-401); with no binding and a workspace, exactly one compatible account in
  `ws.accounts` is auto-selected and **ambiguity is a hard error**
  ("multiple accounts support {agent}; select an account binding", :431-435).
- `AccountConfig::credential_env(agent)` (`accounts.rs:292-353`) builds one env map per
  (account, agent); cross-provider use without explicit model is rejected (:310-318).
- `LoadOptions.account: Option<String>` (`crates/jackin-runtime/src/runtime/launch.rs:149-151`):
  "Registered account ID selected for this launch… the override applies only to the
  selected agent." `with_account_selection` (`launch/programmatic.rs:326-361`) inserts one
  `agent → id` binding; requires a pre-resolved agent (`launch_pipeline.rs:769-772`), so a
  multi-agent role with no `default_agent` cannot use `--account` at all.
- Validation (`accounts.rs:443-479`) enforces single authorized binding per agent per scope.

### 1.2 Host launch resolves, provisions, and stages only the selected agent

Single `agent: Agent` chosen once (`launch_pipeline.rs:1074-1081`, `select_launch_agent`) and
single `auth_mode` derived from that agent's account (`launch_pipeline.rs:1363-1367`).
In `prepare_environment` (`orchestrate.rs:619-772`):

- `resolve_account_env_with(config, &[agent], …)` (:655-665) — credentials resolved for the
  selected agent only. (The resolver itself loops over agents, `jackin-env/src/accounts.rs:35-80`,
  but is always called with a 1-element slice.)
- `let provision_agents = [agent];` (:668) → `RoleState::prepare_for_agents(…, &provision_agents)`
  (:685-697) — only the selected agent's `ProvisionedAuth` slot is created in the foreground.
- `write_account_credentials(root, credentials)` (:698-701), `configure_accounts(…, &[agent])`
  (:702-708), `record_account_configuration` (:709-715) all operate on the selected agent's data.
- `capsule_auth_modes(…, Some(agent))` (`capsule_setup.rs:41-69`): every supported agent
  **other than selected is forced to `Ignore`** (:52-61). `apply_account_models(…, &[agent])`
  (`orchestrate/helpers.rs:106-112`): model override only for selected agent.

**Doc drift (flagged, not fixed):** `mounts.rs:42-46` and `instance/lib.rs:187-191` both claim
the foreground path "provisions all manifest-supported agents" / "every agent in
`supported_agents()`". False against `orchestrate.rs:668`. `RoleState::prepare` (the
all-agents wrapper, `instance/lib.rs:505-524`) is **not** what launch calls.

### 1.3 ProvisionedAuth: one fixed-path slot per agent variant, no account dimension

`crates/jackin-instance/src/lib.rs:248-269`: `ProvisionedAuth { claude, codex, amp, kimi,
opencode, grok }`, each `Option<OneSlot>`. Each slot holds fixed paths, e.g.
`ClaudeAuth { account_json, credentials_json, forward_auth }` (:208-212),
`CodexAuth { auth_json: Option<PathBuf> }` (:217-219). Second account for the same agent has
no slot, no path, no outcome cell (`auth_outcomes: BTreeMap<Agent, AuthProvisionOutcome>`, :373).

- `RoleState.agent_runtime: AgentRuntimeState { agent, model }` (:184-198, :371) — single
  selected agent; `claude_model()/codex_model()/…` return `Some` only when that agent is
  selected (:387-453).
- Provisioners (`instance/auth.rs`) implement one credential source per agent:
  single-file copy/wipe (`provision_single_file_credential`, :926-1029), Claude dual-file
  (:467-593), Kimi dir tree (:688-773). Sync-from-source-dir reads exactly one dir; an explicit
  source dir must never fall back to the default account (:557-583) — the one place that
  already thinks in multi-account terms, and it does so by *excluding* rather than adding.
- Sibling prewarm (`launch_runtime.rs:150-282`) provisions non-selected agents' sync slots in
  background **after** `docker run`; mounts were fixed at create time, so prewarmed files are
  unreachable in-container. Prewarm also never extends `account-credentials.json`, fingerprints,
  `auth_modes`, or model map — siblings stay `Ignore` + credential-less regardless.

### 1.4 Credential transport keyed by agent slug, not by account

- `AgentCredentialEnv(BTreeMap<agent-slug, BTreeMap<var, value>>)` with `for_agent(&str)`
  (`jackin-protocol/src/account_credentials.rs:10-35`) — one credential set per agent, max.
- File staging: `write_account_credentials` → `<data>/<container>/credentials/account-credentials.json`,
  0600 file / 0700 dir, atomic persist (`launch/account_identity.rs:152-168`).
- Mount: credentials dir → `/run/jackin:ro` (`launch/mounts.rs:54-57`); read at
  `container_paths::ACCOUNT_CREDENTIALS = "/run/jackin/account-credentials.json"`
  (`jackin-core/src/container_paths.rs:22`). Note this path violates the file's own
  `/jackin/`-only hard rule (:1-7) — pre-existing inconsistency.
- Container-wide env (`resolved_env`, `launch_pipeline.rs:1478`) **never** carries account
  secrets by construction: manifest account names filtered (:1376-1382), `opts.env` account
  names rejected (:1429-1432), on-demand names rejected (:1464-1468); comment at :1313-1314.
  `lane_agent_env` adds model/effort only for Codex/Claude of the selected agent
  (`programmatic.rs:299-319`, wired at `launch_pipeline.rs:1442-1445`).
- Capsule load+validate (`capsule/src/config.rs:88-145`): every `agents[]` entry with
  `api_key|oauth_token` mode **must** have a non-empty `for_agent` map (:115-128); any entry
  for an agent outside the allowlist, in another mode, with a non-`is_account_env` name, or
  with an empty value is rejected (:129-144). A second account's vars for the same agent have
  no valid encoding.
- `is_account_env` (`jackin-core/src/account_env.rs:35-37`) = usage-registry names + 19 routing
  names (`ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`, …). Single-provider-per-agent assumed: two
  accounts needing different `*_BASE_URL`/`*_MODEL` for one agent collide on var names
  (also baked into `credential_env`, `accounts.rs:321-349`).

### 1.5 Fingerprints and restore/admission checks: whole-policy boolean, per instance

- `account_configuration_fingerprint(config, workspace, role)` (`launch/account_identity.rs:18-66`):
  sha256 over `("account-config-v2", accounts-in-scope, global bindings, ws bindings,
  role bindings)`. Any account add/remove/edit or binding change anywhere in scope → new digest.
- `record_account_configuration` (:87-108) writes `account-config.sha256` (live config at launch)
  and `account-admission.sha256` (persisted snapshot at launch) into the instance root.
- Gates (all boolean; no per-account granularity):
  - `account_configuration_matches` (:73-85): explicit pinned restore is a hard error on
    mismatch ("instance account policy changed; launch a new instance", `launch_pipeline.rs:973-983`).
  - `admit_restore` (:129-150): mismatch silently downgrades any restore resolution to
    `StartFresh`. Applied at early selected scan (`launch_pipeline.rs:855`), early unselected
    scan (:907), and late resolve (:1112).
  - `account_admission_matches` (:115-127) vs read-only snapshot: `require_current_account_admission`
    (`runtime/attach.rs:366-395`) runs on **every** hardline/reconnect path; mismatch denies with
    "recreate it with `jackin load`". `opts.account.is_some()` also disables restore fast paths
    (`launch_pipeline.rs:841,1101`), forcing a fresh pipeline.
- Restore matching itself is per (workspace, role, **single agent**): `InstanceQuery {
  role_key: Some, agent_runtime: Some(agent) }` (`launch/restore.rs:251-269`);
  `InstanceManifest.agent_runtime: String` (`jackin-instance/src/manifest.rs:104`,
  parsed back via `agent()`, :243-250) alongside `supported_agents: Vec<Agent>` (:135).

### 1.6 In-container session identity: agent slug is the only key

- Container argv[1] = initial agent slug = first tab only; "the container's global environment
  does not claim one agent for every session" (`launch_runtime.rs:1048-1052`,
  `capsule/main.rs:325-336`).
- Daemon `LaunchEnv { available_agents, launch_config, agent_credentials, env_passthrough,
  workdir, … }` (`capsule/daemon.rs:412-419`), built once at startup: credentials loaded from
  the fixed path (:547), passthrough = 7 non-secret allowlisted vars from daemon env
  (`SESSION_ENV_PASSTHROUGH`, `session.rs:64-71`, collected at `daemon.rs:548-551`).
- Spawn (`daemon/session_lifecycle.rs:202-264,320-388`): `SpawnRequest::Agent(slug)` validated
  against `available_agents` allowlist (:208-214); `session_launch` builds the command with
  per-agent model (`multiplexer_utils.rs:61-63`), per-agent `auth_mode_for_agent`
  (`jackin-protocol/src/lib.rs:251-253`), then `apply_account_env`.
- `build_agent_command` (`capsule/session.rs:1607-1642`): strips **all** account env (:1619-1626),
  sets per-pane `JACKIN_AGENT` + `JACKIN_AUTH_MODE` (:1632-1637).
  `apply_account_env` (:1710-1724): injects `credentials.for_agent(agent)` **only** for
  `api_key|oauth_token` modes, else nothing. Shell panes strip and inject nothing (:1658-1677).
- `Session { agent: Option<String>, conversation_id: Option<String> (fresh UUID per spawn,
  :409), provider: Option<SessionProvider{label, env_overrides}> (:89-92, :119-123) }` — no
  account field anywhere. `env_for_spawn` (`multiplexer_utils.rs:12-27`) admits only the 7
  passthrough names, so provider `env_overrides` cannot smuggle account vars either; per-pane
  account choice is structurally unrepresentable.
- `runtime_setup::run_agent_setup` (`capsule/runtime_setup.rs:283-310`) reads one `JACKIN_AGENT`
  + one `JACKIN_AUTH_MODE` per entrypoint invocation (entrypoint runs per PTY spawn), seeds one
  forwarded credential per agent with first-seed gating (`seed_agent_home_from_enum`, :960-980;
  policy `apply_forwarded_credential`, :651-719).

### 1.7 Usage relay: single-agent-derived capabilities

- `forwarded_sources_from_launch(state, resolved_env)` (`runtime/usage_relay.rs:214-238`):
  profile surfaces = agents with `AuthProvisionOutcome::Synced` (only the selected agent can
  ever be `Synced`); env keys = account-registry names present in `resolved_env` (operator env,
  no account secrets). No account key in capability or surface identity.
- `resolved_launch_usage_inventory` (:154-161): closed agent list from `CapsuleConfig.agents`.

## 2. Credential staging flow (end to end)

1. `resolve_account` per agent → `AccountConfig{provider, credential}` (config).
2. `resolve_account_env_with(…, &[selected], …)` resolves `op://`/`$VAR` refs; rejects on-demand
   and empty values (`jackin-env/src/accounts.rs:59-74`).
3. `write_account_credentials` → `credentials/account-credentials.json` (0600/0700, atomic).
4. `RoleState::prepare_for_agents([selected])` provisions sync file slots (0600, symlink-safe,
   no-churn guards) + durable home skeleton; `configure_accounts` writes Codex
   `config.toml` (`model_providers.jackin_account`) / OpenCode `opencode.json` into the
   bind-mounted home (`launch/account_config.rs:12-198`).
5. `record_account_configuration` writes both fingerprints.
6. `docker run -v`: `state→/jackin/state`, `credentials→/run/jackin:ro`, per-agent homes
   `→/home/agent/<entry>`, sync files `→/jackin/<agent>/*` (`mounts.rs:47-133`), socket dir
   `→/jackin/run` carrying redacted `agent.toml` with `auth_modes` (`capsule_setup.rs:112-178`).
   Env = `resolved_env` (account-free) + metadata (`launch_runtime.rs:665-862`, secrets via
   `--env-file`, `capsule_setup.rs:275-360`).
7. Daemon loads `agent.toml` + `account-credentials.json` → `LaunchEnv` (one load, immutable).
8. Per-pane spawn: strip account env → set `JACKIN_AGENT`/`JACKIN_AUTH_MODE` → inject
   `for_agent` map iff mode is `api_key|oauth_token` → entrypoint seeds `/home/agent/*`
   from `/jackin/*` on first seed.

Apple-container backend (`runtime/apple_container.rs:228-319`) mounts **only** workspace mounts
+ socket dir — no `agent_mounts`, no credentials mount — so the whole account flow is
effectively docker-only today (also writes `supported_agents: vec![]`, :340).

## 3. Session identity flow (end to end)

`docker run … <image> <initial-agent-slug>` → PID1 daemon loads config+credentials →
first tab spawns initial agent → operator spawns sibling tabs/panes via
`SpawnRequest::{Agent(validated slug)|Shell}` → each pane gets `(agent slug | shell,
fresh conversation_id UUID, optional provider label)` → env = 7 passthrough vars +
per-agent account map (api_key/oauth_token only) → entrypoint per-pane setup keyed on
`JACKIN_AGENT`. Agent history/usage keyed by `(session_id, agent, provider label)`
(`session_lifecycle.rs:393-416`, `multiplexer_utils.rs:417-427`).

## 4. Precise gaps vs multi-account admission/instance requirements

G1. **No account dimension in any identity key.** Bindings (`Agent→1 ID`), `resolve_account`
    (→1 account), `AgentCredentialEnv` (slug→1 map), `ProvisionedAuth` (1 slot/agent),
    `Session` (slug only), usage surfaces (agent only). Admitting account B for an agent that
    already has account A has no representation at any layer.
G2. **Launch stages exactly one agent.** Four `[agent]`-only call sites in `orchestrate.rs`
    (:655 credentials, :668 provisioning, :702-708 config files, `helpers.rs:106-112` models)
    plus forced-`Ignore` for siblings (`capsule_setup.rs:52-61`). Sibling tabs today: no mounts
    (homes not durable, sync files absent), `auth_mode=ignore`, no credentials; background
    prewarm cannot fix mounts post-`docker run`.
G3. **Admission is all-or-nothing per instance.** One digest covers every account+binding in
    scope; adding/rotating/rebinding *any* account (even for an uninvolved agent) invalidates
    every restore (`→StartFresh`) and every hardline reconnect (denied) for the instance.
    No per-account fingerprint, no grandfathering, no migration.
G4. **Per-pane account selection impossible.** No `Session` account field; spawn path strips
    account vars and re-injects purely by slug; `env_for_spawn` allowlist excludes account vars;
    capsule validation rejects out-of-schema credential entries.
G5. **Env-var collision for same-agent multi-provider.** `credential_env` synthesizes singleton
    routing vars (`ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL*`, `OPENAI_BASE_URL`, …); two accounts
    for one agent cannot coexist in one pane env without a namespacing scheme (none exists).
G6. **Instance record is single-agent.** `agent_runtime: String` + per-agent restore matching;
    related-role flow treats a different agent as a *different instance lineage* (`restore.rs:133,
    259-288`). Multi-account instances need either a compound identity or an account axis on
    query/match/admit.
G7. **Backend skew.** Apple-container path stages no credential mounts/homes at all; any
    multi-account design must either bring it to parity or explicitly scope it out.
G8. **Stale comments assert the opposite of the code** (`mounts.rs:42-46`,
    `instance/lib.rs:187-191` claim all-agents provisioning). Any sibling/multi-account work
    built on those comments will mismatch `orchestrate.rs:668`.

## 5. File:line index of load-bearing sites

| Concern | Location |
|---|---|
| Single binding resolution + ambiguity error | `jackin-config/src/accounts.rs:380-437` |
| Per-account env synthesis | `jackin-config/src/accounts.rs:292-353` |
| `--account` single-ID, selected-agent-only | `launch.rs:149-151`, `programmatic.rs:326-361`, `launch_pipeline.rs:765-782` |
| Selected-agent-only staging (4 sites) | `orchestrate.rs:655-715`, `helpers.rs:98-112` |
| Siblings forced `Ignore` | `capsule_setup.rs:41-69` |
| One slot per agent | `jackin-instance/src/lib.rs:200-275, 361-374` |
| Provisioners (single source each) | `jackin-instance/src/auth.rs:149-163, 467-593, 926-1029` |
| Fingerprint + dual-file record | `launch/account_identity.rs:18-108` |
| Restore downgrade / hard error / hardline deny | `account_identity.rs:129-150`, `launch_pipeline.rs:844-983, 1112`, `runtime/attach.rs:366-395` |
| Credential file + RO mount | `account_identity.rs:152-168`, `mounts.rs:47-133`, `container_paths.rs:22` |
| Agent-keyed credential map | `jackin-protocol/src/account_credentials.rs:10-35` |
| Capsule credential validation | `capsule/config.rs:88-145` |
| Per-pane strip + slug-keyed inject | `capsule/session.rs:1607-1642, 1710-1724` |
| Daemon `LaunchEnv`, one-time load | `capsule/daemon.rs:412-419, 531-551` |
| Spawn allowlist + session identity | `daemon/session_lifecycle.rs:202-264, 320-416`, `session.rs:89-123, 409` |
| Entrypoint per-agent setup | `capsule/runtime_setup.rs:283-310, 440-502, 651-719` |
| Container env excludes account secrets | `launch_pipeline.rs:1313-1314, 1376-1382, 1428-1445`, `launch_runtime.rs:685-694` |
| Usage sources from single-agent sync | `runtime/usage_relay.rs:214-238` |
| Instance single-agent identity | `jackin-instance/src/manifest.rs:104, 135, 243-250`, `launch/restore.rs:251-288` |
| Apple backend: no credential mounts | `runtime/apple_container.rs:299-300, 340` |
