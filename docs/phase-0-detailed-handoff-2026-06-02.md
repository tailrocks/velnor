# Phase 0 Detailed Handoff

Date: 2026-06-02

This document is the current handoff for Velnor Phase 0: a Rust/Linux
self-hosted GitHub Actions runner replacement for the current workflows in:

- `jackin-project/jackin`
- `ChainArgos/java-monorepo`

Phase 0 is not a new workflow language project. Pkl, PQL, KCL, typed workflow
authoring, and Velnor-native workflow files are future brainstorming only.
Phase 0 keeps existing `.github/workflows/*.yml` files unchanged and lets
GitHub own workflow parsing, trigger matching, reusable workflow expansion,
matrix expansion, secrets, permissions, scheduling, and UI.

## Current State

The local implementation is ready for live fixture testing, but Phase 0 is not
complete. In this document, "supported" means "implemented and covered by local
target-shaped checks." It does not mean the feature has already passed through a
live GitHub run on Velnor.

Current proof state:

- local verifier gates pass for the current implementation; the last code
  commit under test was `11dc891`
- the public fixture repository exists and GitHub-hosted fixture lanes passed
- Velnor fixture lanes are still queued, waiting for a usable Velnor runner
- this workstation cannot run the fixture proof because Docker is remote and
  `/var/run/docker.sock` is not available
- the next required proof must run on a Linux host whose Docker daemon can see
  Velnor's bind-mounted work directory

Current fixture run:

- `https://github.com/donbeave/velnor-actions-fixture/actions/runs/26762850861`
- `changes`: completed successfully
- `compat-github (app-a)`: completed successfully
- `compat-github (app-b)`: completed successfully
- `compat-velnor (app-a)`: queued
- `compat-velnor (app-b)`: queued

Latest generated readiness report:

- `.velnor-live-evidence/fixture-readiness-report.md`
- generated at `2026-06-01T20:05:44Z`
- source commit recorded in report:
  `ad354fdc83bb434bb27434e56a2117fa4aab2724`
- blocker: missing local Docker socket while `DOCKER_HOST` points at
  `tcp://jk-php3ngrs-thearchitect-dind:2376`

Current local gate evidence from this audit:

- `scripts/target_verify.sh` passed with 80 focused Rust checks, target audit,
  fixture self-tests, live helper self-tests, smoke failure evidence checks, and
  workflow dispatch helper checks.
- `cargo test -q` passed with 289 unit tests and 1 integration test.
- `cargo run -q -p velnor-tools -- check-runner-reference`, through the
  verifier, reported
  `actions/runner v2.334.0` as current.

## What Is Done

### Runner Protocol

Implemented:

- GitHub self-hosted runner registration path
- runner `configure`, `status`, `remove`, `run`, `daemon`, and `preflight`
  commands
- hosted GitHub V2 broker/run-service execution path
- rejection of missing V2 settings for normal hosted GitHub runs
- broker session creation and polling
- run-service job acquire, lock renew, and complete
- job-scoped token use when GitHub provides `SystemVssConnection`
- recognized control messages:
  - broker migration
  - force token refresh
  - runner update/refresh
  - hosted shutdown
  - busy-job cancellation
  - transient poll errors
  - empty-message backoff
- non-retriable stale/unusable job acquire handling for `404`, `409`, and
  `422`

Important design point:

- Velnor's product path is one daemon that manages multiple internal runner
  slots.
- Each slot has its own GitHub runner identity/session.
- Each assigned job gets its own Docker job container.
- The bounded proof path uses `daemon --once --slots N`.

### Host And Label Constraints

Implemented:

- Linux-only execution surface
- non-Linux host rejection
- macOS/Darwin runner label rejection
- ARM Linux label guarded by actual ARM Linux host architecture
- target MVP labels:
  - `hetzner-sentry-ci`
  - `ubuntu-latest`
  - `ubuntu-24.04`
  - `ubuntu-24.04-arm` only when explicitly requested on ARM Linux

Explicit non-targets:

- macOS runner replacement
- broad hosted image parity
- broad marketplace action compatibility
- workflow language replacement

### Docker Job Execution

Implemented:

