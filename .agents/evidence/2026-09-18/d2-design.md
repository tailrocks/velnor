# D2 design — one provider-set schema (`github-hosted`, `github-self-hosted`, `velnor`)

Source: `plans/bastion-three-provider-ci/spec.md` §2 (+§8 watchdog) + `/tmp/d2-gapmap.md`.
Scope: `crates/velnor-workflow` (+ generator-owned `.github-gen`, `config/`, `schemas/`).
Mode: design only. No code in this doc is normative syntax; implementors match final Rust/TOML idioms.

## 0. Principles (non-negotiable)

1. Single `ProviderId` enum is the only provider vocabulary. Every surface stores/transmits
   `Set<ProviderId>` (array in TOML/YAML/JSON), never `github|velnor|both` strings.
2. No legacy: delete `RunnerMode`/`RunnerLane`/`DispatchChoice`/`LaneAdmission` and all
   aliases/inference (removal list §3). No `From<RunnerMode>`, no `"both"` parse arm,
   no `#[serde(alias)]`, no deprecation period. Staged rollout = old *release* vs new
   *release*, never two parsers in one binary (spec §3.1).
3. Fail closed: unknown selector, unknown provider string, unsupported capability,
   empty provider set where non-empty required → hard validation/generation error.
   Never default-to-hosted, never silent filter (kills E16/E17 inverses).
4. Selector = routing only. Authorization lives in typed `Trust` + controller-side checks
   (§8). Fanout lives in the planner (§5). A `runs-on` label never authorizes and never
   fans out.
5. One planner → per-provider fanout. Same source/command/profile/features/fixture/test
   expectation across all three providers for each eligible unit. No per-lane command
   arrays.
6. Expected-result set fixed before execution (§6). Verdict compares observed vs declared;
   never re-derives selection at check time.
7. Control plane stays hosted: planning, policy, recovery, required-result monitoring,
   watchdog always run on `github-hosted` regardless of `[workflow]` provider config.

## 1. New `ProviderId` type

New module, e.g. `crates/velnor-workflow/src/provider.rs` (exact placement: implementor's
choice; must be imported by config/scan/IR/runtime/policy/TUI — not duplicated):

```rust
enum ProviderId { GithubHosted, GithubSelfHosted, Velnor }
// serde: "github-hosted" | "github-self-hosted" | "velnor", deny_unknown, no aliases
// order (canonical, sort/digest/display): GithubHosted < GithubSelfHosted < Velnor
```

Rules:

- Parse: strict. Unknown string → `ConfigError::UnknownProvider { value, span }`.
  Empty string, `"github"`, `"velnor"`, `"both"`, `"lanes"`, case variants → same error.
- Set semantics: `BTreeSet<ProviderId>` in memory; TOML/JSON arrays on the wire.
  Duplicate entries → validation error (not dedup-silently; catches config drift).
  Empty array → error at the fields that require ≥1 provider (§2); allowed only where
  the schema explicitly permits (none currently planned).
- Canonical ordering enforced at parse (sorted) so digests are stable; unsorted input
  is accepted but normalized, and the canonical digest + generated files use sorted order.
- Display names (UI §2l): `github-hosted`, `github-self-hosted`, `velnor` verbatim in
  machine surfaces; human job names `Rust · github-hosted` etc. (spec §2 example uses
  `Rust · velnor-runner / github-self-hosted / bastion` — implementor pins one format;
  recommendation: `Rust · <provider-id>` plus unit name, no legacy `velnor-runner` alias
  except as the literal self-hosted selector string where it is the actual label).
- No `Default` impl that picks hosted. Call sites must pass an explicit set.
- Digest participation: provider set contributes to `plan_digest` and canonical config
  digest in sorted order.

## 2. One schema across all 13 surfaces

### 2a. Config (`src/config/mod.rs`, `src/lib.rs` ProjectConfig, `src/config/canonical.rs`)

Replace (delete, see §3):

