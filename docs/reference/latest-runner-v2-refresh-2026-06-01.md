# Latest GitHub Runner V2 Refresh

Date: 2026-06-01

Reference source:

- `actions/runner` latest release checked: `v2.334.0`
- tag commit: `f1995ede5d885c997d13d8eca5467c4ce97fe69c`
- `main` at check time: `c6a124e18496a6e5d2357415052d1799afc64b63`
- `main` rechecked on 2026-06-01: `c6a124e18496a6e5d2357415052d1799afc64b63`
- `main` rechecked again on 2026-06-01 during implementation planning:
  `c6a124e18496a6e5d2357415052d1799afc64b63`
- latest release rechecked on 2026-06-01 during scope cleanup:
  `v2.334.0`
- release page: <https://github.com/actions/runner/releases/tag/v2.334.0>

Drift check:

```sh
cargo run -q -p velnor-tools -- check-runner-reference
```

This compares the pinned release above with GitHub's latest `actions/runner`
release and verifies `crates/velnor-runner/src/protocol.rs` advertises the same
runner version/user-agent. If it fails, refresh this document, update the
protocol constants, and re-audit the V2 anchors before claiming latest-runner
compatibility.

This refresh exists because Velnor intentionally follows the latest hosted
GitHub self-hosted runner path. Classic distributed-task execution remains a
source reference only; the Velnor runner path targets broker/run-service V2.

## Upstream V2 Control Flow

Source anchors in `actions/runner` `v2.334.0`:

- `src/Runner.Listener/Runner.cs:393-403`: listener selection uses
  `BrokerMessageListener` when `RunnerSettings.UseV2Flow` is true.
- `src/Runner.Listener/BrokerMessageListener.cs:85-92`: broker mode requires
  `ServerUrlV2`.
- `src/Runner.Listener/BrokerMessageListener.cs:293-299`: broker message poll
  sends session id, runner status, runner version, OS, architecture, and
  disable-update flag.
- `src/Runner.Listener/Runner.cs:680-703`: run-service job messages are parsed
  as `RunnerJobRequestRef`; broker acknowledge is best-effort.
