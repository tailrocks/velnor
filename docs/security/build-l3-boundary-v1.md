# Velnor Build L3 Boundary v1

Status: **DESIGN—NOT LIVE PROOF**

This document freezes the proposed lower-trust Build L3 boundary. It does not
claim that the design is implemented, deployed, or SLSA Build Level 3. Public
unmerged code remains GitHub-hosted. Consumers continue to reject
Velnor-signed provenance until the implementation and live adversarial gates
defined here pass.

## Security objective

Each admitted job is one disposable security domain. Tenant-controlled code
may obtain root and Docker authority inside its guest, but cannot reach the
host, another job, durable writable state, signing material, or provenance
fields. A failed or uncertain teardown retires capacity instead of returning
it to service.

The versioned, executable design inventory is
[`build-l3-threat-control-test-v1.tsv`](build-l3-threat-control-test-v1.tsv).
Every row is design-resolved and implementation-not-implemented. Plans 012 and
017 own implementation and live proof respectively.

## Trusted computing base

Inside the TCB:

- Sentry hardware, firmware, host kernel, KVM, and Firecracker (started
  directly through its HTTP API and jailer; not Kata Containers, not
  firecracker-containerd);
- Velnor admission, reservation, digester, provenance, signer, finalizer, and
  quarantine control-plane services;
- immutable, digest-verified guest boot artifacts;
- GitHub OIDC, Actions APIs, artifact service, and immutable workflow identity;
- the Plan 010 package/source/image/compiled-manifest identity chain; and
- operator-controlled networking, storage coordinator, and host recovery.

Outside the TCB:

- workflow YAML after admission, tenant steps, actions, tools, and build output;
- guest root, guest Docker, service containers, BuildKit, and Docker plugins;
- imported dependency/cache payloads before digest and policy verification;
- tenant-provided names, paths, environment values, metadata, and provenance
  claims; and
- every writable object created during a job.

## Production microVM

Production microVM is **Firecracker**: an open-source Rust VMM on Linux KVM.
Velnor starts it **directly through its HTTP API and jailer** (cgroup,
namespace, seccomp, privilege dropping). The five-device model is virtio-net,
virtio-block, virtio-vsock, serial console, and i8042. Official spec:
<https://firecracker-microvm.github.io/> and
<https://github.com/firecracker-microvm/firecracker>. Spec numbers (<125 ms
boot, <5 MiB overhead) are cited from that page, not measured here.

**Not product orchestration:** Kata Containers and firecracker-containerd.
**Not a live or silent alternative:** Cloud Hypervisor — fallback only if a
real estate workflow proves Firecracker's device model cannot support it.

Guest isolation uses **immutable block devices**, **job-local writable
disks**, and **bounded vsock**. virtio-fs, host directory passthrough, PCI
passthrough, GPUs, Windows guests, USB, and a legacy device model are
rejected. Operator selection is `[execution] backend = "docker" | "microvm"`
with no automatic fallback. Live Build L3 remains Plans 012 and 017.

The shipped selection types are `velnor_model::{ExecutionBackendKind,
JobExecutorKind, MicroVmKind, MicroVmControl}`.

## Isolation unit and control flow

Admission validates the complete expanded job and reserves its conservative
full peak before a runner is advertised. The controller then creates a fresh
Firecracker microVM (direct API and jailer) with a copy-on-write encrypted
disk and memory-only key, dedicated cgroup, process and network namespaces,
tap, and vsock. Isolation is immutable block plus job-local writable disk plus
bounded vsock — not virtio-fs or host directory passthrough. The guest
receives no host bind mount, host Docker/containerd socket, host PID
namespace, device, or writable cross-job storage.

The controller supplies only the admitted job material and brokered inputs.
Tenant egress defaults deny. Allowed remote inputs pass through a broker that
records URI and digest as external parameters and resolved dependencies. The
guest exports results through a bounded channel. The control plane rehashes
every subject, independently derives all non-subject provenance fields, signs,
and uploads. The guest never receives signing keys, attestation-write
credentials, or OIDC request credentials in L3 mode.

## Docker and BuildKit boundary

One Docker daemon lives inside each guest. Guest root therefore controls only
the disposable guest. Every Docker, Compose, Testcontainers, service-container,
Docker action, and Buildx operation resolves to that daemon.

