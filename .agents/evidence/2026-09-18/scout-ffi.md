# T19 FFI/bindings parity scout

Date: 2026-09-17. Scope: research-only. Tree is moving fast (T10/S1 lanes
landing in working tree; `usage_broker.rs` gained `metric_groups` mid-scout).
T19 must re-verify every line number before editing.

## 1. Consumer inventory

### 1a. Protocol `control.rs` views → consumers

| Shared type (def) | FFI consumer | CLI consumer (`cli/usage.rs`) | Native/other consumer |
|---|---|---|---|
| `FocusedUsageView` (`control.rs:554`) | `view_dto` (`dto.rs:560`), `view_dto_with_context` (`dto.rs:570`); `bridge.snapshot` (`bridge.rs:225`) | `run_host_snapshot` `snapshot()` + JSON envelope/human (`usage.rs:366-402`) | Capsule dialog via `UsageGenerationView.snapshot` (`usage_broker.rs:1019`); host runtime |
| `FocusedAccountHeader` (`control.rs:682`) | → `UsageViewDto` identity fields (`dto.rs:577-585`) | account/plan/credential lines (`usage.rs:382-388`) | same as above |
| `QuotaBucketView` (`control.rs:808`) | `bucket_dto` (`dto.rs:689`) → `QuotaBucketDto` | bucket lines (`usage.rs:392-397`) | `account_snapshot_views_from_cache` (`usage/view.rs:122`); `project_window`/`project_groups` (`host/projection.rs:293-298`) |
| `Money` (`control.rs:~513`) | `money_dto` (`dto.rs:725`) | via `quota_amounts_for_account_snapshot` (`usage/view.rs:159`) | `UsageMetricValueV1` money payloads |
| `UsageSnapshotStatus/Source/Confidence` (`control.rs:929/949/965`) | string labels via `usage_*_storage_label` (`usage.rs:538/550/560`) | Debug print (`usage.rs:379-381`); verify strings (`usage.rs:612-619`) | storage labels shared |
| `UsageDetailPresentation/Row` (`control.rs:903/877`) | `detail_presentation_dto` + provenance/lifecycle splice (`dto.rs:526-558`); builder `usage_detail_presentation` (`usage/format.rs:783`) | — | Capsule dialog (same builder = parity handoff); Swift `ProviderDetailView` renders `content.detail.rows` verbatim |
| `UsageIdentityPresentation` (`control.rs:597`) | `identity_dto` (`dto.rs:599`); builder (`usage/format.rs:598`) | — | Capsule + Desktop shared |
| `StatusSlot` (`control.rs:707`, closed: session/daily/weekly/spend) | string map (`dto.rs:699-707`) | — | Swift renders without inference (FFI README) |
| `UsageSeverity` (`control.rs:796`) | `"normal/warn/danger"` (`dto.rs:712/636`) | — | Swift geometry/text |
| `UsageProviderTab` (`control.rs:910`) | DROPPED (not in `UsageViewDto`) | — | Capsule tabs only |
| `AccountUsageSnapshotView` (`control.rs:521`) | — | `run_accounts` table (`usage.rs:447/515`), `run_verify` (`usage.rs:491`), `store.rs` upsert/read `accounts.db` | producer `usage/view.rs:132`; transport `runtime/snapshot.rs` + `capsule/client.rs` |

### 1b. Protocol `usage_broker.rs` V1 → consumers

| Shared type | Producer | Consumer | FFI? |
|---|---|---|---|
| `UsageProjectionV1` (`:976`) | `build_canonical_projection` (`host/projection.rs:160`) | CLI bare (`usage.rs:240-270`); Console `from_projection` (`console/.../usage.rs:154`) | NO |
| `UsageAccountV1.windows` | `project_window` (`projection.rs:323+`) | CLI bare (`usage.rs:260`), Console (`usage.rs:165-177`) | NO |
| `UsageAccountV1.metric_groups` (`:910`, NEW in-flight) | `project_groups` (`projection.rs:401`: window+spend+plan groups) | NONE renders yet | NO |
| `credential_expires_at_epoch` (`:915`, NEW) | `None` (`projection.rs:318`) | NONE | NO |
| `UsageMetricGroupV1/Value/Scope/Period/Kind` (`:634/:546/:524/:489/:468`, NEW) | projection only | NONE | NO |
| `UsageQuotaStateV1::{NoPermission,Unknown,NotApplicable}` (NEW) + `IssueScope::Group` (`:308`) | protocol | NONE renders | NO |
| raw percents on windows (`:384/:394`, NEW) | `split_raw` | Console reads clamped `.get()` only (`usage.rs:173-174`); CLI labels only | NO |
| `UsageAccountCapability` (`:20`), `UsageGenerationView` (`:1011`), `UsageCoordinationError` (`:85`) | broker | FFI `refresh/current/join` (`bridge.rs:158-198`); `map_coordination_err` (`bridge.rs:545`) | YES |

