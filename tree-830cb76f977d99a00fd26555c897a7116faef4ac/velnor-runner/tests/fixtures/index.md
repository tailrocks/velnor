# Velnor documentation

All Velnor documentation lives under `docs/`. Velnor is a Rust-first GitHub Actions runner and control plane that acquires workflow jobs, validates their complete action graph and capabilities before side effects, executes them through an explicitly selected Docker or jailed Firecracker backend, and records bounded, durable evidence for operation, recovery, security, and storage decisions.

## CURRENT versus FUTURE

**CURRENT** means behavior supported by the implementation and its relevant tests, or by clearly dated operator evidence that is still stated as current. **FUTURE** means a design, target, proposal, roadmap item, or acceptance criterion. FUTURE material must never be written or read as present behavior, readiness, security proof, or parity. Dated evidence is **HISTORICAL** unless current authority and acceptance gates revalidate it; when claims conflict, use the narrower, directly evidenced claim.

## Reading paths

- **Understand execution:** start with [execution-now.md](execution-now.md), then read [runtime-checking-proof.md](runtime-checking-proof.md).
- **Understand the system:** read [system-now.md](system-now.md), then [interface-reference.md](interface-reference.md).
- **Operate a current installation:** start with [operator-now.md](operator-now.md), then use [execution-now.md](execution-now.md) for lifecycle boundaries.
- **Review security and data handling:** read [security-and-data.md](security-and-data.md), then [runtime-checking-proof.md](runtime-checking-proof.md) for proof limits.
- **Develop and verify changes:** read [development-now.md](development-now.md), then the relevant current-state document.
- **Plan additions:** read [future-direction.md](future-direction.md) only after understanding the current-state documents.
- **Audit claims and history:** read [evidence-record-2026-09-01.md](evidence-record-2026-09-01.md), then follow its source and status vocabulary.

## Document ownership

The owner is the role responsible for keeping each document aligned with its source of truth. Owners update the document when its implementation, tests, operator behavior, or evidence changes; they do not promote another document’s plans into current claims.

| Document | Owner | Scope |
| --- | --- | --- |
| [system-now.md](system-now.md) | Architecture maintainers | Current crate ownership, process topology, readiness, and lifecycle. |
| [execution-now.md](execution-now.md) | Runner execution maintainers | Code-backed job lifecycle and backend behavior. |
| [interface-reference.md](interface-reference.md) | API and CLI maintainers | Current CLI, Unix API, resource, protocol, and error contracts. |
| [ci-estate-contract.md](ci-estate-contract.md) | CI and fleet maintainers | Current generated estate policy and auditor input; live acceptance remains gated. |
| [runner-protocol-reference.md](runner-protocol-reference.md) | Runner and protocol maintainers | Current pinned upstream V2 compatibility reference. |
| [operator-now.md](operator-now.md) | Operator, CLI, and release maintainers | Install, service, command, package, and failure-triage behavior. |
| [runtime-checking-proof.md](runtime-checking-proof.md) | Runtime correctness and test maintainers | Runtime checks, proof boundaries, and non-proof conditions. |
| [security-and-data.md](security-and-data.md) | Security and data-boundary maintainers | Trust boundaries, data classes, controls, retention, and explicit limits. |
| [development-now.md](development-now.md) | Maintainers | Current build gates, dependency law, fixtures, and contributor rules. |
| [future-direction.md](future-direction.md) | Project leads | Future-only work register, dependencies, statuses, and acceptance gates. |
| [evidence-record-2026-09-01.md](evidence-record-2026-09-01.md) | Evidence curator with owning maintainers | Dated observations, authority snapshots, historical records, and reconciliation gaps. |

## Evidence vocabulary

- **Authority snapshot:** a marked contract or dated state capture; it is not proof that runtime behavior still matches.
- **Measured:** an observation retaining its date and identifying context such as run, version, host, or exact value.
- **Proven:** a claim supported by the applicable implementation authority and tests or acceptance gates; process existence or a green isolated check is not enough by itself.
- **Acceptance evidence:** plan or report evidence that counts only when its owning plan says current-HEAD and independent gates pass.
- **Historical:** stale, superseded, or version-bound evidence; useful context, not a current behavior claim.
- **Design-only:** a reviewed design, target, or limit that the implementation does not establish.
- **Open / unproven:** no completion or capability claim is made.
