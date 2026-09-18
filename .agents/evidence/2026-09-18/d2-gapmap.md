# D2 gap map — spec §2 (+§8 watchdog) vs existing `velnor-workflow` code

Source: `plans/bastion-three-provider-ci/spec.md` §2 (and §8 for the hosted watchdog).
Tree: `crates/velnor-workflow` (+ `.github-gen`, `config/`, `schemas/` checked for provider schema).
Method: read-only. `EXISTS` = code implements the spec element today. `GAP` = missing or
contradicted. `PARTIAL` = a neighboring mechanism exists but not the spec element.

Headline: there is no three-provider schema anywhere. The entire codebase runs on a
two-lane `RunnerMode { Github, Velnor, Both }` model (`src/lib.rs:329`) with a separate
two-variant runtime `RunnerLane` (`src/runtime.rs:145`). The string `github-self-hosted`
appears nowhere outside the plan docs; `github-hosted` appears only as a
`runner.environment` inference gate (see inference list). Every §2 surface is a GAP.

## Per-element map

### E1. Canonical provider IDs `github-hosted` / `github-self-hosted` / `velnor`
- GAP. No `ProviderId`/provider-set type exists. Closest types are the legacy enums:
  - `src/lib.rs:329` `enum RunnerMode { Github, Velnor, Both }`
  - `src/runtime.rs:145` `enum RunnerLane { Github, Velnor }`
  - `src/primitives/ir.rs:2353` `enum DispatchChoice { Github, Velnor, Both, Omitted }`
  - `src/primitives/ir.rs:2428` `enum LaneAdmission { Github, Velnor, VelnorTrusted }`

### E2. One provider-set schema across config — GAP
- EXISTS (legacy shape): `[workflow] runners/automatic/default_dispatch_runner/automatic_lanes`
  as `github|velnor|both` strings — `src/config/mod.rs:166-236`, `src/lib.rs:833-882`
  (`ProjectConfig.runners/automatic/default_dispatch_runner/automatic_lanes`).
- GAP: no `providers = [...]`, no `automatic_providers = [...]`, no
  `default_dispatch_providers = [...]` array schema anywhere (config, canonical digest
  `src/config/canonical.rs`, or runtime `project.toml` in `src/lib.rs:943-968`).

### E3. … across scanning — GAP
- EXISTS (legacy): `scan_shape(root, runners: RunnerMode, …)` (`src/scan/mod.rs:34`),
  `RepositoryShape.runners: RunnerMode` (`src/scan/mod.rs:132`), `scan_repository` /
  `scan_target` take `RunnerMode` (`src/lib.rs:1080`, `src/lib.rs:1102`).
- GAP: no provider-set input to scanning; detected capabilities are untyped
  `Vec<String>` (`src/scan/mod.rs:127`).

### E4. … across IR — GAP
- EXISTS (legacy): `WorkflowIr { runners, automatic: RunnerMode, github_runner,
  velnor_labels, … }` (`src/primitives/ir.rs:2287-2320`), `LaneJob.lane: RunnerMode`
  (`src/primitives/mod.rs:144`), `default_lane_jobs` emits ≤2 lanes
  (`src/primitives/ir.rs:3744-3769`), `WorkflowIr::from_config` (`src/primitives/ir.rs:3007`).
- GAP: no provider dimension in IR; `Both` is a pseudo-lane, not a set.

### E5. … across plans (affected/full + fanout) — PARTIAL → GAP on fanout
- EXISTS: one affected/full planner: `Scope::{Affected, Full}` (`src/runtime.rs:229`),
  `plan()` (`src/runtime.rs:723`), `selection_for_diff`, `expand_affected_units_with_full`
  (`src/runtime.rs:1563`), plan outputs `needs.plan.outputs.units/scope` consumed by
  callers (`src/primitives/ir.rs:2481`, `src/lib.rs:12286`).
