# Implementation Roadmap

This roadmap is ordered for the real first goal: run existing GitHub Actions YAML on a Rust Velnor self-hosted runner replacement.

Detailed Phase 0 implementation research lives in `docs/research/phase-0-implementation-plan.md`.
The current upstream source audit is `docs/research/actions-runner-source-audit-2026-06-01.md`.
The latest runner V2 refresh is `docs/research/latest-runner-v2-refresh-2026-06-01.md`; use `cargo run -q -p velnor-tools -- check-runner-reference` before live validation to detect upstream `actions/runner` release drift.
The runner job-message contract is `docs/research/github-runner-job-message-contract-2026-06-01.md`; Velnor receives expanded `AgentJobRequestMessage` payloads, not raw workflow YAML, and the normal hosted-GitHub path targets broker/run-service V2 only.
The normalized plan boundary for GitHub job messages is `docs/research/normalized-plan-contract-2026-06-01.md`.
The implementation-oriented runner blueprint is `docs/research/self-hosted-runner-implementation-blueprint-2026-06-01.md`.
The implementation gap audit is `docs/research/phase-0-implementation-gap-audit-2026-06-01.md`.
The current implementation/proof status is `docs/research/phase-0-current-status-2026-06-01.md`.
The target-only MVP contract is `docs/research/target-mvp-compat-audit-2026-06-01.md`; Phase 0 work should be judged against those two repositories' current `.github` trees, not broad GitHub Actions parity.
The native action adapter boundary is `docs/native-action-adapter-contract.md`;
Velnor keeps GitHub-compatible YAML, but supported marketplace actions should
resolve to Rust-native adapters instead of executing their JavaScript or
TypeScript bundles.

## Milestone 0: Protocol Spike

Goal: prove Velnor can appear as a GitHub self-hosted runner.

Deliverables:

- `velnor-runner configure --url ... --token ... --labels ...`
- `velnor-runner configure --target-mvp-labels` opt-in adds the current target x64 Linux labels: `hetzner-sentry-ci`, `ubuntu-latest`, and `ubuntu-24.04`; it never claims macOS labels, rejects macOS/Darwin labels, and claims ARM only when `--target-mvp-arm-label` is also passed on an ARM Linux host
- `velnor-runner configure`, `run`, and `preflight` reject non-Linux hosts because the Phase 0 runner execution model is Linux-only
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

## Milestone 0.5: Daemon Slot Scheduler

Goal: make Velnor different from the stock runner by presenting one daemon to
the operator while managing multiple GitHub runner slots internally.

Design:

- the daemon owns a configured concurrency limit, for example `--slots <n>`
- after initial configuration, the user-facing operation should be one command:
  run the daemon binary, connect to GitHub, and let it keep accepting work until
  stopped
- each slot has its own GitHub runner identity, broker session, work directory,
  temp directory, cancellation path, and job lock-renewal loop
- the daemon owns slot registration/refresh as product behavior; manually
  preconfiguring slot directories is acceptable only as an intermediate
  implementation step
- the daemon supervises all slots and restarts unhealthy idle slots without
  stopping active jobs in other slots
- each assigned job starts its own isolated Docker job container and per-job
  Docker network
- cache/artifact handoff for Phase 0 still assumes a shared daemon workdir on
  one host, but the scheduler must not share mutable per-job temp state between
  concurrent containers
- live proof scripts should use bounded daemon mode (`daemon --once --slots N`)
  so implementation evidence follows the same model as the product runtime

Exit criteria:

- one `velnor-runner daemon --slots 2` invocation registers or refreshes two
  internal runner slots for a repository.
- two queued GitHub jobs can be acquired by the same Velnor daemon at the same
  time.
- each job runs in a distinct Docker container and reports independent logs,
  cancellation, and final status.

## Validation Ownership Rule

The implementation target is still eventual migration readiness for
`ChainArgos/java-monorepo` and `jackin-project/jackin`, but Velnor development
must not automatically migrate, edit, or retarget those repositories. The agent
should implement Velnor, run local gates, run the public fixture proof, collect
evidence, and then report that the target repositories are ready for manual
operator testing. The user owns the final manual target-repository tests and
will report findings back into the implementation loop. Scripts that operate on
the real target repositories must require an explicit operator confirmation,
currently `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`, before runner registration
or workflow dispatch.