BuildKit is created lazily at most once per Docker-using job and reused only
inside that job. Its CPU, memory, PID, parallelism, writable-byte, reserved
space, maximum-used-space, minimum-free-space, and garbage-collection limits
are bounded by the job reservation. Cross-job BuildKit daemon or mutable cache
reuse is forbidden.

Warm inputs use explicit cache transport: trust/repository/workflow-scoped,
digest-verified import; atomic export only after successful policy validation;
and no lower-trust publication into trusted consumers. Cache identity and
bytes are attributed in evidence and provenance.

## Disk invariant and reservations

Before admission, one atomic coordinator proves:

```text
available blocks
  - active full-peak reservations
  - fixed emergency reserve
  - reconciliation uncertainty
  >= candidate full-peak reservation
```

The same check applies to inodes. Sparse allocation does not reduce the
reservation. Guest disk hard maximum is no larger than the reservation, and
workspace, Docker graph, writable layers, volumes, BuildKit, logs, results,
and temporary cache exports share that bound.

Accounting records logical and allocated bytes, inodes, high-water marks,
storage class, isolation ID, reservations, cleanup debt, unowned state, last
successful probe, and pressure state. Probe failure, stale data, or catalog
disagreement enters `uncertain` and stops admission. Pressure states are
`normal`, `soft`, `hard`, `emergency`, and `uncertain`; only `normal` admits
new work. Garbage collection is ownership-scoped and lease-aware. Broad Docker
or filesystem prune is forbidden.

## Credential and provenance state machine

1. Admission binds repository, immutable ref and workflow, expanded job,
   external parameters, runner group, release identity, and isolation ID.
2. The guest receives only execution credentials required by the admitted job;
   L3 OIDC request and attestation-write credentials remain control-plane-only.
3. Result upload closes before subject admission. The digester computes output
   identity from exported bytes and rejects extra, missing, or ambiguous data.
4. The provenance service obtains trusted GitHub API/OIDC facts and derives
   builder, invocation, source, runner, and dependency fields independently.
5. The signer creates and zeroizes signing material in signer memory, signs the
   exact verified statement, and publishes the bundle.
6. Any mismatch or uncertain custody fails without provenance publication.

Consumers first compare an independently expected SHA-256, then verify the
attestation against exact source repository, source ref, source digest, signer
workflow, one admitted immutable signer digest, and
`https://slsa.dev/provenance/v1`. GitHub-hosted L2 policy denies self-hosted
provenance. Velnor signer admission remains closed until Plan 017 live proof.

## Cleanup and quarantine state machine

Normal finalization stops tenant processes and guest Docker, uploads bounded
results, destroys the guest, discards the disk and memory-only key, removes
tap/vsock/cgroup state, revokes credentials, and proves absence by isolation
ID. Cancellation, timeout, daemon crash, package restart, host reboot, partial
creation, and every teardown fault enter the same idempotent finalizer.

If any absence proof fails, the slot is fenced and unregistered. It accepts no
subsequent job. Boundary uncertainty quarantines the host. Re-admission needs a
clean reboot or reimage plus operator-recorded reconciliation proving no guest,
disk, socket, network, cgroup, credential, Docker object, or writable cache
state survived.

## Required adversarial proof

Plan 012 implements every matrix control and test. Plan 017 executes the same
surface at one slot and eight simultaneous attacker/victim slots, followed by
sequential victims. Proof includes forgery, signing-material reads, host/root
escape, cross-slot and sequential influence, cleanup faults, cache poisoning,
provenance-field injection, reservation races, hard byte and inode pressure,
probe failure, GC races, unowned Docker preservation, and ENOSPC prevention.

Victim output bytes and digests must equal isolated controls. Evidence records
only identities, hashes, booleans, resource measurements, and denials—never
tokens or keys. No implementation deviation is accepted by implication: a
changed boundary invalidates approval and reopens this design gate.

## Approval gate

The immutable design commit requires two current approvals: the configured
security CODEOWNER and a distinct operator-designated security approver.
Neither may be the author, bot, executor, proxy, or each other. Both reviews
must bind the exact head commit. Missing identities or stale reviews block
Plans 012 and 017; they do not turn this design into live proof.
