# Lane S1 — Catalog Expansion: Agent/AiProvider Fan-Out

Branch: `feat/multi-account-support`. Single owner of `Agent`/`AiProvider` fan-out.
No commits (per instructions).

## 1. What changed (summary)

Catalog grows 6 → 12 agents, 8 → 12 providers. Every exhaustive match site
compiles; behavior for the original 6 agents/providers is byte-identical
(no existing assertion changed meaning — only additions + 3 contract-tripwire
bumps documented in §9).

New agents: `antigravity` (Antigravity), `gemini` (Gemini), `cursor` (Cursor),
`muse` (Muse), `omp` (omp), `hermes` (Hermes).
New providers: `google`, `cursor`, `meta`, `openrouter`.

## 2. `Agent` enum (`jackin-core/src/agent.rs`)

- 6 variants + `ALL` (12), `slug`, `label`, `from_slug`, `FromStr` error text.
- `required_env_var`: Antigravity/Gemini → `GEMINI_API_KEY`; Cursor →
  `CURSOR_API_KEY`; Muse → `META_API_KEY` (verified in `muse login --help`
  per T01: overrides login); Omp/Hermes → `None` in ALL modes (no native
  billing; account layer selects the provider variable).
- `supported_modes`: Sync+ApiKey+Ignore for all six (Claude keeps OAuthToken).
- `runtime()`: wired to the six new adapters.

## 3. Adapters (`jackin-core/src/agent/adapters/{antigravity,gemini,cursor,muse,omp,hermes}.rs` + registry)

All follow the sibling shape (install block → `~/.<dir>/bin`, PATH, `--version`
smoke check). Installer URLs verified live 2026-09-17 (see §4).

| agent | binary | state dir | cred file | folder env | install path |
|---|---|---|---|---|---|
| antigravity | `agy` | `.gemini/antigravity-cli` | none (Keychain singleton) | `GEMINI_CLI_HOME`¹ | official install.sh (prefetch via manifest, §5) |
| gemini | `gemini` | `.gemini` | `oauth_creds.json` | `GEMINI_CLI_HOME`¹ | `npm i -g @google/gemini-cli` (fallback; no prefetch) |
| cursor | `agent` + `cursor-agent` | `.cursor` | `auth.json` | `CURSOR_CONFIG_DIR` | official install (fallback; no prefetch — install tree, not a binary) |
| muse | `muse` | `.config/muse`² | `auth.json` | none | FAIL CLOSED (no verified installer) |
| omp | `omp` | `.omp` | `agent/agent.db` (`SQLite`) | `PI_CODING_AGENT_DIR`³ | pinned npm `@18.2.4` (fallback; no prefetch) |
| hermes | `hermes` | `.hermes` | none (profiles dir) | `HERMES_HOME` | official install.sh (fallback; no prefetch — uv/venv env) |

¹ `GEMINI_CLI_HOME` names the *parent* to which `.gemini` is appended (noted in
code, capsule resolver appends it). ² XDG config, NOT `~/.muse` (T01
correction). ³ `OMP_PROFILE` selects a named profile inside (noted in code).

- Cursor smoke-checks `cursor-agent --version`, never bare `agent`: Grok Build
  also ships an `agent` symlink, so resolvers probe absolute paths.
- Muse fallback is a custom fail-closed block (`echo … >&2; exit 1`), NOT the
  shared retry loop (retrying a certain failure would burn ~15 s of sleeps).
- Hermes adapter notes Node ≥ 20 + Python runtime + PTY and `HERMES_TUI_DIR`.

## 4. Installer URLs + sources (all verified, not invented)

- agy: `https://antigravity.google/cli/install.sh` — extracted from the
  official docs page `https://antigravity.google/docs/cli/install` (fetched;
  page shows exactly this curl-pipe-bash + `~/.local/bin/agy`).
- gemini: `npm install -g @google/gemini-cli` (audit D; bin `gemini` verified
  via registry metadata).
- cursor: `https://cursor.com/install` (fetched; serves bash installer
  `cursor-agent-installer.sh`).
