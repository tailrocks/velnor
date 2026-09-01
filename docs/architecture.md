# Velnor architecture

Audience: engineers and operators who know Linux, GitHub Actions, and basic
Rust, but do not yet know this repository. Read this explanation before the
[reference](interface-reference.md) or [operator guide](operator-now.md).

Status: current implementation map, inspected against the source tree on
2026-09-01. A statement marked design-only is not a shipped guarantee.

> Navigation: [← Glossary](glossary.md) · [Index](index.md) · [Next: System now →](system-now.md)

## What Velnor is responsible for

GitHub remains the scheduler, workflow UI, repository authority, and source of
the job message. Velnor is the node-local execution system around one or more
ephemeral GitHub runner identities. It owns:

1. JIT runner registration and Runner V2 broker/run-service communication.
2. Admission: validate the full job/action closure and capabilities before
   checkout, credentials, cache mutation, service startup, or container work.
3. A backend-neutral execution plan and exactly one selected executor: Docker
   or jailed Firecracker microVM.
4. Node supervision, per-slot process isolation, generation fencing, cleanup,
   and bounded recovery.
5. Sanitized control-plane resources, lifecycle events, bounded logs/telemetry,
   action journaling, leases, and storage accounting. The current bundle
   durably wires lifecycle/events/telemetry; query, raw-log, and storage
   projections are still in-memory until their normalized readers land.

The daemon is not a second workflow scheduler. It does not invent jobs when
GitHub has no broker message, and it does not silently fall back to another
backend or hosted runner.

## Components and dependency direction

```text
                    GitHub Actions / GitHub REST
                               │
                    velnor-runner protocol + runner
                               │
               ┌───────────────┴────────────────┐
               │                                │
        admission / plan                 node supervision
               │                                │
        execution backend ────────┬──── Docker
               │                  └──── Firecracker + guest agent
               │
        action journal ── cache service ── CAS
               │
        control store / services ── local Unix API ── velnorctl
               │
        shared versioned model ── renderers / client / control
```

Workspace ownership is deliberately layered:

| Layer | Crates | Rule |
| --- | --- | --- |
| Shared contracts | `velnor-model`, `velnor-action-model` | I/O-free types. `velnor-model` owns versioned resources and DTOs; action model owns physical-action identity. |
| Durable physical state | `velnor-cas`, `velnor-action-journal`, `velnor-cache-service` | Content integrity, crash-safe actions, leases, consumers, and cache reuse are separate concerns. |
| Control plane | `velnor-control`, `velnor-client`, `velnor-render` | Application services, transport contract, and output formatting remain independent. |
| Product surfaces | `velnorctl`, `velnor-runner`, `velnor-tools`, `unit-collector` | CLI/adapters, daemon/runtime, maintainer audits, and build evidence. |

The workspace manifest lists the 12 members. Shared crates do not depend on
Clap or Axum; the client does not depend on daemon internals; the runner facade
is explicitly transitional. See `Cargo.toml:1-19`,
`crates/velnor-model/src/lib.rs:1-15`,
`crates/velnor-client/src/lib.rs:1-14`, and
`crates/velnor-runner/src/lib.rs:1-63`.

## Process topology

```text
systemd
  ├─ guardian                 health/unit supervision; no GitHub or Docker
  └─ velnor-runner daemon     one scope; controller and lifecycle owner
       └─ controller           desired capacity, permits, registrations
            ├─ slot-0          one OS process per ready slot
            │    └─ job         transient process per acquired job
            ├─ slot-1
            └─ ...
```

The controller deliberately does not make a slot a Tokio task-only boundary.
Slot and job children survive controller restart unless explicitly drained;
heartbeats carry generation and sequence, so stale children cannot prove
current readiness. The service command surface defines `guardian`,
`controller`, `slot`, and `job` roles in
`crates/velnor-runner/src/service.rs:35-51`; node ownership is documented in
`crates/velnor-runner/src/node/mod.rs:1-17` and
`crates/velnor-runner/src/node/controller.rs:1-5`.

Readiness is a proof, not a process-state shortcut: local state, selected
backend preflight, runner registration, and fresh slot heartbeat must all hold.
The controller bounds remote reconciliation, JIT concurrency, heartbeat age,
and child drain; see `crates/velnor-runner/src/node/controller.rs:20-82`.

## Job data flow

```text
broker poll
   │ RunnerJobRequest reference
   ▼
decode job message
   │ plan/job identity, variables, masks, actions, steps, services, resources
   ▼
claim (plan_id, job_id) + atomic in-flight marker
   ▼
admission
   │ complete action graph, refs/SHA, inputs, capabilities, limits, backend
   ▼
renew lease + acquire capacity
   ▼
ValidatedPlan
   │ backend-neutral steps, env, services, workspace, cache/artifact operations
   ▼
reserve → prepare → start → execute/cancel → collect → teardown
   ▼
timeline/result upload + completion + local journal/control events
```

Admission is intentionally before side effects. A duplicate broker delivery is
fenced by the on-disk claim. Once admitted, both executors consume the same
`ValidatedPlan`; only the sandbox, transport, process, filesystem, network, and
cleanup implementation changes. See
`crates/velnor-runner/src/runner.rs:200-355`,
`crates/velnor-runner/src/runner.rs:5285-6059`, and
`crates/velnor-runner/src/execution/backend.rs:30-79`.

