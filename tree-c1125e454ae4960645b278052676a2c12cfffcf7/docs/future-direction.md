# Velnor future-direction register

Status date: 2026-09-01. This is a future-only register. It records work that
is incomplete, live-gated, design-only, later, or evidence-gated. Planned work
is not production capability. `LIVE-GATED` means live proof is still missing;
it does not mean the feature is live.

The source paths in this register are historical provenance for the removed plan
records. Their relevant content is consolidated in
[the evidence record](evidence-record-2026-09-01.md); none of those old paths is
an active documentation location.

## Current anchors

These are scan anchors, not completion claims:

| Area | Current truth | Source |
|---|---|---|
| Campaign | **INCOMPLETE**: 4 / 94 leaves done. | `plans/TASKS.md:30-32` |
| Shared migration | 063–065 are **DONE**; 066 is **IN PROGRESS**; 067–080 remain incomplete. | `plans/TASKS.md:48-67`, `plans/velnorctl-migration/README.md:78-97` |
| Command leaves | 75 leaves exist; only C005 is marked done in the mirror, so 74 remain TODO. | `plans/velnorctl-migration/README.md:102-120`, `plans/TASKS.md:69-145` |
| Fleet policy | Plan 039 has generator/audit code evidence, but live restriction, exact closure, runner proof, and operator approval remain pending. | `plans/fleet-operations/039-org-jit-multi-repo-fleet.md:301-320` |
| Build L3 | Firecracker/KVM is the target architecture; the Build L3 boundary remains design-only until Plans 012 and 017 prove it live. | `docs/vision.md:40-52` |
| Native Actions cache | Velnor-native v1/v2 cache code is operator-enabled; fixture roundtrip, signed deployment, and controlled A/B proof remain open. The 188 s Java result is registry-primary proof, not native-service proof. | `docs/ci-speedup-research-2026-08-25.md:10-21`, `docs/ci-speedup-research-2026-08-25.md:71-84` |
| Package cutover | Plan 079 is **TODO** and cannot start its contraction until all architecture plans and C001–C075 are proven. | `plans/velnorctl-migration/079-final-package-and-binary-cutover.md:3-15` |
| Remote operation | Plan 080 is later, P3, and requires explicit approval of the listener, PKI, identity, role, credential, revocation, and firewall surface. | `plans/velnorctl-migration/080-remote-contexts-and-fleet-views.md:7-24` |

## Non-negotiable boundaries

- The estate is exactly 28 repositories: 18 Tailrocks, 3 ChainArgos, and 7
  jackin-project. Generated classes, the plural `lanes` selector, defaults,
  trust routing, and required checks remain one contract. No future item may
  widen or silently replace it. `plans/README.md:3-16`; `docs/vision.md:3-16`
- GitHub remains the workflow scheduler and UI. Velnor owns runner execution,
  local state, and approved native capability; it does not gain a local
  scheduler or marketplace fallback. `docs/vision.md:107-131`;
  `plans/velnorctl-migration/README.md:168-179`
- Strict capability admission stays fail-closed before checkout, cache
  mutation, service startup, or container creation. New capability requires
  exact-surface documentation, operator approval, and fixture proof.
  `docs/strict-capability-contract.md:6-18`
- No aliases, shims, dual implementations, fallback paths, parallel daemon, or
  compatibility period survives the migration. `velnor-runner` terminology may
  remain only in immutable history, migration records, and signed predecessor
  metadata/artifacts. `plans/velnorctl-migration/README.md:6-11,168-179`;
  `plans/velnorctl-migration/079-final-package-and-binary-cutover.md:196-200`

## Future register

