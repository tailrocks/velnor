# Velnor Roadmap (runner-internal implementation reference)

**Top-level direction and sequencing live in [master-plan.md](master-plan.md)**
— that is the plan. This document is the runner-internal implementation
reference: protocol decisions, daemon/slot model, Docker execution model,
adapter strategy, and verification layers.

Status 2026-06-11: the implementation goal below (drop-in V2/JIT runner,
Docker job isolation, native adapters, daemon slots) is **achieved and in
production** — three dual-lane repos run Velnor by default; the daemon ships
as a Debian package with never-exit supervision, sd_notify watchdog,
template instances, and doctor timers (master-plan P0/P1 complete). Active
engineering: master-plan P2 (estate pipeline tuning), P3 (performance core:
native HTTP transport, zero-copy logs, dynamic slots, org-level JIT), P3b
(native-adapter completeness — no JS product path), P4 (UX parity matrix).
Sections below describe the standing architecture; where a detail conflicts
with master-plan, master-plan wins.

## Hard Rules

**Fixture is the contract.** `tailrocks/velnor-actions-fixture` defines the exact
GitHub Actions patterns Velnor must support. Never simplify or remove fixture
content to work around a Velnor gap. Fix Velnor instead.

**actions/runner is the source of truth.** When implementing protocol behavior —
expression types, input structures, JIT fields, broker messages, credentials —
always read https://github.com/actions/runner first. Do not guess. Implement to
match what GitHub's own runner does.

**Always use the latest protocol version.** When the official runner has both a
legacy and a current implementation path, always choose the current one:
- Results Service (WebSocket + Twirp) over Distributed Task timeline API
- V2 broker over classic polling
- JIT config over registration tokens
- Latest crate versions in Cargo.toml at all times

Never implement deprecated paths from the runner just because they exist in older
code. If a newer path exists, that is what Velnor implements.

**Strict capability manifest.** Drop-in compatibility means exact behavior for
the declared surface, not best-effort arbitrary Actions execution. Validate the
expanded job against
[strict-capability-contract.md](strict-capability-contract.md) before any side
effect. Unsupported refs, inputs, values, and combinations fail clearly. New
surface requires explicit operator approval.

**Estate uniformity is concern-based.** Repositories do not gain meaningless
jobs merely to look alike. First inventory which CI concerns each repository
actually has and which baseline concerns its class requires. Every shared or
required concern then uses the same canonical workflow name, job id, lane
matrix, step order, pinned actions, inputs, environment, cache keys, timeouts,
concurrency, writer gate, and required aggregator. Repository-specific work may
differ only where its product surface differs. Missing required concerns are
added from the canonical template; non-applicable concerns are documented and
omitted, never represented by fake no-op jobs.

**Final estate delivery is review-gated.** Every applicable repository ends
with one final, reviewable program PR (or its binding trunk-only delivery
record), canonical concern shapes, local-only sccache, and scheduled `both`
parity. Required-check policy is applied from the canonical handoff only after
the named checks exist. Automation must prepare and verify these deliveries,
but the operator alone decides whether to merge them.

## Implementation Goal

Velnor is a GitHub Actions-compatible runner daemon with a Rust runtime.

Phase 0 does not replace GitHub Actions YAML. Existing repositories keep their
current workflows, triggers, matrices, reusable workflows, secrets, and GitHub
UI. GitHub still parses and schedules the workflow. Velnor starts after GitHub
assigns an already-expanded runner job.

The first product goal is:

- run existing GitHub Actions jobs without changing workflow YAML
- use GitHub's current runner V2 broker/run-service protocol only
- create runner identities through GitHub's JIT configuration API only
- run every assigned Linux job inside a Docker container
- let one Velnor daemon manage multiple internal runner slots
- support the first target workflow surface used by `jackin-project/jackin` and
  `ChainArgos/java-monorepo`

Storage and admission are runner subsystems, not per-workflow conventions. The
canonical implementation contract is
[storage-and-disk-pressure-2026-07-18.md](storage-and-disk-pressure-2026-07-18.md):
Velnor catalogs and bounds all owned persistent data, reclaims inactive data to
reserve worst-case job space before registering a slot, and holds that
reservation through result upload. It never accepts a job and then silently
refuses it for disk pressure.

Hard rule: Velnor must first be a drop-in GitHub Actions runner replacement for
Rust repositories. Both first target projects are Rust projects for Phase 0
purposes. `ChainArgos/java-monorepo` has "java" in the repository name, but the
target workflow surface Velnor must support is Rust, Docker, Buildx, cache,
artifact, Renovate, and related Rust CI/CD behavior.