- GAP: plan has no provider fanout (selection is lane-filtered to ≤2 via
  `plan_lanes`/`selection_for_lanes`, `src/runtime.rs:733-738,906`), no `plan_digest`
  output, no per-provider selection, no pre-expansion exclusion declaration.

### E6. … across validation — GAP
- EXISTS (legacy): `validate_lane_selection` (`src/lib.rs:1622`), `validate_workflow`
  (`src/config/mod.rs:1072`), `automatic_fits_runners` (`src/lib.rs:1614`,
  `src/config/mod.rs:1064`), `validate_dispatch_runner_for_runners` (`src/lib.rs:1649`).
- GAP: no unknown-selector / unsupported-capability failure; unknown unit kinds are
  deliberately *kept* (`src/runtime.rs:900-924`); unknown `runner.environment` /
  backend values silently become the hosted lane (`src/runtime.rs:154-163`).

### E7. … across runs-on — GAP
- EXISTS (legacy): `runner_for` (`src/primitives/ir.rs:4900`), `runner_for_unit`
  (`src/primitives/ir.rs:5451`), `velnor_runner(labels, group)` selectors, policy
  `analyze_runner` (`src/policy.rs:2841`), approved-velnor-runner contract
  (`src/estate.rs:18-30`, `src/policy.rs:2777-2816`).
- GAP: no `github-self-hosted` selector; official-runner vs native-Velnor do not have
  disjoint dedicated local selectors (one `velnor_labels` + optional group serves the
  whole self-hosted lane); self-hosted-ness is *inferred* from label substrings
  (`contains_self_hosted_label`, `src/policy.rs:2975-2978`).

### E8. … across bootstrap — GAP
- EXISTS (adjacent): toolchain/bootstrap steps exist (`render_pinned_toolchain_steps`,
  `hosted_mold_setup` at `src/lib.rs:4599`, `render_workflow_runtime_setup` at
  `src/primitives/ir.rs:4909`).
- GAP: no per-provider bootstrap keyed by the provider set; hosted vs self-hosted
  bootstrap is chosen by lane conditionals, not provider identity.

### E9. … across caching — GAP (infra exists, provider dimension missing)
- EXISTS: `CacheSpec`/`CachePurpose` (`src/lib.rs:708-749`), `CacheBackend`
  (`src/primitives/mod.rs:156`), snapshot key grammar (`src/primitives/snapshot.rs`,
  `unit_snapshot` at `src/primitives/ir.rs:1022`), namespaces `velnor-mbx` /
  `velnor-docker-seed` (`src/primitives/ir.rs:36-41`), hosted-save-only rule
  (`LaneJob.cache_save`, `src/primitives/mod.rs:144-152`).
- GAP: no provider segment in any cache key/namespace; no namespacing by
  repo-id/trust/provider/platform/image/toolchain/ABI; cross-lane reuse policy is
  lane-conditional, not provider-typed.

### E10. … across artifacts — GAP
- EXISTS: pinned upload/download-artifact actions (`src/lib.rs:228-234`), runtime
  artifact names carry revision + `runner.os-arch` (`src/lib.rs:4543-4555`).
- GAP: no provider dimension in artifact names/identity; no per-provider artifact
  preservation outside disposable containers.

### E11. … across policy — GAP
- EXISTS (legacy): `VelnorPolicyContract { runners: String, velnor_labels,
  velnor_runner_group, velnor_trusted_label, pull_request_on_velnor }`
  (`src/policy.rs:2247-2253`), `TrustedRunners` rule (`src/policy.rs:2748-2774`),
  trusted-event gates (`has_trusted_runner_gate`, `src/policy.rs:2980`).
- GAP: policy knows only the legacy lane strings and label inference; no
  provider-set policy, no per-provider trust/capability authorization, no explicit
  unknown-selector failure.