## Executor boundary

The execution file must contain `[execution] backend = "docker"` or
`"microvm"`. Missing or unknown values fail closed. The common phase machine
is:

```text
New → Preflighted → Reserved → Prepared → Started → Executing
  → Stopped → Collecting → TornDown
```

Docker uses the host Docker socket and a systemd cgroup boundary. Firecracker
uses KVM, a verified artifact set, a jailer, per-job writable storage, guest
Docker, per-VM network resources, and a bounded vsock protocol. The microVM
path rejects the host Docker socket. There is no automatic Docker/microVM
fallback. See `crates/velnor-runner/src/execution/mod.rs:121-204`,
`crates/velnor-runner/src/execution/backend.rs:1-28`, and
`crates/velnor-runner/src/execution/isolation.rs:1-112`.

This is an isolation choice, not a claim of equivalent security: the Docker
path is a trusted host-Docker boundary; the microVM path is the stronger
boundary when host-Docker access is unacceptable. Kernel, KVM, image, host
hardening, and secure deletion claims remain outside the source-level proof.
See [security and data](security-and-data.md).

## Control-plane data flow

```text
runner/control services
      │ sanitized resources + durable events
      ▼
        /var/lib/velnor/state.db     SQLite/WAL lifecycle/event state
      │
      ├─ /run/velnor/<instance>/control.sock  GET queries/watch/logs/telemetry
      └─ /run/velnor/<instance>/admin.sock    POST lifecycle mutations
                                                (currently reserved)
      ▲
velnorctl → version negotiation → typed DTOs → renderer
```

The model layer is the source of truth for resource shape; tables and output
formats are views. Control events and state are sanitized and bounded. Raw job
messages, credentials, masks, and raw logs do not belong in the operational
database. At present, `ApplicationServices::with_store` reconstructs the
query, log, and storage services empty; only the event stream, lifecycle
service, and process telemetry are wired to durable paths. This is an important
implementation boundary, not a retention guarantee. See
`crates/velnor-control/src/application.rs:30-52,220-254`, the
[interface reference](interface-reference.md), and
[security and data](security-and-data.md).

## Why these design decisions exist

| Decision | Reason | Consequence |
| --- | --- | --- |
| GitHub owns scheduling | Preserve GitHub Actions semantics and avoid a second source of truth. | Broker/run-service behavior must track `actions/runner`; no local queue can replace GitHub. |
| Validate before side effects | Remove the enabling condition for unsafe action/capability execution. | Unsupported or malformed jobs fail before checkout, secrets, containers, and cache mutation. |
| Explicit executor selection | Make the trust boundary visible and reproducible. | Configuration failure stops the job; there is no silent fallback. |
| Process-per-slot and generation fencing | Keep worker availability independent from controller lifetime and reject stale ownership. | Restart and drain are bounded protocols, not task cancellation alone. |
| Typed shared model plus thin transport | Keep domain contracts usable by daemon, CLI, client, and renderers without framework coupling. | Version/schema negotiation is explicit; handlers cannot invent parallel DTOs. |
| Sanitized operational projections | Make recovery and operations inspectable without turning the control DB into a secret or log vault. | Lifecycle/events are durable today; query/log/storage projections still need normalized readers before they survive service recreation. |
| Plan-first reconciliation | Make destructive/expensive operations reviewable and idempotent. | A GC or lifecycle mutation needs reason, identity/version, and exact confirmation. |
| Independent release identity | Prevent a package from mixing binaries, manifests, and microVM payloads from different builds. | Package hooks verify coherence before service activation. |

These are implementation-backed choices, not promises about all future plans.
The current proof boundaries are recorded in
[runtime checking](runtime-checking-proof.md) and
[future direction](future-direction.md).

## Where to read the implementation

| Question | Start at |
| --- | --- |
| How does a daemon become ready? | `crates/velnor-runner/src/runner.rs`, `crates/velnor-runner/src/node/controller.rs`, `docs/system-now.md` |
| How is a job admitted? | `crates/velnor-runner/src/job_message.rs`, `crates/velnor-runner/src/admission.rs`, `crates/velnor-runner/src/plan.rs`, `docs/execution-now.md` |
| How are Actions steps executed? | `crates/velnor-runner/src/executor.rs`, `crates/velnor-runner/src/workflow_command.rs`, `crates/velnor-runner/src/script_step.rs` |
| How are Docker and microVM kept separate? | `crates/velnor-runner/src/execution/backend.rs`, `crates/velnor-runner/src/execution/docker.rs`, `crates/velnor-runner/src/execution/firecracker.rs` |
| How do operators inspect state? | `velnorctl`, `velnor-client`, `velnor-control`, `docs/interface-reference.md` |
| What is retained or redacted? | `crates/velnor-control/src/store`, `crates/velnor-runner/src/slot_log.rs`, `crates/velnor-runner/src/telemetry.rs`, `docs/security-and-data.md` |
| What does CI prove? | `.github/workflows`, `scripts`, tests, `docs/development-now.md` |