That confirmation is for a human operator only. An agent must not set
`VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`, register Velnor against
`ChainArgos/java-monorepo` or `jackin-project/jackin`, dispatch those workflows,
or perform the repository migration by itself. The correct agent stopping point
is: local gates pass, fixture audit/readiness pass, the fixture smoke proof has
matching GitHub-hosted and Velnor behavior, and the agent reports that manual
target validation is ready.

Before asking for manual target-repository validation, prove equivalent behavior
in the public fixture repository. The fixture must emulate the feature classes
used by both target repositories and compare GitHub-hosted runner behavior
against Velnor behavior, so the remaining target tests are confirmation on the
real projects rather than first discovery of basic CI/CD behavior.

## Milestone 1: Minimal Job Completion

Goal: receive one job and mark it completed.

Deliverables:

- deserialize enough `AgentJobRequestMessage`
- renew job request lock: run-service lock renewal starts before script execution and keeps a background renewal loop alive while user code runs
- report completion output for a no-op job: opt-in `run --complete-noop` probe completes through run-service
- finish job request with success/failure: opt-in `run --complete-noop` probe finishes success
- handle cancellation message: V2 broker cancellation messages are polled while the Docker job runs and kill the active job container
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
- Docker execution uses a shared `/github/home` mount and `HOME=/github/home` across the job and native action containers so setup adapters can install tools into a home directory visible to later script steps
- Docker execution mounts the same host temp directory at `/__t`, `/github/runner_temp`, `/github/file_commands`, and `/tmp` for job/action containers so target workflows that use absolute `/tmp/...` artifact paths still share files with native adapters
- Docker execution mounts a shared Velnor sccache directory at `/var/cache/sccache`, matching the `java-monorepo` Rust workflow's cache assumptions across job containers
- Docker execution mounts the GitHub workflow directory at `/github/workflow` across the job and native action containers; `GITHUB_EVENT_PATH` points at `/github/workflow/event.json`
- the ordered planner now routes known target marketplace action families to `ExecutableStep::Native` before considering any metadata runtime fallback; unimplemented native adapters fail explicitly instead of silently executing marketplace JavaScript
- on Linux hosts, Velnor mounts the host Docker CLI and Buildx plugin directory into the job container and native Docker adapters when it also mounts `/var/run/docker.sock`; target Docker adapters such as `docker/setup-buildx-action`, `docker/login-action`, `docker/build-push-action`, and `docker/bake-action` need this client tooling when they talk to the mounted Docker socket
- script-step plan writes the script under runner temp, exposes per-step command-file env vars, and collects output/env/path/state/summary files after execution
- Docker script executor runs the planned lifecycle through an abstract command runner: create network, start container, exec script, collect state, cleanup container/network
- if Docker startup fails because a previous run left the same job container/network names behind, Velnor removes stale job resources once and retries startup before failing the job
- enabled GitHub script steps can be mapped into internal `ScriptStep` plans for `bash` and `sh`, including `script`, `shell`, `workingDirectory`, and job `defaults.run` shell/working-directory
- `velnor-runner run` executes supported jobs by default in one Docker job container plus Rust-native adapters for the target marketplace action families and finishes success/failure; JavaScript/Docker action sidecar support remains only lower-level compatibility groundwork and is not the Phase 0 target path; `--complete-noop` remains available for completion probes and `--dry-run-jobs` leaves received jobs unacknowledged for inspection
- env written to `GITHUB_ENV` and paths written to `GITHUB_PATH` are propagated to later script steps
- initial workflow/job env expressions are resolved once when the job execution state is created, matching GitHub worker-side environment evaluation
- `GITHUB_ENV` and legacy `set-env` mutations cannot override `GITHUB_*` and `RUNNER_*` defaults or `NODE_OPTIONS`; `ACTIONS_*` remains mutable for target runtime-export actions
- paths written to `GITHUB_PATH` are propagated to later script steps and must be exposed to native setup/tool/cache adapters
- step outputs written to `GITHUB_OUTPUT` are tracked by step id and basic `${{ steps.<id>.outputs.<name> }}` expressions are resolved in later scripts and native adapter inputs/env
- job outputs from the job message are evaluated at the end of execution from final step output state, matching the runner-side evaluation point used by GitHub; run-service typed map payloads are normalized before expression evaluation; target-shaped coverage includes `java-monorepo` `rust-docker` outputs such as `github.event_name == 'workflow_dispatch' && 'true' || steps.filter.outputs.bitcoin-processor` and `steps.targets.outputs.list`, plus Jackin `steps.dispatch.outputs.docs || steps.filter.outputs.docs`
- evaluated job outputs are sent in the V2 run-service `completejob` payload
- before Docker execution starts, Velnor publishes a best-effort in-progress GitHub job timeline record with the runner name; while Docker execution runs, Velnor publishes best-effort in-progress task records as executable steps start, then uploads completed task records and masked stdout/stderr feed lines as executable steps exit; silent and skipped executable steps still produce completion records and run-service step results
- stdout workflow commands are parsed for legacy/state-changing commands: `set-output`, `set-env`, `add-path`, and `save-state`
- basic `${{ github.* }}` and `${{ runner.* }}` context expressions are resolved from runtime env in later scripts and native adapter inputs/env
- basic step `if` evaluation is implemented for output comparisons, step outcome checks, `runner.os`, `github.event_name`, `github.ref`, status functions, and simple `&&`/`||`; target coverage includes Java `steps.check-image.outputs.exists == 'false'` gates over Docker login, Buildx setup, and build script steps
- native `actions/checkout` support is wired before Docker execution through host `git init/fetch/checkout`; self and explicit `repository:` checkouts are supported, including target `path`, `ref`, `token`, and `fetch-depth` inputs

