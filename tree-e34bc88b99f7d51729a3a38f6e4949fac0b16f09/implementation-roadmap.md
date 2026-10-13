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
- message long-poll loop: `run --once` exits after one poll/job; default `run` keeps polling the existing GitHub message route in one session
- log raw message metadata for assigned job
- private GitHub runner protocol dependency accepted as a Phase 0 implementation cost

Exit criteria:

- Velnor registers in a disposable GitHub repo.
- A workflow with `runs-on: velnor` is assigned to Velnor.
- Velnor receives a real job message.

## Milestone 1: Minimal Job Completion

Goal: receive one job and mark it completed.

Deliverables:

- deserialize enough `AgentJobRequestMessage`
- renew job request lock: client method implemented for classic `jobrequests` route; script execution starts with an initial renewal and keeps a background renewal loop alive while user code runs
- report timeline/log output for a no-op job: opt-in `run --complete-noop` probe is implemented for classic timeline/feed routes
- finish job request with success/failure: opt-in `run --complete-noop` probe finishes success
- handle cancellation message: classic `JobCancellation` messages are recognized while a Docker job is running, acknowledged, and used to kill the active job container so the job can finish as canceled; V2 broker cancellation messages are also polled while the Docker job runs and kill the active job container
- V2 broker migration messages are recognized while a Docker job is running and update the broker base URL for later polls

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
- basic service containers from GitHub container resources start on the same Docker network with their GitHub alias as a network alias; Velnor waits for Docker health/running status before starting the job container
- service container environment variables from GitHub container resources are passed to `docker run`
- service container port mappings from GitHub container resources are passed to Docker with `-p`
- job and service container `options`/`createOptions` are split and passed through to Docker `run`
- explicit GitHub job `container:` images are used when present; otherwise Velnor falls back to the CLI/default Docker image
- job container environment variables from GitHub's job container payload are passed to `docker run`
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
- default Docker job image is `ghcr.io/catthehacker/ubuntu:act-latest`, giving target workflows a hosted-runner-like Ubuntu base with common CI tools; `--docker-image` can still override it
- Docker execution uses a shared `/github/home` mount and `HOME=/github/home` across the job, JavaScript action sidecar, and Docker action containers so setup actions can install tools into a home directory visible to later script steps
- JavaScript actions run in a short-lived sidecar image selected from `runs.using`; `--node-action-image` can override the image when an operator wants a custom Node/tooling bundle
- on Linux hosts, Velnor mounts the host Docker CLI and Buildx plugin directory into the job container, JavaScript action sidecars, and Docker action containers when it also mounts `/var/run/docker.sock`; target Docker actions such as `docker/setup-buildx-action`, `docker/login-action`, `docker/build-push-action`, and `docker/bake-action` need this client tooling when they talk to the mounted Docker socket
- script-step plan writes the script under runner temp, exposes per-step command-file env vars, and collects output/env/path/state/summary files after execution
- Docker script executor runs the planned lifecycle through an abstract command runner: create network, start container, exec script, collect state, cleanup container/network
- if Docker startup fails because a previous run left the same job container/network names behind, Velnor removes stale job resources once and retries startup before failing the job
- enabled GitHub script steps can be mapped into internal `ScriptStep` plans for `bash` and `sh`, including `script`, `shell`, `workingDirectory`, and job `defaults.run` shell/working-directory
- `velnor-runner run` executes supported jobs by default in one Docker job container plus JavaScript/Docker action sidecars and finishes success/failure; `--complete-noop` remains available for completion probes and `--dry-run-jobs` leaves received jobs unacknowledged for inspection
- env written to `GITHUB_ENV` and paths written to `GITHUB_PATH` are propagated to later script steps
- `GITHUB_ENV` and legacy `set-env` mutations cannot override `GITHUB_*` and `RUNNER_*` defaults; `ACTIONS_*` remains mutable for target runtime-export actions
- paths written to `GITHUB_PATH` are also propagated to later JavaScript action sidecars via a shell PATH prelude, preserving the sidecar image's default PATH while making shared-home tools such as Rust/Cargo shims visible
- step outputs written to `GITHUB_OUTPUT` are tracked by step id and basic `${{ steps.<id>.outputs.<name> }}` expressions are resolved in later scripts and JavaScript action env
- job outputs from the job message are evaluated at the end of execution from final step output state, matching the runner-side evaluation point used by GitHub
- evaluated job outputs are sent in a classic `JobCompleted` plan event before the agent request is finished
- stdout/stderr from executed script and JavaScript steps is captured and uploaded to GitHub timeline task records after execution, with GitHub/job/add-mask masks applied
- stdout workflow commands are parsed for legacy/state-changing commands: `set-output`, `set-env`, `add-path`, and `save-state`
- basic `${{ github.* }}` and `${{ runner.* }}` context expressions are resolved from runtime env in later scripts and JavaScript action env
- basic step `if` evaluation is implemented for output comparisons, step outcome checks, `runner.os`, `github.event_name`, `github.ref`, status functions, and simple `&&`/`||`
- native `actions/checkout` support is wired before Docker execution through host `git init/fetch/checkout`; self and explicit `repository:` checkouts are supported, including target `path`, `ref`, `token`, and `fetch-depth` inputs