- `runners`, `automatic`, `default_dispatch_runner`, `automatic_lanes` (strings)
- `github_runner`, `macos_runner` untyped labels → move to typed `Platform` (§4)
- `velnor_labels`, `velnor_runner_group`, `velnor_trusted_label`,
  `velnor_trusted_runner_available`, `pull_request_on_velnor`, `velnor_rust_needs`,
  `velnor_concurrency_group`, `velnor_serial_stack_groups` → fold into per-provider
  selector + trust schema below (no `velnor_*` prefix survivors; per-provider tables
  keyed by provider ID)

New `[workflow]` contract (illustrative; field names normative):

```toml
[workflow]
providers = ["github-hosted", "github-self-hosted", "velnor"]
automatic_providers = ["github-hosted", "github-self-hosted", "velnor"]
default_dispatch_providers = ["github-hosted", "github-self-hosted", "velnor"]

[workflow.selectors."github-hosted"]   runs_on = ["ubuntu-26.04"]     # GitHub-managed labels
[workflow.selectors."github-self-hosted"] runs_on = ["bastion-scale-set"]  # disjoint, dedicated
[workflow.selectors."velnor"]          runs_on = ["velnor-native"]     # disjoint, dedicated
```

- `providers`: universe for this repo. Non-empty. Unknown ID → error.
- `automatic_providers`: subset of `providers`; triggers for push/PR/schedule. Replaces
  `automatic` + `automatic_lanes` + `VelnorPullRequest::{TrustedOnly,Automatic}`.
  Trust gating is NOT expressed here (see §8); this is pure event→provider routing.
- `default_dispatch_providers`: subset of `providers`; default for `workflow_dispatch`
  `providers:` multi-select input. Replaces single-string dispatch default.
- Per-provider `[workflow.selectors.<id>]`: the ONLY place `runs-on` labels live.
  `github-self-hosted` and `velnor` selectors must be disjoint (validation error on any
  shared label between the two local providers; hosted labels are GitHub-owned and
  trivially disjoint). No global `runs_on` fallback.
- Canonical digest (`src/config/canonical.rs`): hash the three arrays (sorted) +
  per-provider selectors. Delete `lanes`→`runner` canonicalization.
- Runtime `project.toml` emission (`src/lib.rs:943-968`): emit the same three arrays.
- CLI `--runners` (`src/lib.rs:416`): delete; replace with `--providers` (comma-separated
  strict provider IDs, same validation). Default = config value, never `Both`.

### 2b. Scan (`src/scan/mod.rs`, `src/lib.rs:1080,1102`)

- `scan_shape(root, providers: &BTreeSet<ProviderId>, …)`; `RepositoryShape.providers`
  replaces `RepositoryShape.runners: RunnerMode`.
- Detection yields typed `Capabilities` per unit root (§4), not `Vec<String>`.
- Scan records per-unit `platform: Platform`, `trust: TrustReq`, `capabilities: Capabilities`.
  Scan does NOT filter by provider; filtering is planner+validation's job (scan stays total).

### 2c. IR (`src/primitives/ir.rs`, `src/primitives/mod.rs`, `src/primitives/lanes.rs`)

- `WorkflowIr { providers, automatic_providers, default_dispatch_providers,
  selectors: Map<ProviderId, Selector>, … }` — delete `runners`, `automatic: RunnerMode`,
  `github_runner`, `velnor_labels`, `velnor_*`.
- `LaneJob` → rename to `ProviderJob { provider: ProviderId, … }`
  (`src/primitives/mod.rs:144`). Delete `lanes.rs` whole file; its replacement is a
  provider-expansion module that maps `(unit, provider)` → job (no hosted-first sort
  special case beyond deterministic canonical order).
- `WorkflowIr::from_config` builds the provider universe from config; per-unit jobs come
  from the planner (§5), not `default_lane_jobs`.
- `UnitContract.lanes` (`src/primitives/mod.rs:365`) → `UnitContract.providers:
  BTreeSet<ProviderId>` (the providers this unit actually expands to after
  platform/trust/capability eligibility).
- Delete `DispatchChoice`, `automatic_event_selects_lane`, `dispatch_choice_selects_lane`,
  `dispatch_lane_expression`, `LaneAdmission` + `LANE_ADMITTED_*` env contract,
  `default_lane_jobs`, `runner_for` (Both⇒github), `control_plane_lane` mapping
  (control plane = constant `GithubHosted`, §2m/§7).