- omp: `npm install -g @oh-my-pi/pi-coding-agent@18.2.4` (bin `omp` verified
  via registry metadata; version observed 2026-09-17 — re-pin when the
  installer lane probes the registry).
- hermes: `https://hermes-agent.nousresearch.com/install.sh` (fetched; script
  header documents this exact invocation; uv with venv+pip fallback).
- muse: NO verified standalone installer → fail closed with actionable error
  (host install + prefetch). Never invented a URL.

## 5. Host binary prefetch (`jackin-image/src/agent_binary.rs`)

- NEW `resolve_antigravity`: live platform manifests
  `{base}/manifests/linux_{amd64,arm64}.json` → `{version, url, sha512}`
  (both manifests fetched OK during implementation), tarball member
  `antigravity`. musl flavors exist but role images are glibc — non-musl used.
- Gemini/Omp fail closed (`resolve_npm_only`): npm has no standalone artifact.
- Cursor/Hermes fail closed (`resolve_no_binary`): installer lays down a
  directory/env, not a relocatable binary.
- Muse fails closed (`resolve_no_installer`).
- Structural fix (not a shim): the pipeline only verified SHA-256, but
  Antigravity publishes SHA-512. Added `hash_file_sha512` + `parse_sha512_hex`
  + `ImageError::InvalidSha512Hex`, with length-dispatched verification
  (64 hex → SHA-256, 128 → SHA-512). agy prefetches WITH verification.
- `image_recipe::supported_set_uses_cache_bust` extended: Gemini/Cursor/Muse/
  Omp/Hermes always install from network at build time (no prefetch), so they
  need the cache-bust token like Claude/Grok. Antigravity prefetches → excluded.

## 6. `AiProvider` + account matrix (`jackin-config/src/accounts.rs`)

- New providers `Google`, `Cursor`, `Meta`, `OpenRouter` + slug/from_slug.
- `for_agent` now returns `Option<AiProvider>`: `None` for Omp/Hermes (pure
  multi-provider clients, no native billing). Callers compare
  `Some(provider) == for_agent(agent)` so `None` never matches.
- Call sites fixed: `editor.rs` bootstrap (skip `None`: Omp/Hermes default
  profiles need an explicit operator-chosen provider), `editor.rs` OAuth
  (skip defensively; only Claude ever flows), `account_cmd.rs` scan profile
  import (skip + comment), `account_cmd.rs` OAuth import (skip), `account add`
  provider default (`and_then`), usage `discovery/tests.rs` fixture
  (`expect`), dind `fixtures.rs` (`unwrap_or(OpenRouter)` — routable by both).
- Profile compatibility: owner==agent AND (`Some(provider)==native` OR agent
  in {Opencode, Omp, Hermes} multi-provider store). Prevents e.g. a Gemini
  profile serving Antigravity.
- ApiKey compatibility: native always; Claude/Codex UNCHANGED
  (Moonshot/Zai/Minimax only — OpenRouter deliberately NOT routed directly;
  it reaches Claude/Codex-shaped workloads only via OpenCode/Omp/Hermes with
  explicit model); Opencode/Omp/Hermes accept all providers EXCEPT Amp
  (matches the existing Opencode precedent — an Amp key cannot authenticate
  anything but amp; brief said "decide", this preserves the invariant).
- `api_key_variable`: Antigravity/Gemini → `GEMINI_API_KEY`, Cursor →
  `CURSOR_API_KEY`, Muse → `META_API_KEY`; multi-provider clients select per
  provider (incl. all four new vars). NOTE: OpenCode's map previously fell
  through to `OPENCODE_API_KEY` for unknown providers — new providers now get
  their native vars (behavior change only for previously-unrepresentable
  accounts).
- `default_api_url`: added `(Opencode|Omp|Hermes, OpenRouter)` →
  `https://openrouter.ai/api/v1` (documented base; application for Omp/Hermes
  deferred to the provider-config lane). Native Google/Cursor/Meta need no
  override; unknown combos fail closed (`None`).
