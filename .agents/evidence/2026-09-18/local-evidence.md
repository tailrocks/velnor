# Local installation evidence — agent CLIs on macOS (darwin/arm64, macOS 26.6.2)

Collected: 2026-09-17 ~13:00 UTC. User: donbeave. HOME: /Users/donbeave.
READ-ONLY collection. No login flows run. No secret values, no key material, no 1Password locators below —
only binary paths/versions, file names + sizes + perms, JSON key NAMES (+value lengths/types), and
safe version/schema/model-name markers. Keychain: service/label names + counts only.

## 1. claude — INSTALLED 2.1.274 (Claude Code)

- Binary: /opt/homebrew/bin/claude -> /opt/homebrew/Caskroom/claude-code@latest/2.1.274/claude (204 MB, brew cask claude-code@latest)
- `claude --version` => `2.1.274 (Claude Code)`
- $HOME paths:
  - ~/.claude.json 196499 B mode 0600 (keys incl oauthAccount{accountUuid,emailAddress,organizationUuid,…names only}, migrationVersion=14, lastOnboardingVersion=2.1.215, lastReleaseNotesSeen=2.1.274, machineID len 64, userID len 64)
  - ~/.claude/ : settings.json 1992 B (model len 20, hooks{PreToolUse,SessionStart}, enabledPlugins 5, effortLevel, theme…), history.jsonl 778652 B, stats-cache.json 13860 B (version=5, dailyModelTokensVersion=5), projects/ (72 entries), sessions/, skills/ (58), plugins/, daemon/, backups/, file-history/, shell-snapshots/, tasks/
  - Profile variants present: ~/.claude-chainargos, ~/.claude-scentbird, ~/.claude-scentbird-ai, ~/.claude-scentbird-ai-yolo, ~/.claude-scentbird-yolo, ~/.claude-zhokhov
  - ~/Library/Application Support/Claude (Crashpad/completed seen)
- Keychain (login.keychain-db): "Claude Code-credentials", "Claude Code-credentials-db49ea31", "Claude Code-credentials-93aecf3d", "Claude Code-credentials-74a5d03f", "Claude Safe Storage" (5 items)

## 2. codex — INSTALLED 0.154.0

- Binary: /opt/homebrew/bin/codex -> /opt/homebrew/Caskroom/codex/0.154.0/bin/codex (212 MB, brew cask codex)
- `codex --version` => `codex-cli 0.154.0`
- $HOME paths (~/.codex):
  - auth.json 3969 B mode 0600: keys auth_mode("chatgpt",7ch), OPENAI_API_KEY=null, tokens{id_token,access_token,refresh_token,account_id}, last_refresh=2026-09-15T21:32:53Z
  - config.toml 4488 B mode 0600, 0 secret-looking lines; model="gpt-5.6-luna", reasoning=max; sections: [projects.*] trust_level, [mcp_servers.firecrawl|node_repl|computer-use], [tui], [features], [memories], [agents] max_concurrent_threads_per_session=8, [model_providers.kimi|zai] (env_key NAMES only: KIMI_API_KEY, ZAI_API_KEY), [marketplaces.*], [plugins.*] x11, [hooks.state], [desktop]
  - glm.config.toml 147 B / glm-flash 110 B / kimi.config.toml 148 B / kimi-1m 144 B (model+provider+effort+context_window only)
  - models.json 2964 B keys [models]; models_cache.json 228426 B keys [fetched_at,etag,client_version,models]
  - session_index.jsonl record keys [id,thread_name,updated_at]; installation_id 36 B; .sandbox_migration 3 B
  - .codex-global-state.json 593787 B (+ .bak same size); logs_2.sqlite 227 MB, thread_history_1.sqlite 469 MB, state_5.sqlite 1.5 MB (tables: _sqlx_migrations, backfill_state, external_agent_config_imports, project_idempotency_keys, project_roots, thread_attachments, thread_dynamic_tools, thread_sections, …)
  - Profile variants: ~/.codex-chainargos, ~/.codex-chainargos2, ~/.codex-scentbird, ~/.codex-zhokhov; ~/.codexbar (claude-oauth-cache.lock 0 B); ~/.config/codexbar/config.json 4397 B
  - ~/Library/Application Support/Codex + com.openai.codex; ~/.cache/codex-runtimes
