# G1 source review: release seed and D19 pin

## Verdict

Approve source commit `12cc87b629802c294da9840325cb21087c020df6` for the G1 recovery candidate.
This is the source portion of PR #952 (`a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637`) applied directly to current main `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; `12cc` has parent `abe9ad82`. This source verdict does not approve merge or hosted gates.

## Changed behavior

- `crates/velnor-workflow/src/s2/primitives/release.rs:3180-3191` emits the Docker mutable-mount seed restore and context preparation in every release `(provider, unit)` leg whose cache contract declares `mutable_mount_seed`. The generic `ci-release-*` restore remains suppressed for those units.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:2545-2557` computes literal release keys with the same `unit_snapshot` grammar as the reusable unit workflow: compatibility digest, provider/platform/trust/unit segments, dependency hash, and freshness hash. Generated hosted and Velnor release keys match the corresponding `ci-unit-docker.yml` keys; paths remain `.velnor-docker-cache` and the context is created at `.velnor-docker-cache/seed`.
- `crates/velnor-workflow/src/s2/primitives/release.rs:3247-3254` adds a hosted-only `Fetch D19 pin history` step before the release unit command when that unit runs `--plain --check`. It extracts the configured 40-hex pin, fails closed if absent, checks local object history, and fetches the exact commit at depth 1 when needed. Local legs retain pinned-runtime provisioning and do not fetch.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:1882-1888` centralizes the fetch body shared with collapsed provider jobs; `mod.rs:40-47` exports the shared helpers.

## Exact checkouts and regeneration order

- Source/tests checkout: isolated detached `/private/tmp/velnor-pr952-review.Ls7go7` at exact PR head `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637` (base `a89a5f96dbec475313ba57aceccb1811c96b8fe8`). It contained PR #952's generated `release.yml` and generator-state outputs.
- Integrated source checkout: isolated detached `/private/tmp/velnor-12cc-review.0dn3ZZ` at exact `12cc87b629802c294da9840325cb21087c020df6`, parent/current-main `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`. Before regeneration, `--plain --check` exited 1 only for `.github/workflows/release.yml` and `.github/ci/.github-actions-generator-state`; `--plain --dry-run` reported exactly two updates.
- `--plain --force` was run only afterward in the isolated `12cc` worktree. It modified exactly those two generated files. Their SHA-256 values match PR #952's checked-in outputs: `release.yml` `89ab7b0a5cc6d578fe82c058000d3ac35e0301a9f001ef9a537cfefadc08c53c`; generator-state `fc9132b7875c3d0b2af3fc43fde67eca923a33f8da64f251e103a7d5cd759f36`.
- The 1858-test run therefore predates regeneration and tested source checkout `a5c1c0bd...`; no source file was changed by regeneration. The integrated source implementation is identical; only test placement differs between `a5` and `12cc`. A later pin-adoption regeneration is still required for `12cc`'s declared generator pin.

## Verification

- `cargo test -p velnor-workflow`: **1858 passed**, 20 suites, 45.09s, at exact PR head `a5c1c0bd...` before any generated-output write.
- `cargo test -p velnor-workflow s2::primitives::release::tests`: **96 passed**; both new seed and D19 tests included.
- `cargo fmt --check`: pass.
- `cargo clippy -p velnor-workflow --all-targets -- -D warnings`: pass.
- `actionlint .github/workflows/release.yml`: pass after regeneration.
- Shallow-clone reproduction fetched `fdeed261bd2247a38db6922a7726cd45d3d6f31e` with `git fetch --no-tags --depth 1 <repo> <pin>` and made the commit available to `git cat-file`.
- PR run `35338047632` failures are Velnor `operational_store` admission rejections before workflow execution. They are not source failures for this change; hosted jobs in that run passed where admitted. No hosted-gate or merge approval is implied.

## Required followup

Publish/bootstrap the generator/runtime from `12cc`, advance `[generator].revision` to the published immutable source pin, regenerate all owned outputs and state, then rerun `--plain --check` from a clean checkout. The current generated-output mismatch is the expected post-merge pin/regeneration step, not a source-review rejection.