- `credential_env`: model-required check extended to Omp/Hermes non-native
  (for_agent None ⇒ always requires explicit model); Omp/Hermes skip env
  endpoint injection like OpenCode (provider-config writers are a later lane).

## 7. Discovery (`jackin-config/src/accounts/discovery.rs`)

- Env scan: `GEMINI_API_KEY` canonical + `GOOGLE_API_KEY` alias (documented
  choice), `CURSOR_API_KEY`, `META_API_KEY`, `OPENROUTER_API_KEY`.
- Antigravity: Keychain-only. `settings.json` can never be evidence (prefs
  only) so it is not read at all; evidence = Keychain service `gemini`
  (account `antigravity` per T01; helper probes service only).
- Gemini: `oauth_creds.json`, nonempty access/refresh/token (docs-derived,
  unverified live — honest comment).
- Cursor: `auth.json`, nonempty accessToken/refreshToken (verified).
- Muse: `auth.json`, `providers.meta` present (verified shape
  `{schema_version: 2, providers: {meta: …}}`; secret stays in Keychain).
- OpenCode/Omp/Hermes: wired to the `stores` enumerators (see §8).
- OpenCode primary remains `~/.local/share/opencode/auth.json` (adapter
  already used XDG data — no fallback needed; report note per brief).
- `DiscoveryError::Malformed` message generalized to "credential source is
  not parseable" (SQLite stores are not JSON).

## 8. Stores-lane coordination (shared-tree timeline)

1. Parent directed SQLite-aware OpenCode discovery; `rusqlite` is used
   NOWHERE in the workspace, and the stores lane owns parsing → I first
   shipped a header-only probe + `SqliteStoreUnparsed` issue variant.
2. Parent urgently directed wiring the landed `stores/` enumerators into my
   matchers (dead_code denies blocked the workspace). Wired:
   `inspect_store` → `opencode::enumerate_opencode_store` (auth.json AND
   `credential` table), `omp::enumerate_omp_credentials` (SQLite, content-
   verified — strictly stronger than my presence check),
   `hermes::enumerate_hermes_store` (config.yaml + profiles + auth.json);
   first candidate's source file becomes the `File` evidence; secrets dropped
   at the boundary. `StoreError::{Unreadable,TooLarge,Malformed,Unsupported}`
   map to discovery categories (`Unsupported` → `Malformed`: both mean
   present-but-unverifiable). Removed the interim probe + variant.
3. The stores lane then deleted `StoreCandidate::secret()` /
   `StoreKind::slug()` (chose deletion over waiting for callers), which broke
   my in-progress `discover_store_credentials` import API — I reverted the
   API + `account scan` import to the presence-only wiring. SEAM FOR
   FOLLOW-UP (noted in code): per-value secret import awaits a secret
   accessor on `StoreCandidate`; `account scan` imports stores-backed
   *profiles* but not the individual routed keys.
4. Hermes semantics note: a profile-less `auth.json` (no attributable
   profile) yields NO discovery account by stores contract; empty SQLite
   tables (the observed local opencode.db state) likewise yield nothing —
   honest `Ok(None)`, never a claimed account.

## 9. Match-site list (every file touched, by area)

