# Phase 0 Implementation Plan

This document turns the current research into an implementation contract for the first Velnor target: a Rust runner that can replace the self-hosted GitHub runner for the target repositories.

Sources inspected on 2026-05-31:

- https://github.com/actions/runner
- https://github.com/actions/runner/blob/main/src/Runner.Listener/MessageListener.cs
- https://github.com/actions/runner/blob/main/src/Runner.Listener/Runner.cs
- https://github.com/actions/runner/blob/main/src/Runner.Listener/JobDispatcher.cs
- https://github.com/actions/runner/blob/main/src/Runner.Worker/JobRunner.cs
- https://github.com/actions/runner/blob/main/src/Runner.Worker/ContainerOperationProvider.cs
- https://github.com/actions/runner/blob/main/src/Runner.Worker/FileCommandManager.cs
- https://github.com/jackin-project/jackin/tree/main/.github
- https://github.com/ChainArgos/java-monorepo/tree/main/.github

## Product Rule

Phase 0 is not a new workflow language.

Phase 0 is a GitHub self-hosted runner-compatible Rust agent. GitHub still parses YAML, expands matrices, applies triggers, evaluates job `needs`, owns secrets, owns the UI, and sends Velnor already-expanded job messages.

Velnor must execute the job message well enough that current target workflows keep their existing `.github/workflows/*.yml` files.

The GitHub runner protocol is private. This is acceptable for this project. Velnor should treat upstream `actions/runner` as the protocol oracle and should keep protocol code isolated from the executor so that private protocol drift is contained.

## Upstream Runner Shape

The official runner is split into listener and worker concerns:

```text
Runner.Listener
  -> creates agent session
  -> long-polls messages
  -> dispatches one job at a time
  -> renews the job lock
  -> spawns Runner.Worker
  -> finishes the job request

Runner.Worker
  -> receives AgentJobRequestMessage
  -> initializes job context
  -> starts container resources
  -> executes steps
  -> processes command files
  -> uploads logs/timeline/results
```

Velnor can stay one Rust process, but should keep these as separate modules:

- `protocol`: registration, OAuth, session, message, renew, finish
- `dispatcher`: one assigned job at a time, cancellation, lock renewal
- `job`: deserialize job message and build a normalized execution plan
- `executor`: Docker container lifecycle and step execution
- `reporter`: timeline/log/result upload

## Message Flow To Emulate

Classic runner flow:

```text
CreateAgentSessionAsync(poolId, session)
loop:
  GetAgentMessageAsync(poolId, sessionId, lastMessageId, status, runnerVersion, os, arch, disableUpdate)
  decrypt body if session encryption key is present
  if message is PipelineAgentJobRequest:
    parse AgentJobRequestMessage
    DeleteAgentMessageAsync(poolId, messageId, sessionId)
    dispatch job
  if message is JobCancelMessage:
    cancel running job
DeleteAgentSessionAsync(poolId, sessionId)
```

Current code keeps one GitHub session open and long-polls this classic message route until stopped. `run --once` preserves the probe behavior by exiting after one message/no-message poll. While a Docker job is executing, Velnor starts a busy-status cancellation poll from the dispatched job message id; matching `JobCancellation` messages are acknowledged, the active job container is killed, and final completion is reported as canceled.

Current GitHub can also send broker/run-service messages. Velnor now preserves V2 settings from the registered agent, including `ServerUrlV2` and `UseV2Flow`, and has typed client/request shapes for the official broker `session`, `message`, `acknowledge`, and run-service `acquirejob`, `renewjob`, and `completejob` routes. When `UseV2Flow` is enabled, `velnor-runner run` creates a broker session, polls broker messages, handles `RunnerJobRequest`, optionally acknowledges it, acquires the full job through run-service, renews the job through run-service while Docker executes, and completes the job through run-service. While a V2 Docker job is running, Velnor polls broker messages with busy status and uses matching `JobCancellation` messages to kill the active job container. `BrokerMigration` messages update the broker base URL for subsequent polls.

Minimum message support:

- `PipelineAgentJobRequest`
- run-service job reference messages if V2 is enabled
- job cancellation
- runner refresh/update as log-and-ignore or graceful exit
- broker migration as reconnect-to-broker

## Job Dispatch Contract

The official runner assumes one active job per runner. Velnor should keep the same first:

1. Receive job message.
2. Mark runner status busy for the message poll loop.
3. Start lock renewal before executing user code.
4. Execute the job in one fresh Docker job container.
5. Cancel/kill the container on cancellation.
6. Stop lock renewal.
7. Upload final timeline/log/step results.
8. Finish the job request.
9. Return runner status online.

Lock renewal matters more than step execution. If Velnor runs a real job without renewal, GitHub can abandon or reassign the job while the container is still running. Current code performs an initial renewal before Docker execution and keeps a 60-second renewal loop alive while user code runs.

## Docker Execution Model

Velnor should always run target jobs in a Docker job container, even when the workflow does not declare `container:`.

Initial lifecycle:

```text
create workspace/temp/home/actions/tools dirs
docker network create <job network>
docker run -d --name <job container> --network <job network> \
  -v <workspace>:/__w \
  -v <temp>:/__t \
  -v <home>:/github/home \
  -v <actions>:/__a \
  -v <tools>:/__tool \
  -e HOME=/github/home \
  -v /var/run/docker.sock:/var/run/docker.sock \
  <image> tail -f /dev/null
docker exec ... <script>
docker rm --force <job container>
docker network rm <job network>
```

Current code can map enabled GitHub `run:` message steps into internal script-step plans when the runner receives `Reference.Type = Script`. The mapper supports `script`, `shell` values `bash`/`sh`, and relative or absolute `workingDirectory`; unsupported shells are reported as incomplete mapping until broader shell support is added.

When GitHub sends an explicit job container image, Velnor uses that image for the long-running Docker job container. If no job image is present, Velnor uses the CLI/default image. The default is `ghcr.io/catthehacker/ubuntu:act-latest` rather than plain `ubuntu:24.04`, because the target workflows assume hosted-runner-style tools such as `curl`, `jq`, compilers, Docker/Buildx clients, and common archive utilities. Job container environment variables from the job container payload are passed through to `docker run`.

Basic service containers from GitHub container resources are started on the same per-job Docker network before the job container, using the GitHub alias as Docker network alias. Velnor passes resource environment variables, port mappings, and container options to Docker, waits for Docker health/running status before starting the job container, then removes services during cleanup. Job container `options`/`createOptions` are also passed through to Docker.

If Docker startup fails, Velnor removes any stale job container, service containers, and job network with the same generated names, then retries startup once. This covers crash/restart cases where a previous Velnor process exited before normal cleanup.

Velnor mounts one host-backed home directory at `/github/home` and sets `HOME=/github/home` for the long-running job container, JavaScript action side containers, and Docker action containers. This is required for target setup actions such as `jdx/mise-action` because tools installed under `$HOME` must remain visible to later script steps.

Current code also treats enabled `actions/checkout` as a native host-side checkout before starting the Docker job container. It uses the self repository resource clone URL, job version/ref, and system access token by default; explicit `repository`, `path`, `ref`, `token`, and `fetch-depth` inputs are supported for target shapes such as Jackin's Homebrew tap checkout. Submodules, sparse checkout, LFS, and full credential cleanup remain later compatibility work.

For target workflows, mount the host Docker socket first. This weakens isolation but is the shortest path to `docker/setup-buildx-action`, `docker/bake-action`, `docker/build-push-action`, and direct `docker buildx`.

Later isolation options:

- per-job Docker-in-Docker sidecar
- rootless BuildKit
- Firecracker/microVM worker pool
- Kubernetes executor

## Target Workflow Inventory

`ChainArgos/java-monorepo` is the best first live target because every job uses `runs-on: hetzner-sentry-ci`.

Observed workflows:

- `ansible.yml`
- `rust.yml`
- `rust-docker.yml`
- `rust-docker-build.yml`
- `kestra-build-image.yml`
- `kestra-build-publish.yml`
- `renovate.yml`

Observed action/runtime needs:

- `actions/checkout`
- `actions/cache`
- `actions/setup-python`
- `dorny/paths-filter`
- `jdx/mise-action`
- `dtolnay/rust-toolchain`
- `extractions/setup-just`
- `baptiste0928/cargo-install`
- `Swatinem/rust-cache`
- `renovatebot/github-action`
- `docker/setup-buildx-action`
- `docker/login-action`
- `docker/metadata-action`
- `docker/build-push-action`
- `docker/bake-action`
- reusable workflows through `workflow_call`
- job `needs`, outputs, `if`, `always`, `contains`
- command files: `GITHUB_OUTPUT`, `GITHUB_ENV`, `GITHUB_PATH`
- defaults for `shell: bash` and working directory: implemented for `defaults.run.shell` and `defaults.run.working-directory`