- one fresh Docker job container per acquired job
- per-job Docker network
- workspace, temp, home, actions, tools, and sccache directories
- stale container/network cleanup
- host Docker socket mount when required
- host Docker CLI mount when socket is mounted and CLI is discoverable
- host Docker Buildx plugin directory mount when socket is mounted and Buildx
  plugin directory is discoverable
- bind-mount visibility preflight before acquiring GitHub jobs
- preflight checks for:
  - host Git
  - host Docker CLI
  - Docker daemon access
  - Buildx
  - `/var/run/docker.sock` when required
  - required job image tools
  - job script execution from mounted temp/work directories

Current Docker model:

- Docker-outside-of-Docker through the host socket
- not DinD
- not rootless Docker
- not a containerized daemon per job

Security note:

- Mounting `/var/run/docker.sock` gives the job container effective control of
  the host Docker daemon. Phase 0 uses Docker containers for repeatable job
  environments and cleanup, not for strong tenant isolation.

### GitHub Job Execution

Implemented:

- bash/sh script steps
- `defaults.run.shell`
- `defaults.run.working-directory`
- job/workflow/step env handling
- command files:
  - `GITHUB_ENV`
  - `GITHUB_OUTPUT`
  - `GITHUB_PATH`
  - `GITHUB_STEP_SUMMARY`
- protected env handling:
  - `GITHUB_*`
  - `RUNNER_*`
  - `NODE_OPTIONS`
- step logs
- annotations
- telemetry
- job outputs
- environment URL
- final run-service completion payload

### Expression And Condition Subset

Implemented for target-shaped usage:

- contexts:
  - `github`
  - `runner`
  - `env`
  - `steps`
  - `needs`
  - `matrix`
  - `inputs`
  - `vars`
  - `secrets`
- status functions:
  - `always()`
  - `success()`
  - `failure()`
  - `cancelled()`
- functions:
  - `contains(...)`
  - `toJSON(...)`
  - `hashFiles(...)`
- operators:
  - equality/inequality
  - unary `!`
  - simple `&&` / `||` value expressions
- runtime step output resolution for later steps and job outputs

### Rust-Native Action Adapters

Implemented target-shaped adapters:

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

Important adapter rule:

- Velnor ignores pinned action SHAs/tags for supported native adapters.
- Matching is by normalized action family.
- Velnor does not execute TypeScript/JavaScript action code for these target
  action families.

### Composite Actions

Implemented for target-shaped usage:

- local `.github/actions/...` composite actions
- repository composite action metadata discovery
- nested repository action usage from composites
- composite run steps
- composite inputs
- composite outputs
- `GITHUB_ACTION_PATH`

Target local composites covered:

- `./.github/actions/aggregate-needs`
- `./.github/actions/check-deployed-docs`

### Cache And Artifact Transport

Implemented:

- native cache restore/save through Velnor shared workdir storage
- native upload artifact through shared workdir storage
- native download artifact through shared workdir storage
- same-host same-run handoff for target-shaped workflows

Not implemented yet:

- GitHub service-backed cache transport
- GitHub service-backed artifact transport
- multi-host cache/artifact parity

Current validation constraint:

- Phase 0 live proof should use one Velnor host with a shared workdir.

### Live Handoff Scripts

Implemented:

- `scripts/live_host_doctor.sh`
- `scripts/fixture_readiness.sh`
- `scripts/fixture_report.sh`
- `scripts/fixture_status.sh`
- `scripts/fixture_smoke.sh`
- `scripts/chainargos_target_smoke.sh`
- `scripts/chainargos_rust_target_sequence.sh`
- `scripts/jackin_target_smoke.sh`
- `scripts/jackin_rust_linux_sequence.sh`

Safety behavior:

- fixture readiness/report are non-mutating
- fixture smoke can register/remove fixture runners
- real target smoke scripts require
  `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`