- Keychain: "Codex MCP Credentials" (1), "com.steipete.codexbar.cache" (3)

## 3. amp — INSTALLED 0.0.1789639648-g3c529d

- Binary: /opt/homebrew/bin/amp -> /opt/homebrew/Cellar/ampcode/0.0.1789639648-g3c529d/bin/amp (72 MB, brew formula ampcode)
- `amp --version` => `0.0.1789639648-g3c529d (released 2026-09-17T10:07:28Z)`
- $HOME paths:
  - ~/.config/amp: AGENTS.md 834 B; plugins/ EMPTY
  - ~/.local/share/amp: device-id.json 62 B {installationID str len 36}, secrets.json 141 B mode 0600 {key `apiKey@https://ampcode.com/` str len 102}, session.json 882 B {agentMode,launchCount,lastThreadId,…}, history.jsonl 5605 B (record keys [text,cwd])
  - ~/.cache/amp: logs/{cli.log,cli.log.1,cli.log.2,plugin-runtime.log,threads/}, global-plugins/
  - ~/.amp-scentbird (profile variant: cache/, config/, data/)
- Keychain: none found for amp.

## 4. agy — INSTALLED 1.2.5 (Antigravity CLI, self-updated)

- Binary: /opt/homebrew/bin/agy (175 MB REAL FILE, not symlink; no brew formula `agy`; sibling agy.1789629768677920000.old 183 MB = self-update backup)
- `agy --version` => `1.2.5`; `agy models` works (Gemini 3.8/3.7/3.6 flash high/med/low, 3.1-pro, claude-sonnet-4-6, claude-opus-4-6-thinking, gpt-oss-120b-medium); `agy mcp list` => none; `agy plugin list` => imports[jackin-dev from "antigravity", skills]; remote-control daemon: not running
- Related installs: brew cask antigravity-cli 1.1.26 (ships `antigravity` binary 177 MB, NOT linked to PATH; DIFFERENT bytes from agy), brew cask antigravity 2.14.0 GUI (/Applications/Antigravity.app)
- $HOME paths: NO agy-specific config dir found (~/.agy, ~/.config/agy, ~/Library/*agy* all absent; no AGY_* env). Auth/state shared with Antigravity store under ~/.gemini/antigravity-cli (see §12).
- Keychain: none found for agy/antigravity/auggie.

## 5. kimi — INSTALLED 0.43.0

- Binary: /opt/homebrew/bin/kimi -> ../Cellar/kimi-code/0.43.0/bin/kimi -> …/node_modules/@moonshot-ai/kimi-code/dist/main.mjs (21 MB node app, brew formula kimi-code)
- `kimi --version` => `0.43.0`
- $HOME paths (~/.kimi-code):
  - config.toml 3530 B: default_model="kimi-code/k3"; [providers."managed:kimi-code"] type=kimi base_url=https://api.kimi.ai/coding/v1 + .oauth{storage=file,key=oauth/kimi-code-env-<hex>,oauth_host=https://auth.kimi.ai}; 4 x [models.*] (k3, k3-256k, kimi-for-coding, kimi-for-coding-highspeed, contexts 1M/256k); [thinking] effort=max; [services.moonshot_search|moonshot_fetch]; (3 secret-pattern lines withheld, counts only)
  - tui.toml 607 B (theme=auto, editor, notifications, upgrade.auto_install, status_line command)
  - credentials/kimi-code-env-<hex>.json 1556 B mode 0600; credentials/kimi-code.json 136 B {access_token len 0, refresh_token len 0, expires_at, scope="kimi-code", token_type="Bearer", expires_in=0} (empty placeholder)
  - oauth/{kimi-code,kimi-code-env-<hex>} 0 B markers; device_id 36 B; workspaces.json 2032 B {version=1, 10 workspaces}; feedback-survey-state.json {version=1}; migrations-effort.json; sessions/, telemetry/ (4413 entries), user-history/, workspace-trust/, search-index/, plugins/, skills/, hooks/, logs/, cache/, file-history/
- Keychain: none found for kimi/moonshot.

## 6. muse — INSTALLED 1.3.0 (Muse Code, Meta)

- Binary: /opt/homebrew/bin/muse -> /opt/homebrew/Caskroom/muse-code/1.3.0-R3233.1/muse-aarch64-macos (272 MB, brew cask muse-code)
- `muse --version` => `Muse Code 1.3.0 (1.3.0-R3233.1)`
- $HOME paths:
  - ~/.config/muse: auth.json 295 B mode 0600 {schema_version=2, providers{meta{mechanism,storage,obtained_via,api_base_url,user_full_name,user_email — names only}}}, settings.json 174 B {schema_version=1, provider="meta", model="muse-spark-1.3-contributor", reasoning_effort="max", tui{…}}, trust.json 538 B mode 0600 {schema_version=1, projects: 5 local paths}, lock files 0 B
  - ~/.local/share/muse: session-index.db 1.1 MB, tui-history.jsonl 195934 B, sessions/, memory/, model-catalog/, feature-config/, plugins/, skills/, runtime/, local-tracing/
  - ~/Library/Application Support/Muse
- Keychain: "ai.meta.dev.credentials" (1)

## 7. agent / cursor-agent — INSTALLED

- agent: /opt/homebrew/bin/agent -> /opt/homebrew/Caskroom/grok-build/1.0.30/bin/agent -> grok-1.0.30-macos-aarch64 (135 MB). SAME bytes as grok binary (alias). `agent --version` => `grok 1.0.30 (04b7ffed98c6) [stable]`
- cursor-agent: /opt/homebrew/bin/cursor-agent -> /opt/homebrew/Caskroom/cursor-cli/2026.09.10-fd3934a/bin/cursor-agent (1.1 KB BASH WRAPPER: resolves SCRIPT_DIR, sets NODE_COMPILE_CACHE=~/Library/Caches/cursor-compile-cache on darwin, honors AGENT_CLI_CREDENTIAL_STORE=file to skip system CA, execs $SCRIPT_DIR/node --use-system-ca $SCRIPT_DIR/index.js). `cursor-agent --version` => `2026.09.10-fd3934a`
- $HOME paths (~/.cursor):
  - auth.json 887 B mode 0600 {accessToken len 424, refreshToken len 424 — names+lengths only}
  - cli-config.json 2795 B {version=1, model.modelId=composer-2.5, exploreSubagentModel=default, approvalMode=allowlist, permissions{allow,deny}, editor, display, statusLine, modelParameters, selectedModel, modelSelectionHistory[4], authInfo{email,displayName,userId,authId — names only}, sandbox{mode,networkAccess}, …}
  - agent-cli-state.json 94 B {version=1, hasShownAgentCommandTip, hasClearedLegacyStatsigFields}; hooks.json 143 B {version=1, hooks{preToolUse}}; argv.json 798 B JSONC (enable-crash-reporter=true, crash-reporter-id present UUID 36ch — value withheld); statsig-cache.json 803988 B; agents/, chats/, projects/, extensions/, plugins/, skills/, skills-cursor/, ai-tracking/, statusline.sh
  - ~/.agents (.skill-lock.json 26383 B, skills/ 57), ~/.config/agents (skills/ 25)
  - ~/Library/Application Support/Cursor; ~/Library/Caches/cursor-compile-cache (per wrapper); /Applications/Cursor.app (via brew link 3.20.17)
- Keychain: "cursor-access-token", "cursor-refresh-token", "Cursor Safe Storage" (3)

## 8. grok — INSTALLED 1.0.30 (xAI grok-build)

- Binary: /opt/homebrew/bin/grok -> /opt/homebrew/Caskroom/grok-build/1.0.30/bin/grok -> grok-1.0.30-macos-aarch64 (135 MB, brew cask grok-build; also ships `agent` alias)
- `grok --version` => `grok 1.0.30 (04b7ffed98c6) [stable]`
- $HOME paths (~/.grok):
  - auth.json 1775 B mode 0600: single key `https://auth.x.ai::<uuid>` (uuid withheld) -> {auth_mode,coding_data_retention_opt_out,create_time,email,expires_at,first_name,key,last_name,oidc_client_id,oidc_issuer,principal_id,principal_type — names only}
  - config.toml 443 B, 0 secret lines: [mcp_servers.firecrawl] url=https://mcp.firecrawl.dev/v2/mcp-oauth, [ui.status_line] command, [marketplace] + [[marketplace.sources]] "xAI Official" git=https://github.com/xai-org/plugin-marketplace.git
  - version.json {version=1.0.30, stable_version=1.0.30, checked_at=2026-09-15T16:26:44Z}; .metadata_version="1.0.30"; agent_id 36 B
  - models_cache.json 4717 B {grok_version=1.0.30, models=[grok-4.6, grok-4.5], auth_method="session", origin=https://cli-chat-proxy.grok.com/v1/models, fetched_at, etag}; settings_cache.json 7995 B {payload len 7153, signature[32]}; CHANGELOG.json list[28]; tip_cursor.json {cursor int}; active_sessions.json []; sessions/, skills/ (39), bundled/{agents,personas,roles,skills,workflows,manifest.json 54 KB}, grove/, logs/unified.jsonl, worktrees.db 40 KB, README.md 109 KB, AGENTS.md 355 B
- Keychain: none found for grok/xai.

## 9. opencode — INSTALLED 1.18.30

- Binary: /opt/homebrew/bin/opencode -> ../Cellar/opencode/1.18.30_2/bin/opencode (206 MB, brew formula opencode); /Applications/OpenCode.app + cask opencode-desktop also present
- `opencode --version` => `1.18.30`
- $HOME paths:
  - ~/.config/opencode: opencode.json 455 B {$schema=https://opencode.ai/config.json, command{goal}, mcp{firecrawl}, plugin=[opencode-caveman@0.1.4, ./plugins/rtk.ts, opencode-goal-plugin@0.8.2]}, opencode.jsonc 50 B ($schema only), plugins/rtk.ts, skills/ (52), agents+AGENTS.md 992 B each, node_modules/, package.json
  - ~/.local/share/opencode: opencode.db 258 KB (tables: account, account_state, control_account, credential, data_migration, message, migration, part, permission, project, session_context_epoch, session_input, session_message, session_share, todo — auth lives in `credential` table, not dumped), mcp-auth.json 498 B mode 0600 {firecrawl{tokens,clientInfo,serverUrl — names only}}, log/opencode.log, repos/ (empty)
  - ~/.cache/opencode: models.json 4.6 MB, bin/ (empty), packages/
  - ~/.opencode (project-style dir: AGENTS.md, package.json, node_modules/)
- Keychain: none found for opencode.

## 10. omp — NOT INSTALLED

- No binary: omp, oh-my-posh, ohmyposh all NOT on PATH; no mise shim; no cargo/npm/local bin; no brew formula/cask (brew `libomp` = LLVM OpenMP runtime, unrelated); no ~/.omp* or ~/.config/oh-my-posh config.
- Lookalikes (not omp): ~/.oh-my-zsh (shell framework, 22 entries).
- Keychain: none found for omp/posh.

## 11. hermes — BINARY NOT INSTALLED (skills dir only)

- No binary: hermes/hermesc NOT on PATH; no brew formula/cask.
- $HOME: ~/.hermes/skills/ only (32 skill dirs: brandkit, design-taste-frontend, firecrawl x22, grill-me, redesign-existing-projects). No auth/config files.
- Keychain: none found for hermes.

## 12. gemini CLI — BINARY NOT INSTALLED (config + Antigravity data present)

- No binary: gemini/gemini-cli/google-gemini NOT on PATH; no brew formula/cask named gemini (but casks antigravity 2.14.0 + antigravity-cli 1.1.26 installed — see §4).
- $HOME paths (~/.gemini):
  - settings.json 260 B mode 0600 {hooks{BeforeTool}}; GEMINI.md 964 B; hooks/; skills/ (53); antigravity/skills/ (25)
  - config/: config.json 167 B {plugins,userSettings}, hooks.json 209 B {herdr}, import_manifest.json 185 B {imports}, mcp_config.json 0 B, plugins/, projects/, .migrated 0 B
  - antigravity-cli/ (live Antigravity/agy data): settings.json 800 B {model len 23, statusLine{command,type}, toolPermission len 14, trustedWorkspaces[9]}, installation_id 36 B, bin/{agentapi 53 B, webm_encoder 12.8 MB}, updater/update_status.json, conversations/, conversation_summaries.db, brain/, knowledge/, cache/, log/, cli.log, history.jsonl, skills/, plugin_data/, annotations/, builtin/, crashes/, implicit/, presence/, jetski_state.pbtxt, jetbox_summaries_proto.pb, last_check.timestamp
- Keychain: "gemini" (1 generic-password item, value never read).

## 13. Container / toolchain state

- docker: client+server 29.4.0 (client darwin/arm64, context=orbstack; server linux/arm64, containerd v2.2.2, runc 1.5.1); binary /Users/donbeave/.orbstack/bin/docker -> /Applications/OrbStack.app/Contents/MacOS/xbin/docker
- orbstack: `orbctl status` => Running; ~/.orbstack/{bin,config,log,run,shell,ssh,.installid 36 B,vmconfig.json 25 B,vmstate.json 189 B}; ~/OrbStack/{docker,capture-linux-validation,README.txt}; /Applications/OrbStack.app
- mise: 2026.9.9 macos-arm64 (notes 2026.9.10 available); shim /Users/donbeave/.local/share/mise/shims/mise -> ~/.local/bin/mise; config ~/.config/mise/{config.toml 1055 B, mise.lock 9581 B}; `mise ls` tools (name — installed versions; *=pinned by jackin mise.toml):
  actionlint 1.7.12*, aqua apple/container 1.2.2*, cargo-nextest 0.9.140*/0.9.143, cargo-deny 0.20.2, cargo-audit 0.22.2, cargo-zigbuild 0.23.3, actionlint 1.7.12, protoc 35.1, bun 1.4.0; bun 1.3.14*/1.4.2; cargo-binstall 1.21.0/1.21.1*/1.22.0; cargo alint 0.15.2, boltffi_cli 0.30.1*, cargo-audit 0.22.2*, cargo-deb 3.7.0, cargo-deny 0.20.2*, cargo-dylint 6.0.4*, cargo-fuzz 0.13.2*, cargo-hack 0.6.45*, cargo-hakari 0.9.38*, cargo-llvm-cov 0.8.7*, cargo-mutants 27.1.0*, cargo-semver-checks 0.48.0, cargo-shear 1.13.1/1.13.4*, cargo-zigbuild 0.23.0*/0.23.3, codebook-lsp 0.3.42*, dylint-link 6.0.4*, git-cliff 2.13.1, repolint rev:df4a101, rust-script 0.36.0, sccache 0.16.0*, wasm-pack 0.15.0; cosign 3.1.2/3.1.3*; fnox 1.35.1; gh 2.96.0/2.98.0/2.100.0; github gitleaks 8.30.1, jdx/mise 2026.7.12, weaver 0.24.2*; gitleaks 8.30.1; hyperfine 1.20.0*; java oracle-graalvm-23.0.2/25.0.3/25.0.4; jq 1.8.2; lychee 0.20.1; mr-boxington 1.11.1 (global config); node 22.23.2/24.18.0*/24.20.0/26.8.1; opentofu 1.12.5; periphery 3.8.0*; pipx reuse 6.2.0*; protoc 35.1; python 3.14.6/3.14.7*; ripgrep 15.2.0*; ruby 4.0.6; rust 1.95.0/1.97.1*(rust-toolchain.toml)/1.98.0/1.98.1 (symlinks); shellcheck 0.11.0*; shfmt 3.12.0; swiftlint 0.65.1*; syft 1.46.0*; uv 0.11.29*; xcbeautify 3.2.1*; xcodegen 2.46.0*; zig 0.16.0*; zizmor 1.29.0

## 14. Keychain summary (login.keychain-db; 72 genp + 2 inet items; names only)

- claude: "Claude Code-credentials" x4 (base + -db49ea31, -93aecf3d, -74a5d03f), "Claude Safe Storage"
- codex: "Codex MCP Credentials", "com.steipete.codexbar.cache" x3
- cursor-agent: "cursor-access-token", "cursor-refresh-token", "Cursor Safe Storage"
- muse: "ai.meta.dev.credentials"
- gemini: "gemini"
- NONE for: amp, agy/antigravity, kimi, grok/agent, opencode, omp, hermes
- Method: `security dump-keychain` filtered to class/svce/labl/srvr lines; password-blob (0x00000008) and any value lines excluded by construction; no -g/-w flags used.