Exit criteria:

- workflow with self checkout plus shell steps works.
- step output flows to later step.
- env written to `GITHUB_ENV` flows to later step.
- non-zero exit code fails step/job.

## Milestone 3: Native Action Adapters

Goal: support target marketplace action behavior through Rust-native adapters.

Deliverables:

- broader `actions/checkout` compatibility: `repository`, `path`, `ref`, `token`, `fetch-depth`, `fetch-tags`, `persist-credentials`, and `clean` inputs are implemented for target shapes; `fetch-depth: 0` fetches full branch/tag refs for target path-filter jobs; checkout refs from earlier step outputs, such as the `jackin` preview `ref: ${{ steps.source.outputs.sha }}`, stay ordered after their producer step and are not eagerly collapsed; checked-out worktrees are added to the mounted-home `safe.directory` config for Docker-side Git commands; persisted git extraheaders are removed after the job; sparse checkout, submodules, and LFS remain open
- action resolver/downloader for `owner/repo@ref`: repository action download into `_actions` and metadata discovery are implemented as composite-discovery groundwork; known target marketplace action families bypass pinned metadata for execution and route to Rust-native adapters
- action metadata parser for `action.yml`: JavaScript, composite, and Docker `runs.using` shapes are modeled
- repository action planner for enabled non-checkout `uses:` steps
- native adapter registry maps the current target action families to adapter kinds; unknown actions can still be modeled from metadata during development, but this is not the product direction
- native adapter executor: implement adapter-specific Rust code for cache, artifact, setup/tool, paths-filter, pages, Docker/Buildx, Renovate, and runtime export behavior
- native post hooks: adapters with cleanup/save behavior must model post execution explicitly and receive `GITHUB_STATE` values saved during main execution
- native Docker adapters should run short-lived Docker commands/containers on the job network when the behavior needs Docker, but Velnor owns the implementation and does not execute marketplace action bundles
- action container paths for native adapters should match GitHub expectations where scripts/tools inspect them: `/github/workspace`, `/github/runner_temp`, and `/github/file_commands`, while keeping Velnor's shared `/__w` and `/__t` mounts available
- planned action inputs and metadata defaults are passed to native adapters as typed input maps; adapters may synthesize `INPUT_*` only when a child process requires GitHub-compatible env
- step `env:` from GitHub job messages is parsed for script and native action steps, with basic expression resolution at execution time
- workflow/job `env:` from GitHub job messages is overlaid into the runtime environment; protected `GITHUB_*`/`RUNNER_*` defaults are not overwritten
- runtime env:
  - basic `GITHUB_*` variables are extracted from the job message and injected into script and JavaScript steps, including `GITHUB_ACTIONS=true`
  - target `GITHUB_WORKFLOW` and `GITHUB_REF_NAME` are injected; `GITHUB_REF_NAME` is derived from `GITHUB_REF` when GitHub does not send it directly
  - target GitHub context/env values such as `GITHUB_REPOSITORY_OWNER`, ref metadata, workflow metadata, run attempt, retention days, and server/API URLs are injected when present or derived where safe
  - `github.event` from job `ContextData` is written to `/github/workflow/event.json` and exposed through `GITHUB_EVENT_PATH`
  - repository JavaScript and Docker actions receive per-action `GITHUB_ACTION`, `GITHUB_ACTION_PATH`, `GITHUB_ACTION_REPOSITORY`, and `GITHUB_ACTION_REF`; these values are present while resolving action env/input expressions such as `${{ github.action_path }}`
  - all executable steps receive step-scoped `GITHUB_ACTION` before condition, env, and script expression rendering
  - `github.action_status` is expression-resolvable for composite action steps from the current composite scope, while top-level steps fall back to current job status
  - basic `RUNNER_*` variables are injected for the Docker runner environment, including GitHub-style `RUNNER_ARCH` values such as `X64`/`ARM64`, `RUNNER_NAME`, `RUNNER_WORKSPACE`, `RUNNER_ENVIRONMENT=self-hosted`, `RUNNER_TOOL_CACHE=/__tool`, matching `AGENT_TOOLSDIRECTORY=/__tool` for toolcache actions, and `RUNNER_DEBUG=1` when `ACTIONS_STEP_DEBUG=true`
  - action runtime values from `SystemVssConnection` are injected: `ACTIONS_RUNTIME_URL`, `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_CACHE_URL`, `ACTIONS_RESULTS_URL`, OIDC request URL/token, cache service v2, and orchestration id when GitHub sends them
  - target-shaped cache, artifact, and runtime-export execution is covered by executor tests: `actions/cache`, `actions/upload-artifact`, `actions/download-artifact`, `actions/deploy-pages`, and `crazy-max/ghaction-github-runtime` now run through native adapters for the target input shapes; native cache restores and saves paths through shared Velnor workdir storage, and upload/download use run-scoped artifact storage under the shared work directory with deterministic per-run/per-name artifact ids and target release glob expansion, so cross-job handoff works on one Velnor host while GitHub artifact service upload remains open for multi-runner live parity
  - target-shaped Docker action behavior is covered by executor tests: `docker/setup-buildx-action`, `docker/login-action`, `docker/metadata-action`, `docker/build-push-action`, and `docker/bake-action` now have native adapter coverage that invokes Velnor-owned Docker CLI commands without Node sidecars; Docker login sends credentials through stdin instead of argv; older sidecar shape tests remain only as low-level runtime coverage
  - broader GitHub runner env parity remains incomplete