### 1c. Protocol `lib.rs` + `account_credentials.rs`

| Type | Consumers | FFI/CLI/Native? |
|---|---|---|
| `Provider` (`lib.rs:189`, 5 variants: Anthropic/Openai/Zai/Minimax/Kimi, telemetry/display) | telemetry/display metadata | NO |
| `CapsuleConfig` (`lib.rs:131`; `auth_mode_for_agent` `:251`, `model_for_agent` `:245`) | capsule `config.rs:67/117`, `session.rs:1719`, `daemon/session_lifecycle.rs:245-254` | NO |
| `AgentCredentialEnv` (`account_credentials.rs:12`, agent-keyed map) | writer `resolve_account_env_with` (`jackin-env/accounts.rs:35`) → `write_account_credentials` (`account_identity.rs:152`, 0600/0700, RO mount `/run/jackin/account-credentials.json`) → reader `load_agent_credentials` (`capsule/config.rs:88`) → `validate_agent_credentials` (`:111`) + `session.rs:1719` | NO (Desktop/CLI use `CachedProviderCredentialResolver`, not this path) |

### 1d. Host types → FFI DTOs → Swift

| Host (`jackin-usage`) | FFI DTO (`dto.rs`) + bridge method | Swift consumer |
|---|---|---|
| `HostSurfaceDescriptor` (`host.rs:331`) | `SurfaceDescriptorDto` (`:499`); `list_surfaces` (`bridge.rs:98`) | `PresentationStore` map (`:979`) |
| `HostDesktopProjection` | `DesktopProjectionDto` (`:390`); `desktop_projection` (`bridge.rs:265`) | `RefreshScheduler.desktopProjection` (`:87`); `PresentationStore` intake (`:865-868`) |
| `HostDesktopInventory/Group/Account/State` (`host/accounts.rs:162`) | `DesktopInventoryDto` etc. (`:380-466`); `desktop_inventory` (`bridge.rs:252`) | `mapAccountDto` (`PresentationStore:1044`) |
| `HostProviderGlanceRow` (`host.rs:514`) | `ProviderGlanceRowDto` (`:181`); `provider_glance_rows` (`:362`), `status_bar_*` (`:378`) | `mapGlanceDto` (`:1070`); `icon_key = surface.id()` (`host.rs:1316/1751`) |
| `HostOverviewRow` | `OverviewRowDto` (`:660`); `overview_rows` (`:347`) | popover/Usage overview |
| `HostUsageEvent/Batch` | `UsageEventDto/Batch` (`:509`); `next_events` (`:406`) | poll loop |
| `UsageDiscoveryDiagnostic` | `DiscoveryDiagnosticDto` (`:55`); `discovery_diagnostics` (`:111`) | `PresentationStore.DiscoveryDiagnostic` (`:237`) |
| `HostSurfaceId` 12 variants (`host.rs:74`); `DESKTOP_PROVIDER_ORDER` 7 (`host.rs:120`) | gates `desktop_inventory`/`projection`/`glance`/`list_accounts` (`host.rs:899/1112/1117/1170/1250/1254/1400/1404`; `discovery.rs:596`) | 7-provider assumption everywhere below |

Swift renderers: `UsageWindowModel.UsageDetailPresentation(dto:)` (`:81-93`),
`ProviderMarks.templateImage(forIconKey:)` (resource lookup; only 7 mark sets
bundled: amp/claude/codex/grok/kimi/minimax/zai — no google/cursor/meta/
openrouter/opencode), `VisualQAFixtures.Provider` 7 cases (`:418`),
`OverviewInventory`, `StatusItemMenuModel`, `StatusPopoverFocus`.
Generated (never hand-edit): `JackinUsageBindings/BoltFFI/JackinUsageFfiBoltFFI.swift`
(`desktopProjection` at `:1351`).

## 2. Per-change impact

### (a) T10 `UsageMetricGroupV1` additions — protocol+producer LANDED, zero FFI surface change

