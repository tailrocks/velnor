# actions/runner Architecture Notes

These notes summarize the parts of `github.com/actions/runner` that matter for a Rust Velnor runner.

Source inspected:

- https://github.com/actions/runner

Key files:

- `src/Runner.Listener/Configuration/ConfigurationManager.cs`
- `src/Runner.Listener/Runner.cs`
- `src/Runner.Listener/MessageListener.cs`
- `src/Runner.Listener/BrokerMessageListener.cs`
- `src/Runner.Listener/JobDispatcher.cs`
- `src/Runner.Worker/Worker.cs`
- `src/Runner.Worker/JobRunner.cs`
- `src/Runner.Worker/ActionRunner.cs`
- `src/Runner.Worker/ActionManager.cs`
- `src/Runner.Worker/ActionManifestManager.cs`
- `src/Runner.Worker/Handlers/ScriptHandler.cs`
- `src/Runner.Worker/Handlers/NodeScriptActionHandler.cs`
- `src/Runner.Worker/Handlers/CompositeActionHandler.cs`
- `src/Runner.Worker/Handlers/ContainerActionHandler.cs`
- `src/Runner.Worker/FileCommandManager.cs`
- `src/Runner.Worker/ActionCommandManager.cs`
- `src/Runner.Worker/ContainerOperationProvider.cs`
- `src/Runner.Worker/Handlers/StepHost.cs`
- `src/Runner.Worker/Container/DockerCommandManager.cs`

## Process Model

The official runner has a listener process and a worker process.

```text
Runner.Listener
  -> creates session
  -> polls messages
  -> dispatches one job
  -> spawns Runner.Worker
  -> sends job JSON over process channel
  -> renews job lock
  -> completes job request

Runner.Worker
  -> receives AgentJobRequestMessage
  -> initializes job context
  -> initializes job extension
  -> runs steps
  -> finalizes job
  -> exits with task result code
```

Velnor can use one Rust process with internal async tasks, but should preserve the logical split:

- listener/session task
- job dispatcher
- worker/job executor
- reporting queue

## Registration

Registration uses a GitHub registration token.

The official runner:

1. Gets a runner registration token from GitHub API.
2. Calls `actions/runner-registration` to get tenant credentials.
3. Registers or replaces a `TaskAgent`.
4. Saves:
   - server URL
   - pool ID/name
   - agent ID/name
   - labels
   - OAuth credential data
   - optional V2 broker URL/flow settings

Relevant APIs from source:

- `GetRunnerTokenAsync`
- `GetTenantCredential`
- `AddAgentAsync`
- `ReplaceAgentAsync`
- `GetAgentPoolsAsync`

For Velnor:

- first support repo-level runner registration
- support labels
- support replace mode
- support remove/unregister
- store settings in a local file
- support OAuth credential flow
- support V2 broker flow if GitHub requires it for new runners

## Message Loop

The classic listener does:

```text
CreateAgentSessionAsync(poolId, session)
loop:
  GetAgentMessageAsync(poolId, sessionId, lastMessageId, status, version, os, arch, disableUpdate)
  decrypt message if needed
  dispatch by message type
  DeleteAgentMessageAsync(poolId, messageId, sessionId)
DeleteAgentSessionAsync(poolId, sessionId)
```

Newer settings may use `BrokerMessageListener`, which talks to broker/run-service endpoints. Velnor likely needs to support this because current GitHub runner registration can set `UseV2Flow`. The protocol layer now models the V2 route shapes: broker `session`, `message`, and `acknowledge`, plus run-service `acquirejob`, `renewjob`, and `completejob`; the runner can create a broker session, acquire `RunnerJobRequest` jobs, renew them through run-service, dispatch them into the Docker executor, and complete them through run-service. Broker cancellation/migration remains open.

Important message types:

- job request
- job cancel
- runner refresh/update
- broker migration

For Phase 0, Velnor can skip self-update and treat runner refresh as "log and continue/exit" if needed.

## Job Dispatch

`JobDispatcher` assumes one job at a time per runner.

Important behavior:

- marks runner busy
- starts job lock renewal before worker starts
- sends `AgentJobRequestMessage` to worker
- keeps renewing lock every 60 seconds
- sends cancellation to worker
- finishes job request with final result

For Velnor:

- one assigned GitHub job per runner process is enough initially
- later, multiple runner registrations/processes can share one host
- one Docker container per job
- cancellation should stop/kill job container

## Job Message

The worker receives `AgentJobRequestMessage`.

This is important because Phase 0 does not need raw YAML parsing. GitHub has already converted YAML into a job request.

The message contains:

- job ID/name
- plan/timeline IDs
- variables
- secrets as masked variables
- endpoints and tokens
- repositories/resources
- steps
- job container/service container data
- defaults
- workflow/job environment variable overlays
- expression/context data needed at runtime

