# Velnor documentation

All Velnor documentation lives under `docs/`. Velnor is a Rust-first GitHub Actions runner and control plane that acquires workflow jobs, validates their complete action graph and capabilities before side effects, executes them through an explicitly selected Docker or jailed Firecracker backend, and records bounded evidence for operation, recovery, security, and storage decisions. Durability varies by plane: lifecycle state, events, and telemetry have durable paths; query, log, and storage projections are currently not repopulated after service reopen. See [observability](observability.md).

## Status vocabulary

**CURRENT** means behavior supported by the implementation and its relevant tests, or by clearly dated operator evidence that is still stated as current. **FUTURE** means a design, target, proposal, roadmap item, or acceptance criterion. FUTURE material must never be written or read as present behavior, readiness, security proof, or parity. Dated evidence is **HISTORICAL** unless current authority and acceptance gates revalidate it; when claims conflict, use the narrower, directly evidenced claim.

`NOW` is a document-local synonym for a current observable command result.
`RESERVED` means syntax or an internal model exists but the public path is
unavailable. `PROTOCOL` marks an external wire contract; `INTERNAL` marks an
application/model contract that is not necessarily public. `DESIGN-ONLY` and
`OPEN/UNPROVEN` mark proof limits, not capabilities. `CURRENT`, `FUTURE`, and
`HISTORICAL` remain the canonical document-status labels.

> Navigation: [← README](../README.md) · **Index** · [Next: Glossary →](glossary.md)

## Canonical newcomer path

Read this route from top to bottom. Each step answers the question raised by
the previous step. Use the role shortcuts below only when you already know
which part you need.

| Step | Page | What it answers | Continue to |
| ---: | --- | --- | --- |
| 0 | [README](../README.md) | What Velnor is, who it serves, and its boundary. | [Index](index.md) |
| 1 | [This index](index.md) | How to read the docs and judge claims. | [Glossary](glossary.md) |
| 2 | [Glossary](glossary.md) | What the recurring terms mean. | [Architecture](architecture.md) |
| 3 | [Architecture](architecture.md) | Which components exist and why they are separated. | [System now](system-now.md) |
| 4 | [System now](system-now.md) | How startup, processes, readiness, and restart work. | [Execution now](execution-now.md) |
| 5 | [Execution now](execution-now.md) | How a broker delivery becomes an admitted job and result. | [Integrations](integrations.md) |
| 6 | [Integrations](integrations.md) | Which GitHub, Docker, Firecracker, Git, and cache boundaries exist. | [Interface reference](interface-reference.md) |
| 7 | [Interface reference](interface-reference.md) | Which CLI, Unix routes, schemas, and protocol calls are reachable. | [Runner protocol](runner-protocol-reference.md) |
| 8 | [Runner protocol](runner-protocol-reference.md) | Which upstream Runner V2 behavior Velnor pins and where it differs. | [Operator guide](operator-now.md) |
| 9 | [Operator guide](operator-now.md) | How to install, configure, inspect, and recover a node. | [Observability](observability.md) |
| 10 | [Observability](observability.md) | Where events, logs, telemetry, health, and evidence appear. | [Security and data](security-and-data.md) |
| 11 | [Security and data](security-and-data.md) | What is protected, persisted, redacted, and not proven. | [Development](development-now.md) |
| 12 | [Development](development-now.md) | How to build, test, lint, package, and preserve boundaries. | [Runtime proof](runtime-checking-proof.md) |
| 13 | [Runtime proof](runtime-checking-proof.md) | What repository checks actually establish. | [CI estate](ci-estate-contract.md) |
| 14 | [CI estate contract](ci-estate-contract.md) | How fleet policy and live acceptance are separated. | [Future direction](future-direction.md) |
| 15 | [Future direction](future-direction.md) | What remains open and what evidence would promote it. | [Evidence record](evidence-record-2026-09-01.md) |
| 16 | [Evidence record](evidence-record-2026-09-01.md) | What is historical, measured, and still unproven. | [Contributing](contributing.md) |
| 17 | [Contributing documentation](contributing.md) | How to maintain this route and its claims. | [README / repeat](../README.md) |

## Role shortcuts

