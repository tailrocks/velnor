# Independent review: ten-workload fleet contract

Review target: `workload-contract-10-20260919T233341Z.json`, SHA-256
`910d4db877c4ada0e241ec390546d080b0aed2e34dca1e879f6d6237a628a47b`.
Review mode: read-only exact Git objects; no builds, installs, Docker, workflow
dispatch, hosted/Velnor execution, release, or source mutation.

## Verdict

`BOUNDED_ACCEPT_WITH_MUST_FIX_CONTRACT_DEFECTS`.

The ten-repository inventory is materially source-derived and correctly
classified as `source_derived_not_execution`; it is not integration-ready. The
source trees, unit roots, expected-job coverage, and non-execution boundary
check out. The contract must not be consumed as a complete dependency/platform
graph until the defects below are corrected.

## Exact source identity

For each row I resolved the recorded closing commit, counted
`git ls-tree -r --format='%(objectname) %(path)' <sha>` entries, and recomputed
the recorded SHA-256 over the byte-sorted listing (`LC_ALL=C sort`). Every row
matched:

| repository | closing commit | entries | listing SHA-256 prefix | units / expected IDs | expected jobs | platform rows |
|---|---|---:|---|---:|---:|---:|
| `tailrocks/velnor` | `d20d4d1d17590cca85b501d982cbaad70d42c641` | 680 | `7328126aa6c0` | 17 / 21 | 5 | 2 |
| `tailrocks/parallax` | `6a12bf47a816b63e848b563aaa45ef9694159c79` | 1881 | `ad86ce2f368c` | 23 / 25 | 3 | 3 |
| `tailrocks/tracing-request-level` | `f234158eb3d5caddda83b19e527ffb7d0f675a23` | 21 | `ade5aad63fbc` | 1 / 3 | 3 | 2 |
| `tailrocks/termrock` | `936982e60bce6d19cf33ae09b53545a955f1073d` | 1812 | `4bfec01c1c16` | 8 / 8 | 1 | 2 |
| `tailrocks/termpane` | `7602430f18852350cc3155e58bb96799234cbfe5` | 94 | `198cb98e5386` | 3 / 4 | 2 | 1 |
| `tailrocks/tablerock` | `e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc` | 1065 | `1cfeccd78531` | 11 / 12 | 3 | 2 |
| `tailrocks/schemalane` | `ab49e2dd910ee41e96daea97ad6998f7961e5cfc` | 90 | `3f4d61fc9bf0` | 6 / 8 | 3 | 2 |
| `tailrocks/ruxel` | `3d34049820a6d4ce3707e574f407cbf4047ff932` | 487 | `58be4997ef0a` | 7 / 9 | 3 | 3 |
| `tailrocks/pg-bigdecimal` | `7dc5267d855801dbffb54aa12fc87791aa000a93` | 20 | `a84928d58c1f` | 1 / 2 | 2 | 2 |
| `tailrocks/holla` | `027dac6be2070b7e836c1ca123861fcbade72003` | 85 | `bcee4dfd8f91` | 1 / 4 | 4 | 3 |

All non-termrock source-unit IDs equal an exact closing-commit
`.github/ci/project.toml` unit ID, and every non-root unit root exists in the
same exact tree. Termrock deliberately has no generated project file: its
marker is a scanner failure, not a no-workload proof (`termrock` commit
`936982e...`, `.github-gen/NO_WORKFLOWS_REQUIRED.md:1-10`).

## Capability matrix cross-check

The matrix is nonempty and source-backed, but it remains declarative.

| repository | exact source evidence checked | retained boundary / exclusion |
|---|---|---|
| Velnor | Generator/provider and release declarations in `d20d4d1d...:.github-gen/velnor-workflow.toml:18-27,62-77`; unit IDs/commands and release targets in `d20d4d1d...:.github/ci/project.toml:17-32,281-401`; runtime matrix in `d20d4d1d...:.github/workflows/ci-runtime-products.yml:91-107`. | GitHub/Velnor are configured source providers only; package/runtime/Apple execution and release proof remain absent. |
| Parallax | Exclusion and runner config in `6a12bf47...:.github-gen/velnor-workflow.toml:8-15`; Docker units in `6a12bf47...:.github/ci/project.toml:20-70`; product service contract in `6a12bf47...:README.md:102-106` and `docs/guide/cli.md:11-19`; release target behavior in `docs/guide/releases.md:21-26`. | Docker image builds do not prove GreptimeDB/Turso/OTLP service behavior; release is source-disabled; explicit `release/tests.rs` exclusion is retained. |
| tracing-request-level | Package identity and no lockfile claim in `f234158e...:Cargo.toml:1-8`; generated unit commands in `f234158e...:.github/ci/project.toml:20-35`. | `donbeave/tracing-request-level` Cargo identity differs from fleet `tailrocks/...`; no dependency-resolution or release proof. |
| termrock | Marker failure and reason in `936982e...:.github-gen/NO_WORKFLOWS_REQUIRED.md:1-10`; five workspace crates in `Cargo.toml:1-18`; detached old-revision harness in `tools/oldrev-harness/Cargo.toml:11-19`. | Marker is an exclusion/failure condition, not success; WASM/docs, detached audit, and old-revision obligations remain. |
| termpane | Generated source units and runner policy in `7602430f...:.github/ci/project.toml:20-65`; benchmark/heap obligations in `Cargo.toml:26-70`, `benches/present_frame.rs:1-13`. | Fuzz/bench/heap fixture obligations are retained without execution proof; release is disabled. |
| tablerock | Native platform in `e2fe040c...:native/Package.swift:1-11,54-60`; generated unit/runners in `.github/ci/project.toml:7-18,157-174`; Swift job is Ubuntu at `.github/workflows/ci-unit-swift.yml:103-104`. | macOS 26 arm64/x86_64 native work is source-declared but current reusable workflow is Linux; Velnor is excluded by `runners = "github"`. |
| schemalane | Unit dependency blocks in `ab49e2dd...:.github/ci/project.toml:25-107`; Postgres/Testcontainers and ignored Docker tests in `crates/schemalane-core/tests/postgres_integration.rs:1-14,94-963`; dependency in `crates/schemalane-core/Cargo.toml:23-33`. | Docker/Postgres service capability and ignored-test execution are unknown; release disabled. |
| ruxel | Docker SSH fixture in `3d340498...:tools/fixtures/docker/Dockerfile:4-13`; recursive parity matrix in `tools/fixtures/parity-matrix.json:1-15`; actual gate command in `tools/fixtures/gate.sh:17-24`; unit graph in `.github/ci/project.toml:25-117`. | Image build is not privileged SSH/storage/systemd/iptables/Postgres/multihost proof; target matrix is declaration-only; release disabled. |
| pg-bigdecimal | Package identity in `7dc5267d...:Cargo.toml:1-8`; generated unit in `.github/ci/project.toml:25-35`. | Cargo identity is `donbeave/pg-bigdecimal`, not fleet identity; release disabled. |
| holla | Package/debian metadata and documented release workflow in `027dac6b...:Cargo.toml:1-11,56-77`; apt signing/cross-repository requirements in `docs/debian-apt-repo.md:14-44,51-65`; closing tree has only six CI workflows and no release workflow. | Debian/Homebrew/cross-repository publication is a source contract only; no artifact, signature, install/upgrade, or current release workflow proof. |

