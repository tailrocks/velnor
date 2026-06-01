# Self-Hosted Runner Implementation Blueprint

This document is the implementation-oriented version of the Velnor Phase 0 idea:
be a Rust self-hosted GitHub runner replacement first, then add Pkl as a typed
workflow authoring layer later.

Phase 0 is runner compatibility, not workflow parsing. GitHub sends Velnor an
expanded `AgentJobRequestMessage`, not the original workflow YAML. See
`docs/research/github-runner-job-message-contract-2026-06-01.md` for the
runtime payload contract.

The goal is not full GitHub Actions parity. The goal is to run the current
`.github` trees from:

- `jackin-project/jackin` at `52a457b689940e05ed65015d2a82ff0e22577d2e`
- `ChainArgos/java-monorepo` at `56491ec5b17702186506217452b58bcf57572079`

Upstream source reference:

- `actions/runner` `main` at `c6a124e18496a6e5d2357415052d1799afc64b63`

## Product Shape

Phase 0 should look boring to a repository user:

1. Keep `.github/workflows/*.yml` exactly as they are.
2. Register Velnor as a self-hosted runner with the labels those workflows use.
3. Let GitHub keep parsing YAML, expanding matrices, resolving reusable workflows,
   applying triggers, managing secrets, and rendering the UI.
4. Velnor receives already-expanded job messages, executes the job in Docker, and
   reports status/logs/outputs back to GitHub.

The target workflow files are evidence for what GitHub will likely send in
expanded job payloads. They are not files Velnor should parse in Phase 0.

The later Pkl layer should not replace this executor. It should compile into the
same normalized job plan that the GitHub job-message adapter uses:

```text
GitHub YAML -> GitHub job message -> Velnor normalized plan -> Docker executor -> reporter
Pkl workflow -> Velnor compiler    -> Velnor normalized plan -> Docker executor -> reporter
```

That keeps Phase 0 useful immediately and prevents the future typed language from
forking the execution engine.

The normalized plan boundary is specified in
`docs/research/normalized-plan-contract-2026-06-01.md`.

## What GitHub Owns In Phase 0

Because Velnor registers as a runner, GitHub still owns the expensive workflow
semantics:

- workflow triggers: `push`, `pull_request`, `workflow_dispatch`, `schedule`,
  `workflow_run`, `merge_group`, `workflow_call`
- job graph scheduling and `needs`
- matrix expansion
- reusable workflow expansion
- job-level secrets and permissions
- queueing, cancellation, retry UI, and the visible Actions run page

Velnor owns runtime behavior after a job is assigned:

- runner registration and message protocol
- job lock renew/complete
- Docker job isolation
- checkout/action/script execution
- command files and workflow commands
- action runtime/cache/results env
- logs, annotations, step results, job outputs, and final conclusion

## Upstream Runner Contract To Copy

The official runner has two main halves. Velnor can be one Rust process, but it
should keep the same boundary in code.

### Listener Side

Source anchors:

