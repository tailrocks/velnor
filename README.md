# Velnor

Velnor is a Rust self-hosted GitHub Actions runner and node-local control
plane. GitHub remains the workflow scheduler and source of job truth. Velnor
receives runner jobs, validates the complete action graph before side effects,
executes the admitted plan in one explicitly selected backend, and retains
bounded operational evidence for operators.

> Research project: the implementation contains incomplete and live-gated
> areas. Read the status labels in `docs/` before treating a capability as
> supported or secure.

## Vision and boundary

Velnor is for engineers operating self-hosted GitHub Actions capacity who need
clear control over job admission, execution trust, node lifecycle, and bounded
evidence. It addresses the gap between “a runner process is alive” and “this
job ran through a known, inspectable path.”

Success means a delivered job has an explicit identity, passes complete-plan
admission before execution side effects, runs in the selected backend, reports
completion, and leaves enough bounded evidence to explain the result. Velnor is
not a second GitHub scheduler, a universal GitHub Actions implementation, or a
claim of secure isolation merely because a backend test passes.

## Start here

The complete newcomer route is maintained in the
[documentation index](docs/index.md). Read that route in order; the shorter
list below shows its main milestones.

1. [Documentation index](docs/index.md) — reading paths, document ownership,
   and the `CURRENT`/`FUTURE` evidence vocabulary.
2. [Architecture](docs/architecture.md) — responsibilities, boundaries,
   data flow, and design decisions.
3. [System now](docs/system-now.md) — crate ownership, process topology,
   readiness, registration, and failure boundaries.
4. [Execution now](docs/execution-now.md) — broker message to admission,
   Docker and Firecracker execution, cleanup, and result publication.
5. [Interface reference](docs/interface-reference.md) — `velnorctl`, the local
   Unix API, resource schemas, errors, and runner protocol surfaces.
6. [Runner protocol](docs/runner-protocol-reference.md) — pinned upstream
   V2 behavior and Velnor-specific compatibility boundaries.
7. [Operator guide](docs/operator-now.md) — package installation, service
   setup, backend configuration, diagnostics, and recovery.

Use the [glossary](docs/glossary.md) for project vocabulary and
[documentation contribution guide](docs/contributing.md) when changing these
pages.

The [integration map](docs/integrations.md) lists every external protocol and
service boundary currently visible in source.

For troubleshooting and performance work, use the
[observability guide](docs/observability.md).

Other paths:

- [Development](docs/development-now.md) — build, test, lint, dependency
  boundaries, and fixture/golden evidence.
- [Security and data](docs/security-and-data.md) — trust boundaries, data
  classes, retention, and claims the code does not make.
- [Runtime proof](docs/runtime-checking-proof.md) — what checks establish and
  what they cannot establish.
- [Future direction](docs/future-direction.md) — future-only work and open
  acceptance gates.
- [Evidence record](docs/evidence-record-2026-09-01.md) — dated observations;
  historical evidence is not current behavior.

## Runtime at a glance

```text
GitHub Actions
    │ JIT registration + Runner V2 broker/run-service
    ▼
velnor-runner daemon
    │ controller owns capacity and generations
    ├── slot-N process (one per ready slot)
    │     └── transient job process (one acquired job)
    │           └── Docker container OR jailed Firecracker microVM
    └── durable sanitized control state + bounded telemetry/log views
              ▲
        velnorctl → control.sock (reads)
                 → admin.sock   (mutations; currently reserved)
```

The backend is selected by `/etc/velnor/execution.toml` (or an explicit
configuration path). There is no automatic Docker/microVM fallback. See the
[backend rules](docs/operator-now.md#backend-and-admission-rules).

## Workspace map

| Crate | Responsibility |
| --- | --- |
| `velnor-model` | Versioned control-plane resources, lifecycle, execution, telemetry, and sanitized DTOs. |
| `velnor-action-model` | I/O-free physical-action identity and result model. |
| `velnor-cas` | Digest-verified content-addressed storage. |
| `velnor-action-journal` | Crash-safe action journal, leases, consumers, and retention. |
| `velnor-cache-service` | Lease-safe compiler-cache service over the journal and CAS. |
| `velnor-control` | Daemon-side application services and SQLite operational store. |
| `velnor-client` | Versioned local transport client; no daemon-internal dependency. |
| `velnor-render` | Human and machine output renderers. |
| `velnorctl` | Operator CLI and local control-plane adapter. |
| `velnor-runner` | Runner daemon, protocol adapter, admission, execution, node roles, and package hooks. |
| `velnor-tools` | Maintainer fleet, CI, release, and evidence tooling. |
| `unit-collector` | Structured Cargo evidence and fan-out analysis. |

Source ownership and dependency rules are documented in
[development-now](docs/development-now.md#current-crate-ownership-and-dependency-law).

## Documentation contract

Documentation is versioned with the code. Each technical claim should point to
the current source/test authority, name its status, and state proof limits.
Use the existing documents by reader need: tutorial-like onboarding in this
page and the operator guide, how-to procedures in development/operator docs,
reference tables in the interface document, and architecture/rationale in the
system, execution, security, and future documents.

The organization follows the four user needs described by
[Diátaxis](https://diataxis.fr/): tutorial, how-to, reference, and
explanation. Writing decisions also follow the audience and plain-language
guidance in [Google Technical Writing](https://developers.google.com/tech-writing/one/audience).
