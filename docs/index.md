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

## Reading paths

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
