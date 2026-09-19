# G0 runtime evidence

Observed 2026-09-19 UTC from the Luna/max worker against `tailrocks/velnor` at
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

## Evidence boundary

- Read-only source and upstream research. No Velnor source, protocol, host, or
  Docker changes were made.
- The authorized Mac is present. Root recorded macOS host capacity with
  `sysctl`: 18 CPUs and 128 GiB memory. This is host-capacity evidence only;
  no OrbStack/Docker command or Velnor job was operated in this turn.
- G4/G5 runtime operation is deliberately held behind the G3 full-fleet
  barrier. This is a sequencing hold, not an inaccessible-host claim. The goal
  requires actual-Mac evidence and released-package operation before those
  gates can pass (`velnor-github-first-dual-lane-goal.md:214-220,257-286`).
- Effective worker settings are directly attested in
  `/Users/donbeave/.codex-chainargos/sessions/2026/09/19/rollout-2026-09-19T23-15-32-01a0ba73-9765-7f71-bb51-badc9ceddf87.jsonl`:
  `turn_context.model=gpt-5.6-luna`, `effort=max`.

## Capability matrix

| Capability | Source evidence | G0 result |
| --- | --- | --- |
| Native Mac + OrbStack topology | OrbStack says Docker's engine runs in its Linux VM and its server socket is forwarded to macOS ([architecture](https://docs.orbstack.dev/architecture)); Docker context `orbstack` and `/var/run/docker.sock` compatibility are documented ([Docker containers](https://docs.orbstack.dev/docker/)). | Static topology confirmed by primary docs; installed version, context, endpoint, server OS/arch, image digest, and actual resources remain unmeasured until G3 clears. |
| Velnor host scope | `velnorctl host` is repository-scoped (`crates/velnorctl/src/host.rs:1-4`; `content/docs/guides/macos-host.mdx:7-15`). Startup exposes slots but no pool/global-budget option (`crates/velnorctl/src/host.rs:110-145`; `crates/velnorctl/src/commands.rs:502-528`). | Cannot claim one host process serves all 32 repositories. |
| Global admission | SQLite permit ledger provides atomic host-wide acquire/release/reconcile and uncertain retention (`crates/velnor-control/src/permit_ledger.rs:1-35,403-467,582-696`; `crates/velnor-runner/src/permit_guard.rs:38-67,181-246`). | Correct only when all instances use one canonical ledger and `max_jobs`; current `host start` defaults each instance separately (`host.rs:949-994`). Structural G4 gap. |
| Endpoint/preflight | Endpoint resolver rejects non-local/non-absolute endpoints and has explicit precedence (`crates/velnor-runner/src/docker/engine.rs:78-145,148-218`). Preflight verifies image/platform/cgroup invariants (`crates/velnor-runner/src/preflight.rs:16-148`; `execution/docker.rs:232-307`). | Static positive. Docs currently describe a different endpoint order (`content/docs/guides/macos-host.mdx:21-26`); reconcile before operation. |
| Job Docker ownership boundary | Linux uses a per-job lease proxy with labels, allowlisted routes, ownership checks, host-network/bind/device/privilege rejection, and cgroup rewriting (`crates/velnor-runner/src/docker_lease.rs:1-14,1201-1296,1478-1667`). | Linux path has a real boundary. |
| Mac Docker ownership boundary | On macOS `guest_can_connect_host_bound_unix_lease()` is false because VirtioFS exposes a socket inode but `connect()` returns `ECONNREFUSED`; code therefore mounts the resolved host daemon socket (`crates/velnor-runner/src/container.rs:1066-1100`). Lease guard is only bound when that predicate is true (`crates/velnor-runner/src/executor.rs:5946-5959`). | **G4 blocker:** trusted Mac jobs bypass per-job ownership. Must implement VM-reachable per-job endpoint/relay and prove A cannot inspect/list/control/remove B or unrelated resources. |
| Docker API risk | OrbStack's own issue confirms Unix sockets do not work across its Mac/Linux boundary and recommends `mac link docker` as workaround ([issue comment](https://github.com/orbstack/orbstack/issues/269#issuecomment-1629891534)). OrbStack documents remote unauthenticated TCP as “extremely dangerous” ([Docker engine config](https://docs.orbstack.dev/docker/#engine-config)). Docker documents that daemon credentials grant root-equivalent control ([protect daemon socket](https://docs.docker.com/engine/security/protect-access/)); Docker's authz plugin contract can allow/deny each Engine API request ([authorization plugins](https://docs.docker.com/engine/extend/plugins_authorization/)), but no OrbStack authz deployment is evidenced. | Do not expose OrbStack `tcp://0.0.0.0:2375` or mount the host socket. Any relay must be per-job, authenticated, bounded, and fail closed. |
| No per-container CPU/RAM quotas | Velnor strips CPU/RAM/PID quota inputs and relies on host-wide admission (`crates/velnor-runner/src/container.rs:64-92,267-449`). | Matches goal policy; actual OrbStack global capacity and retention still need live evidence. |
| Action execution | Closed graph admission is bounded and side-effect-free before execution (`crates/velnor-runner/src/admission.rs:1-15,35-69,493-777`). Runtime has native, JavaScript, Docker, and composite paths (`crates/velnor-runner/src/action.rs:203-283`; `executor.rs:3374-3553`). | Static capability broad; 32-repository generated action graphs and representative Mac workloads remain unrun. |
| Cancellation model | Velnor models Requested/Forced levels and registers process groups, containers, and hooks (`crates/velnor-runner/src/execution/cancel.rs:1-27,210-244,267-337`). | Design direction is correct, but production registration is incomplete: only Docker-action sidecar, job container, and services are registered (`crates/velnor-runner/src/runner.rs:9616-9655`); Node sidecar is separately named/generated (`crates/velnor-runner/src/container.rs:672-724`), and BuildKit is created/claimed separately (`executor.rs:5146-5258`). |
| Legacy cancellation route | Backend cancellation still invokes `docker rm --force <job>` directly (`crates/velnor-runner/src/execution/docker.rs:211-228`). | **Gap:** route broker, timeout, and backend cancellation through one ladder; preserve post-step semantics. |
| Recovery/cleanup | Host resume/reconcile, durable completion outbox, lease teardown, and BuildKit teardown exist (`crates/velnorctl/src/host.rs:181-344`; `content/docs/guides/execution.mdx:423-464`; `crates/velnor-runner/src/docker_lease.rs:2387-2515`; `executor.rs:5754-5797`). | Static machinery only. Restart, Docker disconnect, duplicate completion, and owned-orphan behavior need controlled G4 tests. |
| Trust | Exact trusted scope gates host Docker; fork/unknown scopes remain untrusted (`crates/velnor-runner/src/github_adapter.rs:178-189`; `content/docs/guides/macos-host.mdx:108-122`). | Static positive. Live fork/identity/credential tests required. |
| Released package (G5) | Goal requires released Homebrew install, useful job, diagnostics, drain/stop/restart/cleanup (`velnor-github-first-dual-lane-goal.md:214-220,286`). | Not operated before G3; no G5 pass claim. |

## Smallest safe Mac boundary design

The root cause is the transport boundary, not a missing path. A host Unix
socket forwarded through VirtioFS is not a guest-connectable Unix socket, while
the real OrbStack daemon socket is global. Therefore changing only the mounted
path cannot restore ownership isolation.

1. Keep OrbStack's single engine as the outer orchestration engine. Do not
   configure its daemon globally with unauthenticated TCP.
2. Extend the existing `DockerLeaseProxy` with a VM-reachable per-job
   transport. Preferred shape: a short-lived TCP or equivalent relay with a
   unique per-job endpoint and mutual authentication; the proxy remains the
   only authority that talks to OrbStack's real socket.
3. Inject only that endpoint and its job-scoped credential into trusted Docker,
   Node-action, Docker-action, services, and BuildKit clients. Untrusted jobs
   receive no Docker endpoint.
4. Keep existing label ownership and route filtering as the API policy. A
   valid job credential must authorize only its job identity; endpoint
   reachability alone must not authorize another job.
5. Close/revoke the endpoint before permit release and after all bounded
   cleanup. Transport loss is an error, never evidence that resources are
   gone.

This is the smallest coherent extension because it preserves the existing
lease policy and replaces only the Mac-incompatible transport. A separate
Docker daemon per job is a valid fallback, but is a larger design and requires
cache/network/resource evidence.

## Upstream cancellation contract (pinned source)

The `actions/runner` source-of-truth revision inspected was
`80bb1fb827fa44d489263061e71ef4adba7ad8cd` (`main`, 2026-09-19).

- `JobDispatcher.Cancel` looks up the active job by immutable job id and calls
  the worker dispatcher (`src/Runner.Listener/JobDispatcher.cs:140-159`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4adba7ad8cd/src/Runner.Listener/JobDispatcher.cs#L140-L159)).
- The worker first sends `CancelRequest`/shutdown to the worker process, then
  waits for the configured grace; on timeout it cancels the worker process and
  uploads unfinished logs before completing the job (`JobDispatcher.cs:620-711`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4ad7ad8cd/src/Runner.Listener/JobDispatcher.cs#L620-L711)).
- `WorkerDispatcher.Cancel` enforces at least 60 seconds and arms the kill
  token at `timeout - 15s` (`JobDispatcher.cs:1252-1286`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4adba7ad8cd/src/Runner.Listener/JobDispatcher.cs#L1252-L1286)).
- `ProcessInvoker` cancellation sends SIGINT, waits 7.5 seconds, sends
  SIGTERM, waits 2.5 seconds, then kills (`Runner.Sdk/ProcessInvoker.cs:32-33,331-369,443-465`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4ad7ad8cd/src/Runner.Sdk/ProcessInvoker.cs#L331-L465)).
  Current Unix fallback calls `Process.Kill()` on the invoked process
  (`ProcessInvoker.cs:855-869`), so Velnor must not claim upstream kills a
  recursive Unix process tree without separate proof.
- `StepsRunner` re-evaluates the current step on job cancellation and invokes
  `step.ExecutionContext.CancelToken()` when its condition no longer holds;
  post steps are pushed after normal steps are exhausted
  (`Runner.Worker/StepsRunner.cs:52-77,145-186`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4adba7ad8cd/src/Runner.Worker/StepsRunner.cs#L52-L186)).
- Container cleanup is registered as an `always()` post step, then removes each
  job/service container and its network (`ContainerOperationProvider.cs:57-63,144-164,329-356,417-430`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4adba7ad8cd/src/Runner.Worker/ContainerOperationProvider.cs#L57-L164)).
- Docker CLI calls use `killProcessOnCancel: false`, while cleanup uses
  `docker rm --force` (`DockerCommandManager.cs:269-272,386-414`;
  [source](https://github.com/actions/runner/blob/80bb1fb827fa44d489263061e71ef4adba7ad8cd/src/Runner.Worker/Container/DockerCommandManager.cs#L269-L414)).

Velnor's existing cancellation comments cite this contract, but its live
registration and backend route must be brought into one implementation before
runtime changes are accepted.

## Bounded acceptance tests after G3

1. **Host snapshot:** record macOS version/arch, OrbStack version, selected
   Docker context/socket, Docker server OS/arch, engine CPU/memory, Velnor
   released version/source, and job-image digest/platform.
2. **Mac transport:** inside a Docker job, verify `DOCKER_HOST` points to the
   job endpoint; direct `/var/run/docker.sock` host mount is absent or unusable;
   wrong/missing credential fails closed.
3. **A/B ownership:** pre-create unrelated sentinel resources. Jobs A and B
   each create containers, services, networks, volumes, and a BuildKit builder.
   A can inspect/remove only A resources; listing, inspecting, controlling, or
   removing B/unrelated resources fails. B remains healthy after A cleanup.
4. **Endpoint teardown:** cancellation, normal completion, proxy crash, and
   transport disconnect close the endpoint, revoke credentials, reclaim only
   owned resources, and never release another job's permit.
5. **Action coverage:** exercise checkout, JavaScript/Node sidecar, Docker
   action, services/health checks, testcontainers, Buildx/BuildKit, post steps,
   artifacts, and cache operations through the same per-job endpoint.
6. **Cancellation ladder:** cancel a workload with child processes, services,
   Node/Docker sidecars, BuildKit activity, and post steps. Record Requested
   and Forced timestamps/signals, logs, terminal status, post conditions,
   endpoint closure, permit release, and resource ownership.
7. **Recovery:** interrupt one isolated runner, restart it, interrupt Docker
   transport, and prove ledger reconciliation, bounded retries, no duplicate
   completion/publication, owned-orphan cleanup, and successful next work.
8. **Packaged G5:** repeat start/status/useful job/diagnostics/drain/stop/
   restart/cleanup with the released Homebrew product and installed startup
   configuration.

## Critical gaps for G0 records/checker

- Do not record “Mac inaccessible” or “not reachable.” Record “Mac confirmed;
  G4/G5 operation intentionally not run because G3 barrier is unresolved.”
- Mark Mac trusted nested-Docker isolation **blocked by source design** until a
  VM-reachable per-job endpoint is implemented and A/B-tested.
- Mark shared-capacity evidence **unproven** until separately started repo
  instances share one explicit ledger and host-wide `max_jobs`.
- Mark cancellation evidence **unproven** until Node sidecar/BuildKit targets
  are registered and the direct backend `docker rm --force` route is removed or
  proven equivalent to the single upstream-matched ladder.
- Treat the root's 18-CPU/128-GiB values as host facts, not proof of OrbStack
  engine limits, Docker server resources, or Velnor concurrency behavior.