`jackin-project/jackin` is the second target. It has more hosted-runner labels and platform matrix complexity:

- mostly `ubuntu-latest` and `ubuntu-24.04`
- some `runs-on: ${{ matrix.os }}` and `runs-on: ${{ matrix.runner }}`
- Linux, ARM Linux, and macOS targets
- local composite actions in `.github/actions`
- GitHub Pages deploy actions
- artifact-heavy preview/release flows

Jackin first-pass scope should be Linux jobs only. macOS replacement is explicitly out of Phase 0 unless Velnor later has a macOS executor.

## Action Support Order

Implement action support in the order that unlocks target workflows fastest:

1. Script steps: already partially implemented.
2. Command files and env/path propagation: implemented for parsing and per-step files, with `GITHUB_PATH` entries applied to later script steps and JavaScript action sidecars.
3. `actions/checkout`: either native plugin first or JavaScript action support first. Native plugin is faster for Phase 0.
4. Marketplace JavaScript action handler:
   - download action repo/ref: implemented for repository action probe path
   - parse `action.yml`: metadata parser and repository action planner are implemented as groundwork
   - map `with:` to `INPUT_*`: implemented for JavaScript action invocation, including metadata defaults
   - run Node entrypoint: implemented for ordered script/JavaScript execution. JavaScript actions run in a short-lived side container with the same workspace/temp/home/actions/tools mounts and job network so arbitrary job images do not need to carry Node. By default Velnor honors the action's declared Node runtime image and mounts host Docker client tooling when available; operators can override it with `--node-action-image`.
   - run `runs.post` cleanup/save entrypoints: implemented in reverse order with `GITHUB_STATE` to `STATE_*` propagation and target `runs.post-if` condition handling
   - provide `GITHUB_*`, `RUNNER_*`, `ACTIONS_*` runtime env: basic job-message extraction, `GITHUB_ACTIONS=true`, target repository owner/ref/workflow/server URL env, GitHub-style `RUNNER_ARCH` values, runner name/environment/workspace/debug, `GITHUB_EVENT_PATH` payload writing, per-action repository/ref/path env, and step injection are implemented; full runner parity remains open
5. Composite action handler:
   - local `.github/actions/*`: implemented for checked-out self repository actions
   - nested `run`: implemented for shell steps with basic input/action-path/workspace interpolation
   - nested `uses`: implemented for repository JavaScript actions inside local composites
   - step `if`: implemented for local composite `run` and nested repository JavaScript steps
   - repository marketplace composites: implemented for downloaded `runs.using: composite` actions, including recursive discovery of nested repository `uses`
   - composite step `continue-on-error`: parsed and propagated into expanded steps
   - inputs/outputs: input interpolation, defaults, target `toJSON(needs)` caller input rendering, and metadata `outputs.*.value` materialization are implemented
6. Cache/artifact runtime endpoints: implemented from `SystemVssConnection` for JavaScript actions, including OIDC, cache service v2, and orchestration id when present
   - `ACTIONS_RUNTIME_URL`
   - `ACTIONS_RUNTIME_TOKEN`
   - `ACTIONS_CACHE_URL`
   - `ACTIONS_RESULTS_URL`
7. Docker action behavior:
   - `runs.using: docker` action planning and execution is implemented for top-level and repository-nested action expansion
   - `docker://` images run directly; local Dockerfile actions are built before execution
   - Docker actions run as short-lived containers on the same job network with workspace/temp/actions/tools mounts and command-file env
   - job container and JavaScript action sidecars can call host Docker through the mounted socket; the default JavaScript action sidecar includes the Docker CLI
   - buildx action can create/use builders
   - GHA cache backend variables pass through
8. Pages actions for jackin:
   - `actions/upload-pages-artifact`
   - `actions/deploy-pages`

## Reporting Order

GitHub UI compatibility needs reporting early. Minimal order:

1. create/update job timeline record as in-progress
2. create/update step timeline records
3. stream or append logs per step: current code captures stdout/stderr after each executed script or JavaScript step, creates completed task timeline records, and appends masked lines to those records after the job container run.
4. finish step with success/failure/skipped
5. finish job with success/failure/cancelled
6. include job outputs and step results in completion payload
7. mask secrets in logs before upload. Current code builds masks from GitHub mask hints and secret variables for runner-uploaded feed lines, and stores `add-mask` workflow command values for the later step log uploader.
8. support annotations from workflow commands. Current code parses state-changing stdout workflow commands (`set-output`, `set-env`, `add-path`, `save-state`) plus `add-mask`, and counts `error`, `warning`, and `notice` commands on timeline records; richer file/line annotation payloads and log grouping remain open.