- agents must not set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`
- agents must not dispatch, migrate, retarget, or register real target repos
  automatically

Evidence behavior:

- live smoke scripts write sanitized job payloads under `.velnor-job-dumps`
- live smoke scripts write markdown evidence under `.velnor-live-evidence`
- if a run fails after a GitHub run id is known, smoke scripts write
  `failed-before-completion` evidence before cleanup

## What Is Supported

### Supported Workflow Surface For Phase 0

Supported because target repositories use it:

- GitHub-expanded jobs delivered through `AgentJobRequestMessage`
- Linux runner labels used by target jobs
- workflow-level `env`
- job-level `env`
- step-level `env`
- `needs`
- job outputs
- step outputs
- reusable workflows expanded by GitHub before Velnor receives jobs
- matrix jobs expanded by GitHub before Velnor receives jobs
- bash/sh run steps
- working-directory defaults
- workflow dispatch inputs used by target smoke sequences
- required aggregator jobs through local composite actions
- Docker/Buildx/Bake workflows through host Docker socket
- cache/artifact handoff on one Velnor host

### Target Workflows In The Current Audit

ChainArgos target workflows:

- `ansible.yml`
- `rust.yml`
- `rust-docker.yml`
- `rust-docker-build.yml`
- `kestra-build-image.yml`
- `kestra-build-publish.yml`
- `renovate.yml`

Jackin target workflows:

- `ci.yml`
- `construct.yml`
- `docs.yml`
- Linux paths from `preview.yml`
- Linux paths from `release.yml`
- `renovate.yml`
- `renovate-validate.yml`

Accuracy note:

- These workflows are covered by the current target audit and native adapter
  inventory. They are not all live-proven yet.
- The first live target proof should focus on the Rust/Docker/Linux paths. The
  Renovate workflows are locally covered, but can remain a later manual target
  proof unless the user explicitly asks to run them.
- Jackin `preview.yml` and `release.yml` are Linux-only follow-up paths after
  `ci.yml`, `construct.yml`, and `docs.yml` are clean.

### Supported Fixture Surface

Public fixture repository:

- `https://github.com/donbeave/velnor-actions-fixture`

Fixture feature coverage:

- Rust workspace
- multiple packages
- matrix jobs
- `actions/checkout@v6`
- `dorny/paths-filter@v4`
- `dtolnay/rust-toolchain@stable`
- `extractions/setup-just@v4`
- `actions/cache@v5`
- `actions/upload-artifact@v7`
- `actions/download-artifact@v8`
- local composite actions
- command files:
  - `GITHUB_ENV`
  - `GITHUB_OUTPUT`
  - `GITHUB_PATH`
  - `GITHUB_STEP_SUMMARY`
- Docker Buildx setup/build workflow

Fixture accuracy note:

- `compat.yml` is the first fixture proof and already has GitHub-hosted lanes
  passing.
- The fixture Docker workflow is a separate required proof before relying on
  target Docker/Buildx workflows. It has not passed on Velnor yet because the
  current host cannot provide `/var/run/docker.sock` to job containers.

## What Is Not Supported In Phase 0

Not supported by design:

- Pkl/PQL/KCL implementation
- Velnor-native workflow language
- replacing GitHub's YAML parser
- replacing GitHub's scheduler
- macOS runner replacement
- broad GitHub Actions parity
- broad marketplace action parity
- full GitHub-hosted image parity
- TypeScript/JavaScript action execution for target native adapters
- job `container:` support for target workflows
- workflow `services:` support for target workflows
- job-level `concurrency:` support in job payload execution
- `timeout-minutes`
- explicit non-bash shells
- direct `docker://` action references
- checkout `submodules`
- checkout `sparse-checkout`
- checkout `lfs`
- GitHub service-backed cache/artifact transport
- DinD/rootless Docker mode
- multi-host artifact/cache parity

Expected audit exclusions:

- target audit still lists macOS/apple matrix signals from Jackin
- `scripts/target_verify.sh` accepts those signals because macOS is outside
  Phase 0

Potentially confusing implementation detail:

- Lower-level container structs include service-container support, but target
  workflow `services:` is still treated as unsupported Phase 0 surface because
  the selected target workflows do not require it.

## What Is Already Tested

### Local Gates

Authoritative local gates:

```sh
scripts/target_verify.sh
cargo test -q
```

Latest recorded evidence:

- `scripts/target_verify.sh` passed with 80 focused Rust checks and script
  self-tests.
- `cargo test -q` passed with 289 unit tests and 1 integration test.
- `cargo run -q -p velnor-tools -- check-runner-reference` reported
  `actions/runner v2.334.0` as current.

### Target Verifier Coverage

`scripts/target_verify.sh` checks:

- target checkout freshness for `.github` trees
- target workflow/action inventory
- target local action metadata
- target reusable workflow inventory
- triggers
- labels and `runs-on`
- matrices
- permissions
- env
- outputs
- `needs`
- conditions
- defaults
- unsupported feature drift
- cached/fetched action metadata closure
- current target expression subset
- target native action routing
- target Docker action inputs
- target cache/artifact action inputs
- target pages/deploy behavior
- target renovate behavior
- target setup/tool adapters
- daemon slot behavior
- run preflight argument preservation

Script self-tests included:

- fixture audit
- fixture readiness
- fixture report
- fixture smoke defaults
- fixture status
- live evidence helper
- live sequence helper
- smoke failure evidence
- workflow dispatch helper

### Unit/Integration Test Coverage

The Rust tests cover, among other things:

- runner V2 config validation
- daemon multi-slot config behavior
- Docker mount behavior
- Docker CLI/Buildx mount gating
- preflight Docker checks
- target action adapter mapping
- native adapters not using Node sidecars
- checkout behavior
- expression evaluation
- command file parsing
- cache restore/save
- artifact upload/download
- pages environment URL outputs
- aggregate-needs behavior
- path-filter outputs
- setup tool adapters
- runner labels and platform validation

### Fixture Status Already Proven

Already proven in GitHub UI:

- fixture workflows are accepted by GitHub
- GitHub-hosted fixture lanes pass
- fixture audit passes

Not yet proven:

- Velnor consuming queued fixture jobs
- fixture compare job passing after Velnor lanes complete
- fixture Docker workflow passing on Velnor
- target secrets and registry credentials working inside Velnor job containers
- GitHub UI parity for Velnor logs, annotations, summaries, job outputs, and
  final conclusions

## What Still Needs Testing

### What The File Previously Understated

The implementation is close to live fixture testing, but the following points
must stay explicit:

- Local tests prove target-shaped behavior, not GitHub UI parity.
- The current readiness report is blocker evidence, not a green live proof.
- The fixture `compat.yml` proof and the fixture Docker workflow proof are two
  separate gates.
- Docker socket mounting is powerful and should be treated as trusted-runner
  infrastructure, not a secure sandbox boundary.
- Cache/artifact behavior is local shared-workdir behavior. It is enough for
  same-host Phase 0 proof, but it is not GitHub service-backed cache/artifact
  parity.
- Target repository smoke scripts are manual-operator scripts. Agents must not
  set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
- Target audits rely on fresh local target checkouts. Use
  `scripts/target_verify.sh`, not only raw workflow inspection.
- GitHub's runner protocol can drift. Re-run `scripts/target_verify.sh` and its
  runner-reference check before each serious live proof attempt.

### Required Live Fixture Tests

Need to prove:

- Velnor can register as self-hosted runner for fixture repo
- GitHub schedules fixture jobs to Velnor label `velnor-target-mvp`
- Velnor daemon with multiple slots consumes queued fixture jobs
- each fixture job runs in an isolated Docker container
- command files behave like GitHub-hosted runner behavior
- cache/artifact local handoff works for fixture shape
- Docker Buildx path works from job container when fixture Docker workflow is
  tested
- fixture compare job passes
- GitHub UI logs and conclusions are readable

Required fixture order:

1. `compat.yml`: prove basic GitHub/Velnor comparison lanes and command-file,
   matrix, cache, artifact, and composite behavior.
2. Fixture Docker workflow: prove Docker socket, Docker CLI, Buildx plugin, and
   container-to-host Docker behavior before target Docker workflows.

### Required ChainArgos Tests

Need to prove live:

- `ansible.yml`
  - runner registration
  - checkout
  - setup-python
  - defaults.run working directory
  - bash script execution
- `rust.yml`
  - path filters
  - narrow `packages=` workflow-dispatch input
  - broader package selections
  - mise/toolchain setup
  - sccache behavior
  - required-job gates
  - job outputs
- `rust-docker.yml`
  - `targets=...`
  - `push=false` rehearsal
  - Docker socket from job container
  - Docker CLI/Buildx inside job container
  - Docker login with real secrets
  - Bake/Buildx behavior
  - required-job gates
- `rust-docker-build.yml`
  - reusable workflow jobs expanded and scheduled by GitHub
- `kestra-build-publish.yml`
  - GitHub-expanded reusable workflow jobs
  - Buildx image build flow
  - registry check behavior
- `renovate.yml`
  - Renovate Docker adapter path
  - target env/token behavior