Exit criteria:

- workflow with self checkout plus shell steps works.
- step output flows to later step.
- env written to `GITHUB_ENV` flows to later step.
- non-zero exit code fails step/job.

## Milestone 3: Checkout And JavaScript Actions

Goal: support common JavaScript actions.

Deliverables:

- broader `actions/checkout` compatibility: `repository`, `path`, `ref`, `token`, `fetch-depth`, `fetch-tags`, `persist-credentials`, and `clean` inputs are implemented for target shapes; `fetch-depth: 0` fetches full branch/tag refs for target path-filter jobs; checked-out worktrees are added to the mounted-home `safe.directory` config for Docker-side Git commands; persisted git extraheaders are removed after the job; sparse checkout, submodules, and LFS remain open
- action resolver/downloader for `owner/repo@ref`: repository action download into `_actions` and metadata discovery are implemented
- action metadata parser for `action.yml`: JavaScript, composite, and Docker `runs.using` shapes are modeled
- repository action planner for enabled non-checkout `uses:` steps
- Node action handler: JavaScript action invocation and Docker `node <main>` executor shape are implemented and wired into the ordered Docker execution path
- JavaScript actions run in a short-lived Node side container selected from `runs.using` with the same workspace/temp/actions/tools mounts and job network, so job images do not need to provide Node; `--node-action-image` can override this when an operator supplies a custom image that already contains the required Node runtime and tools such as Docker CLI
- JavaScript post hooks: `runs.post` is resolved, respects target `runs.post-if` conditions such as `success()`, executes in reverse order after main steps, and receives `STATE_*` values saved through `GITHUB_STATE`
- Docker action handler: `runs.using: docker` actions are planned and run as short-lived Docker containers on the job network; `docker://` images are used directly, and local Dockerfile actions are built before execution
- `INPUT_*` environment variables: planned action inputs and metadata defaults are converted to `INPUT_*` for JavaScript invocation
- step `env:` from GitHub job messages is parsed for script and JavaScript action steps, with basic expression resolution at execution time
- workflow/job `env:` from GitHub job messages is overlaid into the runtime environment; protected `GITHUB_*`/`RUNNER_*` defaults are not overwritten
- runtime env:
  - basic `GITHUB_*` variables are extracted from the job message and injected into script and JavaScript steps, including `GITHUB_ACTIONS=true`
  - target `GITHUB_WORKFLOW` and `GITHUB_REF_NAME` are injected; `GITHUB_REF_NAME` is derived from `GITHUB_REF` when GitHub does not send it directly
  - target GitHub context/env values such as `GITHUB_REPOSITORY_OWNER`, ref metadata, workflow metadata, run attempt, retention days, and server/API URLs are injected when present or derived where safe
  - `github.event` from job `ContextData` is written to `/__t/_github_workflow/event.json` and exposed through `GITHUB_EVENT_PATH`
  - repository JavaScript actions receive per-action `GITHUB_ACTION`, `GITHUB_ACTION_PATH`, `GITHUB_ACTION_REPOSITORY`, and `GITHUB_ACTION_REF`
  - all executable steps receive step-scoped `GITHUB_ACTION` before condition, env, and script expression rendering
  - basic `RUNNER_*` variables are injected for the Docker runner environment, including GitHub-style `RUNNER_ARCH` values such as `X64`/`ARM64`, `RUNNER_NAME`, `RUNNER_WORKSPACE`, `RUNNER_ENVIRONMENT=self-hosted`, `RUNNER_TOOL_CACHE=/__tool`, matching `AGENT_TOOLSDIRECTORY=/__tool` for toolcache actions, and `RUNNER_DEBUG=1` when `ACTIONS_STEP_DEBUG=true`
  - action runtime values from `SystemVssConnection` are injected: `ACTIONS_RUNTIME_URL`, `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_CACHE_URL`, `ACTIONS_RESULTS_URL`, OIDC request URL/token, cache service v2, and orchestration id when GitHub sends them
  - broader GitHub runner env parity remains incomplete