Until reporting exists, Docker step execution can be locally correct but GitHub will not look correct.

## Expression Boundary

Do not implement the full workflow YAML expression engine in Phase 0.

GitHub already evaluates:

- workflow triggers
- job graph scheduling
- matrix expansion
- reusable workflow calls
- most job-level `if`

Velnor still needs runtime step expression behavior from the job message:

- step `if`: implemented for output comparisons, step outcome checks, selected GitHub/runner context values, generic job `ContextData` (`matrix.*`, `needs.*`, `inputs.*`, `vars.*`), synthesized `secrets.*` from GitHub secret variables, grouped simple `&&`/`||`, target-shaped `contains()`, and status functions `always()`, `success()`, `failure()`, and non-cancelled `cancelled()`
- `steps.<id>.outputs.*`: implemented for direct interpolation in later scripts and JavaScript action env
- job outputs: `JobOutputs` from the GitHub job message are evaluated after step execution from final step outputs and sent through the classic `JobCompleted` plan event or the V2 run-service `completejob` payload
- env/context expansion in scripts and JavaScript action env: basic `steps.*.outputs.*`, `github.*` including `github.workflow`, `github.ref_name`, and nested `github.event.*`, `runner.*`, `env.*`, `secrets.*`, generic job `ContextData`, target-shaped `&&`/`||` value expressions, equality checks, unary `!`, `contains(...)`, `toJSON(...)`, and workspace-backed `hashFiles(...)` interpolation is implemented
- `continue-on-error`: implemented for script and JavaScript action steps; failed steps keep failure outcome for later `steps.<id>.outcome` checks, but do not fail the job

Implement only the expression subset that appears in the job message for target workflows.

## Pkl Layer After Phase 0

Pkl should remain close to GitHub Actions so migration is easy. The strict package should model the same concepts with typed fields and typed unions:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"

name = "Rust"

on = new {
  pullRequest {}
  push {
    branches = List("main")
  }
  workflowDispatch {
    inputs {
      ["packages"] = new StringInput {
        required = false
        default = ""
      }
    }
  }
}

jobs {
  ["check"] = new Job {
    runsOn = new SelfHostedRunner {
      labels = List("hetzner-sentry-ci")
    }

    steps = List(
      new UsesStep {
        uses = "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
      },
      new UsesStep {
        uses = "jdx/mise-action@1648a7812b9aeae629881980618f079932869151"
      },
      new RunStep {
        name = "Check"
        shell = Bash
        command = "cargo check --workspace --all-targets"
      },
    )
  }
}
```

Important design rule: Pkl must compile to the same normalized plan that the GitHub job-message adapter produces. That keeps one executor and one reporter.

```text
GitHub YAML -> GitHub job message -> Velnor normalized plan -> Docker executor
Pkl workflow -> Velnor compiler    -> Velnor normalized plan -> Docker executor
```

## Current Concrete Next Work

The next useful implementation steps are:

1. Live-test `velnor-runner run --once --complete-noop` and `velnor-runner run --once --execute-scripts` against disposable workflows and adjust reporter route details if GitHub rejects them.
2. Start renew-job loop before real job execution.
3. Expand remaining job/message expression support; workflow/job-level `env:`, step-level `env:`, generic `ContextData`, `contains()`, and `toJSON()` are implemented for runner-side script/action execution paths.
4. Provide full GitHub/runner/action runtime environment for JavaScript actions, then test `setup-*`, cache, and Docker actions from target workflows.

This gets Velnor from "polls one message" to "can complete a simple real GitHub job".

Current code can parse enough `AgentJobRequestMessage` to identify job id/name, plan, request id, timeline id, variables, endpoints, repositories, containers, and action steps.

Current code has classic `jobrequests` client methods for lock renewal and finish-job requests. The normal run path does not acknowledge jobs yet, but `velnor-runner run --once --complete-noop` can opt into the completion probe.

Current code models the classic timeline record/feed routes used by the upstream `JobServerQueue`; the no-op completion path still needs a live disposable GitHub test before it should be treated as proven.