### Required Jackin Tests

Need to prove live:

- `ci.yml`
  - changes job
  - check job
  - msrv job
  - build-validator job
  - required aggregate job
  - artifact upload
  - sccache soft-fail gate
- `construct.yml`
  - non-publish rehearsal path first
  - publish path later
  - direct Docker/Buildx shell commands
  - Docker login with real secrets
  - digest artifact upload/download
  - manifest rehearsal/publish flow
  - required aggregate job
- `docs.yml`
  - docs build/link checks
  - Pages artifact upload
  - Pages deploy adapter
  - environment URL output
  - sitemap output
  - check-deployed-docs local action
  - required aggregate job
- Linux paths from `preview.yml` and `release.yml` after the primary paths
  are clean

### Required GitHub UI Checks

For every live run, verify:

- run URL
- Velnor commit SHA
- runner labels shown in GitHub UI
- job assignment to Velnor
- step names
- first/last log lines
- annotations
- summaries
- outputs consumed by downstream jobs
- artifact upload/download behavior
- cache restore/save behavior
- final job conclusions
- final workflow conclusion

## How To Test

### Step 1: Pick Correct Host

Use a Linux host with:

- Git
- Rust/Cargo
- Docker CLI
- Docker daemon
- Docker Buildx plugin
- `/var/run/docker.sock`
- workdir visible to Docker daemon
- GitHub CLI `gh`
- `GITHUB_TOKEN` with permission to register repository self-hosted runners

Preferred:

- local Docker socket at `/var/run/docker.sock`

Remote Docker daemon:

- only acceptable when Velnor's workdir is visible to the daemon
- set `VELNOR_DOCKER_HOST_WORK_DIR` if daemon-visible path differs
- target Docker/Buildx jobs still need Docker access inside the job container

### Step 2: Run Local Gates

```sh
scripts/target_verify.sh
cargo test -q
```

Expected:

- both pass
- no target workflow drift
- no runner reference drift

### Step 3: Run Non-Mutating Fixture Readiness

```sh
scripts/fixture_readiness.sh
```

Expected on valid host:

- fixture status prints current GitHub run state
- fixture audit passes
- live host doctor passes
- no runner is registered
- no workflow is dispatched

If it fails:

```sh
scripts/fixture_report.sh
```

Then inspect:

```sh
.velnor-live-evidence/fixture-readiness-report.md
```

### Step 4: Run Public Fixture Compat Smoke

On a host that passed readiness:

```sh
scripts/fixture_smoke.sh
```

Optional fresh full gate before registration:

```sh
VELNOR_RUN_TARGET_VERIFY=true scripts/fixture_smoke.sh
```

Useful options:

```sh
VELNOR_FIXTURE_RUN_ID=<run-id> scripts/fixture_smoke.sh
VELNOR_FIXTURE_DISPATCH=false VELNOR_FIXTURE_RUN_ID=<run-id> scripts/fixture_smoke.sh
VELNOR_FIXTURE_JOB_COUNT=2 scripts/fixture_smoke.sh
VELNOR_FIXTURE_CLEANUP_RUNNER=false scripts/fixture_smoke.sh
```

After smoke:

```sh
scripts/fixture_status.sh
ls -1 .velnor-live-evidence
find .velnor-job-dumps -type f | sort
```

Expected:

- Velnor runner registers for fixture repository
- Velnor jobs are consumed
- fixture compare job passes
- evidence markdown is written
- job dumps are sanitized

### Step 5: Run Public Fixture Docker Smoke

After `compat.yml` is green, run the fixture Docker workflow on the same
Docker-visible Linux host. The exact workflow name and job count should be
confirmed from the fixture workflow before dispatch.

Expected:

- Docker socket is visible inside the Velnor job container
- Docker CLI works inside the Velnor job container
- Docker Buildx works inside the Velnor job container
- Docker build output matches the GitHub-hosted fixture lane expectation
- GitHub UI logs and conclusions remain readable

Do not treat target Docker/Buildx workflows as ready until this fixture Docker
proof is green.

### Step 6: Stop Agent-Owned Execution

After fixture compat and Docker proofs are green:

- report readiness for manual target validation
- do not register Velnor against ChainArgos or Jackin automatically
- do not dispatch real target workflows automatically
- do not set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`

### Step 7: Manual ChainArgos Sequence

User/operator runs:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true scripts/chainargos_rust_target_sequence.sh
```