Velnor needs a Rust representation for the subset used by target jobs.

The upstream `JobExtension` evaluates `message.EnvironmentVariables` before action preparation, writes the result to `context.Global.EnvironmentVariables`, and also updates the `env` expression context. `AgentJobRequestMessage` documents this field as a hierarchy of environment variables to overlay, where the last entry wins.

## Step Execution

Official runner handler types:

- script step: `run:`
- JavaScript action
- composite action
- Docker/container action
- plugin/internal action

Phase 0 needs:

- script step
- JavaScript action
- composite action
- Docker action
- enough built-in/plugin behavior for checkout/artifact/cache actions to work through normal action packages

## Script Steps

`ScriptHandler`:

- resolves shell
- applies `defaults.run.shell`
- applies `defaults.run.working-directory`
- writes script to temp file
- fixes script contents for selected shell
- exposes contexts as environment variables
- executes via `StepHost`
- fails step on non-zero exit

For Velnor:

- write scripts to mounted temp dir
- run with `docker exec`
- support at least `bash`, `sh`, and `pwsh` eventually
- Phase 0 Linux focus: `bash` and `sh`

## Command Files

`FileCommandManager` creates per-step temp files and exposes paths through GitHub context/env:

- `GITHUB_ENV`
- `GITHUB_OUTPUT`
- `GITHUB_PATH`
- `GITHUB_STATE`
- `GITHUB_STEP_SUMMARY`

After the step exits, it parses files and updates job state.

Required syntax:

```text
NAME=value
NAME<<EOF
multi
line
EOF
```

Velnor must implement this accurately because target workflows rely on:

- `echo "x=true" >> "$GITHUB_OUTPUT"`
- `echo "KEY=value" >> "$GITHUB_ENV"`
- composite actions passing outputs

## Workflow Commands

`ActionCommandManager` parses log commands:

- `::add-mask::`
- `::error::`
- `::warning::`
- `::notice::`
- groups
- stop/resume commands
- older command forms

Phase 0 needs:

- masking
- annotations
- grouping
- debug/notice/warning/error
- enough to make logs and marketplace actions behave

## JavaScript Actions

`NodeScriptActionHandler`:

- reads `action.yml`
- finds `runs.using` and script path
- sets `INPUT_*` env vars
- exposes context env vars
- sets runtime URLs/tokens:
  - `ACTIONS_RUNTIME_URL`
  - `ACTIONS_RUNTIME_TOKEN`
  - `ACTIONS_CACHE_URL`
  - `ACTIONS_RESULTS_URL`
  - `ACTIONS_ID_TOKEN_REQUEST_URL`
  - `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
- executes bundled runner Node binary

For Velnor:

- provide Node runtime in job container or mount runner-provided Node into container
- parse action metadata
- clone/download action repository at pinned ref/SHA
- execute JS action entrypoint with proper env

## Composite Actions

Composite actions are nested steps with their own inputs, env, conditionals, and outputs.

Target repos have local composite actions:

- `.github/actions/aggregate-needs`
- `.github/actions/check-deployed-docs`

Phase 0 must support local composite actions early.

## Docker Actions

Some marketplace actions may be Docker actions.

Docker action support requires:

- parse `runs.using: docker`
- build Dockerfile or pull image
- pass inputs/env
- run container on same job network
- mount workspace/temp as needed

This can come after JS/composite if target action inventory confirms Docker actions block us.

## Container Model

Official runner container flow:

1. check Docker support
2. remove stale containers by runner label
3. prune stale networks
4. create per-job network
5. pull/start service containers
6. pull/start job container
7. mount directories:
   - work
   - externals
   - temp
   - actions
   - tools
   - `/github/home`
   - `/github/workflow`
8. start job container with `tail -f /dev/null`
9. execute each step with `docker exec`
10. print service logs and remove containers
11. remove network

Velnor should copy this shape for Phase 0.

## Reporting

The official runner reports through a queue:

- timeline records
- step logs
- annotations/issues
- attachments/summaries
- final job result

Velnor needs enough reporting for GitHub UI to show:

- live-ish logs
- step success/failure
- annotations
- final job status

Do not attempt perfect parity first. Start with correct final status and readable logs.

## Protocol Stance

The GitHub runner protocol is not a clean public stable API. That is acceptable for Velnor. The project intentionally targets the private protocol because Phase 0 is a self-hosted runner replacement for specific repositories.

Mitigations:

- pin target official runner version being emulated
- send compatible version/user-agent values
- implement only repo-level self-hosted runner first
- build integration tests against real GitHub disposable repo
- keep feature flags for V1/V2 message listener