### E12. … across dispatch — GAP
- EXISTS (legacy): `runner:` choice input (`github|velnor|both`) rendered by
  `workflow_dispatch_inputs` (`src/primitives/ir.rs:2484-2510`), options from
  `dispatch_runner_options` (`src/lib.rs:1631`), `velnor_dispatch_selection_expression`
  (`src/primitives/ir.rs:5060-5062`).
- GAP: no `default_dispatch_providers` set; dispatch default is a single inferred
  string (`src/lib.rs:1138-1147`, `src/primitives/ir.rs:2497-2501`).

### E13. … across UI names — GAP
- EXISTS (legacy): `unit_job_id` = `{lane}-{unit}` (`src/lib.rs:2355`),
  `sidebar_group_name`/`unit_group` kind labels (`src/lib.rs:2359-2370`),
  `control_job_name` (`src/lib.rs:2392`).
- GAP: `unit_job_display_name` explicitly ignores the lane
  (`let _ = (lane, runners);`, `src/lib.rs:2415-2418`); no `Rust · <provider>` display
  names; no `github-self-hosted`/`bastion` surfacing.

### E14. … across aggregation — PARTIAL → GAP on strictness
- EXISTS: `ci_required` flag end-to-end (`src/lib.rs:855`, `WorkflowIr.ci_required`
  at `src/primitives/ir.rs:2292`), `required_callers`/`RequiredCaller`
  (`src/primitives/ir.rs:3572/2471`), per-caller verdict script
  (`render_required_caller_verdicts`, `src/primitives/ir.rs:2983-3000`), `required`
  mirror job needing `ci-required` (`src/primitives/ir.rs:3679`).
- GAP: verdict is per-(unit,lane≤2), not per-(unit,provider×3); no expected-result
  set fixed before execution (selection + admission evaluated at check time from
  `needs.plan.outputs.units` and `LANE_ADMITTED_*`); unselected callers may be
  `skipped` (`src/primitives/ir.rs:3000`), contradicting the strict set.

### E15. No aliases, no deprecation branches, no inference — GAP (inverses exist)
See the mechanical removal list below. The codebase currently *depends* on: the
`both` alias, the omitted-github default, `automatic` inference, empty-string dispatch
alias, `lanes`→`runner` canonicalization, `runner.environment` inference, label-substring
inference, and wildcard/default-to-hosted fallbacks.

### E16. Platform, trust, capabilities as separate typed fields — GAP
- EXISTS (fragments): `Unit { kind: UnitKind, root, toolchain: Option<RustToolchain>,
  services: Vec<UnitService>, requires_trusted: bool, … }` (`src/lib.rs:609-656`);
  `UnitKind` 9 variants (`src/lib.rs:549-559`); capability-command materialization
  (`materialize_capability_commands`, `src/lib.rs:2119`); `macos_runner` label string
  (`src/lib.rs:839`); `TrustClass` lives in the `velnor-runner` crate, only referenced
  by comment (`src/primitives/ir.rs:5053-5057`).
- GAP: no typed `Platform` (platforms are label strings: `github_runner`,
  `macos_runner`, `RUNNER_OS/RUNNER_ARCH` interpolations); no typed `Trust` in this
  crate (`requires_trusted: bool` + `velnor_trusted_label: String`); no typed
  `Capabilities` (detection yields `Vec<String>`); the spec's capability list (Docker,
  nested privileged Docker, Buildx/Compose, Testcontainers, services-with-readiness,
  browser binaries, native macOS/arm64) has no typed representation and no
  supported/unsupported evaluation.

### E17. Unknown selectors / unsupported capabilities fail explicitly — GAP
- Opposite EXISTS: unknown backends default to hosted (`src/runtime.rs:154-163`);
  unknown unit kinds are kept (`src/runtime.rs:900-924`); unknown `lane:` inputs map to
  Velnor (`admission_lane_of` wildcard, `src/lib.rs:3219-3224`); Swift-on-Velnor is
  silently filtered from callers (`lane_supports_unit`, `src/lib.rs:4160-4180`) rather
  than failing or routing explicitly.