| ID | Status | Dependencies | Acceptance gate |
|---|---|---|---|
| F-039 | **IN PROGRESS / LIVE-GATED** | Unified-CI contract; generated policy; approved release-ref ledger; reviewed digest; operator approval per organization. | Generate deterministic exact repo/workflow/ref closure; read back `visibility=selected`, workflow restriction, repositories, and guard fields with semantic equality; prove trusted JIT assignment, denial, public-unmerged GitHub routing, warm reuse, and scheduled read-only drift audit for all three organizations. Stop on ambiguous closure, inherited restriction, or unreviewed removal. `plans/fleet-operations/039-org-jit-multi-repo-fleet.md:91-138,182-246` |
| F-PRD | **TODO / INCOMPLETE** | Plan 039, Plans 063–080, every command leaf, current source/package SHA, and independent verification. | Every atomic row has current evidence, dependency agreement, exact command/exit code/time/artifact record, independent verifier, green fixture/protocol/capability/storage/executor/lifecycle/parity/security gates, signed release, rollback, recovery, and final audit. `plans/production-readiness/README.md:8-11,51-74,270-293` |
| F-L3 | **DESIGN-ONLY** | Plans 012 and 017; explicit `docker`/`microvm` selection; Firecracker/KVM implementation. | Live proof of Firecracker HTTP API plus jailer, guest-local Docker, immutable block storage, job-local writable storage, bounded vsock, no automatic fallback, and rejection of unsupported devices/host passthrough. Until then, do not describe Build L3 as shipped. `plans/README.md:41-48`; `plans/production-readiness/README.md:164-171` |
| F-STATE | **066 IN PROGRESS; 067 TODO** | 063–065; 067 waits for durable history/events. | Sanitized durable history/events, versioned Unix-socket control API, protocol tests, fixture observability, structured errors, and no handler-side duplicate state logic. `plans/TASKS.md:52-57`; `plans/velnorctl-migration/README.md:78-85,129-137` |
| F-AUTHORITY | **TODO** | 068 after 064–067; 074 after 065, 066, 068; 075 after 066–067. | Typed configuration/auth/instance authority, canonical GitHub Actions client/run merge, storage ownership/leases/reservations/GC, and current-run fixture proof. `plans/TASKS.md:57-60`; `plans/velnorctl-migration/README.md:85-93` |
| F-SERVICES | **TODO** | 069–073 after their named architecture/service dependencies; 073 is the lifecycle critical path. | Resource queries, logs, observation, wait, health, preflight, reconciliation, and daemon lifecycle are independently owned services; cancellation, drain, restart, reaping, and recovery are explicit and fixture-proven. `plans/TASKS.md:61-67`; `plans/velnorctl-migration/README.md:86-90,134-145` |
| F-PREP | **TODO** | 076 after 064/068; 077 after 064–069; 078 after 066–077. | Debian-native transition preparation, exact capability/adapter/workflow checks, and sanitized diagnostics collection; no capability expansion or legacy release surface. `plans/TASKS.md:59,64,66`; `plans/velnorctl-migration/README.md:93-97,168-179` |
| F-RELIABILITY | **TODO; broker-outage cancellation gap remains unaccepted** | State/API foundation plus GitHub client, lifecycle, storage, and result paths. | Inject broker outage, registration timeout, credential/Docker/cache/job/result-upload failure, cancellation, restart, reboot, disk pressure, and interrupted deployment. Prove accepted work is never silently abandoned, cancellation is bounded and observable, and no stale runner/job/lease/workspace/cache remains. `plans/production-readiness/README.md:172-177,270-279` |
| F-CACHE | **EVIDENCE-GATED; not an estate-wide live claim** | Landed native v1/v2 service; signed package deployment; fixture; isolated operator canary. | Fixture roundtrip, signed deployment, controlled scoped A/B canary, repo/scope/trust isolation, bounded cardinality, LRU/age eviction, quotas, cleanup, disk attribution, and ≥3-run cold/warm/rerun measurements. Do not substitute the registry-primary 188 s result for this proof. `docs/ci-speedup-research-2026-08-25.md:52-69,103-122`; `plans/production-readiness/README.md:241-268` |
| F-KACHE | **EVIDENCE-GATED candidate** | Strict manifest; one selected compiler backend; equal experiment budget; fixture support. | Test literal `off`, `sccache`, and `kache` jobs separately; enforce `github-cache=false`, local store isolation, leases, pressure/GC behavior, zero wrapper stacking, and cold/warm/rerun correctness. Adoption evidence does not make Kache the default without this gate. `docs/strict-capability-contract.md:114-131,159-184,201-214`; `docs/ci-speedup-research-2026-08-25.md:198-209` |
| F-PERF | **TODO / EVIDENCE-GATED** | Correct fleet admission; cache proof; trace/job-timing export; current workflow baselines. | For each affected repository, capture ≥3 before/after runs with job wall and created→started queue split, runner pickup/boot/teardown percentiles, cache/compiler/network evidence, and `both` parity. Lane comparison flags Velnor above 1.15× its GitHub twin; fix Velnor architecture when it loses. `docs/ci-speedup-research-2026-08-25.md:103-141,227-236` |
| F-079 | **TODO** | 063–078 and all C001–C075; every retained command has fixture proof. | Delete the old crate, binary, package, service invocation, image payload, entrypoint, and custom version machinery. Install only signed `velnorctl` APT package; prove systemd, fixture cold/warm/unchanged lanes, lifecycle/control, logs/artifacts, storage, diagnostics, package integrity, predecessor downgrade, forward recovery, and A/B/A. `plans/velnorctl-migration/079-final-package-and-binary-cutover.md:12-15,36-59,61-81,115-143,172-194` |
| F-080 | **LATER / TODO** | F-079; stable local v1 API; explicit operator approval of exact network, PKI, identity, role, credential, revocation, and firewall surface. | HTTPS mutual certificate authentication, hostname/SAN verification, role mapping, revocation/rotation/expiry failure, context validation, source-labelled fleet views, bounded fan-out, partial failure, reconnect, and typed remote mutations. No SSH fallback, central truth, scheduler, or remote package management. `plans/velnorctl-migration/080-remote-contexts-and-fleet-views.md:63-94,96-135` |