Expected-job coverage is complete: every expected ID occurs in a logical job,
including aggregator `workload_ids`; no expected ID is silently dropped. This
does not turn any logical job into a run result. `actual_jobs`, build, release,
install, and run evidence remain null as required by the source-only scope.

## Must-fix contract defects

1. **Untyped external dependency.** The only dangling `depends_on` value is
   `tailrocks/termrock:rust-oldrev-harness -> termrock-old-revision`. Exact
   source proof is `936982e...:tools/oldrev-harness/Cargo.toml:13-16` and
   `tools/oldrev-harness/src/main.rs:17`: the dependency is
   `https://github.com/tailrocks/termrock.git` at immutable commit
   `5ff94ee117fd4a1b72fdd0d1b1847815055a93ac`. It is not a local source unit.
   Add a typed external Git node/edge carrying URL, commit, package, and
   provenance; do not invent a local unit. The remaining source-unit graph is
   acyclic (`tsort` exit 0).

2. **Missing Velnor platform rows.** `runtime-products-publish`,
   `release-package-signer`, and `preview-publication` are expected IDs but are
   absent from `workload_platform_architecture`. The source is sufficient to
   add exact rows without guessing: runtime products use Linux X64,
   Linux ARM64, and macOS ARM64 (`d20d4d1d...:ci-runtime-products.yml:96-107`);
   signer is Ubuntu 24.04 (`...:ci-release-package-signer.yml:10-26`);
   preview guest/deb matrices use x86_64/aarch64 Linux runners
   (`...:preview.yml:250-263,595-610`). Until rows are added, platform
   coverage is incomplete.

3. **Three per-path blob pins are malformed or wrong.** Exact `git rev-parse
   <closing>:<path>` checks found all other recorded canonical/workflow/action/
   scan pins matching, but these do not:

   - Schemalane `rust-toolchain.toml`: artifact has 39-hex
     `010f002cf56497ce0a1b4ac124a2e2d86a97bc3`; exact blob is
     `010f002cf56497ce0a1b4ac124a2e2d86a97bc3d`.
   - Schemalane `.github/workflows/ci-main.yml`: artifact has
     `50a03003bd393e5dbde9dd3f033b9a55196257b`; exact blob is
     `50a03003bd3935e3dbde9dd3f033b9a55196257b`.
   - pg-bigdecimal `rust-toolchain.toml`: same 39-hex artifact value; exact
     blob is `010f002cf56497ce0a1b4ac124a2e2d86a97bc3d`.

   Correct the immutable contract and enforce a 40-hex object check. The full
   tree listing digests still pass, so this is an artifact pin defect, not
   evidence that those source trees moved.

## Velnor movement classification

The contract correctly records a historical source movement, not an
inaccessible source. `revision_capture.stable_across_window=false` binds the
inventory to closing `d20d4d1d...`; initial `1048337062...` has a different
sorted tree digest (`23534c09...`) while closing has `7328126a...`. The closing
commit message is `fix(workflow): route hosted Apple jobs to macOS 26`. Any
future reread must preserve this initial/closing pair and use only the closing
snapshot for source obligations.

## Bounded acceptance tests for the collector/checker

1. Recompute all ten sorted tree counts/listing digests at the recorded closing
   commits; require exact equality.
2. Require every recorded per-path blob pin to be exactly 40 hex characters and
   equal `git rev-parse <closing>:<path>`; reject the three rows above until
   corrected.
3. Require every source-unit root to exist and every config-derived unit ID to
   map to exactly one source unit; preserve termrock's explicit marker mode.
4. Validate every internal recursive edge against its owning source block;
   validate external edges as typed immutable nodes, including the termrock
   old-revision URL/commit.
5. Require every expected workload ID to map to a logical job and a nonempty
   platform/architecture row; reject the three missing Velnor release rows.
6. Keep source declarations, exclusions, unknown provider/service capacity, and
   null execution fields distinct. No source-only row may be emitted as a run
   pass or release/install proof.

