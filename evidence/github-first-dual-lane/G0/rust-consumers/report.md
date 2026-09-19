# G0 Rust-consumer audit (early G3 input)

Read-only inventory for `tailrocks/{parallax,tracing-request-level,termrock,termpane,schemalane,ruxel,pg-bigdecimal,holla}`. No source checkout, remote, workflow, PR, or dispatch was changed.

## Observation/provenance

- Observed: `2026-09-19T16:40:40Z` (local `main` tips; GitHub API metadata observed the same date).
- Worker: `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`, `gpt-5.6-luna`, reasoning `max`, CLI `0.155.0`, cwd `velnor3` (state DB query).
- `rtk`: `0.49.0` (`/opt/homebrew/bin/rtk`).
- Research clones: `research/<repo>/`, each `git clone --depth 1 --branch main` from `https://github.com/tailrocks/<repo>`.
- Consumer config generator pin: `b9c3156cdb88e63c11b9e595a3e694b02238c09` in every `.github-gen/velnor-workflow.toml` (example: `research/schemalane/.github-gen/velnor-workflow.toml:3-17`).
- Scanner binary used for dry-run inventory: `velnor-workflow 0.1.0`, revision `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`, closure `dc789e9cf3d129d2b72c304fe087db8f011489158df6696936695c0a46bb6dc7`. This differs from the consumer pin; results below are observations, not a rollout/re-generation approval.
- Command shape: `target/debug/velnor-workflow research/<repo> --plain --dry-run`. All seven repositories with generated sidecars produced static unit plans; `termrock` failed before plan emission (details below).

## Exact live tips

| repository | `main` SHA | GitHub metadata |
|---|---|---|
| parallax | `6a12bf47a816b63e848b563aaa45ef9694159c79` | default `main`, public, not archived; pushed `2026-09-18T03:33:59Z` |
| tracing-request-level | `f234158eb3d5caddda83b19e527ffb7d0f675a23` | default `main`, public, not archived; pushed `2026-09-16T15:29:38Z` |
| termrock | `936982e60bce6d19cf33ae09b53545a955f1073d` | default `main`, public, not archived; pushed `2026-09-19T15:43:20Z` |
| termpane | `7602430f18852350cc3155e58bb96799234cbfe5` | default `main`, public, not archived; pushed `2026-09-19T15:43:27Z` |
| schemalane | `ab49e2dd910ee41e96daea97ad6998f7961e5cfc` | default `main`, public, not archived; pushed `2026-09-16T15:28:26Z` |
| ruxel | `3d34049820a6d4ce3707e574f407cbf4047ff932` | default `main`, public, not archived; pushed `2026-09-16T15:28:08Z` |
| pg-bigdecimal | `7dc5267d855801dbffb54aa12fc87791aa000a93` | default `main`, public, not archived; pushed `2026-09-16T15:27:37Z` |
| holla | `027dac6be2070b7e836c1ca123861fcbade72003` | default `main`, public, not archived; pushed `2026-09-16T15:25:18Z` |

All eight tips are the fleet-sync commits for generator pin `b9c3156c`.

## Scanner-unit and obligation inventory

