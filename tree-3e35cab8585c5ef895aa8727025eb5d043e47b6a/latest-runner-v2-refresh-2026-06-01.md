# Latest GitHub Runner V2 Refresh

Date: 2026-06-01

Reference source:

- `actions/runner` latest release checked: `v2.334.0`
- tag commit: `f1995ede5d885c997d13d8eca5467c4ce97fe69c`
- `main` at check time: `c6a124e18496a6e5d2357415052d1799afc64b63`
- release page: <https://github.com/actions/runner/releases/tag/v2.334.0>

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
- Create one broker session per runner process and run one active GitHub job at
  a time.
- Poll broker messages with the runner status that reflects worker state:
  `Online` when idle and `Busy` while a job is running.
- Treat broker acknowledge as best-effort. Acknowledge failure must not prevent
  acquiring or executing the job.
- For `acquirejob`, treat HTTP 404, 409, and 422 as non-retriable stale or
  unusable messages; log, back off, and continue polling.
- Use the job-scoped `SystemVssConnection` token from the acquired job message
  for renew/complete when available.
- Complete V2 jobs through `completejob`, including job outputs and step
  results. UI-grade annotations and environment URL remain an implementation
  gap for full parity.
- Treat `ForceTokenRefresh`, runner refresh/update, cancellation, hosted
  shutdown, and broker migration as control-plane messages, not user steps.

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
container, JavaScript sidecars, and Docker action containers, plus host Docker
socket/CLI/Buildx mounts for the Docker-heavy target workflows.

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
7. Defer macOS `jackin` matrix legs and broad hosted-image parity.

## Current Velnor Delta After Refresh

Implemented in this refresh:

- broker acknowledge failure is logged and does not abort the job
- run-service acquire 404/409/422 is recognized as non-retriable, logged, and
  skipped with a short backoff
- `ForceTokenRefresh` rebuilds broker/run-service clients with a freshly minted
  OAuth access token
- runner refresh messages are recognized as self-update control messages and
  ignored for Phase 0
- runner config refresh and hosted shutdown messages stop the runner loop so a
  supervisor can restart or replace the process

Still open for V2 parity:

- richer run-service completion payload: annotations, environment URL,
  telemetry, and infrastructure failure category
- live proof on the two target repositories
