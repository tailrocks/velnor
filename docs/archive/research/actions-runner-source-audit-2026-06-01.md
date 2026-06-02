# actions/runner Source Audit: Phase 0 Contract

Source inspected: `actions/runner` `main` at `c6a124e18496a6e5d2357415052d1799afc64b63`.

Latest release refresh: `v2.334.0` at
`f1995ede5d885c997d13d8eca5467c4ce97fe69c`; see
`docs/research/latest-runner-v2-refresh-2026-06-01.md`.

This audit turns upstream runner behavior into Velnor implementation rules. The goal is not to port the full runner. The goal is to copy the smallest behavior set needed for the current `jackin-project/jackin` and `ChainArgos/java-monorepo` workflows to run as GitHub-scheduled self-hosted jobs.

## Protocol Loop

Upstream sources:

- `src/Runner.Listener/Runner.cs` lines 525-823
- `src/Runner.Listener/MessageListener.cs` lines 233-453
- `src/Runner.Listener/BrokerMessageListener.cs` lines 272-430
- `src/Sdk/WebApi/WebApi/BrokerHttpClient.cs` lines 59-173
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs` lines 70-210

Official runner behavior:

- one listener session polls messages until shutdown
- classic messages are fetched from distributed task `messages`
- classic `BrokerMigration` switches the next poll to the broker
- V2 broker polls `message` with `sessionId`, runner status, runner version, OS, architecture, and `disableUpdate`
- V2 run-service job references are acknowledged best-effort when requested, then acquired from run-service
- non-retriable run-service acquire errors, such as `404`, `409`, and `422`, are skipped/backed off, not treated as worker bugs
- broker `DeleteMessageAsync` is a no-op; classic messages are deleted after dispatch
- `ForceTokenRefresh`, refresh/update, cancellation, and broker migration are control-plane messages, not user steps

Velnor rule:

- keep protocol code separate from Docker execution
- keep one active GitHub job per internal runner session/slot, while designing
  the Velnor daemon to own multiple slots and run multiple job containers
  concurrently
- require V2 broker/run-service for the normal hosted-GitHub path
- do not treat classic distributed-task polling as target MVP compatibility
- do not implement classic distributed-task polling in the normal runner path
- treat runner update/self-update messages as log-and-ignore or graceful restart for Phase 0
- refresh broker/run-service clients when GitHub sends `ForceTokenRefresh`
- treat broker acknowledge as best-effort; failure should not block job execution
- retry only where upstream retries; skip already-acquired/unprocessable run-service jobs

## Worker Completion

Upstream sources:

- `src/Runner.Worker/JobRunner.cs` lines 35-120
- `src/Runner.Worker/JobRunner.cs` lines 267-430
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs` lines 123-210

Official runner behavior:

- `SystemVssConnection` from the acquired job message becomes the credential source for job server/run-service reporting
- run-service jobs complete through `completejob`
- classic jobs complete by raising a `JobCompleted` plan event when the plan supports that feature
- completion includes result, job outputs, step results, job annotations, environment URL, telemetry, billing owner id, and infrastructure failure category when present
- complete-job retries up to five times except for unauthorized/not-found cases

Velnor rule:

- renew and complete V2 jobs with the job-scoped `SystemVssConnection` token, not only the runner OAuth token
- evaluate job outputs after all steps and before completion
- upload readable timeline/feed logs and structured step annotations before
  completion, because a locally correct job is not sufficient if GitHub UI does
  not show usable state
- preserve step results for `continue-on-error` and downstream `steps.<id>.outcome` behavior

## Step Execution

Upstream sources:

- `src/Runner.Worker/StepsRunner.cs` lines 76-116
- `src/Runner.Worker/StepsRunner.cs` lines 187-335
- `src/Runner.Worker/ActionManager.cs` lines 855-1045
- `src/Runner.Worker/ActionManager.cs` lines 1212-1275

Official runner behavior:

- each step gets expression functions including `always`, `cancelled`, `failure`, `success`, and `hashFiles`
- each step gets `steps` and `env` expression contexts rebuilt from current global state
- action steps set `github.action` before step env evaluation
- step conditions are evaluated immediately before execution
- command result and execution-context result are merged, then `continue-on-error` is applied
- repository actions are resolved through the job/launch server, downloaded to `_actions/<owner>/<repo>/<ref>`, and loaded from `action.yml`, `action.yaml`, or Dockerfile paths