### 2d. Plans — affected/full + fanout (`src/runtime.rs` plan path)

- Planner input: `Scope::{Affected,Full}` (keep) + provider universe + per-unit
  eligibility (platform/trust/capabilities vs provider matrix §4).
- Planner output (new `needs.plan.outputs` contract):
  - `units`: per-unit records `{ unit_id, providers: [...], exclusions: [...] }`
    — NOT a flat unit list; each unit carries its post-eligibility provider set.
  - `plan_digest`: stable hash over (sorted unit IDs × sorted providers × exclusion
    declarations × command/profile/features/fixture digests). New output; part of
    result identity (§6).
  - `excluded`: pre-expansion exclusion declarations `{ unit_id, provider, reason }`
    with reason ∈ { `platform`, `trust`, `genuinely_unaffected`, `capability_unsupported`
    (only for units the repo explicitly scopes out — otherwise unsupported capability
    is a hard error per §2f, not an exclusion) }.
- Fanout rule: each selected eligible Linux unit expands to ALL providers in the repo
  universe that pass eligibility (normally all three). Genuinely-unaffected units
  (affected scope) and platform/trust exclusions (e.g. Swift/macOS units excluded from
  Linux-only providers) are declared in `excluded` BEFORE expansion.
- Delete `plan_lanes()`, `selection_for_lanes`, `Both-or-either` rule
  (`src/runtime.rs:733,906-924`), empty-string/`"both"` parse (`:887-889`).
- Unknown unit kinds: currently kept (`:900-924`) → hard error `UnknownUnitKind`.
  Unknown `runner.environment`/backend strings: currently default-to-hosted
  (`src/runtime.rs:154-163`) → hard error; then delete the inference entirely (§3).

### 2e. Validation (`src/lib.rs` validate_*, `src/config/mod.rs:1072`)

New validation set (all hard errors, fail-closed):

1. `UnknownProvider` — any provider string outside the canonical three, anywhere
   (config, dispatch input, plan output, policy).
2. `UnknownSelector` — a `runs-on` label not declared in `[workflow.selectors.<id>]`
   for exactly one provider, or any label overlap between `github-self-hosted` and
   `velnor` selectors.
3. `UnsupportedCapability` — unit requires capability the provider cannot satisfy
   (matrix §4) and the unit is in-scope → error naming unit + provider + capability.
   (Out-of-scope-via-exclusion is the only non-error path, and exclusions are
   planner-declared, §2d.)
4. `ProviderSubsetViolation` — `automatic_providers` / `default_dispatch_providers`
   not ⊆ `providers`; empty `providers`.
5. `DuplicateProvider` — repeated ID in any provider array.
6. Delete: `validate_lane_selection`, `automatic_fits_runners` (both copies),
   `validate_dispatch_runner_for_runners`, `parse_runner_mode`, `inferred_automatic`.

### 2f. `runs-on` (`runner_for*`, policy `analyze_runner`, estate contract)

- `runs_on_for(provider: ProviderId, unit) -> Vec<String>`: pure lookup of
  `[workflow.selectors.<id>]` + unit platform mapping (e.g. macOS units → hosted
  macOS labels / native macOS routing per §9.1; never label-substring sniffing).
- Disjointness: `github-self-hosted` runs-on set ∩ `velnor` runs-on set = ∅, enforced
  at validation (§2e rule 2) and re-asserted by policy (§2j).
- Delete: estate approved-Velnor-runner labels/group contract as labels-as-auth
  (`src/estate.rs:18-30` logic folded into typed trust §4/§8);
  `contains_self_hosted_label`, `analyze_runner` inference core (`src/policy.rs`);
  `runs_on.contains("self-hosted")` persistence inference (`src/lib.rs:2761`);
  `runner.environment == 'github-hosted'` gates (`src/lib.rs:4473`,
  `release.rs:2713-2719`, README doc).

### 2g. Bootstrap (toolchain/runtime setup per provider)

- Key: `(provider, platform, toolchain)` → bootstrap step list. One function
  `bootstrap_steps(provider, unit) -> Steps`, no lane conditionals.
- `github-hosted`: pinned toolchain setup (existing `render_pinned_toolchain_steps` /
  `render_workflow_runtime_setup` core, minus lane branches).
