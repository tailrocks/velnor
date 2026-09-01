# Velnor documentation

This is the documentation home. The root [README](../README.md) is only a
pointer here. Read this page for the directory structure, the full newcomer
route, document ownership, and claim-status rules.

Velnor is a Rust-first GitHub Actions runner and control plane. It acquires
workflow jobs, validates their complete action graph and capabilities before
side effects, executes them through an explicitly selected Docker or jailed
Firecracker backend, and records bounded evidence for operation, recovery,
security, and storage decisions.

## Status vocabulary

**CURRENT** means behavior supported by the implementation and relevant tests,
or by clearly dated operator evidence that is still stated as current.
**FUTURE** means a design, target, proposal, roadmap item, or acceptance
criterion. FUTURE material is not present behavior, readiness, security proof,
or parity. Dated evidence is **HISTORICAL** unless current authority and
acceptance gates revalidate it.

NOW is a document-local synonym for a current observable command result.
RESERVED means syntax or an internal model exists but the public path is
unavailable. PROTOCOL marks an external wire contract; INTERNAL marks an
application/model contract that is not necessarily public.
DESIGN-ONLY and OPEN/UNPROVEN mark proof limits, not capabilities.

> Navigation: [← Project README](../README.md) · **Index** · [Next: Glossary →](concepts/glossary.md)

## Directory map

Each category has one responsibility. Use the reading route below when learning
the project; use a category when you already know the kind of answer needed.

| Category | Purpose | Contents |
| --- | --- | --- |
| [Concepts](concepts/) | Vocabulary and system explanation. | [Glossary](concepts/glossary.md), [architecture](concepts/architecture.md), [system](concepts/system.md) |
| [Guides](guides/) | Task-oriented procedures and maintenance. | [Execution](guides/execution.md), [operator](guides/operator.md), [development](guides/development.md), [contributing](guides/contributing.md) |
| [Operations](operations/) | Runtime visibility and data handling. | [Observability](operations/observability.md), [security and data](operations/security-and-data.md) |
| [Reference](reference/) | External boundaries and stable interfaces. | [Integrations](reference/integrations.md), [interface](reference/interface.md), [runner protocol](reference/runner-protocol.md) |
| [Verification](verification/) | Checks, fleet contracts, evidence, and support fixtures. | [Runtime checking](verification/runtime-checking.md), [CI estate](verification/ci-estate-contract.md), [evidence](verification/evidence-record-2026-09-01.md) |
| [Roadmap](roadmap/) | Future-only work and promotion gates. | [Future direction](roadmap/future-direction.md) |

Verification support files are kept beside their owning documentation:
[fixture-backend-parity.yml](verification/fixture-backend-parity.yml) and
[program-requirement-evidence.tsv](verification/program-requirement-evidence.tsv).

## Full reading route

Read from step 1 to step 17. Each page answers the question raised by the
previous page and links to the next page at the top and bottom of its content.

| Step | Page | Question answered | Continue |
| ---: | --- | --- | --- |
| 1 | [This index](index.md) | How is the documentation organized, and how should claims be read? | [Glossary](concepts/glossary.md) |
| 2 | [Glossary](concepts/glossary.md) | What do the recurring terms mean? | [Architecture](concepts/architecture.md) |
| 3 | [Architecture](concepts/architecture.md) | Which components exist, and why are they separated? | [System](concepts/system.md) |
| 4 | [System](concepts/system.md) | How do startup, processes, readiness, and restart work? | [Execution](guides/execution.md) |
| 5 | [Execution](guides/execution.md) | How does a broker delivery become an admitted job and result? | [Integrations](reference/integrations.md) |
| 6 | [Integrations](reference/integrations.md) | Which GitHub, Docker, Firecracker, Git, and cache boundaries exist? | [Interface](reference/interface.md) |
| 7 | [Interface](reference/interface.md) | Which CLI, Unix routes, schemas, and protocol calls are reachable? | [Runner protocol](reference/runner-protocol.md) |
| 8 | [Runner protocol](reference/runner-protocol.md) | Which upstream Runner V2 behavior does Velnor pin and where does it differ? | [Operator](guides/operator.md) |
| 9 | [Operator](guides/operator.md) | How do I install, configure, inspect, and recover a node? | [Observability](operations/observability.md) |
| 10 | [Observability](operations/observability.md) | Where do events, logs, telemetry, health, and evidence appear? | [Security and data](operations/security-and-data.md) |
| 11 | [Security and data](operations/security-and-data.md) | What is protected, persisted, redacted, and not proven? | [Development](guides/development.md) |
| 12 | [Development](guides/development.md) | How do I build, test, lint, package, and preserve boundaries? | [Runtime checking](verification/runtime-checking.md) |
| 13 | [Runtime checking](verification/runtime-checking.md) | What do repository checks actually establish? | [CI estate](verification/ci-estate-contract.md) |
| 14 | [CI estate](verification/ci-estate-contract.md) | How are fleet policy and live acceptance separated? | [Future direction](roadmap/future-direction.md) |
| 15 | [Future direction](roadmap/future-direction.md) | What remains open, and what evidence would promote it? | [Evidence record](verification/evidence-record-2026-09-01.md) |
| 16 | [Evidence record](verification/evidence-record-2026-09-01.md) | What is historical, measured, and still unproven? | [Contributing](guides/contributing.md) |
| 17 | [Contributing documentation](guides/contributing.md) | How do I maintain this structure and its claims? | [Start again](index.md) |