- `secrets.*` expressions are resolved from GitHub secret job variables, including `secrets.GITHUB_TOKEN` from `system.github.token`; GitHub mask hints and secret variables are applied to runner-uploaded feed lines, and `::add-mask::` workflow commands are tracked for later step log masking
- basic workflow command parsing: state-changing stdout commands are parsed; `error`, `warning`, and `notice` commands are counted on timeline records and title/location plus group/debug markers are preserved in uploaded step logs; native GitHub UI group folding is not yet reported
- runner-side expression contexts: `env.*`, selected `github.*`/`runner.*` including `github.workflow` and `github.ref_name`, nested `github.event.*` from `ContextData`, `steps.*.outputs.*`, `secrets.*`, and generic job `ContextData` lookup such as `matrix.*`, `needs.*`, `inputs.*`, and `vars.*` are supported for script/action env and basic step conditions; `&&`/`||` value expressions, equality checks, unary `!`, `contains()`, `toJSON()`, and workspace-backed `hashFiles(...)` cover target-shaped cases
- `continue-on-error`: script and JavaScript action steps can ignore a nonzero exit for job failure while preserving `steps.<id>.outcome == 'failure'`, covering target optional `sccache` setup
- ordered job execution for script steps and JavaScript actions in original message order is wired, with JavaScript post hooks executed in reverse order; host-side checkout still runs before the Docker container starts

Exit criteria:

- `actions/checkout` works.
- `jdx/mise-action` or `actions/setup-python` works.
- logs and annotations remain readable.

## Milestone 4: Composite Actions

Goal: support local composite actions used by target repos.

Deliverables:

- parse `runs.using: composite`: metadata parsing exists
- local composite action discovery after checkout: implemented for `./.github/actions/...`
- composite inputs: basic `${{ inputs.* }}` interpolation into `run`, `env`, and nested `with` exists, including metadata defaults; top-level local composite `with:` values are resolved against job `ContextData` first, including target `toJSON(needs)` aggregator inputs
- nested `run` steps: local composite `run` steps are expanded into ordered script steps
- nested `uses` steps: implemented for repository JavaScript actions referenced from local composites
- composite step conditions: `if:` is parsed, rendered with composite inputs, and propagated to expanded `run` and nested repository JavaScript steps
- repository composite actions: downloaded marketplace composites now expand into ordered script steps; nested repository actions inside those composites are discovered and downloaded recursively
- composite run steps receive GitHub-style `GITHUB_ACTION_PATH` pointing at the parent composite action directory, so scripts using `$GITHUB_ACTION_PATH` match runner behavior
- composite `continue-on-error`: parsed for nested composite steps and applied to expanded script/repository steps
- composite outputs: metadata `outputs.*.value` is evaluated after expanded inner steps and exposed as `steps.<composite-id>.outputs.*`
- post-step state if needed

Current limit: `jackin`'s `aggregate-needs` shape is covered by the run-only subset, including `needs-json: ${{ toJSON(needs) }}` input rendering. `check-deployed-docs` can now plan its nested `jdx/mise-action` repository action. Marketplace composites such as `dtolnay/rust-toolchain`, `rui314/setup-mold`, `actions/upload-pages-artifact`, and `extractions/setup-just` can be expanded far enough to run scripts or route nested repository actions, with composite outputs materialized for later steps. Nested local actions and full GitHub expression parity remain open.

Exit criteria:

- `jackin` local `.github/actions/aggregate-needs` works.
- `jackin` local `.github/actions/check-deployed-docs` starts and can run its shell steps.

## Milestone 5: Artifacts And Cache

Goal: support target artifact/cache workflows through existing GitHub actions.

Deliverables:

- pass runtime endpoints/tokens correctly
- ensure mounted paths match action expectations
- resolve target cache keys that use `${{ hashFiles(...) }}` against the checked-out workspace
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

- job container, JavaScript action sidecars, and Docker action containers mount the host Docker CLI/Buildx plugin on Linux when available
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

Fresh target action inventory from the target `.github` trees on 2026-06-01:

- high-frequency: `actions/checkout` 46, `jdx/mise-action` 13, `actions/cache` 13, `mozilla-actions/sccache-action` 7, `actions/upload-artifact` 6, `dorny/paths-filter` 5, `rui314/setup-mold` 5, `docker/setup-buildx-action` 5, `docker/login-action` 5
- important lower-frequency: `actions/download-artifact` 3, `crazy-max/ghaction-github-runtime` 2, local `aggregate-needs` 3, local `check-deployed-docs` 2
- release/pages/docker-specific: `actions/upload-pages-artifact`, `actions/deploy-pages`, `docker/metadata-action`, `docker/build-push-action`, `docker/bake-action`
- reusable workflow job references exist in `java-monorepo` (`./.github/workflows/kestra-build-image.yml`), but GitHub expands those before a runner receives individual job messages, so Phase 0 should not parse workflow-level `jobs.<id>.uses`

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
