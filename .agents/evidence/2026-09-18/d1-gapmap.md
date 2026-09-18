# D1 gap map — bastion spec §5 vs existing Rust code (read-only)

Spec: `plans/bastion-three-provider-ci/spec.md` §5. Upstream contract refs: `/tmp/upstream-scout.md`.
Code surveyed: `crates/velnor-model/src/scheduler.rs`, `crates/velnor-runner/src/node/{scheduler,prove}.rs`,
plus repo-wide grep for acquire/session/JIT/ACK/queue/permit/DinD symbols.

Upstream endpoint/const summary (scout §7): `scaleSetEndpoint=_apis/runtime/runnerscalesets`
(`client.go` L25), `X-ScaleSetMaxCapacity` (L40), `api-version=6.0-preview` (L359-360);
session create/refresh/close + `GET {MessageQueueURL}?lastMessageId={n}` + `DELETE .../{messageID}`
(`session_client.go` L39/L81/L131/L181), `POST .../acquirejobs` body `requestIDs []int64`
→ `acquireJobsResponse{count, value []int64}` (L244/L262), `POST .../generatejitconfig`
(`client.go` L727), `RunnerScaleSetStatistic` (`types.go` L124-141), listener ACK-after-success
(`listener/listener.go` L126), single-method `Scale(ctx, msg)` (L121).

⚠️ Pin drift: `crates/velnor-model/src/scheduler.rs:7` pins `cb0405b2d874...`,
but the audited ref is `fb563005` (scout §7-8; spec evidence). D1 must re-pin and
re-verify — v0.4.0 vs main listener APIs differ (scout §8).

Conventions: EXISTS = live code (not tests/fixtures). Test-only helpers marked TEST.

## §5.1 step 1 — idempotent lifecycle/completion observations + reconcile durable occupied state

- EXISTS `crates/velnor-model/src/lifecycle.rs:170` `JobState` (queued/acquired/waiting/started/completed/canceled/rejected)
- EXISTS `crates/velnor-model/src/lifecycle.rs:209` `is_terminal` — terminal replay is idempotent no-op
- EXISTS `crates/velnor-model/src/lifecycle.rs:248` `transition_target` — single edge-legality table
- EXISTS `crates/velnor-runner/src/node/controller.rs:1416` reconcile durable registration vs GitHub (per-slot JIT path)
- EXISTS `crates/velnor-runner/src/node/cleanup.rs:268` outbox recovery lock; `:454`/:597 recovery passes
- GAP-1: no scale-set event ingestion at all — nothing parses `JobAvailable/Assigned/Started/Completed`
  bodies into any store; no occupied-state keyed by `runnerRequestId`; the `JobState` machine has no
  scale-set feeder (upstream: scout §7 `client.go` L627 dispatch into `RunnerScaleSetMessage`).

## §5.1 step 2 — offered eligible work to global oldest-observed queue; trust before grant

- EXISTS `crates/velnor-runner/src/trust_class.rs:87` `TrustClass::derive`, `:206` `AdmittedTrust::admit` (per-job trust)
- EXISTS `crates/velnor-runner/src/admission.rs:497` `admit_job` (action-graph admission, not queue grant)
- EXISTS `crates/velnor-runner/src/node/prove.rs:345` `routing_drift`, `:410` `GitHubProbe`, `:445` `policy_from_github_url`
- GAP-2a: no global oldest-observed queue — `first_seen_at`/`firstSeen` has ZERO hits repo-wide; no
  cross-scope/cross-engine demand ordering; redelivery-age retention impossible.
- GAP-2b: no grant step — trust derivation exists per job/slot but is not wired as a pre-grant gate
  for scale-set offers (upstream: user-owned `Scale`, scout §7 `listener.go` L121).

## §5.1 step 3 — reserve global permit + persist acquisition intent before AcquireJobs

- EXISTS `crates/velnor-runner/src/service.rs:152` static `--slots` count (fixed pre-registered slots, NOT a ledger)
- EXISTS `crates/velnor-runner/src/capacity.rs:393` `ScopeLease` — cache-store flock lease, not a job permit
- EXISTS (analogue, wrong protocol) `crates/velnor-runner/src/node/complete.rs:356` run-service acquire-intent,
  `:400` retarget, `:442` provisional rebuild — single-job run-service `acquirejob`, not batched `acquirejobs`
- GAP-3a: no host-wide `max_jobs=N` permit ledger shared by both lanes (spec §4.1).
- GAP-3b: no scale-set acquisition-intent record; no `POST .../acquirejobs` client at all
  (upstream: scout §7 `session_client.go` L244/L262).

## §5.1 step 4 — record actual returned acquired request IDs; reconcile partial/uncertain

- EXISTS (analogue, wrong protocol) `crates/velnor-runner/src/protocol.rs:2756` `AcquireJobOutcome`,
  `:2855` `RunServiceClient::acquire_job` (single-job run-service), `:2531` `acquire_reply_is_definitely_gone`