## Target Project Analysis

The target repositories are current at:

- `jackin-project/jackin`: `52a457b689940e05ed65015d2a82ff0e22577d2e`
- `ChainArgos/java-monorepo`: `92b52c3249f1d58e208bd0aeaea20580a4a07139`

### Shared Rust CI Requirements

Velnor must support the common Rust GitHub Actions shape used by both projects:

- checkout current repository and selected external repositories
- install Rust toolchains and cargo tools
- run `cargo fmt`, `cargo clippy`, `cargo check`, and `cargo nextest`
- use `cargo nextest run` as the sole Rust test runner in local verification,
  CI, scripts, and instructions; never invoke `cargo test`
- preserve documentation-example coverage by moving executable examples into
  nextest-discoverable regression tests, because stable nextest does not run
  rustdoc doctests
- support `rustup component add` and target installation
- support `mise` as the main tool installer for all target projects
- support `Swatinem/rust-cache` for ChainArgos shapes
- support `sccache` setup and soft-fail gates
- support cargo registry/git cache restore/save keyed by `hashFiles`
- support command files: `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`,
  `GITHUB_STATE`, and `GITHUB_STEP_SUMMARY`
- support job outputs, step outputs, required aggregate jobs, and runtime
  expression fallback such as `steps.dispatch.outputs.x || steps.filter.outputs.x`
- support bash run steps, working directories, and `defaults.run`
- pass GitHub runtime env, cache env, results env, OIDC env, and masked secrets
  into scripts and native adapters

### ChainArgos Rust Target

Despite the repository name, ChainArgos Phase 0 target is Rust-heavy:

- `ansible.yml`: checkout, mise (Python via mise.toml), bash defaults, and
  basic command execution
- `rust.yml`: path filters, `mise`, Rust formatting/linting/tests, package
  workflow-dispatch inputs, sccache, required jobs, and job outputs
- `rust-docker.yml`: Docker login, Buildx/Bake, Docker cache/runtime env,
  workflow-dispatch inputs, and required jobs
- `rust-docker-build.yml`: reusable workflow expanded by GitHub into Docker
  build jobs
- `kestra-build-image.yml` / `kestra-build-publish.yml`: Rust Docker image
  build/publish workflow with Docker Hub auth, cache, and Buildx
- `renovate.yml`: Renovate Docker/action behavior with repository token/env

Observed runner label:

- `hetzner-sentry-ci`

Implication: ChainArgos is the first practical live target because its jobs
already use a self-hosted-style Linux label. Velnor should run it without YAML
rewrites after JIT/V2 setup is implemented.

### Jackin Rust/Linux Target

Jackin Phase 0 target is Linux Rust CI and related release/docs automation:

- `ci.yml`: path filtering, Rust check/MSRV/validator jobs, sccache, mise,
  mold, cargo cache, nextest, Docker Buildx E2E cache, and aggregate-needs
- `construct.yml`: Docker Buildx, Docker login, artifact fan-in/fan-out,
  publish rehearsal, and aggregate-needs
- `docs.yml`: docs build/link checks, Pages artifact/deploy, environment URL,
  sitemap output, deployed-docs composite, and required aggregator
- `renovate.yml` / `renovate-validate.yml`: Renovate and validation paths
- later Linux portions of `preview.yml` and `release.yml`: release artifacts,
  external checkout, upload/download artifacts, and Homebrew update paths

Observed Linux labels:

- `ubuntu-latest`
- `ubuntu-24.04`
- `${{ matrix.runner }}` resolving to Ubuntu labels

Observed non-target labels:

- `macos-latest` and other macOS release matrix legs

Implication: Velnor may claim Jackin Linux labels when Docker provides Linux
containers with the needed Rust/Docker tools. Velnor must not claim macOS labels
or pretend to build macOS jobs inside Docker.

### Target Action Families

Velnor must support these action families as Rust-native adapters or supported
local/composite behavior for Phase 0:

- `actions/checkout`
- `actions/cache`
- `actions/upload-artifact`
- `actions/download-artifact`
- `actions/upload-pages-artifact`
- `actions/deploy-pages`
- `dorny/paths-filter`
- `jdx/mise-action`
- `extractions/setup-just`
- `Swatinem/rust-cache`
- `mozilla-actions/sccache-action`
- `rui314/setup-mold`
- `crazy-max/ghaction-github-runtime`
- `renovatebot/github-action`
- `docker/login-action`
- `docker/setup-buildx-action`
- `docker/metadata-action`
- `docker/build-push-action`
- `docker/bake-action`
- local `./.github/actions/aggregate-needs`
- local `./.github/actions/check-deployed-docs`
- GitHub-expanded reusable workflow jobs from ChainArgos

### Rust Runner Replacement Requirement

Velnor success is not "a generic container runner starts". Success means these
Rust workflows behave like they do on GitHub runners:

- Rust toolchains install correctly.
- Cargo caches and sccache behave well enough for repeat CI.
- Path-filter outputs gate downstream Rust jobs correctly.
- Required aggregate jobs get correct `needs.*.result` and output behavior.
- Docker/Buildx/Bake flows work from inside the Linux job container.
- Artifacts move between jobs in the same workflow run.
- Pages/environment URLs and job outputs appear correctly in GitHub UI.
- Logs, annotations, summaries, masking, and final conclusions are readable and
  accurate.

## Host And Job Model

Velnor daemon process should be allowed to run on:

- macOS developer hosts
- Linux hosts
- Linux Docker containers, including on Docker Desktop for macOS

Velnor job execution is Linux container execution. A macOS host can run the
Velnor daemon, but assigned jobs still run inside Docker Linux containers.
Velnor must not claim or emulate macOS runner labels, because Docker Desktop
does not provide macOS job containers.

Required behavior:

- Velnor daemon may run natively on macOS.
- Docker job containers must be Linux containers.
- Velnor must reject macOS/Darwin runner labels.
- Velnor must reject target jobs that require true macOS behavior.
- `ubuntu-latest`, `ubuntu-24.04`, and project Linux labels can map to Velnor
  when the job container image supplies the expected tools.
- `ubuntu-24.04-arm` is valid only when Docker can provide ARM Linux containers.

Current implementation status:

- `velnor-runner configure`, `run`, `daemon`, and `preflight` no longer reject
  non-Linux process hosts.
- Host validation is capability-based: runner labels and Docker job containers
  must be Linux-compatible, while macOS/Darwin labels remain rejected.

## GitHub Protocol Decision

Velnor uses runner V2 only.

Supported setup path:

```text
POST /repos/{owner}/{repo}/actions/runners/generate-jitconfig
POST /orgs/{org}/actions/runners/generate-jitconfig
POST /enterprises/{enterprise}/actions/runners/generate-jitconfig
```

GitHub REST docs define required JIT fields as runner `name`,
`runner_group_id`, `labels`, and optional `work_folder`. The response includes
`encoded_jit_config`, which is passed to the runner startup path.

Velnor must decode `encoded_jit_config` and require:

- `UseV2Flow=true`
- `ServerUrlV2`
- runner id
- runner name
- pool id
- GitHub URL
- OAuth credential data
- private RSA credential material needed to mint OAuth runner tokens

Unsupported setup paths:

- repository runner registration tokens
- `actions/runner-registration` as the normal setup path
- classic distributed-task polling
- fallback from missing V2 settings to classic polling

Current implementation status:

- classic registration-token setup and remove flow are removed from the product
  CLI path.
- daemon slot setup uses the JIT `configure` path.
- JIT decode and daemon slot recycling are implemented.

## Daemon Slot Model

GitHub runner protocol assigns one active job to one runner session. Velnor
should hide that from the operator.

Product model:

- operator starts one Velnor daemon
- daemon owns `--slots N`
- each slot has its own JIT runner identity
- each slot has its own V2 broker session
- each assigned job runs in one isolated Docker job container
- daemon can run multiple jobs concurrently by supervising multiple slots

JIT runner lifecycle:

1. create one JIT config for a slot
2. decode and store slot settings
3. start V2 broker session
4. acquire one job
5. execute job in Docker
6. complete job through run-service
7. recycle slot by creating a fresh JIT runner config

Cleanup rule:

- JIT runners are ephemeral and should disappear after handled jobs.
- If Velnor creates a JIT runner but fails before execution, cleanup must delete
  only the exact runner id created by Velnor.
- Velnor must never remove unrelated runners or unrelated Docker containers.

## Docker Execution Model

Every target job runs in a fresh Linux Docker job container, even if the GitHub
workflow does not declare `container:`.

Required Docker behavior:

- per-job Docker container
- per-job Docker network
- per-slot and per-job temp/work/action/tool directories
- Docker bind-mount preflight before acquiring jobs
- Docker socket mount when target jobs need Docker/Buildx
- no host Docker CLI binary mount by default
- default job image is project-owned `velnor/job-ubuntu:26.04`, built from
  official `ubuntu:26.04`
- job image includes Docker CLI and Buildx, so Docker-heavy workflow steps can
  use the mounted Docker socket

On macOS Docker Desktop:

- Velnor daemon can run on host macOS.
- Docker daemon still runs Linux containers.
- `--docker-host-work-dir` may be needed when daemon-visible and
  process-visible paths differ.
- preflight must prove Docker can see the bind-mounted Velnor work directory
  before Velnor polls GitHub for jobs.

## Native Action Adapter Strategy

Phase 0 should not execute supported marketplace JavaScript or TypeScript action
bundles as the product path.

Instead:

- route known target action families to Rust-native adapters
- support target local composite actions
- support target repository composite actions where needed
- fail clearly for unsupported actions
- do not hardcode target workflow ids, job ids, or step ids

Target adapter families include checkout, cache, artifacts, setup/tool actions,
paths-filter, Pages, Renovate, Docker login/setup/build/bake/metadata, runtime
export, Rust cache, and sccache.

## Verification Strategy

Verification must be staged.

Local non-mutating gates:

```sh
cargo fmt --check
cargo nextest run --workspace --locked
scripts/target_verify.sh
scripts/fixture_readiness.sh
```

On macOS, full Rust tests that depend on current Linux host guards may need to
run inside a Linux test container until host validation is fixed.

Docker safety rule:

- never stop, delete, prune, or mutate unrelated containers
- use explicit `velnor-*` names for Velnor-spawned proof containers
- use `--rm` for temporary verification containers
- cleanup only exact Velnor job containers/networks/runner ids

Live proof stages:

1. public fixture readiness
2. public fixture smoke through Velnor JIT/V2
3. Sentry fixture proof with comparison evidence
4. readiness handoff for user/operator-owned target testing

Completion requires GitHub Actions UI evidence:

- jobs assigned to Velnor
- readable logs
- correct step conclusions
- correct job conclusions
- command files and outputs work
- artifacts/cache behavior works for target jobs
- Docker/Buildx paths work
- no unsupported feature drift in target workflow scans

## Testing Plan

Velnor testing has three layers.

### 1. Local Machine Verification

Goal: prove the binary, Docker job image, planner, native adapters, and local
executor behavior work before touching real GitHub jobs.

Required checks:

```sh
cargo fmt --check
cargo nextest run --workspace --locked
cargo run -q -p velnor-tools -- check-runner-reference
docker build -f docker/job-ubuntu.Dockerfile -t velnor/job-ubuntu:26.04 .
docker build -t velnor-runner:local .
```

Local Docker checks must not stop, remove, or prune unrelated containers. Any
temporary container must use an explicit `velnor-*` name and `--rm`.

Local fixture project:

- create or maintain a small Rust fixture repository
- include multiple Rust crates
- run fmt, clippy, tests, and nextest-shaped commands
- use `actions/cache`-style Cargo cache keys
- use sccache-like env and wrapper behavior
- use `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`, and summaries
- upload and download artifacts between jobs
- run Docker/Buildx steps from inside the Linux job container
- include a rebuild path so repeat runs prove cache behavior and incremental
  speed

This fixture must model the Rust CI behavior from Jackin and ChainArgos without
requiring their full repositories.

### 2. Public GitHub Fixture Verification

Goal: prove GitHub can schedule real jobs to Velnor and GitHub UI receives
correct status, logs, outputs, cache/artifact behavior, and conclusions.

Required path:

1. run non-mutating fixture readiness
2. create V2 JIT runner slots
3. run fixture GitHub-hosted lanes
4. run equivalent Velnor lanes
5. compare artifacts/outputs between GitHub-hosted and Velnor lanes
6. inspect GitHub UI logs, annotations, summaries, and conclusions

Current fixture repo:

- `tailrocks/velnor-actions-fixture`

Future fixture expansion should cover:

- Cargo cache restore/save
- repeat rebuild speed
- sccache behavior
- Buildx Docker builds
- artifact fan-in/fan-out
- path filter outputs
- required aggregate job logic

### 3. Real Server Verification

Goal: prove Velnor works on the real runner host and is ready for operator-led
target repository validation.

Server access:

```sh
ssh sentry
```

On the server:

- verify Docker daemon and Velnor workdir visibility
- build or pull `velnor/job-ubuntu:26.04`
- run Velnor preflight
- run public fixture smoke through V2 JIT
- compare fixture outputs, artifacts, logs, conclusions, and timing against the
  GitHub-hosted lane
- collect GitHub run URLs and evidence
- confirm no unrelated containers are stopped, deleted, pruned, or modified

Only after the fixture is green on Sentry, behavior is equivalent, and evidence
is collected should Velnor be marked ready for manual target repository testing.

### 4. Manual Target Repository Handoff

Target repositories are not part of the automated roadmap execution. They are
manual validation surfaces owned by the user/operator.

When Sentry and fixture verification pass, Velnor should produce a readiness
notice for operators:

```text
Velnor is ready for manual target repository testing.
Fixture verification passed on Sentry.
Behavior matched the GitHub-hosted comparison lane for required logs, outputs,
artifacts, cache behavior, conclusions, and timing evidence.
Please run the target repository validation steps below and send feedback with
GitHub run URLs, failures, logs, artifacts, and timing notes.
```

Recommended manual order:

1. ChainArgos Rust workflow set
2. ChainArgos Docker/Buildx workflow set
3. Jackin `ci.yml`
4. Jackin `construct.yml`
5. Jackin `docs.yml`
6. later Jackin Linux release/preview paths

User/operator owns all target repository execution. Agent must not set
`VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`, dispatch target workflows, or claim
target validation complete without operator evidence.

Acceptance rule:

- contributing user should not notice the runner change
- same workflow YAML
- same required checks
- same output/artifact/cache behavior needed by target jobs
- faster or at least no worse queue-to-result latency after warm caches

## Performance Plan

Performance is a first-class goal. Velnor exists to make Rust CI as fast and
reactive as possible while staying compatible with the target GitHub Actions
usage.

Performance targets:

- minimize time from GitHub job assignment to first log line
- keep daemon slots warm and ready
- avoid unnecessary process/container startup work
- use project-owned Ubuntu job image with required tools preinstalled
- keep Cargo registry/git cache hot
- support sccache and shared cache directories
- avoid marketplace JavaScript action startup for known target actions
- use Rust-native adapters for setup, cache, artifact, Docker, and Pages paths
- recycle JIT runner slots quickly after job completion
- expose clear timing evidence in fixture and target runs

## Native Adapter Performance vs GitHub Defaults

**Parallelism model:** GitHub-hosted runner executes steps serially within a
job. Velnor does the same (steps run in order) because step ordering is
workflow-defined. Parallelism is at the *job* level: Velnor's `--slots N`
daemon runs N jobs concurrently on the same host, each in an isolated Docker
container. This matches or beats GitHub's parallel job execution since Velnor
avoids queue wait.

**Cache tactics (warm vs cold):**

- Cache restore (`_velnor_caches/`) is a local file copy — typically 10–200ms
  vs GitHub's 1–30s GHA cache API download. Timing is emitted in each step's
  stdout as `({ms}ms)`.
- `Swatinem/rust-cache` uses the same local store. Warm Cargo registry + target
  dir restore takes <100ms; GitHub equivalent is 10–60s network download.
- sccache server starts immediately from a warm install in the job image.
  `sccache --show-stats` runs in the post step to expose hit/miss rates.

**Adapter startup vs JS startup:**

Native Rust adapters skip the Node.js process startup + action bundle load
that marketplace JS requires (~0.5–1s cold, ~0.3s warm per action). For a
workflow with 10+ action steps, this saves 3–10s of pure startup overhead.

**Phase 0 measured wins (emitted in step logs):**

- Cache restore time: printed as `Cache restored from key '…' ({ms}ms)`
- Cache save time: printed as `Saved cache '…' with N path(s) ({ms}ms)`
- Rust cache restore time: printed similarly
- sccache stats: printed in post step via `sccache --show-stats`

Performance evidence to collect:

- queue wait time
- JIT setup time
- broker session setup time
- container startup time
- first step start time
- total job runtime
- warm-cache rebuild runtime
- cache hit/miss state (now in step stdout with timing)
- Docker Buildx cache hit/miss state
- sccache hit/miss rate (now in post step)

## Implementation Roadmap

### 1. Make V2 JIT the only setup path

- add JIT config REST client
- support repository JIT endpoint first
- add org and enterprise endpoint mapping
- decode `encoded_jit_config`
- decode inner runner/settings JSON
- decode credential payload and RSA params
- convert RSA params to existing OAuth credential storage or teach OAuth code to
  sign directly from RSA params