- `github-self-hosted`: official-runner-profile bootstrap — homogeneous image assumed;
  bootstrap asserts tool presence / installs only via declared typed capabilities,
  never guesses from request ID.
- `velnor`: native-adapter bootstrap (existing native paths: mise/mbx where the
  capability matrix says so).
- Delete `hosted_mold_setup` lane gate shape (`src/lib.rs:4599` — keep the mold logic,
  re-key by provider), `unit_lane_facts` github_lane branch (`ir.rs:4058-4077`).

### 2h. Caching (`CacheSpec`, snapshot keys, namespaces)

Cache key grammar gains a provider segment plus the full namespace tuple from spec §6:

```text
key = repo_id / trust / provider / platform / image-toolchain-ABI / options / dep-source-compat / unit / purpose
```

- Implement: extend `CacheSpec`/`CacheBackend` + `unit_snapshot` key builder with
  `provider: ProviderId`, `trust: TrustClass`, `platform: Platform`,
  `toolchain_digest`, `options_digest`. Namespaces `velnor-mbx`, `velnor-docker-seed`
  gain the same segments (no un-namespaced writers survive).
- Cross-provider reuse policy (typed, not lane-conditional): compiler-cache reuse
  allowed where `(trust, toolchain-ABI, options)` match; test reports NEVER shared
  across providers (each provider executes, §6). Untrusted inputs cannot publish to
  trusted cache keys (key includes trust; writer authorization checked).
- Delete `LaneJob.cache_save` hosted-save-only rule as a lane conditional; replace with
  per-provider cache role table (which providers may save/restore which purposes).

### 2i. Artifacts (upload/download pins, manifest)

- Artifact names include provider: existing `revision + runner.os-arch` scheme
  (`src/lib.rs:4543-4555`) + `provider` + `plan_digest` (short) segments.
- Runtime artifact manifest (`src/lib.rs:4553`) gains `provider`, `plan_digest`,
  `run_attempt`, `fixture_digest`, `command_profile_features_digest` — i.e. the full
  result-identity tuple (§6) so the verifier can correlate without re-derivation.
- Per-provider preservation: artifacts uploaded from the provider's own job context;
  nothing inside disposable containers is referenced post-cleanup (diagnostic export
  before teardown, §7 deadlines).

### 2j. Policy (`src/policy.rs`, `VelnorPolicyContract`, `TrustedRunners`)

- `VelnorPolicyContract { providers, automatic_providers, selectors, trust: TrustPolicy,
  capabilities: CapabilityMatrix, … }` — delete `runners: String`, `velnor_labels`,
  `velnor_runner_group`, `velnor_trusted_label`, `pull_request_on_velnor`.
- `TrustedRunners` rule → `TrustedProviders` rule: asserts (a) local-provider selectors
  disjoint, (b) trust exclusions match controller-side checks (§8), (c) no unknown
  selectors (re-assert validation §2e at policy-eval time for generated YAML).
- Delete: `lanes`→`runner` canonicalization (`:2365,:2992`), generated-gate shape
  matchers (`:2391-2429`, `:2351`), `has_trusted_runner_gate` label-inference gate,
  `trust_gated_velnor_job_skipped` + `VelnorTrusted` admission rendering, hostname
  printout as identity (`render_velnor_runner_identity_step` — delete or repurpose as
  pure diagnostics explicitly NOT consumed by any verdict).
- Policy stays hosted-authoritative: evaluation context pinned to `github-hosted`.

### 2k. Dispatch (`workflow_dispatch_inputs`, `dispatch_runner_options`)

- `workflow_dispatch` input `providers:` multi-select (array of the canonical three),
  options = repo `providers`, default = `default_dispatch_providers`.
  Delete single `runner:` choice input, `Both ⇒ automatic.as_str()` default,
  `velnor_dispatch_selection_expression`, omitted⇒automatic admission.
- Dispatch selection intersects the plan universe (dispatch can narrow, never widen
  beyond `providers`; never invent providers).

### 2l. UI names (`unit_job_id`, display names, sidebar/control names)

