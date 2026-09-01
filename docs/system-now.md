# System now

Code-backed snapshot of the current Velnor runtime. Citations use `path:line` and only cover the requested source set. “Design-only” means this code does not establish the guarantee.

## Shape and ownership

The workspace has 12 members. The complete model/control/client/render/runner,
action, storage, cache, tooling, and collector map is in
[architecture](architecture.md#components-and-dependency-direction). The
runner is the self-hosted Actions runtime; its public boundary exposes
configuration, execution, node, protocol, runner, and service modules. Its
migration scaffold is explicitly temporary, not a compatibility promise.
(`Cargo.toml:1-19`, `crates/velnor-model/src/lib.rs:1-5`,
`crates/velnor-runner/src/lib.rs:1-63`)

> Navigation: [← Architecture](architecture.md) · [Index](index.md) · [Next: Execution now →](execution-now.md)

`velnor-runner` exposes the daemon entrypoint through `main`; the service entrypoint owns machine-invoked daemon plumbing, while operator commands live in `velnorctl`. Service roles are guardian, controller, slot, and transient job. (`crates/velnor-runner/src/main.rs:4-20`, `crates/velnor-runner/src/service.rs:1-12`, `crates/velnor-runner/src/service.rs:35-51`)

Runtime topology:

```text
velnor-runner main
  └─ service::execute
      └─ daemon
          └─ controller (per scope)
              ├─ slot-N (one OS process per ready slot)
              │   └─ job (transient worker per acquired job)
              └─ GitHub JIT registration / routing / completion effects
```

The normal daemon owns the controller, slot, and job children. The package also
ships standalone `velnor-controller@`, `velnor-slot@`, and `velnor-job@`
systemd units; those direct entrypoints are a separate operator topology and
must not be confused with the daemon-managed child lifecycle.
(`crates/velnor-runner/debian/velnor-controller@.service:19-20`,
`crates/velnor-runner/debian/velnor-slot@.service:19`,
`crates/velnor-runner/debian/velnor-job@.service:16`)

The process boundary is intentional: the daemon `JoinSet` is not the availability boundary; each ready slot is a child process. The controller owns desired capacity, permits, and slot/job children. (`crates/velnor-runner/src/node/mod.rs:1-7`, `crates/velnor-runner/src/node/controller.rs:1-5`, `crates/velnor-runner/src/node/controller.rs:236-250`)

## Startup and readiness

1. `main` builds a multi-thread Tokio runtime, invokes `service::execute`, then shuts down with a five-second timeout. (`crates/velnor-runner/src/main.rs:4-20`)
2. `daemon` validates slot arguments and execution mode. Supervised mode requires a URL, is not `--once`, and is not a dry run; failed daemon passes retry with backoff. (`crates/velnor-runner/src/runner.rs:2049-2147`)
3. One daemon pass initializes state/configuration, reserves capacity permits, runs preflight, writes execution configuration, and starts controller supervision. (`crates/velnor-runner/src/runner.rs:2150-2258`)
4. The controller reconciles local desired state and remote registration on a bounded cycle. It ingests surviving heartbeats before permit reconciliation, proves the selected executor, applies registration intent, and spawns slot waiters. (`crates/velnor-runner/src/node/controller.rs:632-837`)
5. Slot heartbeats are atomic, generation-bound files. The controller rejects stale, old-sequence, or too-old heartbeats. (`crates/velnor-runner/src/node/slot.rs:33-61`, `crates/velnor-runner/src/node/controller.rs:2156-2246`)

Readiness is therefore a conjunction of local lifecycle state, executor proof, runner registration, and a live slot child—not merely a successful process start. (`crates/velnor-runner/src/node/controller.rs:632-837`)

## Control-state persistence boundary

The SQLite store durably backs lifecycle state and event history. Telemetry is a
separate instance-scoped JSONL file; it is not stored in SQLite.
`ApplicationServices::with_store` currently creates fresh in-memory query, log,
and storage projections; reopening the daemon does not repopulate those three
services. This is implemented behavior and an open control-plane limitation,
not a promise that every resource or log survives a restart.
(`crates/velnor-control/src/application.rs:30-52`,
`crates/velnor-control/src/application.rs:220-254`,
`crates/velnor-control/src/telemetry.rs:21-29`)

## Registration and polling

Production scheduling is per-slot GitHub JIT V2. The Scale Set helper is fixture-only and cannot be selected as the live scheduler. (`crates/velnor-runner/src/node/scheduler.rs:1-12`, `crates/velnor-runner/src/node/scheduler.rs:57-62`)

Each slot requests a JIT config with its name, runner group, labels, and no work-folder override. The protocol requires a successful `201` response and deliberately does not auto-repeat a POST after a lost response or server error; recovery belongs to the pending-marker/supervisor path. (`crates/velnor-runner/src/node/scheduler.rs:14-35`, `crates/velnor-runner/src/protocol.rs:1133-1159`)

The V2 slot path validates `ServerUrlV2`, creates broker and run-service clients, creates a task-agent session, then polls. Empty polls back off after the configured threshold; transport/auth failures are retried or force client refresh according to error class. (`crates/velnor-runner/src/runner.rs:4289-4585`, `crates/velnor-runner/src/runner.rs:4937-5036`)

Broker responses have three states: message, empty, or error. Unauthorized/not-found responses are errors, not idle polls. Job acquisition is bounded to five attempts; transient acquisition failures retain the session for retry. (`crates/velnor-runner/src/protocol.rs:1694-1767`, `crates/velnor-runner/src/protocol.rs:1949-1984`, `crates/velnor-runner/src/protocol.rs:1986-2094`)

## Slot and job ownership

The controller starts a slot with state directory, scope, index, and generation. A job worker must match the slot’s generation and assignment; stale or unknown identities are rejected before execution. (`crates/velnor-runner/src/node/controller.rs:2208-2308`, `crates/velnor-runner/src/node/job.rs:44-109`)

The runner claims a plan/job pair with an on-disk flock because GitHub may duplicate delivery. It persists an in-flight marker with atomic replacement before continuing admission. (`crates/velnor-runner/src/runner.rs:200-277`, `crates/velnor-runner/src/runner.rs:279-355`)

Registration, acquire, renewal, execution, completion, and cleanup are ordered. Admission is persisted before renewal, leases, checkout, downloads, containers, or credentials. (`crates/velnor-runner/src/runner.rs:5353-5609`)

After a successful job, the slot prewarms a successor where possible. Ephemeral JIT identity is consumed after one job; the slot promotes the prepared successor or obtains a fresh JIT config. Local runner failures preserve registration with per-slot backoff; remote deletion triggers cleanup and reconfiguration. (`crates/velnor-runner/src/runner.rs:3000-3068`, `crates/velnor-runner/src/runner.rs:3071-3135`, `crates/velnor-runner/src/runner.rs:2522-2790`)

## Drain, restart, and failure boundaries

Signals move the daemon to draining/stopping. Busy workers are allowed to finish; idle slots deregister and exit. Controller child drain first sends `SIGTERM`, preserves active assigned/running/completing jobs, then uses bounded `SIGKILL` escalation. (`crates/velnor-runner/src/runner.rs:1938-2046`, `crates/velnor-runner/src/node/controller.rs:533-598`)

Controller restart is designed not to stop existing workers; surviving heartbeats are ingested and fenced by generation. A controller `--once` cycle leaves child processes running before it exits. (`crates/velnor-runner/src/node/controller.rs:1-5`, `crates/velnor-runner/src/node/controller.rs:390-449`)

If remote registration disappears, the controller records registration loss and shuts down dependent slot workers. Missing registered config/token state fails closed. (`crates/velnor-runner/src/node/controller.rs:1079-1201`)

## Explicit limits and design-only notes

- Backend selection is explicit in `execution.toml`; missing config fails closed. Docker and microVM are separate paths; there is no microVM-to-Docker fallback. (`crates/velnor-runner/src/execution/mod.rs:121-203`)
- Controller remote reconciliation has a fifteen-second budget; JIT registration is capped at four concurrent operations and paced/backed off. (`crates/velnor-runner/src/node/controller.rs:30-82`, `crates/velnor-runner/src/node/controller.rs:839-872`, `crates/velnor-runner/src/node/controller.rs:946-1077`)
- Self-update is disabled in the V2 message path; configuration refresh requests shutdown/restart handling. (`crates/velnor-runner/src/runner.rs:4495-4555`, `crates/velnor-runner/src/runner.rs:5038-5282`)
- The runner library calls its current migration scaffold temporary. No compatibility or legacy-path guarantee follows from this document. (`crates/velnor-runner/src/lib.rs:58-63`)
- Design-only / not established here: HA across multiple controllers, arbitrary active-job crash resume, complete GitHub feature parity, and production support for the fixture Scale Set scheduler. The inspected code proves the bounded process, registration, and execution paths above—not those guarantees.