| repository | generated scanner units at live tip | observed real obligations / platform | central gap or bounded follow-up |
|---|---:|---|---|
| `parallax` | 23: 1 Bun, 2 Docker, 20 Rust (18 workspace members + policy + fixture) | Rust 1.97; Bun UI; Docker image builds; release docs require deterministic single-binary archives, Apple native builds, Linux Zig/cargo-zigbuild, Homebrew preview; runtime supervises GreptimeDB and exposes OTLP/API ports (`research/parallax/docs/guide/quickstart.md:38-50`) | Docker units are image-build-only; service/compose/Greptime/Turso/OTLP exercise is not a typed service workload. Release and provenance contract is real but generated `[release] enabled=false` (`research/parallax/.github/ci/project.toml:20-22`). Keep as later G2/G3 release adapter work; no arbitrary publish now. |
| `tracing-request-level` | 1 Rust (`rust-tracing-request-level`) | One package, seven optional features (`axum`, tonic client/server/interceptor/logging, request context); no external service; no `Cargo.lock`, so generated command cannot honestly claim locked dependency resolution (`research/tracing-request-level/Cargo.toml:12-36`) | Cargo repository/homepage identify `donbeave/tracing-request-level` while fleet identity is `tailrocks/tracing-request-level` (`:7-9`); publishing identity must be explicit before G2. Release remains fail-closed. |
| `termrock` | **No plan**: current scanner aborts on dynamic `include_str!`; historical generated snapshot had 9 units (1 Bun docs + 8 Rust: policy, oldrev, contract-audit, and 5 direct crates) | Five root crates (`termrock`, catalog, catalog-web, cli, raster), two detached workspaces (`tools/oldrev-harness`, `docs/contract-audit`), Bun/Fumadocs/Playwright, WASM preview build, native catalog/raster/snapshot tests, crossterm/serde feature paths, old-revision compatibility; prior release declaration built `termrock` for Linux x86_64/aarch64 | Current `.github-gen/NO_WORKFLOWS_REQUIRED.md` suppresses all CI despite this surface. Fix scanner macro-path evaluation centrally (safe static `env!(CARGO_MANIFEST_DIR)` + literal `concat!` resolution), then regenerate. Do not preserve the no-op marker. Details below. |
| `termpane` | 3 Rust: policy, root crate, detached `fuzz` crate | `termpane` 0.1.0, four custom Criterion benches, `dhat-heap`, proptest/serialization fixtures, fuzz binary `damage_grid_process` (`research/termpane/Cargo.toml:24-72`, `research/termpane/fuzz/Cargo.toml:4-23`); no service | Generic all-targets/all-features unit sees root tests/benches; fuzz is separate and present. Publishable crate/release policy is not enabled; later package contract must state whether fuzz/bench are CI-only. |
| `schemalane` | 6 Rust: `pg-query-fmt`, CLI, core, embed-tests, macros, version (`research/schemalane/.github/ci/project.toml:20-105`) | Six-member workspace; core has `testcontainers-modules` Postgres dev-dependency (`research/schemalane/crates/schemalane-core/Cargo.toml:31-35`). Integration tests start Docker Postgres and are explicitly `#[ignore = "requires Docker daemon"]` (`research/schemalane/crates/schemalane-core/tests/postgres_integration.rs:10-17,625-640`); migration fixtures/embed tests and proc-macro trybuild paths also matter | Scanner emits only Rust units; no service capability/Docker endpoint appears. Add typed test-service discovery/adapter and prove hosted Docker boundary before G3 claims. `--no-tests pass` must not be treated as coverage of ignored Postgres integration. |
| `ruxel` | 7: 1 Docker image-build, policy, 5 Rust (`research/ruxel/.github/ci/project.toml:20-117`) | Rust toolchain asks for Linux musl/gnu and Apple arm64/x86_64 (`research/ruxel/rust-toolchain.toml:1-10`). Fixture Dockerfile installs `openssh-server`/Python and exposes SSH 22 (`research/ruxel/tools/fixtures/docker/Dockerfile:1-13`); gate runs cold/warm remote SSH nextest (`research/ruxel/tools/fixtures/gate.sh:1-26`); parity matrix covers Postgres, ext4/two-tier/xfs storage, systemd/control-flow/multihost (`research/ruxel/tools/fixtures/parity-matrix.json:1-15`) | Generated Docker unit only builds the image. SSH, privileged mounts/systemd/iptables, PostgreSQL, multi-host and storage fixtures need explicit fixture/capability units plus runner policy; do not infer G3 support from image build. Target matrix/release artifact adapter is also missing. |
| `pg-bigdecimal` | 1 Rust (`rust-pg-bigdecimal`) | Small single crate; NUMERIC encode/decode tests are local type tests, no live DB; likely crates.io package candidate (`research/pg-bigdecimal/Cargo.toml:1-25`) | Cargo repository/homepage identify `donbeave/pg-bigdecimal` while fleet identity is `tailrocks/pg-bigdecimal` (`:7-9`); publishing identity and provenance need explicit G2 input. |
| `holla` | 1 Rust (`rust-holla-cli`) | `holla-cli` 1.0.3 binary with build script; mise tasks include cargo-deb, cargo-zigbuild, Zig, nextest (`research/holla/Cargo.toml:1-16,50-77`; `research/holla/mise.toml:1-24`). Docs promise Homebrew preview, amd64/arm64 Debian packages and cross-repo signed apt publication (`research/holla/README.md:53-73`; `research/holla/docs/debian-apt-repo.md:40-65`); Linux/macOS cfg surfaces exist | Generic unit covers Rust build/test only. Current source has no generated release-deb/Homebrew workflow; release metadata is fail-closed. Preserve package/platform contract for later G2/G3 release adapter work; no arbitrary product release now. |

## `termrock` NO_WORKFLOWS_REQUIRED finding

The marker is a false negative, not a valid omission:

- Marker says static analysis failed at `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), ...))` and forbids workflows (`research/termrock/.github-gen/NO_WORKFLOWS_REQUIRED.md:1-10`).
- Current source has the exact dynamic forms at `crates/termrock/src/lib.rs:40,60,118-120,158`; these are test/source-evidence paths, not an unknown runtime include.
- Root workspace still declares five crates and Rust 1.97.1 (`research/termrock/Cargo.toml:1-18`). Catalog native feature links crossterm and raster (`research/termrock/crates/termrock-catalog/Cargo.toml:22-39`); catalog-web is a WASM `cdylib`/`rlib` (`research/termrock/crates/termrock-catalog-web/Cargo.toml:4-20`); raster embeds deterministic fonts (`research/termrock/crates/termrock-raster/src/fonts.rs:26-31`).
- Detached workspaces are real obligations: old-revision harness pins a previous `termrock` git revision and compares it with the head crate (`research/termrock/tools/oldrev-harness/Cargo.toml:4-19`); contract audit is a separate workspace (`research/termrock/docs/contract-audit/Cargo.toml:1-10`).
- Docs package defines `wasm-pack` plus generated catalog/contract/content/preview checks, and separate Playwright suites (`research/termrock/docs/package.json:9-33`). Catalog tests include capture/catalog/coverage/parity/shots/tablepro paths (`research/termrock/crates/termrock-catalog/tests/`).
- Historical generated object `ee6f3115aae790118bc2ac5d185e9b2af01821d9` (source `e05aee6de1d1614d752b4d1b1d26a49ac2c5ef91`) confirms the intended plan: `bun-termrock-docs`, `rust-policy`, five root crates, `rust-oldrev-harness`, and `rust-termrock-contract-audit`; `workflow.files` included `release.yml`. Its release declaration was `kind="rust-binary"`, package/binary `termrock`, targets `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. Reproduce with `git -C /Users/donbeave/Projects/tailrocks/termrock show ee6f3115aae790118bc2ac5d185e9b2af01821d9:.github/ci/project.toml` and the source config at that object.

Bounded central fix: teach the analyzer to evaluate only deterministic static macro expressions (`include_str!`/`include_bytes!` plain literal, recursive literal `concat!`, and `env!("CARGO_MANIFEST_DIR")` joined to a literal). Emit resolved paths into watch inputs; reject all non-static expressions. Add fixtures for this exact shape, then rerun the live tip. Do not add a termrock-specific exclusion or restore copied YAML.

## Shared detector/adapter gaps and bounded next work

1. **Static macro resolver (required before termrock G3 inventory):** implement safe recursive literal evaluation and source-path watch edges; add scanner tests; remove marker after a fresh plan proves all meaningful units.
2. **Service capability adapter:** detect `testcontainers`/ignored Docker tests and represent Postgres/Docker endpoint requirements. Schemalane is the proof case. A Rust unit with `--no-tests pass` cannot silently stand in for ignored service integration.
3. **Privileged fixture adapter:** discover explicit SSH fixture scripts, systemd/iptables/mount/storage/Postgres preparation and multi-host matrices. Ruxel needs capability declarations and a runner/image decision; Docker image build alone is insufficient.
4. **Target matrix adapter:** consume `rust-toolchain.toml` target lists and package scripts. Parallax, ruxel, holla, termrock and their release docs request Apple/Linux cross targets; a Linux-only G3 lane must report unsupported targets rather than fabricate coverage.
5. **Publishing-contract adapter:** preserve source-declared Homebrew/APT/archive/release verification obligations while leaving release fail-closed until immutable artifact, registry, provenance and tag policy are explicit. Applies to parallax/holla and historical termrock; no product release is authorized by this audit.
6. **Repository identity check:** tracing-request-level and pg-bigdecimal package metadata use `donbeave/*` while fleet repos are `tailrocks/*`; resolve identity before any crates.io/release declaration.
7. **Generator provenance gate:** consumer configs pin `b9c3156c`, while this audit binary is `abe9ad82`; keep the mismatch explicit and do not regenerate/roll out before G2’s pinned generator decision.

## Workflow state snapshot

On the 2026-09-19 main-branch check, the seven repositories with generated sidecars reached `Control/Planning`, then failed `Policy`; Velnor admission and workload/provider jobs were skipped, and required status failed. This is a policy-gate observation, not workload pass/fail evidence. Run IDs: parallax `35430912434`, tracing `35431028510`, termpane `35430790855`, schemalane `35430963356`, ruxel `35430809057`, pg-bigdecimal `35431160590`, holla `35430821753`. Termrock’s current main has no generated sidecar/workflow; its last generated run was `35077947676` and also skipped workload jobs after policy failure.
