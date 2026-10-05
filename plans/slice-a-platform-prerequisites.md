# Slice A: platform requirements, prerequisites, environment

Implemented in `crates/velnor-workflow`. New module + minimal wiring into the
existing scan → config → IR → render → runtime chain. No consumer names; the
`generic_surface_literals` deny list passes unchanged.

## Abstraction

Units declare what they need; lanes offer executors; generation matches them.

- `PlatformRequirement { os, arch, capabilities }` (`src/platform.rs`) is the
  only placement vocabulary. No runner label, pool, or provider appears in it.
  `Os`: any/linux/macos. `Arch`: any/x86_64/aarch64. Capabilities are
  lowercase shell-safe names; `xcode` and `xcframework` are the Apple-only
  ones (`requires_apple()`).
- `Executor { id, lane, os, arch, capabilities }` models what a lane offers:
  `github-linux` (Linux, no caps), `github-macos` (macOS + xcode/xcframework),
  `velnor` (Linux, no caps). `satisfies()` is OS/arch agreement (either side
  `Any`) plus capability superset.
- Scan derives the requirement from evidence: SwiftPM packages are portable
  (no Apple need — ordinary Swift no longer implies macOS); Xcode scheme
  units are macOS + `xcode`. Rust FFI shape (`[lib] crate-type` with
  `staticlib`/`cdylib`) is scan evidence (`rust-ffi:<name>` tag), never a
  placement need — the crate still verifies anywhere.
- Prerequisites: producers declare `[[units.products]]` (name, task, env
  outputs); consumers declare `[[units.prerequisites]]` (producer, product,
  task override, env inputs). Tasks are repository task-runner names
  (`mise run <task>`), never shell strings — consistent with the existing
  ban on command arrays in generation config.
- `platform::resolve()` compiles each edge into (a) a `depends_on` entry, so
  producer changes select the consumer through the existing transitive
  closure, (b) a prepare command (`[KEY='v' … ]mise run <task>`) prepended to
  every command vector, so local runs and jobs rebuild the product before
  checks, and (c) product-output env merged into the consumer's job env.
- `env` (`[units.env]`) carries build flags and product outputs generically;
  declared keys shadow generator defaults (e.g. `RUSTFLAGS`). `mbx = false`
  opts one Rust unit out of the object transport; `mbx = true` on any other
  kind is refused.
- Fail-closed placement: `validate_placement` errors when no enabled lane
  serves a unit (names unit + requirement + lane + remedy). `runners = both`
  keeps the explicit `jobs` opt-out rule, now unit-aware (kind or platform).

## Behavior changes (all intended, revision 49 → 50)

- SwiftPM units render on the GitHub default executor (Linux), not macOS.
  Xcode units stay on macOS. A kind split across both renders two collapsed
  jobs (`verify-github` + `verify-github-apple`) gated by a new
  `apple_executor` workflow_call input the callers pass per unit.
- Repo workflows regenerated (`--force`): 5 kind reusables gain the
  `apple_executor` input declaration only, plus the ownership-state refresh.

## Files touched

- NEW `crates/velnor-workflow/src/platform.rs` — requirements, executors,
  products/prerequisites, validation + materialization + placement, unit tests.
- `src/lib.rs` — `mod platform`; `GENERATOR_REVISION` 50; `Unit` gains
  `platform/products/prerequisites/env/mbx` + `uses_mbx()`; `lane_supports_unit`
  consults platform; `scan_target` calls `platform::resolve()`; per-unit mbx in
  `enable_mr_boxington_commands`; `apply_unit_rows/row` now `Result` with new
  fields; `report_unit_runners`/actionlint/reasons platform-aware;
  `validate_both_lane_unit_coverage` unit-aware; two Apple tests rewritten for
  the split contract; mechanical literal updates.
- `src/config/mod.rs` — `UnitSection`: `os/arch/capabilities/env/mbx` +
  `[[units.products]]` / `[[units.prerequisites]]` (`ProductSection`,
  `PrerequisiteSection`); row builders used by both validation and apply;
  `validate_units` + `validate_unit_references` extended (producers).
- `src/scan/mod.rs` — `unit()` helper defaults (portable, empty, `mbx: None`).
- `src/scan/swift.rs` — SwiftPM portable, Xcode macOS+`xcode`.
- `src/scan/rust.rs` — `[lib] crate-type` parsing + `rust-ffi:<name>` tag.
- `src/primitives/ir.rs` — `runner_for_unit` via platform;
  `uses_mr_boxington`/`tools_for_unit` per-unit; `render_unit_runtime` by
  `requires_apple()`; unit env in legacy env blocks; kind header renders
  agreed env (`agreed_env`, conflicts fail closed); `apple_executor` lane
  input + partitioned collapsed GitHub jobs.
- `src/runtime.rs` — one transitive-selection unit test only. No contract
  change: prerequisites compile to `depends_on`, so pinned runtimes
  (`deny_unknown_fields`) keep parsing new output.
- `src/primitives/release.rs`, `runtime_products.rs`, `tui/*` — mechanical
  `Unit` literal updates only.
- NEW `crates/velnor-workflow/tests/platform_prerequisites.rs` — 9
  integration tests (executor split, fail-closed Velnor placement, FFI→Swift
  edge incl. prepare + env + macOS resolution, unknown product/producer,
  build flags, mbx opt-out + mismatch, capability override).
- Regenerated: `.github/workflows/ci-unit-*.yml` (5), ownership state.

## Wiring points for the integrator

1. `scan_target` (`src/lib.rs`): `platform::resolve(&mut config)` sits
   between `apply_generation_config` and `enable_mr_boxington_commands`.
   Port as one call; order matters (materialized `depends_on` feeds mbxify,
   templates, and later validation).
2. `Executor` table (`src/platform.rs::executors_for`): parent may add
   executors (e.g. a macOS self-hosted pool) — `lane_supports_platform`,
   `runner_for_unit`, and `validate_placement` all derive from it, plus the
   `execute` mapping of executor id → runs-on in `ir.rs`.
3. `apple_executor` lane input (`lane_input`, `LaneStepFacts`,
   `unit_lane_facts`, `render_collapsed_kind_verify_job`): the pattern for
   any future per-executor split. Single-partition kinds render byte-identical
   gates to before (no clause when unsplit).
4. Kind env is agreed-env (`agreed_env`): members of one kind with different
   values for one key fail generation. If the parent needs per-unit env
   inside collapsed jobs, the upgrade is a new lane input + `GITHUB_ENV`
   export step (see memo note, not implemented).
5. Runtime gap (documented, not fixed): prerequisite-tier Rust units
   (`prerequisite_commands`) run check-only and drop prepare tasks; and
   `velnor-workflow run` does not export job env locally (same pre-existing
   gap as mold `RUSTFLAGS`, which is YAML-only). Both follow existing
   precedent; changing either needs a `project.toml` schema bump.
6. `Unit` serialization: new fields ride the scan JSON digest (inputs), but
   `ProjectConfig::toml()` deliberately omits them (pinned-runtime compat).
   Keep it that way unless the schema revs.
