# Development now

This is the repository’s current development contract. Citations use `path:line`.

## Current: build, test, lint, format

The workspace has 12 members and uses Cargo resolver 3, Rust edition 2024, and a
shared dependency table. The pinned toolchain is Rust 1.98.0 with `rustfmt` and
`clippy`; the workspace keeps rustfmt’s style edition at 2021. [`Cargo.toml:1-21`](../Cargo.toml)
[`rust-toolchain.toml:1-10`](../rust-toolchain.toml) [`rustfmt.toml:1-2`](../rustfmt.toml)

`mise` is the task entrypoint. Its lockfile is mandatory, and CI/jobs use locked
artifacts. Run these current gates:

```text
mise run fmt
mise run lint
mise run test
mise run test-production-topology
mise run test-release-feature-boundary
mise run actionlint
mise run deny
mise run check
```

- `fmt` is read-only: `cargo fmt --all --check`. `fmt-fix` is the in-place formatter.
- `lint` runs workspace/all-target clippy with all targets, `--locked`, runner
  `test-support`, and `-D warnings`.
- `test` runs workspace `cargo nextest` with `--locked` and runner `test-support`.
- `test-production-topology` checks every workspace target with default production
  features. The release-boundary task must fail if release code is built with the
  loopback-only `test-support` feature.
- `actionlint` checks workflow syntax; `deny` runs advisory checks. `check` is the
  aggregate gate and also runs the offline CI/fleet policy tasks.

These commands and their exact arguments are defined in [`mise.toml:18-51`](../mise.toml)
[`mise.toml:68-85`](../mise.toml) [`mise.toml:142-149`](../mise.toml). For a
focused loop, use `mise run test-focused -- -p <package>` or
`mise run test-focused-runner`; the latter enables runner fixture support.
[`mise.toml:31-37`](../mise.toml)

The current tagged release build uses `cargo zigbuild --release --locked
--target "$TGT"`, followed by `cargo deb -p velnor-runner --target "$TGT"
--no-build --deb-version "$VERSION"`. The runner manifest explicitly documents
this cross-build/package sequence and the no-strip requirement.
[`crates/velnor-runner/Cargo.toml:25-34`](../crates/velnor-runner/Cargo.toml)

## Current: crate ownership and dependency law

Ownership is layered:

- `velnor-model` owns shared versioned control-plane types and is the workspace
  root; `velnor-action-model` owns the canonical I/O-free physical-action model.
  [`crates/velnor-model/Cargo.toml:1-20`](../crates/velnor-model/Cargo.toml)
  [`crates/velnor-action-model/Cargo.toml:1-16`](../crates/velnor-action-model/Cargo.toml)
- `velnor-cas` owns digest-verified content-addressed storage; the crash-safe
  action journal is limited to the action and shared models; cache service owns
  lease-safe compiler caching over journal, action model, and CAS.
  [`crates/velnor-cas/Cargo.toml:1-20`](../crates/velnor-cas/Cargo.toml)
  [`crates/velnor-action-journal/Cargo.toml:1-24`](../crates/velnor-action-journal/Cargo.toml)
  [`crates/velnor-cache-service/Cargo.toml:1-20`](../crates/velnor-cache-service/Cargo.toml)
- `velnor-control` owns daemon-side application services and directly consumes
  only the journal and shared model. `velnor-client` owns the transport contract
  and consumes only the shared model. `velnor-render` owns output renderers and
  consumes only the shared model.
  [`crates/velnor-control/Cargo.toml:1-24`](../crates/velnor-control/Cargo.toml)
  [`crates/velnor-client/Cargo.toml:1-15`](../crates/velnor-client/Cargo.toml)
  [`crates/velnor-render/Cargo.toml:1-17`](../crates/velnor-render/Cargo.toml)
- `velnorctl` owns operator composition and the Axum adapter; it currently uses
  the runner library as an explicitly interim execution facade. `velnor-runner`
  owns the daemon/runtime binaries and consumes control, cache, action, and model
  crates. `velnor-tools` owns maintainer policy/verification tooling; the
  `unit-collector` tool owns structured Cargo evidence collection
  (`tools/unit-collector/src/main.rs:1-41`).
  [`crates/velnorctl/Cargo.toml:9-42`](../crates/velnorctl/Cargo.toml)
  [`crates/velnor-runner/Cargo.toml:9-23`](../crates/velnor-runner/Cargo.toml)
  [`crates/velnor-runner/Cargo.toml:107-144`](../crates/velnor-runner/Cargo.toml)
  [`crates/velnor-tools/Cargo.toml:8-26`](../crates/velnor-tools/Cargo.toml)

The law is enforced from `cargo metadata`, not by convention: the graph must be
acyclic; shared crates must not depend on Clap or Axum; client must not reach
daemon internals transitively; control’s direct workspace edges are allowlisted;
and the legacy runner allowance is explicitly transitional.
[`crates/velnor-client/tests/dependency_boundaries.rs:1-10`](../crates/velnor-client/tests/dependency_boundaries.rs)
[`crates/velnor-client/tests/dependency_boundaries.rs:370-423`](../crates/velnor-client/tests/dependency_boundaries.rs)
[`crates/velnor-client/tests/dependency_boundaries.rs:425-507`](../crates/velnor-client/tests/dependency_boundaries.rs)
[`crates/velnor-client/tests/dependency_boundaries.rs:509-565`](../crates/velnor-client/tests/dependency_boundaries.rs)

## Current: fixture and golden evidence

Evidence is structured, deterministic, and fail-closed:

- `unit-collector` consumes Cargo newline-delimited JSON. It never launches
  Cargo, uses human log lines as primary evidence, or invents causality. Freshness
  comes from the structured `fresh` field; timing comes only from structured
  fields; caller paths are normalized. Missing, unreadable, contradictory, or
  incomplete output evidence is `unknown`.
  (`tools/unit-collector/src/evidence.rs:91-201`)
- Fan-out attribution requires explicit path-free unit IDs, graph edges, and
  invalidation roots. Missing roots/data, duplicate edges, unknown endpoints,
  and cycles produce unknown metrics; zero means a validated graph with no
  downstream units (`tools/unit-collector/src/fanout.rs:1-6`,
  `tools/unit-collector/src/fanout.rs:182-244`).
- Committed collector fixtures cover fresh, touched-source, and dependency-bump
  passes. Their tests assert expected unit kinds/freshness and preserve a known
  target triple (`tools/unit-collector/tests/fixtures.rs:9-70`).
  [`tools/unit-collector/tests/fixtures.rs:9-70`](../tools/unit-collector/tests/fixtures.rs)
- Renderer goldens cover 12 resource nouns through every approved output format,
  compare bytes against committed files, require deterministic machine formats,
  and keep warnings on stderr. Regeneration requires
  `UPDATE_GOLDENS=1 cargo nextest run -p velnor-render`, followed by human diff
  review. [`crates/velnor-render/tests/goldens.rs:1-6`](../crates/velnor-render/tests/goldens.rs)
  [`crates/velnor-render/tests/goldens.rs:148-178`](../crates/velnor-render/tests/goldens.rs)
  [`crates/velnor-render/tests/goldens.rs:251-260`](../crates/velnor-render/tests/goldens.rs)
- Model goldens require byte-identical JSON round trips, YAML round trips,
  `schemaVersion` on every resource, RFC 3339 timestamps, fail-closed unknown
  enums, and `null` for unavailable durations rather than zero.
  [`crates/velnor-model/tests/golden_schema.rs:1-3`](../crates/velnor-model/tests/golden_schema.rs)
  [`crates/velnor-model/tests/golden_schema.rs:130-201`](../crates/velnor-model/tests/golden_schema.rs)
- Fixture rendering must retain run/repository/job identity while stripping
  credential-bearing URL userinfo and secret markers from both stdout and
  stderr. [`crates/velnor-render/tests/fixture_run_model.rs:1-5`](../crates/velnor-render/tests/fixture_run_model.rs)
  [`crates/velnor-render/tests/fixture_run_model.rs:87-135`](../crates/velnor-render/tests/fixture_run_model.rs)
- Runner telemetry is exact NDJSON evidence: the committed golden checks event
  order, unknown trust domain, and absence of secrets; store-failure tests also
  require transaction rollback with no partial durable rows.
  [`crates/velnor-runner/tests/telemetry_integration.rs:90-130`](../crates/velnor-runner/tests/telemetry_integration.rs)
  [`crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson:1-3`](../crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson)

## Current: contributor and security rules

- Match runner protocol behavior to `actions/runner` before writing protocol code;
  do not guess. Finish migrations by deleting old paths—no compatibility
  shims, aliases, or deprecation periods.
- Never cancel the named release/package workflows or release dispatches. Report
  suspected stale runs in `COORDINATION.md`. This is a research project and is
  not production-ready.
- For bugs, inspect why the architecture allowed the bug class and related bugs;
  prefer a structural fix that removes the enabling condition. Use a symptom
  patch only when the root fix is infeasible or separately scoped, and name it.
- Keep dependency resolution and CI commands locked. Run `cargo deny check
  advisories`; the sole current advisory exception is `RUSTSEC-2023-0071`,
  documented as the RSA Marvin timing side-channel not reachable on Velnor’s
  local ephemeral OAuth-JWT signing path.
  [`mise.toml:10-15`](../mise.toml) [`mise.toml:71-72`](../mise.toml)
  [`deny.toml:1-8`](../deny.toml)
- Keep test-only loopback support out of release builds. The release feature
  boundary is an explicit gate, and the runner manifest says `test-support` is
  for repository tests while release binaries reject it.
  [`mise.toml:43-51`](../mise.toml) [`crates/velnor-runner/Cargo.toml:154-163`](../crates/velnor-runner/Cargo.toml)
- Treat fixture and golden data as potentially sensitive. Sanitize endpoint
  credentials, assert secret absence, and preserve only structured evidence;
  the collector's fail-closed evidence behavior is implemented in
  `tools/unit-collector/src/evidence.rs:260-291`.
  [`crates/velnor-runner/tests/telemetry_integration.rs:26-35`](../crates/velnor-runner/tests/telemetry_integration.rs)

## Current: product boundary

- Phase 0 means proving existing Rust CI/CD workflows against the public fixture
  first. Velnor-native workflow language, YAML scheduling, and macOS job execution
  are outside the current scope (`docs/system-now.md:63-69`).

## Future / planned — not current procedure

- The `velnor-runner` library dependency in `velnorctl` is interim. Plan 079 is
  the stated deletion point for the runner binary and remaining plumbing; do
  not add new dependencies to extend that transition.
  (`docs/future-direction.md:93-107`,
  `crates/velnorctl/Cargo.toml:31-42`).
- `unit-collector` TASK-004 is not complete until the real Parallax ordinary-PR
  attribution report exists. Committed synthetic fixtures are current evidence,
  not completion proof (`tools/unit-collector/tests/fixtures.rs:9-70`,
  `docs/evidence-record-2026-09-01.md:649-657`).
