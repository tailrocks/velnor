# Velnor future direction

Status: future-only register, reviewed 2026-09-01. `CURRENT` implementation
descriptions live in [architecture](../concepts/architecture.md), [system](../concepts/system.md),
and [execution](../guides/execution.md). Nothing in this file is a shipped
capability, readiness claim, or security proof.

> Navigation: [← CI estate contract](../verification/ci-estate-contract.md) · [Index](../index.md) · [Next: Evidence record →](../verification/evidence-record-2026-09-01.md)

## Boundaries that future work must preserve

- GitHub remains the workflow scheduler, UI, and job source of truth.
- Admission remains fail-closed and completes the action graph before checkout,
  credentials, cache mutation, service startup, or container creation.
- Docker and microVM remain explicit execution choices. No silent fallback,
  compatibility alias, or hidden alternate implementation is acceptable.
- The local control API remains versioned, Unix-socket based, and split between
  read and mutation authorization until an explicitly approved replacement
  exists.
- New capabilities require source ownership, interface documentation, focused
  tests, fixture proof, and an explicit statement of trust/data consequences.
- Claims advance only from current source, tests, and relevant live evidence;
  old reports and green isolated tests stay historical.

These constraints come from `AGENTS.md`, the model dependency law in
`crates/velnor-model/src/lib.rs:1-15`, admission ordering in
`crates/velnor-runner/src/runner.rs:5285-5609`, and the executor boundary in
`crates/velnor-runner/src/execution/mod.rs:121-204`.

## Open work register

| ID | Status | What remains | Acceptance evidence |
| --- | --- | --- | --- |
| F-LIVE | Open / external-evidence gated | Prove a complete deployed installation across real hosts, GitHub registration, job acquisition, backend execution, cleanup, restart, drain, disk pressure, and recovery. | Dated host/package/run evidence plus independent verification. Repository tests alone are insufficient. |
| F-MUTATE | Implemented internally; public adapter reserved | `MutationPort` and durable lifecycle logic exist, but the current HTTP adapter advertises `mutations: false` and returns `501` for lifecycle POSTs. | Enable only after API contract, authorization, idempotency, version conflicts, cancellation, drain, and end-to-end tests agree. See `crates/velnorctl/src/http.rs:858-908` and `crates/velnor-control/src/ports.rs:165-242`. |
| F-REMOTE | Not implemented in current transport | The client/API currently target local Unix sockets. Remote contexts, authentication, authorization, certificate lifecycle, and revocation need a separate reviewed boundary. | Approved network/PKI/identity/role design plus reconnect, partial failure, and mutation tests. No SSH or unreviewed listener shortcut. |
| F-PARITY | Open | The runner implements a substantial Actions subset, but source/tests do not prove complete GitHub Actions parity. | Enumerated capability matrix, fixture coverage, upstream protocol comparison, and explicit unsupported behavior. |
| F-CACHE | Code present; estate-wide benefit unproven | CAS, action journal, and compiler-cache services have unit/integration coverage. That does not prove deployment isolation, quota behavior, cleanup, or cold/warm/rerun benefit across the estate. | Signed deployment, isolated fixture, scoped A/B runs, retention/pressure evidence, and rollback path. |
| F-SECURITY | Source controls present; host proof incomplete | Admission, integrity, identity fencing, and data-boundary controls are implemented. Source review does not prove kernel, KVM, host, filesystem, network, encryption-at-rest, or secure-erasure properties. | Threat-specific live tests and host/dependency review. Keep the limits in [security and data](../operations/security-and-data.md). |
| F-CUTOVER | Explicit migration seam remains | `velnorctl` still depends on the `velnor-runner` runtime facade, and the runner source labels the scaffold transitional. | Remove the seam only after replacement ownership, package/service invocations, fixtures, and rollback/recovery paths are independently proven. |
| F-DOCS | Ongoing | Keep this map and all current docs synchronized with code, tests, package units, and evidence. | Every changed interface updates its reference and at least one task/proof path; stale claims are removed. |

## How to turn future work into a current claim

1. State the user/operator goal and the trust/data boundary.
2. Assign one owning crate/module and one public interface, if any.
3. Write the reference contract before implementation: inputs, outputs, errors,
   timeouts, idempotency, authorization, persistence, and cleanup.
4. Add unit, integration, fixture, and failure-path tests that prove the
   contract; do not count parser acceptance as behavior.
5. Run the relevant build, lint, protocol, packaging, and safety gates.
6. For live behavior, record version, host, command, exit status, time, and
   artifact/run identity. Keep external mutations explicitly authorized.
7. Update the current document, then move the old design/evidence to the
   historical record instead of leaving two competing truths.

## Explicit non-goals

Velnor is not becoming a second GitHub scheduler, a marketplace, a universal
remote fleet manager, or a claim of complete job isolation merely because one
backend test passes. Those would require new product and security decisions,
not implied behavior from existing modules.