- EXISTS `crates/velnor-runner/src/node/complete.rs:576` resolve crash-window provisionals via `renewjob` oracle
- GAP-4: no `acquireJobsResponse{count, value []int64}` parsing; no returned-subset vs requested-set
  reconciliation; no partial/uncertain handling for batched acquire (upstream response shape: scout §7 L109).

## §5.1 step 5 — persist JIT/provisioning intent; idempotent runner creation via stable op/ownership IDs

- EXISTS `crates/velnor-runner/src/node/scheduler.rs:39` `scaleset_v2_generate_jit_path` (URL STRING ONLY, no HTTP call)
- EXISTS (per-slot path, not per-acquired-job) `crates/velnor-runner/src/protocol.rs:1728`
  `RegistrationClient::generate_jit_config`, `:796` `GitHubJitConfigRequest`, `:2243` `decode_jit_config`
- EXISTS `crates/velnor-runner/src/runner.rs:106` `.jit-registration-pending.json` crash marker (slot replace flow)
- GAP-5a: no scale-set `GenerateJitRunnerConfig` client, no `RunnerScaleSetJitRunnerConfig`/`EncodedJITConfig`
  types (upstream: scout §7 `client.go` L727, `types.go`).
- GAP-5b: no provisioning-intent store; no stable operation/ownership IDs for runner creation; no
  official-runner container provisioning of any kind (see §5.2).

## §5.1 step 6 — durable ACK only after replay-safe effects; offer validity/session boundaries explicit

- EXISTS (wrong protocol, classic pool) `crates/velnor-runner/src/protocol.rs:3357`
  `DistributedTaskClient::delete_message` (`pools/{id}/messages`, NOT scale-set `MessageQueueURL`)
- EXISTS (wrong protocol, broker) `crates/velnor-runner/src/protocol.rs:2678`
  `BrokerClient::acknowledge_runner_request` — best-effort Busy-marker POST, redelivered regardless (`:2700`)
- EXISTS (wrong layer) `crates/velnor-runner/src/protocol.rs:2445` `CompletionAcknowledgement`,
  `crates/velnor-runner/src/node/complete.rs:853` `ack_remote` — completion outbox, not message ACK
- GAP-6: no scale-set `DELETE {MessageQueueURL}/{messageID}` client; no ACK-after-success listener loop;
  no `lastMessageID` tracking for scale-set sessions; no offer-validity/session-boundary model
  (upstream: scout §7 `session_client.go` L131/L181, `listener.go` L126).

## §5.1 step 7 — authoritative Statistics.TotalAssignedJobs for population convergence

- EXISTS `crates/velnor-model/src/scheduler.rs:73` `RunnerScaleSetStatistic` (all 7 counters), `:86` `desired_runners()`
- TEST-ONLY `crates/velnor-runner/src/node/scheduler.rs:109` truncated-body fixture test (51 msgs vs stats=3)
- GAP-7: `desired_runners()` has NO callers outside its own test file (grep: only the two scheduler
  files + lib re-export reference any scale-set symbol); no live statistics ingestion, no convergence/
  population loop, nothing distinguishes stats truth from capped batches at runtime.

## §5.1 step 8 — per-session capacity advertisement derived from shared grants + commitments

- EXISTS `crates/velnor-model/src/scheduler.rs:12` `SCALESET_MAX_CAPACITY_HEADER` const (name only)
- EXISTS `crates/velnor-runner/src/node/scheduler.rs:45` `scaleset_v2_max_capacity_header()` (returns the const)
- GAP-8: no session advertises capacity — no scale-set `GET` message poll exists to carry the header;
  no shared-grant derivation; total-vs-free semantics unverified in code; each daemon's static `--slots`
  spends independently, exactly what the spec forbids (upstream: scout §7 L93 header on every poll).

## §5.1 step 9 — reconcile on idle polls; backoff/rate-limit/bounded calls; unknown events → reconcile

- EXISTS (broker analogue) `crates/velnor-runner/src/protocol.rs:2353` `BrokerPollClass::Empty`,
  `crates/velnor-runner/src/runner.rs:6501` long-poll drain cancel, `:2652` drain latch, `:1646` token-expiry-mid-poll
- EXISTS `crates/velnor-runner/src/protocol.rs:310` `GitHubRateLimitStatus`, `:562` `github_api_retry_delay`,
  `crates/velnor-runner/src/node/controller.rs:103` PAT-exhaustion JIT hold
- GAP-9a: no scale-set poll loop, so no idle-poll reconcile, no 202-timeout handling, no
  `refreshMessageSession` (PATCH on 401) (upstream: scout §7 L81/L91-95).
- GAP-9b: no unknown-event path — no scale-set message parsing exists to meet an unknown event.

## §5.1 cross-cutting — session lifecycle + generation fencing + App credentials

- EXISTS `crates/velnor-model/src/node.rs:291` `Generation`; `crates/velnor-runner/src/node/prove.rs:267`
  generation-bound heartbeat freshness; `crates/velnor-control/src/store/migrations.rs:636` fenced slot lifecycle