- `secrets.*` expressions are resolved from GitHub secret job variables, including `secrets.GITHUB_TOKEN` from `system.github.token`; GitHub mask hints and secret variables are applied to runner-uploaded feed lines, and `::add-mask::` workflow commands are tracked for later step log masking
- basic workflow command parsing: state-changing stdout commands are parsed; deprecated `set-output`/`save-state` commands emit masked run-service telemetry, `error`, `warning`, and `notice` commands are counted on timeline records and preserved as structured run-service step annotations, and title/location plus GitHub `##[group]`, `##[endgroup]`, and `##[debug]` markers are preserved in uploaded step logs; live validation of native GitHub UI folding remains open
- job `environment.url` template tokens are evaluated after all steps from final step outputs and sent as run-service `environmentUrl`, with masked values skipped; executor coverage matches the target `jackin` docs deployment shape `steps.deployment.outputs.page_url`
- runner-side expression contexts: `env.*`, selected `github.*`/`runner.*` including `github.workflow` and `github.ref_name`, nested `github.event.*` from `ContextData`, `steps.*.outputs.*`, `secrets.*`, and generic job `ContextData` lookup such as `matrix.*`, `needs.*`, `inputs.*`, and `vars.*` are supported for script/action env and basic step conditions; `&&`/`||` value expressions, equality checks, unary `!`, `contains()`, `toJSON()`, and workspace-backed `hashFiles(...)` cover target-shaped cases, including target required-job gates over `needs.*.result == 'failure' || needs.*.result == 'cancelled'`
- run-service typed input/env/output maps are normalized for job env, run defaults, script steps, native checkout, repository actions, local composite actions, and `JobOutputs`, so V2 payloads still become normal job env, step env, defaults, `INPUT_*` values, and `needs.*.outputs.*` values for target actions and downstream jobs
- target expression coverage now checks workflow YAML from both target repositories, cached marketplace action metadata under `/tmp/velnor-actions`, and target-local composite action metadata under `jackin/.github/actions`, including multiline composite expressions, multi-argument `hashFiles(...)`, and the exact `jackin` `workflow_run` preview gate over `github.event.workflow_run.conclusion`, event, repository, branch, and `head_sha`
- target `dorny/paths-filter` execution shape is covered by an executor test that verifies pull request event payload writing, `GITHUB_EVENT_PATH`, repository/workspace/event env, resolved token/filter inputs, command-file outputs, and downstream `steps.<id>.outputs.*` gating
- target setup action shape now has native adapter coverage for `jdx/mise-action`, `actions/setup-python`, `dtolnay/rust-toolchain`, `extractions/setup-just`, `baptiste0928/cargo-install`, `mozilla-actions/sccache-action`, and `Swatinem/rust-cache`; the adapters use job-container shell commands or env/path/output mutations without Node sidecars; `jdx/mise-action`, `actions/setup-python`, `extractions/setup-just`, and `rui314/setup-mold` install missing target tools instead of silently accepting absent or wrong binaries
- older setup/cache sidecar tests remain only as lower-level JavaScript runtime coverage while target execution moves toward native adapters
- target Pages action shape is covered by executor/action tests that verify `actions/upload-pages-artifact` composite output flow through nested `actions/upload-artifact`, `actions/deploy-pages` Node24 sidecar env, results/OIDC runtime values, GitHub deployment context, resolved token inputs, `page_url` output capture, the target docs sitemap step receiving `PAGE_URL=${{ steps.deployment.outputs.page_url }}`, the local `check-deployed-docs` action preserving `sitemap-url: ${{ steps.sitemap.outputs.url }}` until runtime resolution, and the runtime handoff from sitemap output plus `JACKIN_REPO_*` env into deployed-docs validation inputs
- target `renovatebot/github-action` now runs through a native adapter that launches the configured Renovate Docker image with resolved token, repository context, target Renovate env, mounted Docker socket, and host Docker CLI; executor coverage verifies no Node sidecar is used
- target `mozilla-actions/sccache-action` execution shape is covered by an executor test that verifies the native adapter can soft-fail honestly, target `steps.sccache.outcome == 'success'` can gate the wrapper-enabling script, and the optional cache setup step can remain non-fatal
- cached target action metadata verification now uses the same `_actions/<repo>/<ref>/<path>` layout as the runtime downloader, so the direct workflow action inventory and nested composite action closure are checked against the runner's real on-disk action resolution shape
- cached target composite metadata is also expanded into the same script/repository/output invocation variants used by execution planning, catching unsupported nested composite constructs before live runs
- target workflow repository action references are assembled into a synthetic GitHub job and passed through Velnor's real repository-action planning and ordered executable-step expansion against cached metadata
- `continue-on-error`: script and native action steps can ignore a nonzero exit for job failure while preserving `steps.<id>.outcome == 'failure'`, covering target optional `sccache` setup
- V2 typed `continueOnError` wrapper values are accepted for target soft-fail action steps
- ordered job execution for script steps and native adapters in original message order is wired, with native cache/save post hooks modeled explicitly; host-side checkout still runs before the Docker container starts

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
- nested `uses` steps: implemented for repository actions referenced from local composites, with target marketplace families routed to the native adapter registry
- composite step conditions: `if:` is parsed, rendered with composite inputs, and propagated to expanded `run` and nested repository action steps
- repository composite actions: downloaded marketplace composites now expand into ordered script steps; nested repository actions inside those composites are discovered and downloaded recursively
- composite run steps receive GitHub-style `GITHUB_ACTION_PATH` pointing at the parent composite action directory, so scripts using `$GITHUB_ACTION_PATH` match runner behavior
- composite `continue-on-error`: parsed for nested composite steps and applied to expanded script/repository steps
- composite outputs: metadata `outputs.*.value` is evaluated after expanded inner steps and exposed as `steps.<composite-id>.outputs.*`
- post-step state if needed