Landed in working tree: `metric_groups` + `credential_expires_at_epoch` on
`UsageAccountV1` (`usage_broker.rs:906-917`, validate `:931-940`), new
`UsageMetricGroupV1/Value/Scope/Period/Kind`, `+3` quota states,
`IssueScope::Group`, `UsagePercent::{clamp_raw,split_raw,meter_fill}`,
raw percent fields, `project_groups` producer (`projection.rs:401`).
- FFI: NO CHANGE. `dto.rs` has no metric-group DTO; `UsageViewDto.buckets`
  is `QuotaBucketView`-based and `QuotaBucketView` is untouched. Desktop stays
  on principal windows (matches limits-only scope + T02 §6: Console is the
  groups subscriber).
- CLI: bare-host JSON gains `metric_groups` (serde `default` + `skip_empty` =
  wire-compat); human renderers ignore groups. NO CHANGE required.
- Native: NO binding regen needed for T10 alone; Swift needs nothing.
- T19: add one FFI golden test proving a groups-carrying projection does not
  alter Desktop DTOs; note the scoping decision in FFI README.

### (b) S1 Agent/AiProvider variants — catalog LANDED, Desktop/CLI lag in 9 places

Landed: `Agent` 12 (`core/agent.rs:23`), `AiProvider` 12 + `for_agent→Option`
(`config/accounts.rs:20/71`), `HostSurfaceId` 12 + provider_id/label/prefix/
usage_url/alias/agent maps (`host.rs:74-326`, Omp/Hermes→OpenCode).
FFI DTOs use free strings for surface/agent/provider and
`icon_key=surface.id()`, so new surfaces flow with NO signature change
(unknown keys hit Swift `hasMark`→fallback-glyph path).
Gaps T19 must close (or explicitly defer with owner sign-off):
1. `DESKTOP_PROVIDER_ORDER` still 7 (`host.rs:120` + doc `:118`): excludes
   OpenCode (intentional) AND Google/Cursor/Meta/OpenRouter (undecided).
   Gates 8 call sites (`host.rs:899` … `discovery.rs:596`). DECISION NEEDED:
   extend to 11 vs keep 7 (+plan).
2. CLI `--agent` help lists 8 surfaces (`usage.rs:126`, missing
   google/cursor/meta/openrouter though `from_id` accepts them `:310`).
3. CLI `usage_verify_provider_aliases` only 7 (`usage.rs:552-562`); new
   providers never verify. `SYNONYMS` (`:624-629`) lacks google/gemini/
   antigravity/cursor/meta/muse/openrouter groups (host `from_provider_alias`
   `:285-306` has them — mirror it).
4. Console `well_known_provider_name` (`console/.../usage.rs:414-426`) lacks
   the 4 new ids (falls back to raw id; cosmetic).
5. `ProviderMarks` assets: no google/cursor/meta/openrouter/opencode
   png/pdf + `PROVENANCE.md` entries. `VendorProvenanceTests` will need updates.
6. Swift `ArchitectureTests.testSwiftSourcesHaveNoProviderProbeImports`
   (`:33-58`) BANS `\bCursor\b` and `Gemini` tokens in ALL Swift sources —
   MUST narrow (to probe imports) before any Cursor/Google desktop work,
   else the build fails by design.
7. `VisualQAFixtures.Provider` 7 cases (`:418`); fixture-count tests
   (`VisualQAFixturesTests:12/33`) and glance-bar test (`ArchitectureTests:380`
   `[claude,amp,grok]`) encode the 7-set.
8. "seven-provider" docs: FFI README (`:47/:50`), `jackin-usage` README
   (`:79/:100`), `PresentationStore.swift:344`, `host.rs:118`.
9. `Provider` enum (`protocol/lib.rs:189`) still 5 variants — telemetry/
   display only; confirm with S1 owner whether it stays frozen (likely yes;
   do NOT extend in T19 without a consumer).
Also: `CapsuleConfig.agents` is `Vec<String>` — no change needed for new agents.

### (c) S3 instance-keyed credential transport — PLANNED, not started; NO FFI/CLI/native touch