- EXISTS broker sessions: `crates/velnor-runner/src/protocol.rs:2624` create / `:2640` delete (V2 broker, id-less);
  classic pool sessions: `:3278` create / `:3300` delete (`pools/{id}/sessions`, api 5.1-preview.1)
- GAP-S: no scale-set message-session client — zero code hits for `runnerscalesets` outside the two
  scheduler files; no `createMessageSession`/`refreshMessageSession`/`deleteMessageSession`, no
  `MessageSessionClient` equivalent (upstream: scout §7 `session_client.go` L39-233).
- GAP-CRED: JIT/OAuth per-slot credentials exist (`protocol.rs:1043` `OAuthClient`, `:1185` `RunnerKeyPair`,
  `:1728` registration) but no scale-set admin-token chain (`getRunnerRegistrationToken` →
  `getActionsServiceAdminConnection` → `fetchAccessToken`, scout §7 L81) and no session
  `MessageQueueAccessToken` handling.

## §5.2 — homogeneous official workers (pinned runner image + private DinD; lifecycle)

- GAP-10 (total): case-insensitive `dind` grep over runner+model+control hits NOTHING (only `EndIndex`
  lexer noise + one unrelated microvm snapshot fixture string `execution/snapshot.rs:299`). No official
  runner image reference, no digest pinning for runner/DinD/toolchain images, no homogeneous-profile
  definition, no `observed → eligible → reserved → acquire-intent → acquired/uncertain → provision-intent
  → DinD ready → runner connected → running → terminal → diagnostic export → owned cleanup → permit
  released` state machine (existing `JobState`, `lifecycle.rs:170`, is broker/native-shaped and has no
  DinD/runner-connected/provision states). Upstream container pointers: scout §9
  (`ContainerOperationProvider.cs`, `DockerCommandManager.cs`, hooks protocol).

## §5.3 — private Docker semantics (no host socket; shared netns; bind identity; ownership; isolation)

- EXISTS (native lane only — explicitly NOT private DinD per spec §5.4): `docker_lease.rs`, `container/`,
  `docker/` mediated host-backed Docker API
- GAP-11 (total): no private DinD daemon provisioning, no Unix-socket identity proof, no runner+DinD
  shared-network-namespace pairing, no identical-absolute-path bind mapping (checkout/`_work`/`TMPDIR`/
  HOME/tool cache/file-command dirs), no recorded ownership identity for official objects, no same-inner-
  name/port collision isolation for official jobs, no pre-deletion log export for official lane.

## Gap list (return)

1. GAP-1 — scale-set event ingestion into durable occupied state (Available/Assigned/Started/Completed).
2. GAP-2a — global oldest-observed queue (`first_seen_at` = 0 hits); 2b — trust-before-grant gate.
3. GAP-3a — host-wide N permit ledger; 3b — scale-set acquire-intent + `acquirejobs` client.
4. GAP-4 — returned-ID subset recording + partial/uncertain reconcile.
5. GAP-5a — scale-set `GenerateJitRunnerConfig` client + config types; 5b — provisioning intent + stable op IDs.
6. GAP-6 — scale-set `DeleteMessage` ACK-after-success + `lastMessageID` + offer-validity model.
7. GAP-7 — `TotalAssignedJobs` convergence loop (`desired_runners()` exists but callee-less).
8. GAP-8 — shared-grant-derived per-session capacity advertisement (header const exists, no poller).
9. GAP-9 — scale-set poll loop: idle reconcile, 202/401-refresh, backoff, unknown-event path.
10. GAP-S/GAP-CRED — scale-set message-session client + admin-token chain + queue token handling.
11. GAP-10 — §5.2 homogeneous official runner + private DinD + digest pins + worker lifecycle (absent).
12. GAP-11 — §5.3 private-Docker semantics: socket/netns/binds/ownership/isolation/log-export (absent).
13. PIN-DRIFT — model pins `cb0405b`, audited ref is `fb563005`; re-pin + re-verify in D1.

What EXISTS and is reusable: scale-set model types + desired-count helper + URL/header consts
(model `scheduler.rs`, node `scheduler.rs`); broker-V2 + classic-pool + run-service protocol clients
(`protocol.rs` `BrokerClient`/`DistributedTaskClient`/`RunServiceClient`); per-slot JIT register/decode
(`protocol.rs:1728`/`:2243`, `runner.rs:106` pending marker); run-service acquire-intent/retarget/renew-
oracle durability pattern (`node/complete.rs:356-632`); `JobState` transition table with idempotent
terminal replay (`lifecycle.rs:170-266`); per-job trust derivation + admission (`trust_class.rs`,
`admission.rs:497`); generation fencing primitives (`node.rs:291`, `prove.rs:267`); rate-limit/backoff
helpers (`protocol.rs:310-347`/`:562`, `controller.rs:103`).
