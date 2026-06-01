# GitHub Runner Job Message Contract

This is the key Phase 0 rule: Velnor does not receive the original workflow
YAML. GitHub parses the workflow, expands the job graph, applies scheduling, and
then sends a job payload to the runner.

The target `.github` files are still important, but only as a way to predict
which expanded job-message fields and action/runtime behavior Velnor must
support.

Upstream reference:

- `actions/runner` `main` at `c6a124e18496a6e5d2357415052d1799afc64b63`
- [`AgentJobRequestMessage.cs`](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Sdk/DTPipelines/Pipelines/AgentJobRequestMessage.cs)
- [`Runner.cs` message loop](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Listener/Runner.cs#L525-L735)
- [`Worker.cs` job-message deserialization](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/Worker.cs#L65-L94)
- [`JobRunner.cs` job server setup and completion](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/JobRunner.cs#L35-L110)
- [`StepsRunner.cs` step loop and runtime expression setup](https://github.com/actions/runner/blob/c6a124e18496a6e5d2357415052d1799afc64b63/src/Runner.Worker/StepsRunner.cs#L42-L125)

## Communication Path

Hosted GitHub V2 is the target for Velnor. The normal runner path should require
`UseV2Flow` and `ServerUrlV2` from registration and should fail fast if they are
missing. Classic polling is legacy for this project; it can exist as internal
debug/migration reference code, but it is not target MVP compatibility.

New target compatibility work must be validated through broker/run-service V2.

Classic path:

```text
Velnor registers as TaskAgent
  -> creates distributed-task runner session
  -> long-polls session messages
  -> receives TaskAgentMessage {
       messageType: "PipelineAgentJobRequest",
       body: AgentJobRequestMessage JSON
     }
  -> deletes/acknowledges the message after dispatch
  -> renews job lock while running
  -> uploads timeline/logs/results
  -> finishes job request
```

Broker/run-service V2 path:

```text
Velnor registers as TaskAgent
  -> creates broker session
  -> long-polls broker messages
  -> receives TaskAgentMessage {
       messageType: run-service job reference
       body: RunnerJobRequestRef JSON
     }
  -> best-effort broker acknowledge
  -> POST acquirejob to run-service
  -> receives AgentJobRequestMessage JSON
  -> renewjob while running
  -> completejob with conclusion, outputs, step results, annotations
```

The important invariant: both paths eventually produce an
`AgentJobRequestMessage`. That is the runtime contract Velnor must execute.

## What The Runner Receives

`AgentJobRequestMessage` is already an expanded job. It contains:

- `MessageType`: job request type, classic or run-service shaped
- `Plan`: orchestration plan id, group, artifact info
- `Timeline`: timeline id/change id for UI reporting
- `JobId`, `JobName`, `JobDisplayName`
- `RequestId` and `LockedUntil`
- `Variables`: GitHub/system variables such as `github.repository`,
  `github.ref`, `github.workflow`, `system.github.token`, feature flags, and
  runtime endpoints
- `MaskHints`: regex/value masks that must be applied to logs
- `Resources.Endpoints`: especially `SystemVssConnection`, the job-scoped token
  and URL used for logs/results/cache/runtime reporting
- `Resources.Repositories`: checkout repository metadata when GitHub includes it
- `Resources.Containers`: job/service container resources when present
- `ContextData`: expanded contexts such as `github`, `matrix`, `needs`,
  `inputs`, `vars`, and event data
- `EnvironmentVariables`: workflow/job environment overlays
- `Defaults`: `defaults.run.shell` and `defaults.run.working-directory`
- `JobContainer` and `JobServiceContainers`: evaluated or template container data
- `JobOutputs`: output expressions to evaluate after all steps
- `ActionsEnvironment`: environment deployment data
- `Steps`: ordered job steps
- `ActionsDependencies`: action dependency/lockfile entries when sent

The runner does not need to parse:

- workflow triggers
- `jobs.<id>.needs` graph construction
- matrix expansion
- reusable workflow call expansion
- most job-level `if`
- concurrency groups

GitHub has already handled those before assigning a job.

## Step Payload Shape

Every runtime step is represented as a `JobStep`/`ActionStep` entry in
`AgentJobRequestMessage.Steps`.

Important fields for Velnor:

- `Id`, `Name`, `DisplayName`, `ContextName`
- `Enabled`
- `Condition`
- `ContinueOnError`
- `TimeoutInMinutes`
- `Reference.Type`
- `Reference.Name`
- `Reference.Ref`
- `Reference.Path`
- `Reference.Image`
- `Inputs`
- `Environment`

Observed step reference types:

- `Script`: GitHub `run:` step. Inputs contain `script`, `shell`, and
  `workingDirectory` shapes. Velnor maps these to Docker `exec` script files.
- `Repository`: `uses: owner/repo@ref` or local action references. Velnor must
  resolve/download action metadata, then run JavaScript, Docker, or composite
  handlers.
- `ContainerRegistry`: `docker://...` actions. Not used directly by the current
  target workflows, but the type exists.

## Why Target YAML Still Matters

The target YAML is not the runtime input, but it tells us which job-message
features GitHub will generate.

Examples:

- `defaults.run.shell: bash` in `java-monorepo` becomes `Defaults` entries that
  Velnor must normalize.
- `with:` maps on actions become `Inputs`, sometimes as run-service typed map
  values rather than plain JSON strings.
- `env:` maps become `EnvironmentVariables` and step `Environment`.
- `if:` expressions become `Condition` strings.
- `needs.*.outputs.*`, `inputs.*`, and `matrix.*` appear in `ContextData`.
- `actions/checkout` appears as a `Repository` action step, but Velnor can
  implement it natively as long as observable behavior matches.
- reusable workflow calls in `kestra-build-publish.yml` do not arrive as
  `jobs.<id>.uses`; GitHub expands them into normal assigned jobs.
- required-gate jobs arrive as ordinary script/composite action steps, not as
  scheduler logic.

This is why the target audit is still the Phase 0 scope boundary: it predicts
which expanded payload branches Velnor must execute.

## Velnor Adapter Boundary

Velnor should convert `AgentJobRequestMessage` into internal execution state in
one place:

```text
AgentJobRequestMessage
  -> normalize V2 typed maps and template values
  -> derive runtime env and context
  -> perform native checkout shortcuts
  -> resolve repository/local action metadata
  -> expand composites
  -> ordered ExecutableStep list
  -> Docker executor
  -> step logs/results/outputs
  -> GitHub reporter completion payload
```

Current modules already match this boundary:

- `job_message.rs`: deserializes the job-message subset
- `script_step.rs`: maps `Reference.Type = Script` into internal script steps
- `checkout.rs`: native target `actions/checkout` support
- `action.rs`: action metadata and runtime parsing
- `runner.rs`: protocol dispatch, acquisition, renewal, cancellation, completion
- `executor.rs`: ordered Docker execution and command-file processing

## Target Payload Requirements

For `ChainArgos/java-monorepo` and Linux `jackin`, Velnor must support these
expanded payload features:

- `Reference.Type = Script` with bash shell and working-directory defaults
- `Reference.Type = Repository` for all pinned target marketplace actions
- local repository actions under `.github/actions/*`
- repository composite actions with nested `run` and `uses`
- JavaScript actions with `runs.pre`, `runs.main`, `runs.post`, and `post-if`
- Docker actions and Docker action sidecar path aliases
- `Inputs` in direct object and run-service typed map forms
- `Environment` in direct object and run-service typed map forms
- `ContinueOnError` in boolean and typed wrapper forms
- `JobOutputs` in classic and V2 typed forms
- `ContextData.github`, `github.event`, `inputs`, `needs`, `matrix`, `vars`
- `Variables` for GitHub runtime env, system token, cache/runtime/results URLs,
  OIDC, feature flags, and secret masking
- command files for output/env/path/state/summary
- step conditions using the target expression subset
- timeline/feed reporting for readable GitHub UI output

Explicitly outside Phase 0 unless GitHub sends it for the target workflows:

- parsing raw workflow YAML inside Velnor
- implementing trigger scheduling
- implementing matrix expansion
- implementing reusable workflow expansion
- implementing all shells
- implementing all possible marketplace action edge cases
- replacing macOS hosted runners with Linux Docker containers

## Implementation Implication

Research should focus on the official runner worker pipeline, not workflow YAML
syntax:

1. What exact `AgentJobRequestMessage` JSON does GitHub send for target jobs?
2. How does official `actions/runner` map each field into env/context/steps?
3. Which parts of that behavior are needed by the target action inventory?
4. Which behavior can Velnor implement natively while preserving observable
   GitHub UI/runtime compatibility?

The next live proof should therefore capture and archive real dry-run job
payload metadata from `java-monorepo`, then compare it with the target audit.
That payload is stronger evidence than reading YAML alone.

## Payload Capture Command

Velnor has a runner-side inspection mode for this:

```sh
cargo run --bin velnor-runner -- run \
  --once \
  --dry-run-jobs \
  --dump-job-message ./target-job-payloads
```

Behavior:

- the runner still registers/polls normally
- when a job is assigned, Velnor parses the acquired `AgentJobRequestMessage`
- Velnor writes a pretty JSON snapshot named like
  `job-<request-id>-<job-id>.json` under the provided directory
- secret variables, mask hints, endpoint authorization parameters, and obvious
  token/password/secret fields are replaced with `***`
- `--dry-run-jobs` leaves the job unacknowledged and unexecuted, so this should
  be used only with disposable or intentionally triggered target runs

Recommended first live capture:

1. Register a Velnor runner with the `hetzner-sentry-ci` label.
2. Trigger the smallest `ChainArgos/java-monorepo` workflow job.
3. Run the command above.
4. Compare the captured `Steps`, `Defaults`, `EnvironmentVariables`,
   `ContextData`, `Resources`, and `JobOutputs` shapes against
   `docs/research/target-mvp-compat-audit-2026-06-01.md`.