- `unit_job_id = "{provider}-{unit}"` (provider, not lane).
- `unit_job_display_name` MUST include provider: `Rust · github-hosted` /
  `Rust · github-self-hosted` / `Rust · velnor` (+ unit qualifier). Delete the
  `let _ = (lane, runners)` ignore.
- Sidebar group / control job names carry provider where per-provider, `plan` /
  `required` / `watchdog` for control-plane jobs (hosted, no provider suffix ambiguity).
- No `github-self-hosted`/`bastion` surfacing gaps: every generated per-provider job's
  `name:` contains its provider ID verbatim (grep-able invariant for tests).

### 2m. Aggregation (`ci_required`, caller verdicts, `required` mirror)

Aggregation becomes strict expected-set comparison (§6). Structural changes:

- Verdict input: `plan.outputs` (units × providers + `plan_digest` + `excluded`) frozen
  at plan time; observed results keyed by full result identity.
- Per-(unit, provider) verdict, not per-(unit, lane≤2).
- `required` mirror job + `ci-required` check retained, still hosted, still excluding
  self + telemetry from the workload set.
- Delete: `LANE_ADMITTED_*` re-derivation at check time, `skipped`-as-pass for
  unselected/unadmitted callers, `VelnorPullRequest` trust-shape branching in verdicts.

## 3. RunnerMode legacy removal list (no shims, no aliases)

Delete or provider-set-replace each item (line refs from gap map; implementor re-verifies):

Core lane model:

- `src/lib.rs:329-333` `enum RunnerMode` + `as_str` (`:336`)
- `src/lib.rs:1599-1608` `parse_runner_mode`
- `src/lib.rs:1610-1612` `inferred_automatic`
- `src/lib.rs:1614-1620` + `src/config/mod.rs:1064-1070` `automatic_fits_runners`
- `src/lib.rs:1622-1629` `validate_lane_selection`
- `src/lib.rs:1631-1637` `dispatch_runner_options`
- `src/lib.rs:1649-1662` `validate_dispatch_runner_for_runners`
- `src/lib.rs:1138-1147` omitted-dispatch-default inference
- `src/lib.rs:1670-1685` `apply_lane_generation_config`
- `src/lib.rs:321-325` `DEFAULT_DISPATCH_RUNNER` / `DEFAULT_AUTOMATIC_LANES`
- `src/lib.rs:416` CLI `--runners` default `Both` (+ test `:8050-8055` — rewrite, not keep)
- `src/lib.rs:3219-3224` `admission_lane_of` wildcard
- `src/lib.rs:4160-4180` `lane_supports_unit*` silent Swift filter → explicit exclusion (§2d)
- `src/lib.rs:1807,:2288,:4078-4093,:4104,:4143` lane conditionals
- `src/runtime.rs:145-168` `enum RunnerLane` + `from_execution_backend` + `current()`
- `src/runtime.rs:887-889` `""|"both"` parse; `:906-924` `selection_for_lanes`;
  `:733` `plan_lanes()`; `:1056-1059,:171` lane command arrays
- `src/primitives/lanes.rs` whole file
- `src/primitives/ir.rs:2353-2358` `DispatchChoice`; `:2369-2386`
  `automatic_event_selects_lane`; `:2388-2414` dispatch-lane fns; `:2428-2465`
  `LaneAdmission` + `LANE_ADMITTED_*`; `:2484-2510` `workflow_dispatch_inputs`;
  `:3744-3769` `default_lane_jobs`; `:4900-4907` `runner_for` Both-arm; `:4938-4943`
  `control_plane_lane` mapping (→ const hosted); `:5060-5062`,
  `:5150-5153`, `:5140-5146`, `:5415`, `:5455-5462` dispatch/admission/trust-gate rendering
- `src/primitives/mod.rs:144` `LaneJob.lane` (+`:365` `UnitContract.lanes`)
- `src/lib.rs:621-624` per-lane command arrays; TUI mirrors
  (`src/tui/mod.rs:1005-1006,1052`, `src/tui/view.rs:438-473,684-744,832-834,999-1017`);
  runtime `CiConfig`/`Workflow` legacy fields (`src/runtime.rs:49-86`);
  `vars.VELNOR_AUTOMATIC_LANES` + default (`src/lib.rs:2485,:2710,:8042-8046`)