- require `UseV2Flow=true` and `ServerUrlV2`
- store `ephemeral=true`
- store runner id for exact cleanup

### 2. Remove orphaned native adapters

- remove `NativeActionAdapter::SetupPython` variant from `action.rs`
- remove `native_setup_python` implementation from `executor.rs`
- remove all `SetupPython` test fixtures and match arms
- remove `NativeActionAdapter::RustToolchain` variant from `action.rs`
- remove `native_rust_toolchain` implementation from `executor.rs`
- retain `NativeActionAdapter::SetupJust` because current Jackin target
  workflows use `extractions/setup-just`
- remove all `RustToolchain` test fixtures and match arms in `action.rs` and
  `runner.rs`
- remove `NativeActionAdapter::CargoInstall` variant from `action.rs`
- remove `native_cargo_install` implementation from `executor.rs`
- remove all `CargoInstall` test fixtures and match arms in `action.rs`
  and `executor.rs`
- remove orphaned reference entries from `velnor-tools/src/main.rs`
- rule: when a target workflow removes an action entirely, its Velnor native
  adapter must be removed in the same pass; no dead adapter code

### 3. Remove classic registration product code

- remove registration-token setup from normal CLI path
- remove `actions/runner-registration` from normal setup
- keep classic protocol notes only in docs as reference
- change errors to point at JIT setup only
- update `configure`/`daemon` help text to say JIT

Open naming choice:

- keep `configure` as command name but make it JIT-only
- or add explicit `jit-configure` and deprecate/remove old `configure`

### 4. Make macOS daemon host valid

- replace process-host Linux rejection with capability validation
- keep macOS/Darwin runner label rejection
- keep Linux job-container requirement
- make preflight work from macOS against Docker Desktop
- ensure path mapping instructions and errors are precise
- run Linux-only Rust tests in container where process OS matters

### 5. Implement daemon slot recycling

- one JIT runner config per slot
- after job handled, discard slot config and create next JIT config
- retry transient JIT/setup/broker failures without killing healthy slots
- exact cleanup for failed/unstarted Velnor-created runner ids
- no cleanup of unrelated GitHub runners

### 6. Update scripts to JIT semantics

- fixture smoke uses JIT-only setup
- remove-token language disappears from user-facing script errors
- readiness remains non-mutating
- smoke cleanup deletes exact Velnor-created JIT runner ids when needed
- live scripts support macOS daemon host only when Docker preflight passes

### 7. Run public fixture proof

- build `velnor/job-ubuntu:26.04`
- run readiness
- dispatch or use queued fixture run
- start Velnor daemon with JIT slots
- verify Velnor lanes pass and compare-results passes
- collect evidence markdown

### 8. Produce manual target-testing handoff

- summarize Sentry fixture evidence
- state that Velnor is ready for manual target repository testing
- provide exact commands and expected evidence for user/operator-run ChainArgos
  and Jackin validation
- request feedback with GitHub run URLs, failures, logs, artifacts, and timing
  notes
- agent does not set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`, dispatch target
  workflows, or perform target validation itself

## Resolved Questions (historical)

All questions below were resolved during Phase 0/P1; answers inline.


1. CLI naming: should `configure` remain and become JIT-only, or should Velnor
   expose a clearer `jit-configure` command?
2. macOS host support: should Velnor officially support native macOS daemon
   process from Phase 0, or mark it required before manual target testing?
3. Runner labels: should Velnor allow `ubuntu-latest`/`ubuntu-24.04` on macOS
   daemon hosts when Docker provides Linux containers?
4. ARM: should ARM label support require host CPU ARM, Docker platform ARM, or
   explicit `--docker-platform linux/arm64`?
5. Fixture: should next live proof reuse existing queued fixture run, or always
   dispatch a fresh fixture workflow after JIT lands?
6. Cleanup: should Velnor keep failed JIT runner configs for debugging behind a
   flag, or always delete them by default?
7. Artifact/cache: the earlier Phase-0 one-host artifact decision is
   superseded. Upload and download use Results Service v4
   (Create/Finalize/List/GetSignedArtifactURL) so job placement never changes
   correctness; `_velnor_artifacts/{run_id}-{attempt}/` is diagnostic/offline
   fallback only. `_velnor_caches/` remains an explicitly owned runner-local
   acceleration store under the storage contract.
