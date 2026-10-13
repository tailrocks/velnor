# Implementation Roadmap

This roadmap is ordered for the real first goal: run existing GitHub Actions YAML on a Rust Velnor self-hosted runner replacement.

Detailed Phase 0 implementation research lives in `docs/research/phase-0-implementation-plan.md`.

## Milestone 0: Protocol Spike

Goal: prove Velnor can appear as a GitHub self-hosted runner.

Deliverables:

- `velnor-runner configure --url ... --token ... --labels ...`
- local settings/credential store
- repo-level runner registration
- runner appears online in GitHub UI
- session create/delete
- message long-poll loop
- log raw message metadata for assigned job

Exit criteria:

- Velnor registers in a disposable GitHub repo.
- A workflow with `runs-on: velnor` is assigned to Velnor.
- Velnor receives a real job message.

## Milestone 1: Minimal Job Completion

Goal: receive one job and mark it completed.

Deliverables:

- deserialize enough `AgentJobRequestMessage`
- renew job request lock: client method implemented for classic `jobrequests` route
- report timeline/log output for a no-op job: opt-in `run --complete-noop` probe is implemented for classic timeline/feed routes
- finish job request with success/failure: opt-in `run --complete-noop` probe finishes success
- handle cancellation message

Exit criteria:

- GitHub Actions UI shows job running on Velnor.
- Job completes successfully even if no user steps run.
- Failure path marks job failed.

## Milestone 2: Script Steps In Docker

Goal: run `run:` steps inside Docker isolation.

Deliverables:

- per-job workspace/temp/actions/tools directories
- per-job Docker network
- long-running job container
- `docker exec` for each script step
- shell resolution for `bash` and `sh`
- working directory support
- workflow/job/step env support
- command file support:
  - `GITHUB_OUTPUT`
  - `GITHUB_ENV`
  - `GITHUB_PATH`
  - `GITHUB_STATE`
  - `GITHUB_STEP_SUMMARY`

Current code progress:

- runner can parse the outer `AgentJobRequestMessage` subset from GitHub job messages: plan/timeline ids, request/job ids, variables, endpoints, repositories, containers, and action steps
- command-file parser supports `NAME=value` and heredoc `NAME<<EOF` syntax
- Docker job container command builder covers network/container start, `docker exec`, and cleanup command shapes
- script-step plan writes the script under runner temp, exposes per-step command-file env vars, and collects output/env/path/state/summary files after execution
- Docker script executor runs the planned lifecycle through an abstract command runner: create network, start container, exec script, collect state, cleanup container/network
- enabled GitHub script steps can be mapped into internal `ScriptStep` plans for `bash` and `sh`, including `script`, `shell`, and `workingDirectory` inputs
- opt-in `run --execute-scripts` can execute script-only jobs in one Docker job container and finish success/failure; enabled non-script actions are refused instead of being faked
- env written to `GITHUB_ENV` and paths written to `GITHUB_PATH` are propagated to later script steps; step outputs are parsed but not yet expression-resolved into later scripts
- native self-repository `actions/checkout` support is wired before Docker execution through host `git init/fetch/checkout`; other repository actions remain unsupported

Exit criteria:

- workflow with self checkout plus shell steps works.
- step output flows to later step.
- env written to `GITHUB_ENV` flows to later step.
- non-zero exit code fails step/job.

## Milestone 3: Checkout And JavaScript Actions

Goal: support common JavaScript actions.

Deliverables:

- broader `actions/checkout` compatibility: repository input, sparse checkout, submodules, LFS, credentials cleanup
- action resolver/downloader for `owner/repo@ref`
- action metadata parser for `action.yml`: JavaScript, composite, and Docker `runs.using` shapes are modeled
- repository action planner for enabled non-checkout `uses:` steps
- Node action handler
- `INPUT_*` environment variables
- runtime env:
  - `GITHUB_*`
  - `RUNNER_*`
  - `ACTIONS_RUNTIME_URL`
  - `ACTIONS_RUNTIME_TOKEN`
  - `ACTIONS_CACHE_URL`
  - `ACTIONS_RESULTS_URL`
- secret masking
- basic workflow command parsing

Exit criteria:

- `actions/checkout` works.
- `jdx/mise-action` or `actions/setup-python` works.
- logs and annotations remain readable.

## Milestone 4: Composite Actions

Goal: support local composite actions used by target repos.

Deliverables:

- parse `runs.using: composite`
- composite inputs
- nested `run` and `uses` steps
- composite outputs
- composite step conditions
- post-step state if needed

Exit criteria:

- `jackin` local `.github/actions/aggregate-needs` works.
- `jackin` local `.github/actions/check-deployed-docs` starts and can run its shell steps.

## Milestone 5: Artifacts And Cache

Goal: support target artifact/cache workflows through existing GitHub actions.

Deliverables:

- pass runtime endpoints/tokens correctly
- ensure mounted paths match action expectations
- support cache action env and command behavior
- support upload/download artifact actions

Exit criteria:

- `actions/cache`
- `actions/upload-artifact`
- `actions/download-artifact`
- `Swatinem/rust-cache`

all work on a disposable repo workflow.

## Milestone 6: Docker Build Workflows

Goal: run Docker-heavy target workflows.

Deliverables:

- job container image includes Docker CLI or mounts host Docker CLI
- mount `/var/run/docker.sock`
- support Buildx/Bake actions
- preserve cache env and workspace paths
- Docker Hub login works

Exit criteria:

- `docker/setup-buildx-action` works.
- `docker/login-action` works.
- `docker/bake-action` works.
- direct `docker buildx` shell commands work.

## Milestone 7: Target Repo Subset

Goal: run the current workflows from target repos with minimal/no YAML changes.

Initial targets:

- `ChainArgos/java-monorepo/.github/workflows/ansible.yml`
- `ChainArgos/java-monorepo/.github/workflows/rust.yml`
- `ChainArgos/java-monorepo/.github/workflows/rust-docker.yml`
- `ChainArgos/java-monorepo/.github/workflows/kestra-build-publish.yml`
- selected Linux-only `jackin-project/jackin` workflows

Deferred:

- macOS `jackin` builds
- full GitHub Pages parity if OIDC/pages internals block first pass
- every marketplace action outside target inventory

Exit criteria:

- target workflows complete on Velnor in GitHub UI.
- branch protection can point at Velnor-backed checks.

## Milestone 8: Pkl Authoring Layer

Goal: introduce typed workflow authoring after runner compatibility.

Deliverables:

- `velnor.workflow` Pkl package
- compile Pkl to GitHub Actions YAML or Velnor native plan
- strict validation/lints
- migration examples from target YAML

Exit criteria:

- one target workflow can be expressed in Pkl and produce equivalent GitHub YAML or Velnor plan.