- **Understand execution:** start with [execution-now.md](execution-now.md), then read [runtime-checking-proof.md](runtime-checking-proof.md).
- **Understand the system:** read [architecture.md](architecture.md), then [system-now.md](system-now.md) and [interface-reference.md](interface-reference.md).
- **Learn the vocabulary:** use [glossary.md](glossary.md) while reading the architecture.
- **Review external boundaries:** read [integrations.md](integrations.md) for GitHub, Docker, Firecracker, cache, Git, and release services.
- **Understand observation:** read [observability.md](observability.md) for event, log, telemetry, health, and evidence planes.
- **Operate a current installation:** start with [operator-now.md](operator-now.md), then use [execution-now.md](execution-now.md) for lifecycle boundaries.
- **Review security and data handling:** read [security-and-data.md](security-and-data.md), then [runtime-checking-proof.md](runtime-checking-proof.md) for proof limits.
- **Develop and verify changes:** read [development-now.md](development-now.md), then the relevant current-state document.
- **Plan additions:** read [future-direction.md](future-direction.md) only after understanding the current-state documents.
- **Audit claims and history:** read [evidence-record-2026-09-01.md](evidence-record-2026-09-01.md), then follow its source and status vocabulary.

## Document ownership

The owner is the role responsible for keeping each document aligned with its source of truth. Owners update the document when its implementation, tests, operator behavior, or evidence changes; they do not promote another document’s plans into current claims.

| Document | Owner | Status | Scope |
| --- | --- | --- | --- |
| [README.md](../README.md) | Maintainers | CURRENT | Project vision, first reading path, and runtime overview. |
| [index.md](index.md) | Maintainers | CURRENT | Documentation routing, ownership, and status rules. |
| [glossary.md](glossary.md) | Maintainers | CURRENT | Short definitions for recurring system terms. |
| [integrations.md](integrations.md) | Runner and release maintainers | CURRENT | External APIs, local engines, credentials, and integration limits. |
| [observability.md](observability.md) | Runtime and operations maintainers | CURRENT | Event, log, telemetry, health, retention, and evidence contracts. |
| [architecture.md](architecture.md) | Architecture maintainers | CURRENT | Newcomer explanation of responsibilities, process/data flow, boundaries, and design decisions. |
| [system-now.md](system-now.md) | Architecture maintainers | CURRENT | Current crate ownership, process topology, readiness, and lifecycle. |
| [execution-now.md](execution-now.md) | Runner execution maintainers | CURRENT | Code-backed job lifecycle and backend behavior. |
| [interface-reference.md](interface-reference.md) | API and CLI maintainers | CURRENT | Current CLI, Unix API, resource, protocol, and error contracts. |
| [ci-estate-contract.md](ci-estate-contract.md) | CI and fleet maintainers | CURRENT | Current generated estate policy and auditor input; live acceptance remains gated. |
| [runner-protocol-reference.md](runner-protocol-reference.md) | Runner and protocol maintainers | CURRENT | Current pinned upstream V2 compatibility reference. |
| [operator-now.md](operator-now.md) | Operator, CLI, and release maintainers | CURRENT | Install, service, command, package, and failure-triage behavior. |
| [runtime-checking-proof.md](runtime-checking-proof.md) | Runtime correctness and test maintainers | CURRENT | Runtime checks, proof boundaries, and non-proof conditions. |
| [security-and-data.md](security-and-data.md) | Security and data-boundary maintainers | CURRENT | Trust boundaries, data classes, controls, retention, and explicit limits. |
| [development-now.md](development-now.md) | Maintainers | CURRENT | Current build gates, dependency law, fixtures, and contributor rules. |
| [future-direction.md](future-direction.md) | Project leads | FUTURE | Future-only work register, dependencies, statuses, and acceptance gates. |
| [evidence-record-2026-09-01.md](evidence-record-2026-09-01.md) | Evidence curator with owning maintainers | HISTORICAL | Dated observations, authority snapshots, historical records, and reconciliation gaps. |
| [contributing.md](contributing.md) | Maintainers | CURRENT | Documentation types, evidence rules, API/procedure checklist, and verification. |

## Evidence vocabulary

- **Authority snapshot:** a marked contract or dated state capture; it is not proof that runtime behavior still matches.
- **Measured:** an observation retaining its date and identifying context such as run, version, host, or exact value.
- **Proven:** a claim supported by the applicable implementation authority and tests or acceptance gates; process existence or a green isolated check is not enough by itself.
- **Acceptance evidence:** plan or report evidence that counts only when its owning plan says current-HEAD and independent gates pass.
- **Historical:** stale, superseded, or version-bound evidence; useful context, not a current behavior claim.
- **Design-only:** a reviewed design, target, or limit that the implementation does not establish.
- **Open / unproven:** no completion or capability claim is made.

## Documentation maintenance

This is docs-as-code. Keep documentation in the same change as the behavior it
describes. Prefer a short current document over a large stale archive. Every
reference contract should name its source and tests; every procedure should
state prerequisites, expected output, failure meaning, and whether it changes
external state. Mark proposals and old measurements `FUTURE`, `DESIGN-ONLY`, or
`HISTORICAL`; never let a plan silently become a current capability claim.