### E18. One planner → each eligible Linux unit fans out to all three providers — GAP
- Opposite EXISTS: ≤2 lanes; per-lane *divergent* command arrays
  (`github_pr_commands` vs `velnor_pr_commands`, `src/lib.rs:621-624`,
  `src/runtime.rs:1056-1059`, `CiUnit::commands`, `src/runtime.rs:171`); contracts carry
  `lanes` not providers (`contract_for`/`UnitContract`, `src/primitives/mod.rs:365`).
- GAP: no three-way fanout; no same-source/command/profile/features/fixture/test
  expectation enforcement across providers.

### E19. Selector is routing, not authorization, not fanout — GAP
- Opposite EXISTS: labels double as authorization (`velnor_trusted_label` appended to
  `runs-on`, `src/primitives/ir.rs:5455-5462`; approved-runner contract,
  `src/policy.rs:2777-2816`) and `Both` doubles as a fanout instruction.

### E20. Result identity (repo+sha+run+attempt+plan_digest+unit+provider+platform+cmd/profile/features/fixture) — GAP
- EXISTS (fragments): manifest accept-filters bind repository/revision/run_id/platform
  (`src/lib.rs:3890,4543`); runtime artifact manifest carries
  repository/revision/closure/platform/run_id/job_id/digests (`src/lib.rs:4553`);
  `GITHUB_RUN_ATTEMPT` used only in a timing-dir name (`src/primitives/ir.rs:1066`).
- GAP: no `plan_digest`, no `provider` in any identity, no `fixture_digest`, no
  command/profile/features binding, no run-attempt distinction in verdicts, no
  identity-mismatch or duplicate-conflicting detection.

### E21. Strict expected-result set (missing/skipped/cancelled/timed-out/failed/duplicate/identity-mismatch cannot pass; pre-declared exclusions) — GAP
- EXISTS (nearest): selected+admitted caller must be `success`, anything else fails
  — so cancelled/timed-out/failed *do* fail there (`src/primitives/ir.rs:3000`).
- GAP: selected-but-unadmitted expects `skipped` (a pass); unselected callers may be
  `skipped` (a pass); missing results are not distinguished from unselected; no
  duplicate/identity-mismatch concepts; exclusions are runtime admission predicates,
  not planner pre-declarations; genuinely-unaffected units are not declared before
  expansion (selection is diff-derived at plan time but the *verdict* re-derives it).

### E22. Each provider executes; no cross-certification; legit cache reuse; matrix fail-fast off — PARTIAL
- EXISTS: no cross-lane test-report substitution found (lanes render independent
  jobs); nextest `fail-fast = false` is enforced in scan fixtures (`src/lib.rs:9419`
  et al.); lanes are not rendered as a GitHub matrix (per-lane caller jobs), so no
  matrix fail-fast can trip.
- GAP: nothing to certify per-provider execution because there is one hosted lane and
  one self-hosted lane, not three providers.

### E23. Planning/policy/recovery/required-result monitoring stay hosted; one writer; DCO retained — PARTIAL
- EXISTS: control-plane lane + hosted planning default (`control_plane_lane`,
  `src/primitives/ir.rs:4938`; `render_plan`, `src/primitives/ir.rs:4945`); policy
  runtime pinning; `dco_required` config exists (`src/config/mod.rs` policy section,
  cf. test at `src/config/mod.rs:1907`).
- GAP: `control_plane_lane` follows `[workflow] automatic`, so `automatic = velnor`
  moves planning off hosted (`src/primitives/ir.rs:4938-4943`) — spec forbids this.

### E24. Hosted watchdog / fault contract (spec §8) — GAP (near-total)
- EXISTS (nearest): `ci-required` required check + `required` mirror job
  (`src/primitives/ir.rs:3600-3680`); nightly failure signal issue writer
  (`src/primitives/ir.rs:3707`).
