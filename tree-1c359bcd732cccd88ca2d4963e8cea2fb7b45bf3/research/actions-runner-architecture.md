# actions/runner Architecture Notes

These notes summarize the parts of `github.com/actions/runner` that matter for a Rust Velnor runner.

Source inspected:

- https://github.com/actions/runner
- latest local refresh during implementation: `c6a124e18496a6e5d2357415052d1799afc64b63`
- latest release refresh: `v2.334.0`
  `f1995ede5d885c997d13d8eca5467c4ce97fe69c`; see
  [latest-runner-v2-refresh-2026-06-01.md](latest-runner-v2-refresh-2026-06-01.md)

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

## Velnor Setup

Velnor setup uses GitHub's JIT runner configuration API, not the classic runner
registration-token path.

Velnor:

1. Calls the repository, organization, or enterprise `generate-jitconfig`
   endpoint.
2. Decodes `encoded_jit_config`.
3. Requires `UseV2Flow=true` and `ServerUrlV2`.
4. Saves:
   - server URL
   - V2 broker URL
   - pool ID/name
   - agent ID/name
   - labels
   - OAuth credential data
   - ephemeral runner settings

Classic official-runner APIs remain source reference only:

- `GetRunnerTokenAsync`
- `GetTenantCredential`
- `AddAgentAsync`
- `ReplaceAgentAsync`
- `GetAgentPoolsAsync`

For Velnor:

- support repo-level JIT runner configuration first
- support labels
- support exact cleanup for Velnor-created runner ids
- store settings in a local file
- support OAuth credential flow
- require V2 broker/run-service flow for hosted GitHub target runs

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

Current hosted GitHub settings use `BrokerMessageListener` when `UseV2Flow` is set, which talks to broker/run-service endpoints. Velnor treats this as the only normal runner path. The protocol layer models the V2 route shapes: broker `session`, `message`, and `acknowledge`, plus run-service `acquirejob`, `renewjob`, and `completejob`; the runner can create a broker session, acquire `RunnerJobRequest` jobs, renew them through run-service, dispatch them into the Docker executor, complete them through run-service, poll broker cancellation while jobs run, and reconnect to a new broker base URL on `BrokerMigration`.

The classic listener remains useful as upstream reference for shared message shapes and migration behavior, but it is not an MVP compatibility target.

Important message types:

- job request
- job cancel
- runner refresh/update
- broker migration

For Phase 0, Velnor can skip self-update and treat runner refresh as "log and continue/exit" if needed.

## Job Dispatch

`JobDispatcher` assumes one job at a time per registered runner identity. The
source explicitly says the dispatcher is not thread-safe and relies on the
service not sending another job while the current one is running. That is the
GitHub scheduler/session boundary Velnor must respect.

Important behavior:

- marks runner busy
- starts job lock renewal before worker starts
- sends `AgentJobRequestMessage` to worker
- keeps renewing lock every 60 seconds
- sends cancellation to worker
- finishes job request with final result

For Velnor:

- the operator-facing product is one long-running `velnor-runner daemon`
  process, not a pile of manually managed runner processes
- the daemon owns a configured concurrency limit such as `--slots 4`
- each slot maps to one GitHub runner identity and one broker session, because
  GitHub still treats a runner session as one-active-job-at-a-time
- the daemon supervises all slots, receives jobs continuously, and starts each
  acquired job immediately when a slot is free
- every acquired job gets a distinct Docker job container, Docker network,
  workspace/temp/actions/tools directories, cancellation path, and lock-renewal
  loop
- cancellation should stop/kill only the affected job container and must not
  stop the daemon or unrelated slots

This is the key difference from the stock runner UX. GitHub sees multiple
runner agents/sessions, but the user runs and operates one Velnor daemon.

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

Phase 0 Velnor needs:

- script step
- composite action expansion for the target local actions
- enough action metadata parsing to understand target repository and composite
  action shape
- Rust-native adapters for every target marketplace action family
- Docker execution support for target shell steps and native Docker adapters

Phase 0 intentionally does not execute marketplace JavaScript/TypeScript
bundles. The YAML still says `uses: actions/cache@...`,
`uses: docker/build-push-action@...`, etc., but Velnor ignores the pinned
implementation package and routes those known action families to Rust-native
adapters. Unknown marketplace actions should fail explicitly until they are
added to the supported target inventory.

## Runtime Context Env

Official worker handlers expose context objects through `IEnvironmentContextData` immediately before executing a step:

- `ScriptHandler`
- `NodeScriptActionHandler`
- `ContainerActionHandler`

`GitHubContext.GetRuntimeEnvironmentVariables()` allowlists `github.*` fields and emits matching `GITHUB_*` env vars. `RunnerContext.GetRuntimeEnvironmentVariables()` emits all `runner.*` fields as `RUNNER_*`.

Important step-scoped behavior from `StepsRunner` and `ActionRunner`:

- `StepsRunner` sets `github.action` to `ActionStep.Action.Name` before evaluating and merging step env.
- `ActionRunner` sets `github.action_repository` and `github.action_ref` for non-self repository actions; local/self actions clear those fields.
- `CompositeActionHandler` shallow-copies the parent `github` context for embedded steps and sets `github.action_path` to the composite action directory.
- `CompositeActionHandler` also sets `github.action_status` as expression-only state for embedded composite steps. It is not in the `GitHubContext` env allowlist, so it is not exported as `GITHUB_ACTION_STATUS`.

Velnor implications:

- `GITHUB_ACTION` must be step-scoped for script, native action adapter,
  Docker, checkout, and composite-output pseudo-steps before
  condition/env/script rendering.
- Repository action steps get their action-scoped env overlay for
  `GITHUB_ACTION_PATH`, `GITHUB_ACTION_REPOSITORY`, and `GITHUB_ACTION_REF`
  before action env/input expressions are resolved, even when the marketplace
  action is executed by a Rust-native adapter.
- Composite `run:` steps need `GITHUB_ACTION_PATH` pointing at the parent composite action directory, including repository composites expanded from marketplace actions.
- `github.action_status` is lower priority for the target repositories because no target workflow/action currently references it. Velnor now tracks composite execution boundaries so embedded composite steps resolve it from the current composite scope; top-level steps fall back to current job status.

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
- sets `ACTIONS_CACHE_SERVICE_V2=True` when the `actions_uses_cache_service_v2` variable is true
- optionally sets `ACTIONS_ORCHESTRATION_ID` when the orchestration feature flag is enabled
- executes bundled runner Node binary

For Velnor, this is reference material for environment shape only:

- parse action metadata
- expose matching `INPUT_*`, `GITHUB_*`, `RUNNER_*`, and `ACTIONS_*` values
  where target native adapters and composite actions need them
- do not execute marketplace JavaScript or TypeScript bundles in Phase 0
- route known target action families to Rust-native adapters

## Composite Actions

Composite actions are nested steps with their own inputs, env, conditionals, and outputs.

Target repos have local composite actions:

- `.github/actions/aggregate-needs`
- `.github/actions/check-deployed-docs`

Phase 0 must support local composite actions early.

## Docker Actions

Some marketplace actions may be Docker actions. The official behavior remains
useful reference material, but current target marketplace Docker-like behavior
is implemented through Rust-native adapters and shell-level Docker/Buildx
commands.

Direct generic Docker action support would require:

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
- keep live compatibility tests pinned to the latest official V2 runner behavior