Velnor rule:

- do not parse full workflow YAML in Phase 0; use the `AgentJobRequestMessage` that GitHub already expanded
- implement only runtime expression features that target job messages and target action metadata actually use
- preserve original step order, with JavaScript/Docker post hooks in reverse order
- use cached/direct git action downloads as a pragmatic replacement for launch-server action archives, but keep the same `_actions/<owner>/<repo>/<ref>/<path>` runtime layout
- keep native `actions/checkout` as a Phase 0 shortcut because it unlocks target workflows earlier than a full JavaScript handler

## Command Files

Upstream sources:

- `src/Runner.Worker/FileCommandManager.cs` lines 42-78
- `src/Runner.Worker/FileCommandManager.cs` lines 107-184
- `src/Runner.Worker/FileCommandManager.cs` lines 293-420

Official runner behavior:

- each step gets fresh command files through GitHub context values
- files are processed after the step exits
- `GITHUB_PATH` prepends non-empty lines to a global path list
- `GITHUB_ENV` updates later step env and expression env context
- `GITHUB_OUTPUT` writes step outputs
- env/output files support both `NAME=value` and heredoc `NAME<<EOF` syntax
- `NODE_OPTIONS` is blocked for `GITHUB_ENV`

Velnor rule:

- command file behavior is required, not optional; target workflows rely on outputs, env, path, state, and summaries
- path entries must affect later script steps and native/repository action
  execution
- output/env parsing should keep matching upstream heredoc behavior
- `GITHUB_*`, `RUNNER_*`, and `NODE_OPTIONS` should remain protected in Velnor; target runtime-export actions still need mutable `ACTIONS_*`

## Docker And Container Shape

Upstream sources:

- `src/Runner.Worker/ContainerOperationProvider.cs` lines 293-327
- `src/Runner.Worker/ContainerOperationProvider.cs` lines 406-450

Official runner behavior:

- job containers mount work, externals, temp, actions, and tools directories
- temporary home is mounted at `/github/home` and exported as `HOME`
- temporary workflow directory is mounted at `/github/workflow`
- job container workdir is the GitHub workspace
- job container entrypoint is a long-running `tail -f /dev/null`
- a per-job Docker network is created and removed
- service containers expose container id, ports, and network into job context
- service containers with health checks are waited on until healthy

Velnor rule:

- always use a fresh Docker job container for target Linux jobs, even when the workflow does not declare `container:`
- mount shared workspace/temp/home/actions/tools into job container and any
  action helper containers
- for action containers, preserve GitHub's static `/github/workspace` workdir, `/github/runner_temp` temp alias, and `/github/file_commands` command-file alias
- mount Docker socket plus host Docker CLI/Buildx for target Docker workflows
- preflight bind-mount visibility before running user steps
- start service containers on the same job network with GitHub aliases and wait for running/healthy state

## Target Workflow Priority

Current target inventory is generated by `scripts/target_verify.sh` from:

- `jackin-project/jackin`
- `ChainArgos/java-monorepo`, specifically its Rust, Docker, Buildx, and
  related workflow paths

GitHub expands `workflow_call`, matrices, triggers, and job graph scheduling
before Velnor sees a job message. Velnor should not implement those scheduler
features in Phase 0. It should implement the runner-side behavior needed by the
already-expanded job messages that GitHub sends to a registered self-hosted
runner.

Priority proof order:

1. keep V2 broker/run-service compatibility current with latest `actions/runner`
2. prove Docker bind mounts with a real daemon-visible work directory
3. prove the public fixture repository in GitHub UI
4. run ChainArgos `ansible.yml` end to end as the smallest target workflow
5. run ChainArgos Rust script/action jobs
6. run ChainArgos Docker-heavy Buildx/Bake jobs
7. run Jackin Linux `ci.yml`, `construct.yml`, and `docs.yml`
8. exclude macOS runner replacement; optional ARM is supported only on a real
   ARM Linux host
9. defer broad hosted-image parity beyond the target Linux workflow needs