Current limit: `jackin`'s `aggregate-needs` shape is covered by the run-only subset, including `needs-json: ${{ toJSON(needs) }}` input rendering, the exact bash/JQ failure-or-cancelled gate, and `GITHUB_ACTION_PATH` propagation. `check-deployed-docs` can now plan its nested `jdx/mise-action` repository action. Marketplace composites such as `dtolnay/rust-toolchain`, `rui314/setup-mold`, `actions/upload-pages-artifact`, and `extractions/setup-just` can be expanded far enough to run scripts or route nested repository actions, with composite outputs materialized for later steps. Nested local actions and full GitHub expression parity remain open.

Exit criteria:

- `jackin` local `.github/actions/aggregate-needs` works.
- `jackin` local `.github/actions/check-deployed-docs` starts and can run its shell steps.

## Milestone 5: Artifacts And Cache

Goal: support target artifact/cache workflows through Rust-owned adapters while
preserving GitHub-compatible YAML and runtime env.

Deliverables:

- pass runtime endpoints/tokens correctly
- ensure mounted paths match action expectations
- resolve target cache keys that use `${{ hashFiles(...) }}` against the checked-out workspace
- support native `actions/cache` restore/save behavior, outputs, state, and `fail-on-cache-miss`
- support native upload/download artifact actions through run-scoped workdir storage for same-run cross-job handoff on one Velnor host; add GitHub artifact service transport for multi-runner live parity