Inference + alias sites:

- `runner.environment` gates: `src/lib.rs:4473`, `src/primitives/release.rs:2713-2719`,
  `src/runtime.rs:154-163`, `crates/velnor-workflow/README.md:101` (doc)
- Label-substring: `src/policy.rs:2975-2978`, `:2841-2973`, `src/lib.rs:2761`,
  `src/primitives/ir.rs:5901-5904`, `:4058-4077`
- `lanes`→`runner`: `src/policy.rs:2365,:2992,:2391-2429,:2351`

Velnor-prefixed options folded into provider-set schema (§2a):

- `velnor_labels`, `velnor_runner_group`, `velnor_trusted_label`,
  `velnor_trusted_runner_available`, `pull_request_on_velnor`, `velnor_rust_needs`,
  `velnor_concurrency_group`, `velnor_serial_stack_groups`, `macos_runner`,
  `github_runner`, `VelnorPullRequest`, `requires_trusted: bool` (both copies),
  estate approved-runner contract labels.

Proof of no-legacy (acceptance): `rg -l 'RunnerMode|RunnerLane|DispatchChoice|LaneAdmission|automatic_lanes|default_dispatch_runner|LANE_ADMITTED|runner\.environment|contains_self_hosted_label|velnor_trusted_label|both'` returns zero hits in
`crates/velnor-workflow/src` (excluding historical changelog text, if any).

## 4. Typed `Platform` / `Trust` / `Capabilities`

New types (same module as `ProviderId` or adjacent `capability.rs`):

```rust
enum Platform { LinuxX64, LinuxArm64, MacosArm64 /* extend explicitly, never stringly */ }
enum TrustReq { UntrustedOk, TrustedOnly }
struct Capabilities { docker: bool, nested_privileged_docker: bool, buildx_compose: bool,
    testcontainers: bool, services_with_readiness: bool, browser_binaries: bool,
    native_macos_arm64: bool }
struct ProviderCaps { platform: Set<Platform>, max_trust: /* trusted-tier? */,
    caps: Capabilities }
```

- Capability matrix (static table, provider × platform → `ProviderCaps`):
  - `github-hosted`: Linux x64 (+ macOS arm64 for the two genuine macOS units);
    Docker yes, nested-privileged-Docker no (hosted runners cannot DinD-privilege —
    implementor verifies against actual hosted image; if hosted gains it, matrix
    changes by explicit edit + test, not inference), Buildx/Compose yes,
    Testcontainers yes, services-with-readiness yes, browser binaries per image,
    native macOS/arm64 yes (macOS platform only).
  - `github-self-hosted` (official lane): Linux x64, homogeneous Docker-capable
    profile (§5 spec): Docker yes, nested privileged yes (private DinD), Buildx/Compose
    yes, Testcontainers yes, services yes, browser binaries per pinned image content
    (verified, not assumed), native macOS no.
  - `velnor` (native lane): Linux x64 (native Docker backend): same Docker-side yeses
    via mediated backend; native macOS no (until generic native macOS routing lands
    per §9.1 — then an explicit matrix row, not a label coincidence).
- No CPU/RAM resource classes anywhere (spec §2 + §4.3).
- Eligibility predicate: `eligible(unit, provider) = platform_ok && trust_ok(unit, event,
  provider) && caps_ok(unit_caps ⊆ provider_caps)`. Failure of `caps_ok` for an in-scope
  unit = `UnsupportedCapability` hard error (§2e); platform/trust mismatch =
  planner-declared exclusion (§2d), never silent.
- `TrustClass` currently lives in `velnor-runner` (referenced only by comment):
  move or re-export the canonical type so workflow/policy share ONE trust type.
  `requires_trusted: bool` dies in both copies.

## 5. Planner → 3-provider fanout

Pipeline:

```text
config providers × scan units
  → eligibility eval (platform/trust/caps per (unit, provider))
  → Scope filter (Full: all units; Affected: diff-selected, rest → genuinely_unaffected exclusions)
  → fanout: (unit × eligible providers) jobs, same command/profile/features/fixture
  → emit plan.outputs { units[], excluded[], plan_digest }
```