## Command migration grouping

Command status is per leaf. A family heading never transfers proof between
siblings. Every leaf needs its parser, handler, output, tests, and fixture
validation; execute only after that leaf's named dependencies are `DONE`.
`plans/velnorctl-migration/README.md:49-58,122-145`;
`plans/TASKS.md:8-19,27-28`

| Family | Leaves | Status at scan | Family gate |
|---|---:|---|---|
| Utility | C001–C005 (5) | C005 **DONE**; C001–C004 **TODO** | Shared CLI/render contract; exact leaf proof. |
| `get` | C006–C015 (10) | **TODO** | Resource model, query services, and named leaf dependencies. |
| `describe`, `logs`, `events`, `top`, `wait` | C016–C020 (5) | **TODO** | Resource/log/observation services; C020 also needs 073. |
| Health, preflight, reconcile | C021–C026 (6) | **TODO** | Read-only doctor plus explicit reconciliation services. |
| Lifecycle | C027–C033 (7) | **TODO** | 073 daemon/lifecycle engine; safe state transitions. |
| `run` | C034–C042 (9) | **TODO** | 074 GitHub run client where named; C037 also needs 073. |
| `storage` | C043–C050 (8) | **TODO** | 075 storage control and cache/storage proof. |
| `config` | C051–C054 (4) | **TODO** | Configuration service and strict typed validation. |
| `context` | C055–C059 (5) | **TODO** | Local context semantics; remote extension waits for 080. |
| `auth` | C060–C061 (2) | **TODO** | Auth service and explicit failure semantics. |
| `instance` | C062–C065 (4) | **TODO** | C063 needs 076; C064–C065 need 072 and 073; install never bypasses APT. |
| `capability` | C066–C069 (4) | **TODO** | 077 manifest/adapter services; fail-closed exact surface. |
| `adapter` | C070–C072 (3) | **TODO** | 077 native adapter/workflow services and fixture coverage. |
| `workflow`, diagnostics, service daemon | C073–C075 (3) | **TODO** | C074 needs 078; C075 needs 073; daemon is service-only. |
| **Total** | **C001–C075 (75)** | **1 DONE; 74 TODO** | All leaves `DONE` before F-079. |

Known cross-family blockers in the task mirror: C008, C012, C020, C027–C033,
C037, and C075 require 073; C011 and C012 require 074; C063 requires 076;
C064–C065 require 072 and 073; C074 requires 078. `plans/TASKS.md:95-97,105,124-145`

## Old-surface contraction map

This is a destination map, not a compatibility promise. No old spelling is
retained as a hidden alias.

| Old behavior family | Final destination |
|---|---|
| Status and inspection | `velnorctl get slots`, `velnorctl describe instance/...` |
| Mutating doctor and preflight | Read-only `velnorctl doctor`; explicit `velnorctl reconcile`; `velnorctl preflight` |
| Storage and cache | `velnorctl storage paths/status/du/gc` |
| Capability check | `velnorctl capability check` |
| Configuration and instance lifecycle | `velnorctl instance init/install/apply/delete` |
| Daemon | `velnorctl daemon` only |
| Single-job execution | Internal `velnorctl daemon --once`; operator `velnorctl run` stays GitHub workflow-run space |
| Release/version machinery | Removed; signed APT/dpkg is the only installed-version authority |

Source contract: `plans/velnorctl-migration/079-final-package-and-binary-cutover.md:61-81`.

## Proof and stop rules

1. A status advances only from current-HEAD evidence. Historical evidence,
   green local tests, shared commits, or partial workflow success do not make a
   row complete. `plans/production-readiness/README.md:8-11,51-59`
2. Every completed leaf carries focused nextest, `rtk mise run check`,
   integration/fixture proof, safety scan, independent review, index agreement,
   and the required signed commit trailers. `plans/TASKS.md:16-19`
3. Live fleet, Sentry, package, policy, cancellation, and remote mutations need
   pre/post snapshots and explicit authorization. Release/deployment proof uses
   signed APT only; no copied binary, local package, or direct `dpkg -i`.
   `plans/production-readiness/README.md:60-70,206-224`
4. Stop on missing capability approval, ambiguous workflow/ref closure,
   inherited/read-only policy restriction, missing PKI approval, or any proposed
   fallback/alias. Repair the enabling architecture; do not hide the gap in a
   repository workflow or compatibility layer. `docs/strict-capability-contract.md:15-18`;
   `plans/fleet-operations/039-org-jit-multi-repo-fleet.md:237-248`;
   `plans/velnorctl-migration/080-remote-contexts-and-fleet-views.md:137-139`