## Role shortcuts

- **Understand the system:** [architecture](concepts/architecture.md) →
  [system](concepts/system.md) → [interface](reference/interface.md).
- **Understand execution:** [execution](guides/execution.md) →
  [runtime checking](verification/runtime-checking.md).
- **Operate a node:** [operator](guides/operator.md) →
  [observability](operations/observability.md).
- **Review external boundaries:** [integrations](reference/integrations.md) →
  [runner protocol](reference/runner-protocol.md).
- **Review security and data:** [security and data](operations/security-and-data.md) →
  [runtime checking](verification/runtime-checking.md).
- **Develop and verify:** [development](guides/development.md) →
  [CI estate](verification/ci-estate-contract.md).
- **Audit claims and history:** [evidence record](verification/evidence-record-2026-09-01.md) →
  the source and status vocabulary above.
- **Plan additions:** read [future direction](roadmap/future-direction.md) only
  after understanding the current-state documents.

## Document ownership

The owner keeps each page aligned with its source of truth. Owners update the
page when implementation, tests, operator behavior, or evidence changes. They
do not promote another page's plans into current claims.

| Page | Owner | Status |
| --- | --- | --- |
| [Glossary](concepts/glossary.md) | Maintainers | CURRENT |
| [Architecture](concepts/architecture.md) | Architecture maintainers | CURRENT |
| [System](concepts/system.md) | Architecture maintainers | CURRENT |
| [Execution](guides/execution.md) | Runner execution maintainers | CURRENT |
| [Operator](guides/operator.md) | Operator, CLI, and release maintainers | CURRENT |
| [Development](guides/development.md) | Maintainers | CURRENT |
| [Contributing](guides/contributing.md) | Maintainers | CURRENT |
| [Observability](operations/observability.md) | Runtime and operations maintainers | CURRENT |
| [Security and data](operations/security-and-data.md) | Security and data-boundary maintainers | CURRENT |
| [Integrations](reference/integrations.md) | Runner and release maintainers | CURRENT |
| [Interface](reference/interface.md) | API and CLI maintainers | CURRENT |
| [Runner protocol](reference/runner-protocol.md) | Runner and protocol maintainers | CURRENT |
| [Runtime checking](verification/runtime-checking.md) | Runtime correctness and test maintainers | CURRENT |
| [CI estate](verification/ci-estate-contract.md) | CI and fleet maintainers | CURRENT |
| [Evidence record](verification/evidence-record-2026-09-01.md) | Evidence curator with owning maintainers | HISTORICAL |
| [Future direction](roadmap/future-direction.md) | Project leads | FUTURE |

## Evidence vocabulary

- **Authority snapshot:** a marked contract or dated state capture; it is not
  proof that runtime behavior still matches.
- **Measured:** an observation retaining its date and identifying context such
  as run, version, host, or exact value.
- **Proven:** a claim supported by the applicable implementation authority and
  tests or acceptance gates.
- **Acceptance evidence:** plan or report evidence that counts only when its
  owning plan says current-HEAD and independent gates pass.
- **Historical:** stale, superseded, or version-bound evidence; useful context,
  not a current behavior claim.
- **Design-only:** a reviewed design, target, or limit that the implementation
  does not establish.
- **Open / unproven:** no completion or capability claim is made.

## Documentation maintenance

This is docs-as-code. Keep documentation changes with the behavior they
describe. Keep pages short and current. Every reference contract should name
its source and tests; every procedure should state prerequisites, expected
output, failure meaning, and external-state impact. Mark proposals and old
measurements FUTURE, DESIGN-ONLY, or HISTORICAL; never let a plan silently
become a current capability claim.