Exit criteria:

- `actions/cache`
- `actions/upload-artifact`
- `actions/download-artifact`
- `Swatinem/rust-cache`

all work on a disposable repo workflow.

## Milestone 6: Docker Build Workflows

Goal: run Docker-heavy target workflows.

Deliverables:

- job container and native Docker adapters mount the host Docker CLI/Buildx plugin on Linux when available
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

- `jackin-project/jackin` at `52a457b689940e05ed65015d2a82ff0e22577d2e`
- `ChainArgos/java-monorepo` at `56491ec5b17702186506217452b58bcf57572079`
- high-frequency: `actions/checkout` 46, `jdx/mise-action` 13, `actions/cache` 13, `mozilla-actions/sccache-action` 7, `actions/upload-artifact` 6, `dorny/paths-filter` 5, `rui314/setup-mold` 5, `docker/setup-buildx-action` 5, `docker/login-action` 5
- important lower-frequency: `actions/download-artifact` 3, `crazy-max/ghaction-github-runtime` 2, local `aggregate-needs` 3, local `check-deployed-docs` 2
- one-off target actions: `actions/setup-python`, `baptiste0928/cargo-install`, `dtolnay/rust-toolchain`, `Swatinem/rust-cache`
- release/pages/docker-specific: `actions/upload-pages-artifact`, `actions/deploy-pages`, `docker/metadata-action`, `docker/build-push-action`, `docker/bake-action`
- reusable workflow job references exist in `java-monorepo` (`./.github/workflows/kestra-build-image.yml`), but GitHub expands those before a runner receives individual job messages, so Phase 0 should not parse workflow-level `jobs.<id>.uses`

Deferred:

- macOS runner replacement is not a target
- full GitHub Pages parity if OIDC/pages internals block first pass
- every marketplace action outside target inventory

Exit criteria:

- target workflows complete on Velnor in GitHub UI.
- branch protection can point at Velnor-backed checks.

## Out Of Current Scope: Configuration Languages

Pkl, KCL, PQL, and any Velnor-native workflow language are not part of the
current implementation plan. They are archived brainstorming only. Do not
implement a parser, compiler, package, CLI, scheduler, or runtime path for any
of them while Phase 0 is focused on GitHub Actions runner compatibility.

Current non-goals:

- no Pkl CLI command
- no Pkl package
- no PQL package or schema
- no KCL package or schema
- no parser, evaluator, compiler, binding, or runtime integration for any of
  those languages
- no workflow renderer/compiler
- no replacement for GitHub's YAML parser
- no Velnor-native scheduler before Phase 0 target proof