- GAP, per §8 clause:
  - No hosted authority that starts after planning *without* `needs` on every local
    job — the verdict consumes `NEEDS_JSON` over all callers, i.e. it waits on them.
  - No authenticated outbound Velnor health records (no repo/source/run/attempt/
    provider/sequence/freshness/permit/progress binding; no telemetry writer).
  - No identity/freshness validation of health data; a nearest violation of the
    spirit: `render_velnor_runner_identity_step` prints hostname-ish identity
    (`src/primitives/ir.rs:5901`) but nothing correlates it as placement evidence —
    and spec explicitly says hostname printout is not placement evidence.
  - No runner/job metadata correlation, no API pagination, no run-attempt
    distinction in verdicts, no no-overwrite-of-failure guard, no rerun revalidation.
  - No operating targets (180s reserve-to-connected / 120s cleanup / 5min stall /
    10min outage / execution deadlines) anywhere in the generator.

## Legacy / alias / inference sites to remove (mechanical no-legacy proof inputs)

Core lane model:
- `src/lib.rs:329-333` `enum RunnerMode { Github, Velnor, Both }` + `as_str` (`:336`)
- `src/lib.rs:1599-1608` `parse_runner_mode` (`github|velnor|both`)
- `src/lib.rs:1610-1612` `inferred_automatic`
- `src/lib.rs:1614-1620`, `src/config/mod.rs:1064-1070` `automatic_fits_runners`
- `src/lib.rs:1622-1629` `validate_lane_selection`
- `src/lib.rs:1631-1637` `dispatch_runner_options`
- `src/lib.rs:1649-1662` `validate_dispatch_runner_for_runners`
- `src/lib.rs:1138-1147` omitted-dispatch-default inference (Velnor-only → `velnor`)
- `src/lib.rs:1670-1685` `apply_lane_generation_config` runners/automatic application
- `src/lib.rs:321-325` `DEFAULT_DISPATCH_RUNNER` / `DEFAULT_AUTOMATIC_LANES` (`"github"`)
- `src/lib.rs:416` CLI `--runners` default `Both` (+ `src/lib.rs:8050-8055` test pinning it)
- `src/lib.rs:3219-3224` `admission_lane_of` — wildcard `_ => Velnor`
- `src/lib.rs:4160-4180` `lane_supports_unit` / `lane_supports_unit_kind` /
  `lanes_support_unit_kind` (silent Swift filtering)
- `src/lib.rs:1807`, `:2288`, `:4078-4093`, `:4104`, `:4143` lane conditionals
- `src/runtime.rs:145-168` `enum RunnerLane` + `from_execution_backend` (default Github;
  `"self-hosted"`/`"unknown"` → Github) + `current()` env inference
- `src/runtime.rs:887-889` `"" | "both"` parse (empty-string alias)
- `src/runtime.rs:906-924` `selection_for_lanes` (+ `:921-923` Both-or-either rule)
- `src/runtime.rs:733` `plan_lanes()` in `plan()`
- `src/primitives/lanes.rs` whole file — two-lane matrix, hosted-first sort (`:93-97`),
  "github lane unless velnor" gate (`:84-90`), "omitted default" (`:88`)
- `src/primitives/ir.rs:2353-2358` `DispatchChoice` (+ `Omitted` variant)
- `src/primitives/ir.rs:2369-2386` `automatic_event_selects_lane`
- `src/primitives/ir.rs:2388-2414` `dispatch_choice_selects_lane` /
  `dispatch_lane_expression` (omitted ⇒ both lanes)
- `src/primitives/ir.rs:2428-2465` `LaneAdmission` + `LANE_ADMITTED_*` env contract
- `src/primitives/ir.rs:2484-2510` `workflow_dispatch_inputs` (`runner:` choice; Both ⇒
  `automatic.as_str()` default at `:2500`)