- Same-everything enforcement: delete per-lane command arrays; each `CiUnit` carries ONE
  `commands` array + ONE profile/features/fixture. The fanout clones the job spec per
  provider; only `runs-on`, bootstrap key, cache/artifact provider segment, and display
  name vary. A test asserts byte-identical `steps:` (modulo provider-keyed fields) across
  the three jobs of one unit.
- Display names: `name: "Rust · github-hosted — <unit>"` (exact separator pinned by
  implementor; provider ID verbatim, grep-able).
- Disjoint selectors: fanout reads `[workflow.selectors.<id>]`; validation guarantees
  local-provider disjointness before any job renders.
- Per-provider bootstrap: `bootstrap_steps(provider, unit)` (§2g) selected by provider
  key, not `if: runner.environment`.
- Contracts: `UnitContract.providers` = post-eligibility set; a contract with an empty
  set is an error unless the unit is fully excluded with declared reasons.

## 6. Strict expected-result set

Result identity (spec §2, verbatim tuple):

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

- `plan_digest` computed by the planner (§2d), frozen in `needs.plan.outputs`.
- Every observed result (test report, artifact manifest, cache/timing report) carries the
  full tuple (§2i). `run_attempt` from `GITHUB_RUN_ATTEMPT` (no longer just a timing-dir
  name).
- Verdict algorithm (hosted, per run/attempt):
  1. Load expected set E = {(unit, provider)} from plan outputs (+ digest check: recompute
     digest over the declared set; mismatch = `identity-mismatch` fail).
  2. Load observed set O keyed by identity tuple; reject records with wrong
     repo/sha/run/attempt/digest/unit/provider/platform/cmd-digest as
     `identity-mismatch` (fail, never ignore).
  3. Two observed records, same identity, conflicting outcome → `duplicate-conflicting`
     (fail).
  4. For each e ∈ E: outcome must be exactly `success` with matching identity.
     `missing | skipped | cancelled | timed-out | failed | duplicate-conflicting |
     identity-mismatched` → required result FAILS.
  5. `excluded[]` entries are NOT in E and are rendered as visible reduced coverage
     (diagnostic subset), never as success.
