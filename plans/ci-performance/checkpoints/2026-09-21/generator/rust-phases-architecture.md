# Velnor Rust validation phase architecture

Read-only assessment for the Rust validation phase slice. Source inspected at `fix/ci-validation-contract` / `39b67ecd`, with Velnor schema 2 and open PRs 978 and 979 both targeting `main` at `97bac4c4`. No Velnor source or generated workflow was edited for this assessment.

## Current execution shape

Both generation pipelines flatten Rust work into command arrays. The scanners in `crates/velnor-workflow/src/scan/rust.rs` and `src/s2/scan/rust.rs` create commands in format, Clippy, and test order. The schema-1 `Unit` stores shared and provider-specific command arrays; schema 2 stores provider-neutral affected/full arrays and rejects arbitrary `pr_commands` and `full_commands` in its generation config. Schema 2 still supports scanned Rust units, `workspace_check`, and named `ci_tasks`.

That order is not visible as GitHub Actions phases. The schema-1 and schema-2 IRs emit one `Run unit checks` step that calls `velnor-workflow run --config .github/ci/project.toml --scope ... --unit ...`. The runtime reads one selected command vector and loops it inside that step. The two renderers also have per-unit surfaces with the same opaque run step. Release verification renderers call the same runtime command in one Rust checks step.

The runtime contract duplicates the command arrays in strict `CiUnit` deserializers in `src/runtime.rs` and `src/s2/runtime.rs`; both use `deny_unknown_fields`. Schema dispatch routes runtime commands to schema 2 from the checked-out `.github-gen/velnor-workflow.toml`, so the new serialized phase contract must be understood by the pinned runtime on each route.

Several decisions currently depend on command spelling. Both IRs decide whether to install Nextest by looking for `cargo nextest` or `mbx nextest` substrings. Both runtimes turn prerequisite-tier Clippy commands into Cargo `check` commands with string replacement. Tool setup and lock validation consume the same command arrays. `mbxify_cargo_command` rewrites Cargo strings for Mr. Boxington. These couplings let one opaque array determine ordering, tool provisioning, and what counts as compilation-only work.

There is a separate `velnor-workflow test-crates` CLI path. It enumerates package manifests and runs Nextest or `cargo test`; it does not provide formatting or Clippy phases. Keep its local behavior on the same typed plan or migrate its callers to that plan. `test-crates` itself is a special case in the Mr. Boxington wrapper and must remain explicit.

Velnor has no Rust Nextest archive, test-sharding, or `--no-run` path in the inspected tree. It does have a Velnor-only Cargo-source preparation job, dependency prerequisite tiers, and schema-2 `workspace_check` compile-only units. Those are real paths to preserve. Jackin may have archive/shard paths; validate those in the consumer checkout without inventing equivalent Velnor modes.

## Structural model

Use a shared typed `RustValidationPlan` as the source of truth. The scanner should construct phase invocations from manifest, workspace, target, feature, lockfile, and test-runner facts. A phase command should carry its working directory, executable/backend, argv, and environment as data; execution must not recover a phase by matching command text.

Represent the discovered phases directly:

- `Format`: formatting check commands.
- `Clippy`: lint commands with the existing targets, features, profile, lockfile, and warning policy.
- `Tests`: Cargo test or Nextest commands, selected by a `RustTestRunner` enum.
- `Doctests`: a separate Cargo doc-test invocation when Nextest is used and the package has a library target. Cargo test's existing doc-test behavior must not be duplicated.
- `Compile`: compile-only work such as `workspace_check` and prerequisite compilation.
- `Custom`: explicitly assigned repository commands or `mise run` tasks. Preserve their order and effects, but do not infer that a custom string is formatting, Clippy, Nextest, or test work.

Schema-1 custom commands remain executable as generic custom checks, with explicit phase assignment available for consumers that need a command to participate in `Format`, `Clippy`, `Tests`, or `Doctests`. Schema-2 `ci_tasks` remain generic custom work unless the generation config assigns them a phase. This preserves custom behavior while making standard Rust phases inspectable and preventing accidental Nextest installation or Clippy rewriting based on text inside a custom shell command.

Have the scanner and schema-specific `Unit` models carry the plan. The generated `.github/ci/project.toml` must serialize the selected plan for every scope/provider variant schema 1 currently supports and every affected/full schema-2 variant. Add a versioned runtime-contract field and bump it with the change; both strict runtime parsers should reject an unsupported contract before executing any command. The generator pin and runtime product must be promoted together before consumers emit the new contract.