- [`Runner.cs` message loop](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Listener/Runner.cs#L525-L735)
- [`MessageListener.cs` classic polling](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Listener/MessageListener.cs#L233-L453)
- [`BrokerMessageListener.cs` V2 polling](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Listener/BrokerMessageListener.cs#L272-L430)
- [`RunServiceHttpClient.cs` acquire/renew/complete](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Sdk/RSWebApi/RunServiceHttpClient.cs#L70-L210)

Rules for Velnor:

- Create one runner session, then long-poll for messages.
- Require broker/run-service V2 for hosted GitHub target runs.
- Do not implement classic distributed-task messages in the normal runner path.
- Treat `BrokerMigration` as a control-plane redirect to broker polling.
- Treat `RunnerJobRequest` as a job reference that must be acquired from
  run-service before execution.
- Treat broker acknowledge as best-effort.
- Ignore or gracefully exit for runner refresh/update messages in Phase 0.
- Refresh broker/run-service credentials when GitHub sends `ForceTokenRefresh`.
- Poll for cancellation while a job is busy; if a matching cancellation arrives,
  kill the active Docker job container and complete as canceled.
- Renew the job lock while executing. Without renewal, GitHub can abandon the job
  while Velnor is still running containers.
- Complete jobs through run-service `completejob`.

### Worker Side

Source anchors:

- [`ActionRunner.cs` command files and action handler lifecycle](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/ActionRunner.cs#L170-L283)
- [`ContainerOperationProvider.cs` job container mounts](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/ContainerOperationProvider.cs#L293-L327)
- [`ContainerOperationProvider.cs` network and health checks](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/ContainerOperationProvider.cs#L406-L450)
- [`ContainerActionHandler.cs` Docker action paths](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/Handlers/ContainerActionHandler.cs#L177-L207)
- [`FileCommandManager.cs` command files](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/FileCommandManager.cs#L42-L78)
- [`FileCommandManager.cs` env/path/output parsing](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/FileCommandManager.cs#L107-L184)
- [`FileCommandManager.cs` heredoc syntax](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/FileCommandManager.cs#L293-L420)

Rules for Velnor:

- Each job gets a fresh workspace/temp/home/actions/tools layout.
- Each job gets a per-job Docker network.
- Target Linux jobs should always run inside a long-running Docker job
  container, even when the workflow does not declare `container:`.
- JavaScript actions run in short-lived Node sidecar containers with the same
  workspace/temp/home/actions/tools mounts and job network.
- Docker actions run as short-lived containers with GitHub-style static paths:
  `/github/workspace`, `/github/runner_temp`, `/github/file_commands`,
  `/github/home`, and `/github/workflow`.
- Every step gets fresh command files. After the step exits, Velnor processes
  `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`, `GITHUB_STATE`, and
  `GITHUB_STEP_SUMMARY`.
- Env/output parsing must support both `NAME=value` and heredoc `NAME<<EOF`.
- `NODE_OPTIONS`, `GITHUB_*`, and `RUNNER_*` should stay protected from user
  env-file mutation; `ACTIONS_*` must remain mutable for target runtime-export
  actions.

## Target Workflow Contract

The target scan found these practical constraints.

### `ChainArgos/java-monorepo`

This is the first live target because every job uses `runs-on:
hetzner-sentry-ci`.

Required workflows:

- `ansible.yml`
- `rust.yml`
- `rust-docker.yml`
- `rust-docker-build.yml`
- `kestra-build-image.yml`
- `kestra-build-publish.yml`
- `renovate.yml`

Runtime requirements:

- default self checkout and `fetch-depth: 0`
- `defaults.run.shell: bash`
- `defaults.run.working-directory`
- `actions/setup-python`
- Rust and Just setup actions
- cache service env for `actions/cache` and `Swatinem/rust-cache`
- `dorny/paths-filter` output-driven gating
- reusable workflow expansion from `kestra-build-publish.yml`, handled by GitHub
- Docker socket, Docker CLI, and Buildx plugin for setup/build/push/bake actions
- Docker Hub login secrets
- direct shell `docker buildx` commands
- required-check jobs using `always()` and `needs.*.result`

### `jackin-project/jackin`

This is the second target because it has hosted-runner labels and macOS/ARM
matrix legs.

Required workflows:

- `ci.yml`
- `construct.yml`
- `docs.yml`
- `preview.yml`
- `release.yml`
- `renovate.yml`
- `renovate-validate.yml`
- local actions `aggregate-needs` and `check-deployed-docs`

Phase 0 scope:

- Linux jobs on `ubuntu-latest`, `ubuntu-24.04`, and optionally
  `ubuntu-24.04-arm`
- macOS matrix legs are not Docker-runner work
- GitHub Pages actions are required for docs flows
- upload/download artifacts are required for preview/release flows
- external checkout of `jackin-project/homebrew-tap` with explicit token/path/ref
  is required
- local composites must work, including `toJSON(needs)` input passing

## Current Velnor Shape

The current code is already aligned with the upstream split:

- `protocol.rs`: registration, OAuth, distributed task, broker, run-service,
  timeline/feed calls
- `runner.rs`: session loop, broker migration, job acquisition, lock renewal,
  cancellation polling, job completion, checkout/action planning
- `job_message.rs`: deserialization for the GitHub job payload subset
- `checkout.rs`: native checkout shortcut for the target `actions/checkout`
  shapes
- `action.rs`: action metadata parsing and repository/composite/Docker action
  planning
- `executor.rs`: Docker job lifecycle, script steps, JavaScript sidecars, Docker
  actions, command-file state, runtime env, expression subset
- `container.rs`: Docker command shape for job/action containers
- `command_files.rs` and `workflow_command.rs`: env/output/path/state/summary
  files and stdout workflow commands

Important implementation rule: keep all GitHub protocol drift contained in
`protocol.rs`/`runner.rs`; keep the Docker executor independent of whether the
plan came from GitHub YAML or future Pkl.

## Minimum End-To-End Sequence

For the target MVP, a real Velnor job should follow this sequence:

1. Register runner with target labels, especially `hetzner-sentry-ci`.
2. Create broker session.
3. Poll for V2 `RunnerJobRequest`.
4. Best-effort acknowledge, then acquire full job message.
5. Normalize typed run-service maps into normal job env, inputs, defaults, and
   job outputs.
6. Native-checkout target `actions/checkout` steps before container startup.
7. Create job dirs and per-job Docker network.
8. Start service containers if GitHub ever sends target service resources.
9. Start long-running job container.
10. For each enabled step, evaluate the target expression subset immediately
    before execution.
11. Run script, JavaScript action sidecar, Docker action, or expanded composite
    in original order.
12. Process command files after every step.
13. Run JavaScript/Docker post hooks in reverse order with `STATE_*`.
14. Evaluate job outputs from final step outputs.
15. Upload readable timeline/feed logs, annotations, step results, and final job
    status.
16. Complete V2 job with the correct conclusion.
17. Cleanup containers, network, checkout credentials, and temp dirs.

## Pkl Design Constraint

Pkl is the later authoring language, not the Phase 0 scheduler. It should stay
close to GitHub Actions vocabulary but remove YAML ambiguity through types.

The first useful Pkl package should model:

- `Workflow`
- typed event triggers
- typed workflow inputs
- `Job`
- typed runner labels
- `RunStep`
- `UsesStep`
- typed cache/artifact/pages/Docker helper primitives
- constrained identifiers and non-empty command strings

Example direction:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"

name = "Rust Docker"

on = new {
  pullRequest {}
  workflowDispatch {
    inputs {
      ["push"] = new BooleanInput { default = false }
    }
  }
}

jobs {
  ["docker-bake"] = new Job {
    runsOn = new SelfHostedRunner {
      labels = List("hetzner-sentry-ci")
    }

    steps = List(
      new UsesStep {
        uses = "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
      },
      new DockerBuildx {
        name = "chainargos-builder"
        driver = "docker-container"
      },
      new DockerBake {
        files = List("backend-rust/docker-bake.hcl")
        targets = List("bitcoin-processor-app")
        cacheScope = "rust-workspace"
        push = false
      }
    )
  }
}
```

The Pkl compiler should lower helper primitives like `DockerBake` into the same
normalized executable steps that the GitHub job-message adapter produces today.

## Remaining Proof Path

The next evidence must be live, not only unit tests:

1. Run Velnor on a Linux host where the Docker daemon can see Velnor's bind
   mounted work directory.
2. Register it to a disposable repo and prove a checkout plus bash script job
   succeeds with readable GitHub UI logs.
3. Register it with `hetzner-sentry-ci`.
4. Run `java-monorepo` `ansible.yml`.
5. Run `java-monorepo` `rust.yml` with path-filter and required-check behavior.
6. Run `java-monorepo` Docker build workflows with Buildx/Bake/cache/login.
7. Add Linux-compatible labels for `jackin`, then run CI/docs/construct Linux
   paths.
8. Verify artifacts, cache, Pages, local composites, job outputs, step outcomes,
   annotations, masking, cancellation, and final required-check status in the
   GitHub UI.

Until those live runs pass, the objective is not complete even if local unit
coverage is strong.
