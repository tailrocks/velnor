# Velnor Roadmap

Status: draft for user review.

This document is the implementation roadmap for Velnor. It lists what must be
implemented, what is still missing, and how work is verified.

The high-level product vision lives in [vision.md](vision.md). This roadmap is
the source of truth for current implementation work.

Older Phase 0 notes, handoffs, and research snapshots live under
`docs/archive`. They are useful history, but this roadmap is the source of truth
when archived docs disagree with current product direction.

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

Hard rule: Velnor must first be a drop-in GitHub Actions runner replacement for
Rust repositories. Both first target projects are Rust projects for Phase 0
purposes. `ChainArgos/java-monorepo` has "java" in the repository name, but the
target workflow surface Velnor must support is Rust, Docker, Buildx, cache,
artifact, Renovate, and related Rust CI/CD behavior.

## Target Project Analysis

The target repositories are current at:

- `jackin-project/jackin`: `52a457b689940e05ed65015d2a82ff0e22577d2e`
- `ChainArgos/java-monorepo`: `56491ec5b17702186506217452b58bcf57572079`

### Shared Rust CI Requirements

Velnor must support the common Rust GitHub Actions shape used by both projects:

- checkout current repository and selected external repositories
- install Rust toolchains and cargo tools
- run `cargo fmt`, `cargo clippy`, `cargo check`, `cargo test`, and
  `cargo nextest`
- support `rustup component add` and target installation
- support `mise` as the main tool installer for Jackin
- support `dtolnay/rust-toolchain`, `setup-just`, `cargo-install`, and
  `Swatinem/rust-cache` for ChainArgos shapes
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

- `ansible.yml`: checkout, setup-python, bash defaults, and basic command
  execution
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
- `actions/setup-python`
- `dorny/paths-filter`
- `jdx/mise-action`
- `dtolnay/rust-toolchain`
- `extractions/setup-just`
- `baptiste0928/cargo-install`
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

Current implementation gap:

- `velnor-runner configure`, `run`, and `preflight` currently reject non-Linux
  process hosts.
- That guard must be changed. Host OS validation should move from "daemon
  process must be Linux" to "claimed runner labels and job container OS must be
  Linux-compatible".

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

Current implementation gap:

- code still contains classic registration-token setup and remove flow.
- daemon slot setup still calls the classic `configure` path.
- JIT decode and JIT slot recycling are not implemented yet.

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
- default job image is project-owned `velnor/job-ubuntu:24.04`, built from
  official `ubuntu:24.04`
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
cargo test -q
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
3. ChainArgos target proof, user/operator owned
4. Jackin Linux target proof, user/operator owned

Completion requires GitHub Actions UI evidence:

- jobs assigned to Velnor
- readable logs
- correct step conclusions
- correct job conclusions
- command files and outputs work
- artifacts/cache behavior works for target jobs
- Docker/Buildx paths work
- no unsupported feature drift in target workflow scans

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

### 2. Remove classic registration product code

- remove registration-token setup from normal CLI path
- remove `actions/runner-registration` from normal setup
- keep classic protocol notes only in docs as reference
- change errors to point at JIT setup only
- update `configure`/`daemon` help text to say JIT

Open naming choice:

- keep `configure` as command name but make it JIT-only
- or add explicit `jit-configure` and deprecate/remove old `configure`

### 3. Make macOS daemon host valid

- replace process-host Linux rejection with capability validation
- keep macOS/Darwin runner label rejection
- keep Linux job-container requirement
- make preflight work from macOS against Docker Desktop
- ensure path mapping instructions and errors are precise
- run Linux-only Rust tests in container where process OS matters

### 4. Implement daemon slot recycling

- one JIT runner config per slot
- after job handled, discard slot config and create next JIT config
- retry transient JIT/setup/broker failures without killing healthy slots
- exact cleanup for failed/unstarted Velnor-created runner ids
- no cleanup of unrelated GitHub runners

### 5. Update scripts to JIT semantics

- fixture smoke uses JIT-only setup
- remove-token language disappears from user-facing script errors
- readiness remains non-mutating
- smoke cleanup deletes exact Velnor-created JIT runner ids when needed
- live scripts support macOS daemon host only when Docker preflight passes

### 6. Run public fixture proof

- build `velnor/job-ubuntu:24.04`
- run readiness
- dispatch or use queued fixture run
- start Velnor daemon with JIT slots
- verify Velnor lanes pass and compare-results passes
- collect evidence markdown

### 7. Run target proofs with operator control

- user/operator runs ChainArgos sequence after fixture green
- user/operator runs Jackin Linux sequence after ChainArgos green
- agent does not set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`

## Open Questions For User

1. CLI naming: should `configure` remain and become JIT-only, or should Velnor
   expose a clearer `jit-configure` command?
2. macOS host support: should Velnor officially support native macOS daemon
   process from Phase 0, or mark it required before target proof?
3. Runner labels: should Velnor allow `ubuntu-latest`/`ubuntu-24.04` on macOS
   daemon hosts when Docker provides Linux containers?
4. ARM: should ARM label support require host CPU ARM, Docker platform ARM, or
   explicit `--docker-platform linux/arm64`?
5. Fixture: should next live proof reuse existing queued fixture run, or always
   dispatch a fresh fixture workflow after JIT lands?
6. Cleanup: should Velnor keep failed JIT runner configs for debugging behind a
   flag, or always delete them by default?
7. Artifact/cache: is one-host shared-workdir parity enough for Phase 0 target
   proof, or must GitHub service-backed artifact/cache transport land first?