- `src/primitives/ir.rs:3744-3769` `default_lane_jobs` ("GitHub is the omitted default")
- `src/primitives/ir.rs:4900-4907` `runner_for` (Both ⇒ github label)
- `src/primitives/ir.rs:4938-4943` `control_plane_lane` (Both ⇒ Github)
- `src/primitives/ir.rs:5060-5062` `velnor_dispatch_selection_expression`
  (`runner == 'velnor' || runner == 'both'`)
- `src/primitives/ir.rs:5150-5153` omitted-dispatch ⇒ automatic-lane admission

`runner.environment` / backend inference (spec: no inference from `runner.environment`):
- `src/lib.rs:4473` `if: runner.environment == 'github-hosted'` runtime-setup gate
- `src/primitives/release.rs:2713-2719` `runner.environment == 'github-hosted'` gates
- `src/runtime.rs:154-163` `VELNOR_EXECUTION_BACKEND` ⇒ lane inference
- `src/lib.rs:2761` `runs_on.contains("self-hosted")` persistence inference
- `crates/velnor-workflow/README.md:101` documents the `runner.environment` gate

Label-substring inference (spec: no inference from label substrings):
- `src/policy.rs:2975-2978` `contains_self_hosted_label` (`"self-hosted"`|`"velnor"`)
- `src/policy.rs:2841-2973` `analyze_runner` built on it
- `src/primitives/ir.rs:5901-5904` `render_velnor_runner_identity_step` (hostname
  printout; §8 says this is not placement evidence)

`lanes`→`runner` rename canonicalization (alias):
- `src/policy.rs:2365`, `:2992` (`lanes` rewritten to `runner` before matching)
- `src/policy.rs:2391-2429` generated-gate shape matchers
  (`runner=='velnor'||runner=='both'||runner==''`)
- `src/policy.rs:2351` `matches!(runners, "velnor"|"both")`

Per-lane divergent contracts (must become one provider fanout):
- `src/lib.rs:621-624` `Unit.github_pr/full_commands` vs `velnor_pr/full_commands`
- `src/runtime.rs:1056-1059`, `:171` `CiUnit` lane command arrays + `commands()`
- `src/primitives/mod.rs:365` `UnitContract.lanes`
- `src/primitives/ir.rs:4058-4077` `unit_lane_facts` github_lane branch
  (mise/mbx hosted-only paths)

Velnor-specific options to fold into the provider-set schema:
- `velnor_labels`, `velnor_runner_group`, `velnor_trusted_label`,
  `velnor_trusted_runner_available`, `pull_request_on_velnor`, `velnor_rust_needs`,
  `velnor_concurrency_group`, `velnor_serial_stack_groups`
  (`src/config/mod.rs:181-250`, `src/lib.rs:840-890`, `src/primitives/ir.rs:2291-2308`)
- `macos_runner` untyped platform label (`src/config/mod.rs:170`, `src/lib.rs:839`)
- `github_runner` untyped platform label (`src/config/mod.rs:166`, `src/lib.rs:836`)
- `vars.VELNOR_AUTOMATIC_LANES` + `__VELNOR_AUTOMATIC_LANES_DEFAULT__`
  (`src/lib.rs:2485`, `:2710`, `:8042-8046`)
- `VelnorPullRequest::{TrustedOnly, Automatic}` (`src/primitives/ir.rs:2340`)
- `trust_gated_velnor_job_skipped` (`src/primitives/ir.rs:5415`) + `VelnorTrusted`
  admission rendering (`:5140-5146`, `:5455-5462`)
- `requires_trusted: bool` on `Unit` (`src/lib.rs:649`) and generation-config
  `requires_trusted` (`src/config/mod.rs:350`)
- `estate.rs` approved-Velnor-runner labels/group contract
- TUI mirrors of `RunnerMode`: `src/tui/mod.rs:1005-1006,1052`,
  `src/tui/view.rs:438-473,684-744,832-834,999-1017`
- `src/runtime.rs:49-86` runtime `CiConfig`/`Workflow` legacy fields
  (`runners`/`automatic` strings, `github_runner`, `velnor_labels`)