- Per-class negatives (each gets a dedicated test, §6.1 of work-plan mapping):
  missing result, skipped caller, cancelled run, timed-out job, failed job,
  duplicate-conflicting reports, identity-mismatched report (wrong attempt; wrong
  provider claiming another's; tampered digest), stale-attempt replay, unselected-unit
  `skipped` claimed as pass.
- No cross-certification: a `github-hosted` success never satisfies the `velnor` or
  `github-self-hosted` member of E. Matrix fail-fast off: per-provider caller jobs (no
  GitHub matrix), nextest `fail-fast = false` retained.
- Legit reuse boundary: compiler-cache hits across providers allowed (§2h); test reports
  never substituted (each provider's JUnit/nextest report is its own; verifier checks
  per-provider report presence + counts).

## 7. Hosted watchdog (§8)

One generated hosted authority (`watchdog` job + required-result reporter), control-plane,
always `github-hosted`:

- Start-after-planning: `needs: [plan]` ONLY. Never `needs` on workload jobs (else queued
  work blocks outage detection). Runs on a schedule/timeout loop independent of workload
  completion; initial targets: reserve-to-connected ≤180s, owned cleanup ≤120s,
  free-capacity provisioning stall diagnosed ≤5m, full local outage → failed/incomplete
  ≤10m, plus explicit execution/full-run deadlines tuned from cold baselines.
- Authenticated outbound health records from Velnor (least-privilege telemetry writer —
  NOT a runner controller): each record binds
  `repository_id / source_sha / run_id / run_attempt / provider / sequence / freshness_ts /
  occupied_permits / provisioning_progress / operation_ids`. Watchdog validates identity +
  freshness; missing/stale = UNAVAILABLE (failed/incomplete), never "ordinary backlog".
- Correlation: GitHub runner/job metadata (all API pages enumerated, attempts
  distinguished) × Velnor provisioning IDs × engine versions (official runner ver +
  native ver recorded separately) × image digests × selected tests + JUnit counts ×
  cache/timing reports × cleanup receipts. Hostname printout explicitly NOT accepted as
  placement evidence.
- No-overwrite-of-failure: once required result = failed, no later reporter/cancellation
  flips it to success. Reruns revalidate full identity + outcome provenance (new attempt =
  new identity; stale-attempt records rejected per §6).
- Adversarial coverage (from §8 table, workflow-generator-owned rows): runner/DinD/worker
  kill → no false success + diagnostic export + once-only capacity release; fork spoof /
  input substitution → hosted-only exclusion explicit; missing/skipped/wrong-provider/stale
  report → aggregate fails with greens elsewhere; hidden ancestor limits → real
  container/cgroup/env inspection proves quota-free.

## 8. Trust enforcement (spec §6 trust paragraph, as it constrains §2 schema)

- Controller-side (outside PR-editable YAML): event/source/ref checks
  (fork, bot PR, same-repo PR, main, schedule, dispatch, tag, merge-group) +
  verified runner-group/workflow restrictions where available. Same-repo origin or a
  requested label is NEVER sufficient.
- Defaults: untrusted fork execution → `github-hosted` ONLY (planner emits trust
  exclusions for both local providers with reason `trust`, visible in `excluded[]`).
  Untrusted checkout is NEVER executed under privileged `pull_request_target`.
- Negative tests (required): label spoofing (PR edits labels → still hosted-only),
  reusable-workflow input substitution, fork→local-provider attempt, `pull_request_target`
  privilege probe. Each asserts NO bastion execution + explicit hosted-only exclusion.
- Schema consequence: trust is a typed `TrustReq`/`TrustClass` field evaluated against
  (event, provider), not a label appended to `runs-on` (deletes `velnor_trusted_label`
  append + `TrustedRunners`-as-labels + `trust_gated_velnor_job_skipped`).

## 9. Generic capability tests plan (repo-independent)

Proven in fixture repos (renamed to catch name special-cases), not only in velnor/jackin:

1. Schema round-trip: all-3 / hosted-only / pairwise subsets parse; unknown ID, dup ID,
   empty set, non-subset automatic/dispatch → exact errors.
2. Selector disjointness: shared label across local providers → error; undeclared label
   in generated YAML → policy error.
3. Fanout identity: one Linux fixture unit → N jobs (N = eligible providers), byte-identical
   steps modulo provider-keyed fields; display names contain provider IDs verbatim.
4. Eligibility matrix: fixture units requiring each capability (Docker, nested-privileged,
   Buildx/Compose, Testcontainers, services+readiness, browser binaries, macOS/arm64) × 3
   providers → expected pass/exclusion/error per cell; unsupported-in-scope → hard error.
5. Expected-set negatives: all 8 per-class cases from §6 fail the aggregate; rerun with
   new attempt + stale record → stale rejected.
6. Watchdog: plan-only start (no workload `needs`); stale/missing health → unavailable;
   §8 deadline timers present and wired (reserve 180s / cleanup 120s / stall 5m / outage 10m).
7. Trust: fork/bot/label-spoof/input-substitution fixtures → hosted-only + explicit
   exclusions; `pull_request_target` never privileged.
8. No-legacy grep gate (§3 proof) + regeneration determinism (byte-identical regen) +
   ownership inventory (no unexpected workflow files).
9. Cache/artifact namespacing: key/name contains repo/trust/provider/platform/toolchain
   segments; cross-provider test-report substitution attempt → verifier rejects.
10. No-inference gates: `runner.environment` / label-substring strings absent from
    generated YAML and generator source (except this doc + tests asserting absence).

## 10. Suggested implementation order (for work-plan authors)

P1 provider enum + config schema + validation (§1, §2a, §2e) → P2 scan/IR/provider-job
types (§2b, §2c, §4) → P3 planner fanout + plan outputs + digest (§2d, §5) →
P4 runs-on/bootstrap/selectors (§2f, §2g) → P5 cache/artifact identity (§2h, §2i) →
P6 policy/dispatch/UI (§2j–§2l) → P7 strict aggregation (§2m, §6) →
P8 watchdog + trust negatives (§7, §8) → P9 generic fixtures + no-legacy gates (§9).
Each phase deletes its legacy counterparts in the same diff (no dual-model windows).
