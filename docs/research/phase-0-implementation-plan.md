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

Current GitHub can also send broker/run-service messages. Velnor already stores whether registration returned V2 settings; it still needs broker support before claiming broad drop-in compatibility.

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

Lock renewal matters more than step execution. If Velnor runs a real job without renewal, GitHub can abandon or reassign the job while the container is still running.

## Docker Execution Model

Velnor should always run target jobs in a Docker job container, even when the workflow does not declare `container:`.

Initial lifecycle:

```text
create workspace/temp/actions/tools dirs
docker network create <job network>
docker run -d --name <job container> --network <job network> \
  -v <workspace>:/__w \
  -v <temp>:/__t \
  -v <actions>:/__a \
  -v <tools>:/__tool \
  -v /var/run/docker.sock:/var/run/docker.sock \
  <image> tail -f /dev/null
docker exec ... <script>
docker rm --force <job container>
docker network rm <job network>
```

Current code can map enabled GitHub `run:` message steps into internal script-step plans when the runner receives `Reference.Type = Script`. The mapper supports `script`, `shell` values `bash`/`sh`, and relative or absolute `workingDirectory`; unsupported shells are reported as incomplete mapping until broader shell support is added.

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
- defaults for `shell: bash` and working directory

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
2. Command files and env/path propagation: partially implemented for parsing and per-step files.
3. `actions/checkout`: either native plugin first or JavaScript action support first. Native plugin is faster for Phase 0.
4. Marketplace JavaScript action handler:
   - download action repo/ref
   - parse `action.yml`
   - map `with:` to `INPUT_*`
   - run Node entrypoint inside job container
   - provide `GITHUB_*`, `RUNNER_*`, `ACTIONS_*` runtime env
5. Composite action handler:
   - local `.github/actions/*`
   - nested `run`
   - nested `uses`
   - inputs/outputs
6. Cache/artifact runtime endpoints:
   - `ACTIONS_RUNTIME_URL`
   - `ACTIONS_RUNTIME_TOKEN`
   - `ACTIONS_CACHE_URL`
   - `ACTIONS_RESULTS_URL`
7. Docker action behavior:
   - job container can call host Docker
   - buildx action can create/use builders
   - GHA cache backend variables pass through
8. Pages actions for jackin:
   - `actions/upload-pages-artifact`
   - `actions/deploy-pages`

## Reporting Order

GitHub UI compatibility needs reporting early. Minimal order:

1. create/update job timeline record as in-progress
2. create/update step timeline records
3. stream or append logs per step
4. finish step with success/failure/skipped
5. finish job with success/failure/cancelled
6. include job outputs and step results in completion payload
7. mask secrets in logs before upload
8. support annotations from workflow commands

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

- step `if`
- `always()`
- `success()`, `failure()`, `cancelled()`
- `steps.<id>.outputs.*`
- env/context expansion already present in action inputs or script env

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

1. Live-test `velnor-runner run --once --complete-noop` against a disposable workflow and adjust reporter route details if GitHub rejects them.
2. Start renew-job loop before real job execution.
3. Connect script-step Docker executor to real script steps from the job message.
4. Add a native `actions/checkout` equivalent or implement JavaScript action execution.

This gets Velnor from "polls one message" to "can complete a simple real GitHub job".

Current code can parse enough `AgentJobRequestMessage` to identify job id/name, plan, request id, timeline id, variables, endpoints, repositories, containers, and action steps.

Current code has classic `jobrequests` client methods for lock renewal and finish-job requests. The normal run path does not acknowledge jobs yet, but `velnor-runner run --once --complete-noop` can opt into the completion probe.

Current code models the classic timeline record/feed routes used by the upstream `JobServerQueue`; the no-op completion path still needs a live disposable GitHub test before it should be treated as proven.