T02 §4: rekey `AgentCredentialEnv` agent-slug → config/instance id
(synthesized `{account-id}@{agent-slug}`), envelope `schema_version=2`,
Capsule version-mismatch → explicit restart/upgrade error (H03); staged
layout/permissions/RO mount unchanged; per-instance maps.
Current state: transparent agent-keyed map (`account_credentials.rs:12`).
Full touch list (capsule+runtime+env only): `account_credentials.rs`
(envelope struct, keep Debug redaction); `jackin-env/accounts.rs:35`
(key by `ResolvedInstance`, not `&[Agent]`); `orchestrate.rs:655/:698`;
`account_identity.rs:152`; capsule `config.rs:88` (v2 gate + H03),
`config.rs:111` (validate vs manifest bindings, not `config.agents`),
`session.rs:1719` (instance lookup), `daemon.rs:415`,
`daemon/session_lifecycle.rs:245-255`; tests
(`config/tests.rs`, `session/tests.rs`, `daemon/tests.rs:9008`,
`amp_launch.rs:148`, `codex_launch.rs:197`, `launch/tests.rs`).
T19: verify-only — confirm the S3 diff touches none of
`jackin-usage-ffi/`, `cli/usage*`, `native/Sources|Tests`. No binding regen.
Watch: if S3 adds instance bindings to `CapsuleConfig`, FFI still unaffected
(it never consumes `CapsuleConfig`).

## 3. Generation / verification commands (macOS only)

| Command | What it does |
|---|---|
| `mise run desktop-bindings` (= `cargo xtask desktop bindings`, `desktop.rs:546`) | boltffi `generate swift` from `crates/jackin-usage-ffi` cwd; pins `MACOSX_DEPLOYMENT_TARGET=26.0`, profile `desktop-release`; strips stray `boltffi.h`; normalizes files. NEVER run boltffi directly. Output: `native/Sources/JackinUsageBindings` (`boltffi.toml:29-36`). |
| `mise run desktop-bindings-check` (`desktop.rs:551`) | Nonmutating drift gate: regen into `native/.build/bindings-check` via overlay, byte-compare both trees (`tree_differences`, `:591`). FIRST gate of `desktop-ci`. |
| `mise run desktop-ci` (`mise.toml:221`) | PR graph: bindings-check → generate → format-check → lint → `desktop test` (parity harnesses) → build → `test-swift` (counted xUnit) → verify. |
| `mise run desktop-merge` (`mise.toml:235`) | `desktop-ci` + `desktop-test-ui` (real-host UI suite). |
| `mise run desktop-scheduled` (`mise.toml:243`) | `desktop-merge` + `desktop-deadcode` (periphery). |
| `cargo nextest run -p jackin-usage-ffi` / `cargo clippy -p jackin-usage-ffi --all-targets -- -D warnings` | Rust-side FFI gate (FFI README). |
| CI | reusable `ci-native.yml` → `macos-26` runs `desktop-ci`; `desktop-cadence.yml` merge/scheduled (`PROJECT_STRUCTURE.md:80`). |

## 4. Sequenced T19 task list

1. Sync + re-scout: `git status/diff --stat`; re-verify all §1 line numbers
   (tree moving under T10/S1).
2. DECISION (orchestrator/product): Desktop provider scope — extend
   `DESKTOP_PROVIDER_ORDER` to 11 (google/cursor/meta/openrouter in, opencode
   out?) or hold 7. Record in plan; tasks 3-7 branch on it.
3. Host order + discovery: apply decision to `host.rs:120` (+doc `:118`),
   confirm `discovery.rs:596` + 8 order call sites; update
   `host/tests.rs:1815` closed-domain test.
4. FFI: confirm zero DTO signature change; add groups-ignoring golden test
   (`bridge/tests.rs`); refresh "seven-provider" README lines if scope grew.
5. CLI: `--agent` help (`usage.rs:126`), verify aliases+synonyms
   (`:552-562/:624-629`, mirror `from_provider_alias`); extend
   `cli/usage/tests.rs` provider matrix.
6. Console: `well_known_provider_name` (`usage.rs:414`) new ids.
7. Native: ProviderMarks assets + `PROVENANCE.md`; narrow
   `ArchitectureTests:33-58` Cursor/Gemini ban to probe imports;
   `VisualQAFixtures.Provider` + count tests; `PresentationStore:344` comment.
8. Regen ONLY if DTOs changed: `mise run desktop-bindings`; else skip.
9. Verify on logged-in Mac: `desktop-bindings-check`, `desktop-ci`,
   `desktop-merge`; `nextest` on `jackin-usage-ffi`, `jackin-usage`,
   `jackin-protocol`; Console/CLI usage tests.
10. S3 watch: when S3 lands, `git diff --stat` must show zero FFI/CLI/native
    files; else re-scout.
