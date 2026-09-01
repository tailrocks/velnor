# Velnor

Velnor is a Linux GitHub Actions runner and control plane. It receives jobs
from GitHub, checks trust and capability requirements before side effects, and
executes admitted jobs in exactly one selected backend: Docker or Firecracker
microVM.

GitHub remains the scheduler and source of truth. Velnor supplies ephemeral
runner slots, admission, execution, supervision, and bounded local operational
state.

Velnor is a research project. Expect breaking changes. Current documentation
describes implemented behavior; it is not a production-readiness claim.

## Start here

New to Velnor? Follow this path:

1. [Install and run the first job](getting-started.md)
2. [Operate a node](guides/operator.md)
3. [Observe health, jobs, logs, events, and telemetry](operations/observability.md)
4. [Diagnose failures by lifecycle stage](getting-started.md#if-the-first-job-does-not-run)
5. [Understand the architecture and job flow](concepts/architecture.md)
6. [Check supported interfaces and known limits](reference/interface.md)

## Choose by task

| Need | Read |
| --- | --- |
| Install, configure, register, and run a first workflow | [Getting started](getting-started.md) |
| Configure labels, scope, slots, and backend | [Operator guide](guides/operator.md) |
| Understand GitHub communication and execution | [Architecture](concepts/architecture.md), [Execution](guides/execution.md), [Integrations](reference/integrations.md) |
| Inspect a running node | [Observability](operations/observability.md), [CLI/interface reference](reference/interface.md) |
| Investigate a failed or stuck job | [Getting started diagnosis](getting-started.md#if-the-first-job-does-not-run), [Execution](guides/execution.md) |
| Understand credentials, isolation, and persisted data | [Security and data](operations/security-and-data.md) |
| Look up commands and protocol details | [Interface reference](reference/interface.md), [Runner protocol](reference/runner-protocol.md) |
| Build, test, or maintain Velnor | [Development](guides/development.md), [Contributing](guides/contributing.md) |

## Current boundaries

- The packaged runtime targets Debian hosts and systemd.
- A backend is explicit. Docker and microVM do not fall back to each other.
- Capability admission is strict and cannot be bypassed.
- The operator PAT registers runners and accesses GitHub management APIs. Job
  credentials come from the acquired GitHub job and are not substituted with
  the PAT.
- The control socket exposes reads, events, logs, and telemetry. Lifecycle
  mutation routes, reconciliation actions, and diagnostics bundles are not
  currently available through the public control interface.
- Local query and log projections are bounded and some projections are not
  rebuilt after reopening the service. Treat GitHub as authoritative for job
  state and use local events, files, and service logs as operational evidence.

## Documentation map

The remaining pages are organized for operators and maintainers:

- [Concepts](concepts/): architecture, system behavior, and vocabulary.
- [Guides](guides/): execution, operations, development, and contribution.
- [Operations](operations/): observability and security/data boundaries.
- [Reference](reference/): integrations, interfaces, and runner protocol.