Suggested staged approach:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_TARGET_WORKFLOW=ansible.yml \
VELNOR_TARGET_JOB_COUNT=1 \
scripts/chainargos_target_smoke.sh
```

Then:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_CHAINARGOS_RUST_PACKAGES=bitcoin-processor-app \
VELNOR_CHAINARGOS_RUST_JOB_COUNT=4 \
scripts/chainargos_rust_target_sequence.sh
```

For Docker rehearsal:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_CHAINARGOS_DOCKER_TARGETS=bitcoin-processor-app \
VELNOR_CHAINARGOS_DOCKER_PUSH=false \
scripts/chainargos_rust_target_sequence.sh
```

### Step 8: Manual Jackin Sequence

User/operator runs:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true scripts/jackin_rust_linux_sequence.sh
```

Suggested staged approach:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_TARGET_WORKFLOW=ci.yml \
VELNOR_TARGET_JOB_COUNT=5 \
scripts/jackin_target_smoke.sh
```

Then:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_TARGET_WORKFLOW=construct.yml \
VELNOR_TARGET_JOB_COUNT=5 \
scripts/jackin_target_smoke.sh
```

Then:

```sh
VELNOR_REAL_TARGET_MANUAL_CONFIRM=true \
VELNOR_TARGET_WORKFLOW=docs.yml \
VELNOR_TARGET_JOB_COUNT=5 \
scripts/jackin_target_smoke.sh
```

### Step 9: Fix Only Evidence-Backed Failures

When live failures happen, use:

- `.velnor-live-evidence/*.md`
- `.velnor-job-dumps/**`
- GitHub run URL
- GitHub step logs
- target audit drift output

Do not guess from workflow YAML alone. GitHub sends Velnor expanded job
payloads, so live job dumps are authoritative for runner behavior.

## Current Blocker

This environment cannot finish the live proof.

Observed:

- `DOCKER_HOST=tcp://jk-php3ngrs-thearchitect-dind:2376`
- `/var/run/docker.sock` missing
- Docker daemon cannot see Velnor workdir at the local path
- `scripts/fixture_readiness.sh` fails at Docker preflight

Required external change:

- run on a Linux host with local `/var/run/docker.sock`, or
- use a remote Docker daemon where Velnor workdir is mounted and visible to the
  daemon, with `VELNOR_DOCKER_HOST_WORK_DIR` set correctly

Until that is fixed, Velnor should not acquire queued fixture or target jobs
because Docker/Buildx workflows would fail inside job containers.

## Next Plan

1. Move to a Docker-visible Linux host.
2. Run:

   ```sh
   scripts/fixture_readiness.sh
   ```

3. If readiness fails, run:

   ```sh
   scripts/fixture_report.sh
   ```

   Fix host readiness before continuing.

4. If readiness passes, run fixture compat smoke:

   ```sh
   scripts/fixture_smoke.sh
   ```

5. Verify fixture compat evidence:

   ```sh
   scripts/fixture_status.sh
   ls -1 .velnor-live-evidence
   ```

6. Run fixture Docker smoke on the same Docker-visible host.
7. If either fixture proof fails, fix only evidence-backed issues.
8. If both fixture proofs pass, report ready for manual ChainArgos validation.
9. User/operator manually runs ChainArgos sequence with
   `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
10. Fix only evidence-backed target failures.
11. User/operator manually runs Jackin sequence with
    `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
12. If both target sequences pass in GitHub UI, record final evidence and then
    Phase 0 can be considered proven for these repositories.

## Completion Criteria

Phase 0 is complete only when all of the following are true:

- public fixture Velnor lanes pass
- fixture compare job passes
- fixture Docker workflow passes on Velnor
- ChainArgos selected Rust/Docker workflows pass on Velnor in GitHub UI
- Jackin selected Linux workflows pass on Velnor in GitHub UI
- logs are readable in GitHub UI
- step/job conclusions are correct
- job outputs are correct
- required aggregate jobs behave correctly
- cache/artifact behavior needed by target jobs works
- no unsupported target feature drift appears in `scripts/target_verify.sh`
- evidence markdown and sanitized job dumps are captured for the final runs

Local tests alone are not completion proof.