Extend the runtime interface with an explicit phase selector, for example `velnor-workflow run --phase format|clippy|tests|doctests|compile|custom ...`. It should retain the existing selection-file, scope, unit, event, and provider checks, then execute only the requested phase. Non-Rust `run` behavior stays on its existing path. Replace prerequisite Clippy string rewriting with an explicit `Compile` intent. A prerequisite or compile-only job renders formatting, Clippy, and then the applicable compile work; it has no test step when that unit has no test phase.

Every generated Rust verification workflow should render separate ordinary GitHub Actions steps in this order: formatting, Clippy, tests, doctests when applicable. Each step calls the explicit runtime phase interface and relies on the default success dependency so tests cannot start after either check fails. Compile-only units retain the format and Clippy steps followed by a named compilation step. Custom checks remain a distinct named step unless their generation input explicitly assigns them to a standard phase. This is a set of steps in the same job, sharing checkout, toolchain, Cargo state, caches, and provider setup.

The typed `RustTestRunner` drives Nextest installation and lock validation directly. The typed execution backend drives Cargo versus Mr. Boxington invocation. Keep `rust-toolchain.toml` provisioning and existing `mise.lock` validation; do not discover phase tools from shell command strings.

## File ownership for implementation

| Concern | Files |
| --- | --- |
| Shared phase model, invocation types, validation, and display/render helpers | `crates/velnor-workflow/src/rust_validation.rs` (new) |
| Manifest-derived plans for both schema pipelines | `src/scan/rust.rs`, `src/s2/scan/rust.rs` |
| Typed unit data, serialization, and config application | `src/lib.rs`, `src/s2/mod.rs`, `src/config/mod.rs`, `src/s2/config/mod.rs` |
| Separate steps for reusable and per-unit Rust workflows; phase-driven tool setup | `src/primitives/ir.rs`, `src/s2/primitives/ir.rs` |
| Separate Rust release-verification steps | `src/primitives/release.rs`, `src/s2/primitives/release.rs` |
| CLI dispatch and phase execution in the pinned runtimes | `src/runtime.rs`, `src/s2/runtime.rs`, `src/s2/dispatch.rs` |
| Primitive contract description | `src/primitives/pipeline.rs` |
| Consumer adoption and regenerated workflows | Jackin generation input and generated output; Velnor generation input and generated output, owned by the integration parent |

Keep the implementation to one owner per overlapping source file. PR 978 is open from `ce75f7c3` and changes both IR files, check-profile/tool setup, and generated `ci-unit-rust.yml`; its current generated Rust workflow still has one `Run unit checks` step. PR 979 is open from `39b67ecd` and changes both IR files for required-verdict behavior. Rebase phase edits after those source slices land, preserve their gates and instrumentation, and regenerate instead of editing generated workflow YAML by hand.

## Fixture and regression coverage

Add generator fixtures and runtime behavior tests for:

1. A workspace using Cargo test with a library and doc tests; emitted format, Clippy, test, and applicable doctest steps have strict order.
2. A workspace using Nextest; typed tool resolution installs the locked Nextest tool, a test defect cannot pass, and doctests still execute separately.
3. A standalone crate, a testless crate, multiple manifests, and feature/target variation to preserve scan-derived flags and `--no-tests pass` behavior.
4. A `workspace_check` compile-only unit; it emits formatting, Clippy, then compilation, with no fabricated test run.
5. Schema-1 provider-specific custom command arrays and schema-2 `ci_tasks`; opaque commands run in the custom phase in declared order and are never reclassified from text. An explicitly phase-assigned custom invocation lands in that phase.
6. Cargo and Mr. Boxington execution backends, including the `test-crates` path and Rust toolchain/Mise setup.
7. Runtime contract compatibility: matching version executes; a stale runtime rejects the new contract before running any phase.
8. Runtime phase failure cases: format failure prevents Clippy/tests, Clippy failure prevents tests, and an actual test failure is reported by the test step.

Rendered-workflow assertions alone are insufficient. Exercise emitted commands through the runtime executor with fixture scripts that record invocations and exit status, then verify the generated YAML has separately named steps with success dependencies. Keep coverage generic; do not add repository-specific names or paths to the generator.