- `src/Runner.Listener/Runner.cs:725-739`: run-service `acquirejob` returns the
  full `AgentJobRequestMessage`; 404, 409, and 422 are non-retriable acquire
  cases and should be skipped/backed off rather than treated as runner bugs.
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs:70-116`: `acquirejob` request body
  is `{ jobMessageId, runnerOS, billingOwnerId }`.
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs:123-178`: `completejob` reports
  conclusion, outputs, step results, annotations, environment URL, telemetry,
  billing owner id, and infrastructure failure category.
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs:184-222`: `renewjob` uses plan id
  and job id.
- `src/Runner.Worker/JobRunner.cs:337-379`: V2 worker completion uses
  run-service `CompleteJobAsync`.

## Velnor Rules From This Refresh

- Advertise the latest supported runner version string and keep it pinned in
  one place.
- Require `UseV2Flow` and `ServerUrlV2` at registration/run time for hosted
  GitHub targets.
- Model one active GitHub job per broker session, but let the Velnor daemon own
  multiple internal runner slots/sessions so one daemon can execute multiple
  assigned jobs concurrently.
- Retry broker session creation briefly before failing, matching the upstream
  expectation that transient GitHub/network startup errors are not immediately
  fatal.
- After a broker session exists, delete it best-effort when the runner loop
  exits, including error exits, to avoid stale session conflicts on the next
  live smoke run.
- Poll broker messages with the runner status that reflects worker state:
  `Online` when idle and `Busy` while a job is running.
- Retry transient broker message poll failures with bounded backoff, and back
  off after repeated empty polls, so long live target runs are not killed by a
  short broker/network interruption.
- Treat broker acknowledge as best-effort. Acknowledge failure must not prevent
  acquiring or executing the job.
- For `acquirejob`, treat HTTP 404, 409, and 422 as non-retriable stale or
  unusable messages; log, back off, and continue polling.
- Use the job-scoped `SystemVssConnection` token from the acquired job message
  for renew/complete when available.
- Complete V2 jobs through `completejob`, including job outputs, step results,
  workflow-command annotations, evaluated environment URL when present,
  telemetry for implemented workflow-command cases, billing owner id, and
  infrastructure failure category.
- Treat `ForceTokenRefresh`, runner refresh/update, cancellation, hosted
  shutdown, and broker migration as control-plane messages, not user steps.

## Source-To-Implementation Trace

This is the compact implementation trace to keep current when `actions/runner`
changes. Each row states what Velnor should copy, where that behavior belongs,
and what should prove it.

| Upstream source | Behavior to copy | Velnor owner | Proof |
| --- | --- | --- | --- |
| `Runner.cs` selects `BrokerMessageListener` when `UseV2Flow` is true. | Hosted GitHub target path is V2 broker/run-service only. | `protocol.rs`, `runner.rs` | `cargo run -q -p velnor-tools -- check-runner-reference`, protocol tests, live fixture job acquisition. |
| `BrokerMessageListener.cs` requires `ServerUrlV2`, creates a broker session, and polls with session id, runner status, version, OS, architecture, and update flag. | Velnor must require V2 settings and report `Online`/`Busy` per internal slot. | `protocol.rs`, `runner.rs` | runner config tests, broker poll-state tests, live fixture evidence. |
| `Runner.cs` treats run-service messages as `RunnerJobRequestRef` and best-effort acknowledges before acquire. | Broker messages are references, not workflow YAML; acknowledge failure must not block execution. | `runner.rs` | acquire/ack tests, sanitized job-message dumps. |
| `RunServiceHttpClient.cs` sends `acquirejob`, `renewjob`, and `completejob`. | Use run-service for acquire, renewal, and completion; use job-scoped credentials when available. | `protocol.rs`, `runner.rs` | renew/complete tests and GitHub UI final status. |
| `JobRunner.cs` completes V2 jobs with outputs, step results, annotations, environment URL, telemetry, billing owner id, and infrastructure failure category. | Local success is not enough; GitHub UI must get readable step/result data. | `runner.rs`, `executor.rs`, `workflow_command.rs` | timeline/feed tests and fixture/target UI review. |
| `ContainerOperationProvider.cs` creates one network, mounts work/temp/actions/tools/home/workflow, starts a long-running job container, and waits for services. | Velnor should keep one fresh Docker job container and one per-job network, even when YAML has no `container:`. | `container.rs`, `executor.rs`, `preflight.rs` | Docker command-shape tests and live Docker fixture. |
| `ContainerActionHandler.cs` uses `/github/workspace`, `/github/runner_temp`, `/github/file_commands`, `/github/home`, and `/github/workflow`. | Native/action helper containers must share GitHub-style paths with the job container. | `container.rs`, `executor.rs` | action adapter tests and artifact/cache fixture proof. |

What not to copy in Phase 0:

- the legacy/classic distributed-task worker path as a normal execution path
- broad hosted image parity outside current target Linux workflows
- marketplace JavaScript/TypeScript execution for supported target action
  families
- macOS runner behavior or labels
- full YAML parsing, matrix scheduling, expression evaluation, or reusable
  workflow expansion; GitHub already performs that before Velnor receives a job

## Docker Execution Shape To Keep

Source anchors:

- `src/Runner.Worker/Container/ContainerInfo.cs:54-59`: job containers map work,
  tools, and externals to GitHub-style container paths and mount Docker socket
  when Docker is available.
- `src/Runner.Worker/ContainerOperationProvider.cs:293-327`: job containers
  mount work, externals, temp, actions, tools, temporary home, and workflow
  directories.
- `src/Runner.Worker/ContainerOperationProvider.cs:406-450`: a per-job Docker
  network is created and service containers are health-checked.
- `src/Runner.Worker/Handlers/ContainerActionHandler.cs:193-206`: Docker action
  containers use `/github/workspace`, `/github/runner_temp`,
  `/github/file_commands`, `/github/home`, and `/github/workflow`.

Velnor should keep its current model: fresh Docker job container for every
target Linux job, shared workspace/temp/home/actions/tools mounts across job
container and any action helper containers, plus host Docker socket/CLI/Buildx
mounts for the Docker-heavy target workflows.

## Target-Repos Implementation Order

Use the target compatibility audit as the scope boundary. The fastest path to
proof is:

1. Keep the V2 broker/run-service loop aligned with latest `actions/runner`.
2. Run `ChainArgos/java-monorepo` first because it already uses
   `runs-on: hetzner-sentry-ci`.
3. Prove a small `java-monorepo` workflow in GitHub UI on a daemon-visible
   Docker work directory.
4. Prove Rust/setup/cache action jobs.
5. Prove Docker-heavy Buildx/Bake jobs with mounted host Docker socket, Docker
   CLI, and Buildx plugin.
6. Add compatible Linux labels or temporary retargeting for `jackin` Linux jobs.
   Velnor's `--target-mvp-labels` preset covers x64 labels:
   `hetzner-sentry-ci`, `ubuntu-latest`, and `ubuntu-24.04`. Add
   `--target-mvp-arm-label` only on an ARM Linux runner; macOS labels remain
   excluded because macOS runner replacement is not a Velnor target.
7. Exclude macOS `jackin` matrix legs and broad hosted-image parity.

## Current Velnor Delta After Refresh

Implemented in this refresh:

- broker session creation retries transient startup failures before giving up
- broker sessions are deleted best-effort after runner-loop errors
- broker acknowledge failure is logged and does not abort the job
- transient broker message poll failures retry with 15s/30s bounded backoff;
  repeated empty polls trigger a 15s backoff
- run-service acquire 404/409/422 is recognized as non-retriable, logged, and
  skipped with a short backoff
- `ForceTokenRefresh` rebuilds broker/run-service clients with a freshly minted
  OAuth access token
- runner refresh messages are recognized as self-update control messages and
  ignored for Phase 0
- runner config refresh and hosted shutdown messages stop the runner loop so a
  supervisor can restart or replace the process

Still open for V2 parity:

- richer live GitHub UI log/timeline behavior. Velnor now sends in-progress
  job and step timeline records during execution, uploads completed step logs
  as steps exit, then serializes telemetry and infrastructure failure category
  for implemented cases.
- live proof on the two target repositories
