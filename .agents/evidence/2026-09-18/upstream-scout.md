# Upstream protocol scout (bastion campaign, read-only research for later D1)

Inspected in /tmp only, no repo edits. All paths below are repo-relative.

- `actions/runner` @ `/tmp/upstream-runner`, HEAD `80bb1fb` (post-v2.337.0 main) + tag `v2.337.0` fetched for container pointers.
- `actions/scaleset` @ `/tmp/upstream-scaleset`, checked out at `fb563005` ("Change listener API...", #113). `v0.4.0` = `6ce02590` ("Document how to multi-label on GHES", #98).

---

## 1. actions/runner — job messages

Message envelope + queue API (classic pool queue):

- `src/Sdk/DTWebApi/WebApi/TaskAgentMessage.cs` — `TaskAgentMessage` (MessageId, MessageType, Body).
- `src/Sdk/DTWebApi/WebApi/TaskAgentMessageTypes.cs` — `TaskAgentMessageTypes` (`ForceTokenRefresh`).
- `src/Sdk/DTWebApi/WebApi/JobRequestMessageTypes.cs` — `JobRequestMessageTypes.PipelineAgentJobRequest`, `.RunnerJobRequest`.
- `src/Sdk/DTGenerated/Generated/TaskAgentHttpClientBase.cs` — `GetMessageAsync` (~L458, long poll w/ `lastMessageId`), `DeleteMessageAsync` (~L420, ACK), `CreateAgentSessionAsync` (~L744), `DeleteAgentSessionAsync` (~L773), `SendMessageAsync`, `RefreshRunnerConfigAsync` (~L840).
- `src/Sdk/DTWebApi/WebApi/TaskAgentHttpClient.cs` — `RenewAgentRequestAsync`, `FinishAgentRequestAsync`, `GetAgentsAsync`, `ReplaceAgentAsync` (extends base).
- `src/Runner.Common/RunnerServer.cs` — `IRunnerServer`/`RunnerServer`: `GetAgentMessageAsync(poolId, sessionId, lastMessageId, status, runnerVersion, os, architecture, disableUpdate)`, `DeleteAgentMessageAsync`, `CreateAgentSessionAsync`, `DeleteAgentSessionAsync`, `RenewAgentRequestAsync`, `FinishAgentRequestAsync`, `RefreshConnectionAsync`, `SetConnectionTimeout`.
- `src/Runner.Listener/MessageListener.cs` — `IMessageListener`: `CreateSessionAsync`, `GetNextMessageAsync` (poll loop, `_lastMessageId` tracked L43/L253/L288), `DeleteMessageAsync` (L420), `AcknowledgeMessageAsync` (L441), `RefreshListenerTokenAsync`; handles `BrokerMigrationMessage` inline (L264).
- `src/Runner.Listener/RunnerJobRequestRef.cs` — broker message body: `Id`, `RunnerRequestId`, `ShouldAcknowledge`, `RunServiceUrl`, `BillingOwnerId`.
- `src/Runner.Common/Util/MessageUtil.cs` — `MessageUtil.IsRunServiceJob(messageType)` (true iff `RunnerJobRequest`).
- `src/Runner.Listener/Runner.cs` (~L608–L790) — dispatch switch: `AgentRefreshMessage` (self-update), `PipelineAgentJobRequest` (body IS the `AgentJobRequestMessage`), `RunnerJobRequest` (broker: best-effort ACK → fetch job via run service → dispatch), `JobCancelMessage` → `jobDispatcher.Cancel`.
- `src/Sdk/DTPipelines/Pipelines/AgentJobRequestMessage.cs` — `AgentJobRequestMessage` (the job payload; `MessageType`, `JobContainer`, services, orchestration id).
- Other wire messages: `src/Sdk/DTWebApi/WebApi/AgentRefreshMessage.cs`, `RunnerRefreshMessage.cs`, `RunnerRefreshConfigMessage.cs`, `BrokerMigrationMessage.cs`, `JobCancelMessage.cs`.

## 2. actions/runner — broker

- `src/Runner.Common/BrokerServer.cs` — `IBrokerServer`/`BrokerServer` over `RawConnection` + `BrokerHttpClient`: `ConnectAsync`, `CreateSessionAsync`, `GetRunnerMessageAsync(sessionId, status, version, os, architecture, disableUpdate)`, `AcknowledgeRunnerRequestAsync(runnerRequestId, ...)`, `DeleteSessionAsync`, `UpdateConnectionIfNeeded`, `ForceRefreshConnection`, `ShouldRetryException`.
- `src/Sdk/WebApi/WebApi/BrokerHttpClient.cs` — `BrokerHttpClient : RawHttpClientBase`: `GetRunnerMessageAsync` (L59), `CreateSessionAsync` (L138), `DeleteSessionAsync` (L169), `AcknowledgeRunnerRequestAsync` (L187).
- `src/Runner.Listener/BrokerMessageListener.cs` — `BrokerMessageListener : IMessageListener`: same `CreateSessionAsync`/`GetNextMessageAsync`/`DeleteSessionAsync` shape as `MessageListener` but no `_lastMessageId` (broker sessions are id-less long polls); handles migrated-settings retry (`_isMigratedSettings`, `_maxMigratedSettingsRetries`).
- `src/Sdk/DTWebApi/WebApi/BrokerMigrationMessage.cs` — pool→broker migration signal consumed in `MessageListener.GetNextMessageAsync`.
- Tests: `src/Test/L0/Sdk/RSWebApi/BrokerHttpClientL0.cs`.

## 3. actions/runner — expressions

- Parse/eval SDK (legacy v1): `src/Sdk/Expressions/` — `ExpressionParser.cs`, `EvaluationResult.cs`, `FunctionInfo.cs`, `IFunctionInfo.cs`, `INamedValueInfo.cs`, `IExpressionNode.cs`, `EvaluationOptions.cs`, `ISecretMasker.cs`/`NoOpSecretMasker.cs`.
- Parse/eval SDK (v2, `GitHub.DistributedTask.Expressions2`): `src/Sdk/DTExpressions2/Expressions2/` (same file set, `Expressions2` namespace).
- Template evaluators: `src/Sdk/DTPipelines/Pipelines/ObjectTemplating/PipelineTemplateEvaluator.cs` (legacy `PipelineTemplateEvaluator`); `src/Sdk/WorkflowParser/WorkflowTemplateEvaluator.cs` (new `WorkflowTemplateEvaluator`).
- Worker entry: `src/Runner.Worker/PipelineTemplateEvaluatorWrapper.cs` — `PipelineTemplateEvaluatorWrapper : IPipelineTemplateEvaluator`, holds `_legacyEvaluator` + `_newEvaluator` (feature-flagged); `EvaluateStepIf`, `EvaluateStepContinueOnError`, `EvaluateStepDisplayName/Environment/Inputs/Timeout`, `EvaluateJobContainer`, `EvaluateJobServiceContainers`, `EvaluateJobOutput`, `EvaluateEnvironmentUrl`, `EvaluateJobDefaultsRun`, `EvaluateJobSnapshotRequest`.
- Expression data/functions: `src/Runner.Worker/ExecutionContext.cs` — `ExpressionValues` (`DictionaryContextData`: `github`/`env`/`job`/`steps`/`runner`/`strategy`/`matrix`/`needs`/`secrets`...), `ExpressionFunctions` (`IList<IFunctionInfo>`).
- Built-in functions (dual impls, legacy + new SDK): `src/Runner.Worker/Expressions/` — `AlwaysFunction.cs` (`AlwaysFunction` + `NewAlwaysFunction`), `CancelledFunction.cs`, `FailureFunction.cs`, `SuccessFunction.cs`, `HashFilesFunction.cs`.
- Callers: `StepsRunner.cs`, `ActionRunner.cs`, `JobExtension.cs`, `BackgroundStepCoordinator.cs`, `ActionManifestManager{,Wrapper}.cs`, `GlobalContext.cs` all reference `TemplateEvaluator`/`EvaluateCondition`.

## 4. actions/runner — credentials

- `src/Runner.Common/CredentialData.cs` — `CredentialData` (`Scheme`, `Data` dict) — the `.credentials` file shape.
- `src/Runner.Listener/Configuration/CredentialProvider.cs` — `ICredentialProvider`/`CredentialProvider(scheme)`: `GetVssCredentials(context, allowAuthUrlV2)`, `EnsureCredential`; `OAuthAccessTokenCredential` (`Constants.Configuration.OAuthAccessToken`).
- `src/Runner.Listener/Configuration/OAuthCredential.cs` — `OAuthCredential` (`Constants.Configuration.OAuth`): RSA-key JWT bearer client credentials (`VssOAuthJwtBearerClientCredential` + `VssOAuthCredential` ClientCredentials grant), auth-URL-v2 migration, `oauthEndpointUrl` back-compat.
- `src/Runner.Listener/Configuration/CredentialManager.cs` — `CredentialManager`: `CredentialTypes` registry, `GetCredentialProvider`, `LoadCredentials(allowAuthUrlV2)`; `GitHubRunnerRegisterToken`, `GitHubAuthResult` (`TenantUrl`, `TokenSchema`, `Token`, `UseRunnerAdminFlow`, `ToVssCredentials`).
- `src/Runner.Listener/Configuration/ConfigurationManager.cs` — `configure`/`remove` flows, server URL/auth negotiation, `.runner`/`.credentials` persistence.
- Primitives: `src/Sdk/Common/Common/Authentication/VssCredentials.cs` (`VssCredentials`); RSA key stores `RSAFileKeyManager.cs`, `RSAEncryptedFileKeyManager.cs`, `IRSAKeyManager.cs`; auth migration `src/Runner.Common/AuthMigration.cs` (`HostContext.AllowAuthMigration`, `DeferAuthMigration`).
- Listener token refresh: `MessageListener.RefreshListenerTokenAsync` / `RunnerServer.RefreshConnectionAsync`; force-refresh via `TaskAgentMessageTypes.ForceTokenRefresh`.

## 5. actions/runner — run-service (job fetch/renew/complete)

- `src/Runner.Common/RunServer.cs` — `IRunServer`/`RunServer` (run-service URL from `RunnerJobRequestRef.RunServiceUrl`, v2 creds): `ConnectAsync`, `GetJobMessageAsync(id, billingOwnerId)`, `CompleteJobAsync`, `RenewJobAsync(planId, jobId)`.
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs` — `RunServiceHttpClient : RawHttpClientBase`: `GetJobMessageAsync` (L70), `CompleteJobAsync` (L123), `RenewJobAsync` (L184). Contracts in `src/Sdk/RSWebApi/Contracts/` (`AcquireJobRequest.cs`, `CompleteJobRequest.cs`, `RenewJobRequest.cs`, `RenewJobResponse.cs`, `RunServiceError.cs`, `BrokerError*.cs`, `Telemetry.cs`, ...).
- `src/Runner.Common/ActionsRunServer.cs` — `IActionsRunServer`/`ActionsRunServer` (fallback when `RunServiceUrl` empty; server URL + v1 creds): `ConnectAsync`, `GetJobMessageAsync(id)`; HTTP client `src/Sdk/DTWebApi/WebApi/ActionsRunServerHttpClient.cs`.
- Renew loops: `JobDispatcher.RenewJobRequestAsync(IRunServer, planId, jobId, ...)` (L748, run-service jobs) vs `RenewJobRequestAsync(IRunnerServer, poolId, requestId, lockToken, orchestrationId, ...)` (L854, pool jobs); `_isRunServiceJob` set from `MessageUtil.IsRunServiceJob(jobRequestMessage.MessageType)` (L93).
- Dispatch/spawn: `JobDispatcher.Run(AgentJobRequestMessage, runOnce)` (L89) spawns `Runner.Worker` (`_workerProcessName`, L49), `Cancel(JobCancelMessage)` (L140), `WaitAsync`, `ShutdownAsync`, `RunOnceJobCompleted`.
- Service hosting: `src/Runner.Service/Windows/` (Windows service wrapper); entry scripts at repo root: `run.sh`, `run.cmd`, `run-helper.sh.template`, `config.sh`, `config.cmd`, `env.sh`, `safe_sleep.sh`; service managers in `src/Runner.Listener/Configuration/` (`ServiceControlManager.cs`, `SystemdControlManager.cs`, `OsxServiceControlManager.cs`, `WindowsServiceControlManager.cs`, `NativeWindowsServiceHelper.cs`).

## 6. actions/runner — timeline message flow (worker → job server)

- Worker produces records: `src/Runner.Worker/ExecutionContext.cs` — `_record`/`_detailRecords`, `_mainTimelineId`/`_detailTimelineId`, `InitializeTimelineRecord`, `UpdateDetailTimelineRecord`, `UpdateTimelineRecordDisplayName`; every state change funnels into `IJobServerQueue.QueueTimelineRecordUpdate`.
- Batching queue: `src/Runner.Common/JobServerQueue.cs` — `IJobServerQueue : IRunnerService, IThrottlingReporter`: `Start(jobRequest, resultsServiceOnly)`, `QueueTimelineRecordUpdate`, `QueueWebConsoleLine` (log feed lines), `QueueFileUpload`, `QueueResultsUpload`, `ShutdownAsync`, `JobRecordUpdated`, `JobTelemetries`, throttling reporter.
- Wire API: `src/Runner.Common/JobServer.cs` — `IJobServer`: `ConnectAsync(jobConnection)`, `InitializeWebsocketClient` (L143, websocket feed path), `CreateTimelineAsync`, `GetTimelineAsync`, `UpdateTimelineRecordsAsync`, `AppendTimelineRecordFeedAsync` (console lines w/ `startLine`), `AppendLogContentAsync`, `CreateLogAsync`, `CreateAttachmentAsync`, `RaisePlanEventAsync<T>(JobEvent)`, `ResolveActionDownloadInfoAsync`.
- Types: `src/Sdk/DTWebApi/WebApi/Timeline.cs`, `TimelineRecord.cs`, `TimelineRecordState.cs`, `TimelineRecordFeedLinesWrapper.cs`, `TimelineRecordLogLine.cs`, `TimelineReference.cs`, `TimelineAttempt.cs`, `TaskLog.cs`, `TaskAttachment.cs`, `JobEvent.cs`, `TaskOrchestrationPlanReference.cs`, `TaskOrchestrationOwner.cs`.
- Results service sibling: `src/Runner.Common/ResultsServer.cs`.

## 7. actions/scaleset @ fb563005 — session/message protocol

Base path + headers (`client.go`):

- `scaleSetEndpoint = "_apis/runtime/runnerscalesets"` (L25); `runnerEndpoint` for `GetRunner`/`RemoveRunner`; `HeaderScaleSetMaxCapacity = "X-ScaleSetMaxCapacity"` (L40); `api-version=6.0-preview` default query (L359–360); GitHub config URL parsing in `config.go` (`gitHubConfig`, `parseGitHubConfigFromURL`, `gitHubAPIURL`).

Registration / admin connection (`client.go`):

- `NewClientWithGitHubApp` (L167), `NewClientWithPersonalAccessToken` (L196), `NewClientWithJWTProvider` (L218, pluggable `JWTProvider` — see `jwt_provider.go`) → `newClient` (L236).
- Token chain: `getRunnerRegistrationToken` (L845) → `getActionsServiceAdminConnection` (L939) → `fetchAccessToken` (L899); `updateTokenIfNeeded` (L1061) / `actionsServiceAdminTokenSnapshot` (L1111); `actionsServiceAdminTokenExpiresAt` (L1045).
- Scale-set CRUD: `CreateRunnerScaleSet` (L541, +`ensureLabels`/`applyDefaultLabelTypes`), `GetRunnerScaleSet` (L396), `ListRunnerScaleSets` (L432), `GetRunnerScaleSetByID` (L460), `UpdateRunnerScaleSet` (L575), `DeleteRunnerScaleSet` (L607), `GetRunnerGroupByName` (L485); runners: `GetRunner` (L759), `GetRunnerByName` (L786), `RemoveRunner` (L819).

Session create/renew (`session_client.go`, `MessageSessionClient` — safe for concurrent use):

- `Client.MessageSessionClient(ctx, scaleSetID, owner, options...)` (`client.go` L698) → `createMessageSession` (`POST /{scaleSetEndpoint}/{id}/sessions`, body `{ownerName}`).
- `refreshMessageSession` (L81, mutex-guarded, stale-session short-circuit): `PATCH .../sessions/{sessionID}` on `MessageQueueTokenExpiredError` (401), then single retry. Same retry wrapper on `GetMessage`/`DeleteMessage`/`AcquireJobs`.
- `Close` (L39) → `deleteMessageSession` (`DELETE .../sessions/{sessionID}`, expect 204).
- `Session()` (L233, atomic load) exposes `MessageQueueURL`, `MessageQueueAccessToken`, `Statistics`.

Long poll + lastMessageID + capacity reporting (`session_client.go` L131 `getMessage`):

- `GET {session.MessageQueueURL}?lastMessageId={n}` (query only when `lastMessageID > 0`), headers `Accept: application/json; api-version=6.0-preview`, `Authorization: Bearer {MessageQueueAccessToken}`, `X-ScaleSetMaxCapacity: {maxCapacity}`.
- `202 Accepted` = timeout, no message → `(nil, nil)`; `200` → `parseRunnerScaleSetMessageResponse` (`client.go` L627); `401` → `MessageQueueTokenExpiredError` → refresh + retry once.
- HTTP timeout default 5 min = long-poll window: `common_client.go` `httpClientOption.defaults` (L105–106), override `WithTimeout` (L255); other options `WithRetryMax`, `WithRetryWaitMax`, `WithLogger`, `WithProxy`, `WithRootCAs`, `WithoutTLSVerify`, `WithTLSClientCertificate{,FromFile}` (mTLS).

ACK (`session_client.go` L181/L199 `DeleteMessage`/`deleteMessage`):

- `DELETE {MessageQueueURL}/{messageID}` (+ `Content-Type: application/json`, Bearer, UA); expect `204`; 401 → refresh + retry once. Doc: "acts as an acknowledgment"; un-ACKed messages are redelivered.

Message envelope + JobAvailable (`types.go`, `client.go` L627):

- `runnerScaleSetMessageResponse{messageId, messageType=="RunnerScaleSetJobMessages", body, statistics}`; `body` is a JSON array of batched job messages dispatched on `JobMessageType.messageType` into `RunnerScaleSetMessage{MessageID, Statistics, JobAvailableMessages, JobAssignedMessages, JobStartedMessages, JobCompletedMessages}`.
- `MessageType` consts (L14–19): `JobAvailable`, `JobAssigned`, `JobStarted`, `JobCompleted`.
- `JobAvailable{AcquireJobURL; JobMessageBase}`; `JobMessageBase` (L47): `runnerRequestId`, `repositoryName`/`ownerName`, `jobId`, `jobWorkflowRef`, `jobDisplayName`, `workflowRunId`, `eventName`, `requestLabels`, `queueTime`, `scaleSetAssignTime`, `runnerAssignTime`, `finishTime`. `JobStarted`/`JobCompleted` add `runnerId`/`runnerName` (+`result` on completed).

AcquireJobs (`session_client.go` L244/L262):

- `POST /{scaleSetEndpoint}/{id}/acquirejobs` via `innerClient.newActionsServiceRequest` but authorized with the session's `MessageQueueAccessToken` (not the admin token); body = JSON array of `requestIDs []int64`; response `acquireJobsResponse{count, value []int64}` — acquired subset returned.

JIT config (`client.go` L727, `types.go`):

- `GenerateJitRunnerConfig(ctx, *RunnerScaleSetJitRunnerSetting{Name, WorkFolder}, scaleSetID)` → `POST .../{id}/generatejitconfig` → `RunnerScaleSetJitRunnerConfig{Runner *RunnerReference, EncodedJITConfig string}` ("encoded configuration that can be used to directly start a new runner").

Statistics / TotalAssignedJobs semantics (`types.go` L124–141):

- `RunnerScaleSetStatistic{totalAvailableJobs, totalAcquiredJobs, totalAssignedJobs, totalRunningJobs, totalRegisteredRunners, totalBusyRunners, totalIdleRunners}` — attached to every message/session; `TotalAssignedJobs` is the desired-runner-count signal (old listener passed it to `HandleDesiredRunnerCount`; example scaler still does: `examples/dockerscaleset/scaler.go` L61).

Listener loop (`listener/listener.go` @ fb563005):

- `Config{ScaleSetID, MaxRunners, Logger}` + `Validate`; `Client` interface (`GetMessage`, `DeleteMessage`, `AcquireJobs`, `Session`); `New(client, config)`; `SetMaxRunners` (atomic, safe mid-run); `InitialMessageID = -1` synthetic first message.
- `Scaler` interface (L121): single method `Scale(ctx, *RunnerScaleSetMessage) error` — must handle nil message (long-poll timeout), initial message (stats only), and call `AcquireJobs` itself for wanted `JobAvailable`s.
- `Run` (L126): sends initial session statistics → loop `GetMessage(lastMessageID, maxRunners)` → `Scale(msg)` → on success `lastMessageID = msg.MessageID` then ACK via `DeleteMessage(WithoutCancel(ctx))`; any error stops the listener WITHOUT ack → redelivery. `Scale` never concurrent.
- Errors: `errors.go` — `MessageQueueTokenExpiredError`, `RunnerNotFoundError`, `RunnerExistsError`, `JobStillRunningError`, top-level `BadRequest/NotFound/Unauthorized/ConflictError`, `newRequestResponseError` (activity/github-request IDs), `wrapResponseErrorType`.

## 8. actions/scaleset — v0.4.0 (6ce02590) → fb563005 API differences

`git log 6ce02590..fb563005` (13 commits): #113 (listener API), #117 (pluggable JWT), #120 (mutex), #119 (test certs), #92 (`ListRunnerScaleSets`), #110 (omitempty on embedded structs), #102 (credential validation), #111 (lock contention), #109 (deps), #86 (top-level HTTP errors), #101 (mTLS), #93 (Go bump).

- Listener (breaking, #113): `Scaler` was 3 methods — `HandleJobStarted`, `HandleJobCompleted`, `HandleDesiredRunnerCount(ctx, count) (int, error)` — and the listener OWNED the flow: ACK-first (`DeleteMessage` before handling), auto-`AcquireJobs` for ALL `JobAvailable` (`acquireAvailableJobs`), `latestStatistics` tracking with nil-message `HandleDesiredRunnerCount` calls, and a `MetricsRecorder` option (`RecordStatistics/RecordJobStarted/RecordJobCompleted/RecordDesiredRunners`, `WithMetricsRecorder`, `New(..., options ...Option)`). New API: single `Scale(ctx, msg)`, user acquires, ACK-after-success, `New(client, config)` (no options), no metrics recorder, `InitialMessageID` const added. Old code: `git show 6ce02590:listener/listener.go` (L139–275).
- Auth (#117, additive): `jwt_provider.go` + `NewClientWithJWTProvider(config, provider, options...)` for KMS/HSM signing; (#102) `GitHubAppAuth.Validate` / `actionsAuth.validate` on construction.
- Client (additive): `ListRunnerScaleSets` (#92); mTLS `WithTLSClientCertificate{,FromFile}` (#101); `WithTimeout` existed already (long-poll tuning unchanged).
- Errors (#86): new top-level `BadRequest/NotFound/Unauthorized/ConflictError` + `wrapResponseErrorType`; `newRequestResponseError` shape changed.
- Types (#110): `RunnerScaleSet.RunnerSetting`/`CreatedOn` lost `omitempty` (now always serialized); wire otherwise identical.
- `session_client.go`: refresh-mutex fix (#120), lock reduction (#111) — same endpoints/methods.
- Example updated to new contract: `examples/dockerscaleset/scaler.go` — `Scale` calls `AcquireJobs` then `HandleDesiredRunnerCount(TotalAssignedJobs)` then `GenerateJitRunnerConfig` per acquired job.

## 9. Official runner v2.337.0 — container behavior pointers

Verified at tag `v2.337.0` (`src/runnerversion` = `2.337.0`): `git diff v2.337.0..HEAD -- src/Runner.Worker/Container src/Runner.Worker/ContainerOperationProvider.cs` is EMPTY, so these HEAD paths are exactly v2.337.0's:

- `src/Runner.Worker/ContainerOperationProvider.cs` — `IContainerOperationProvider`: `StartContainersAsync` (L46: plain-docker path vs container-hooks path via `ACTIONS_RUNNER_CONTAINER_HOOKS` / `FeatureManager.IsContainerHooksEnabled`; `MountWellKnownDirectories` for job container; `RunContainersHealthcheck` for services), `StopContainersAsync` (L144).
- `src/Runner.Worker/Container/ContainerInfo.cs` — `ContainerInfo(hostContext, JobContainer, isJobContainer=true, networkAlias)`: `ContainerName` (= alias), `IsJobContainer`, `ContainerNetworkAlias`, mounts/env/ports/options mapping from `Pipelines.JobContainer`.
- `src/Runner.Worker/Container/DockerCommandManager.cs` — `IDockerCommandManager`: `DockerVersion/Pull/Build/Create/Run/Start/Remove/Logs/PS/NetworkCreate/NetworkRemove/NetworkPrune/Exec/Inspect/Port/Login` (+`DockerPath`, `DockerInstanceLabel`).
- `src/Runner.Worker/Container/DockerUtil.cs` — docker helpers.
- `src/Runner.Worker/Container/ContainerHooks/` — `ContainerHookManager.cs` (`PrepareJobAsync`, `RunContainerStepAsync`, `RunScriptStepAsync`, `CleanupJobAsync`), `HookInput.cs`, `HookResponse.cs` — the hook-protocol (used by ARC/k8s-style runners instead of direct docker).
- Flags/constants: `src/Runner.Common/Constants.cs` — `Hooks.ContainerHooksPath = "ACTIONS_RUNNER_CONTAINER_HOOKS"` (L280), `...RequireJobContainer = "ACTIONS_RUNNER_REQUIRE_JOB_CONTAINER"` (L312), `AllowRunnerContainerHooks = "DistributedTask.AllowRunnerContainerHooks"` (L170, server feature flag via `FeatureManager`).
- Step execution in containers: `src/Runner.Worker/Handlers/StepHost.cs`, `ScriptHandler.cs`, `ContainerActionHandler.cs`, `HandlerFactory.cs`; job container shape `GitHub.DistributedTask.Pipelines.JobContainer` (`src/Sdk/DTPipelines/...`), evaluated by `PipelineTemplateEvaluatorWrapper.EvaluateJobContainer/EvaluateJobServiceContainers`.