Core: `agent.rs`, `agent/adapters.rs`, `agent/adapters/{antigravity,gemini,
cursor,muse,omp,hermes}.rs` (new), `agent/tests.rs`, `env_model.rs`
(+4 key names, +4 registry owners), `container_paths.rs` (+6 dirs, +6 auth
files), `manifest.rs` (new agents → `false`/`None`: no `[<agent>]` tables
until the agent-map schema bump).
Config: `accounts.rs`, `accounts/tests.rs`, `accounts/discovery.rs`,
`accounts/discovery/tests.rs`, `editor.rs`, `lib.rs` (no export change;
`discover_store_credentials` reverted — lib.rs back to original).
Instance: `lib.rs` (6 slot structs + fields + dispatch + slot fns + stale
paths), `auth.rs` (validation arms + 6 provisioners + `provision_single_blob_
credential` for omp's SQLite + `provision_hermes_dir_credential`),
`tests.rs` (Ignore arm). Muse provisioner arg-order bug I introduced caught
by the compiler and fixed.
Runtime: `mounts.rs` (6 mounts), `launch_runtime.rs` (debug map),
`docker_profile.rs` (egress: google→generativelanguage, cursor→api2.cursor.sh
(from `CURSOR_API_ENDPOINT` default), muse→none (fail closed), omp/hermes→
openrouter.ai), `launch/account_config.rs` (opencode provider map: google→
`@ai-sdk/google`/v1beta, openrouter→openai-compatible/v1; Cursor/Meta BAIL
with explicit deferred-custom-provider message — no catalog entry exists).
Capsule: `runtime_setup.rs` (dispatch + `setup_*` × 6 + path resolvers
honoring `GEMINI_CLI_HOME` append-semantics, `CURSOR_CONFIG_DIR`,
`PI_CODING_AGENT_DIR`, `HERMES_HOME`; hermes dir-seeds like kimi).
Usage: `host.rs` (+4 surfaces; `from_agent` maps Omp/Hermes→OpenCode surface
as documented approximation; `DESKTOP_PROVIDER_ORDER` deliberately
UNCHANGED), `host/discovery.rs` (`provider_surface` + `profile_identity`:
Muse email label + Cursor cli-config label are real verified readers;
Gemini/Hermes anonymous-when-present; Omp presence; Antigravity Missing with
Keychain-probe note; +4 credential-matrix rows), `token_monitor.rs` (polls
Unchanged, delta 0, provider names incl. new telemetry values; Omp/Hermes
None), `host/tests.rs` + `host/discovery/tests.rs` (contract bumps).
Image: `agent_binary.rs`, `binary_artifact.rs`, `error.rs`,
`process_telemetry.rs`, `image_recipe.rs`, `derived_image/tests.rs` (PATH
expectation extended — derived automatically from adapters).
Telemetry: `registry/attributes.yaml` + `registry/rust.toml` + regenerated
`schema/enums.rs` via `cargo xtask telemetry-registry --generate`
(+6 agent, +4 provider, +6 executable names), `process.rs` classifier (bare
`agent` stays `Other`: Grok-alias/Cursor-binary ambiguity),
`schema/tests.rs` (AgentName 6→12 contract bump).
Agent-status: `process.rs` (`agy`→Antigravity, `cursor-agent`→Cursor, npm
wrappers `@google/gemini-cli`→Gemini, `@oh-my-pi/pi-coding-agent`→Omp),
`rules/tests.rs` (reviewed NO_SCREEN_DETECTOR opt-outs × 6 — packs need live
TUI capture, agent-status lane).
Console: `auth.rs` (+6 `AuthKind`s, panels, labels, modes, env vars,
source-folder), `auth_config.rs` (kind→agent), `input/global_mounts/auth.rs`
(kind→provider; Omp/Hermes creation refused with CLI pointer, EDITS keep the
existing provider), `screens/settings/model/auth_impls.rs` (`ACCOUNT_KINDS`,
owner-aware `account_kind` — FIXES a latent corruption where provider-only
mapping would rewrite an Antigravity profile's owner to Gemini on save),
tests, 1 insta snapshot + 4 PNG baselines re-blessed (drift confined to
auth/account screens; render-twice determinism passed).
CLI: `account_cmd.rs` (for_agent sites), `dind_e2e/fixtures.rs`.
Docs: `account.mdx` provider/agent lists.
Deliberately NOT changed (correct fail-closed defaults): capsule
`agent_model_args` (no verified `--model`/`-m` flags for newcomers — wildcard
yields no flag), `grade_for_runtime` (new runtimes → Partial), `coauthor_
trailer_for_agent` (None), `account_env` routing names (new agents emit no
base-URL env).

## 10. Decisions log (explicit)

1. `GEMINI_API_KEY` for both Google agents; `GOOGLE_API_KEY` discovery-only
   alias (documented in code).
2. `META_API_KEY` for Muse ApiKey (verified per T01 `muse login --help`).
3. Omp/Hermes: `required_env_var` None everywhere; accept all providers but
   Amp for ApiKey (brief-delegated decision; preserves the Amp invariant).
4. Muse credential dir `.config/muse` (T01 correction over the brief's
   `.muse`); no `MUSE_HOME`.
5. Cursor ships both `agent` + `cursor-agent` paths; never resolve bare
   `agent` (Grok collision).
6. Muse/antigravity-fail-closed philosophy: no invented URLs anywhere.
   (Antigravity got a REAL verified installer, so only Muse fails closed.)
7. Omp npm pin `@18.2.4` observed 2026-09-17 (re-pin by installer lane).
8. SHA-512 verification plumbing added (not skipped) for agy.
9. Cursor/Meta → OpenCode config bails explicitly (no catalog entry; custom
   provider lane deferred). Matrix still lists them compatible per brief;
   the bail is the honest boundary.
10. Stores seam: presence-only wiring; secret import reverted for lack of a
    reader (follow-up noted in code).
11. `DESKTOP_PROVIDER_ORDER` unchanged (Desktop FFI contract — usage lane).
12. OpenCode SQLite: content-verified via stores (empty tables → honest None).

## 11. Test results

| suite | result |
|---|---|
| jackin-core | 131 pass |
| jackin-config | 331 pass (incl. new matrix + discovery tests) |
| jackin-instance | 140 pass |
| jackin-image | 132 pass |
| jackin-telemetry | 63 pass |
| jackin-agent-status | 111 pass |
| jackin-usage | 385 pass |
| jackin-capsule lib | 831 pass, 3 fail — PRE-EXISTING test-isolation defect: `conformance_wire_*` OTLP tests fail with "OTLP providers are already active" when run together, each passes alone; my `runtime_setup` area: 29/29 pass |
| jackin-runtime | lib+tests COMPILED green (workspace --all-targets green); test RUN blocked by foreign `usage/kimi.rs` churn (provider lane) at report time |
| jackin-console | 1268 pass + insta/PNG baselines re-blessed for the 4 auth screens; full re-run blocked by foreign `usage/tests.rs` churn at the time |
| clippy | clean on core/config/instance/image (+telemetry/agent-status/usage-mine); remaining usage warnings are the provider lane's files |
| fmt | all touched files rustfmt-clean |

`cargo check --workspace --all-targets`: green at multiple points; the shared
tree oscillates as lanes land (protocol openrouter/usage_broker churn observed
and recovered during this session — none of it mine).

## 12. Deferred items (for later lanes, with owners)

1. Omp/Hermes provider-config writers (provider-config lane): endpoint/model
   application for routed keys; Cursor/Meta custom OpenCode entries.
2. `StoreCandidate` secret accessor + `account scan` per-key import
   (stores/import lane): re-add `discover_store_credentials`-style API.
3. Token-spend readers + per-provider attribution for the 6 newcomers;
   Antigravity Keychain probe (usage lane).
4. Screen-detector rule packs × 6 (agent-status lane; reviewed opt-outs in
   place).
5. Manifest `[<agent>]` tables (agent-map schema bump).
6. omp npm re-pin when the installer lane probes the registry.
7. Capsule model-flag mapping for newcomers (needs verified CLI surfaces).
8. `DESKTOP_PROVIDER_ORDER` + FFI/Desktop additions (usage/Desktop lane).

## 13. Shared-tree incidents (for the orchestrator)

- `jackin-console/src/tui/auth.rs` + `auth_config.rs`: my 7 edits were
  clobbered by a concurrent whole-file write (files found at pristine HEAD);
  re-applied and re-verified. Final 43-marker clobber audit: all present.
- `stores/mod.rs` restructure mid-session (re-exports added then removed;
  `secret()`/`slug()` deleted) — coordinated by reverting to presence-only.
- Foreign breakage observed (not mine, recovered or reported): test-support
  scaffolding errors, `stores/sqlite` + `stores/omp/tests` missing modules,
  `jackin-protocol::usage_broker` iterations, `usage/openrouter.rs` 8 errors,
  console `usage/tests.rs` struct-shape churn, capsule OTLP test contention.
