# Security and data contract

This is the current implementation contract for security boundaries and data
handling. It is an evidence map, not a certification or a promise that every
host, guest, dependency, or operator configuration is secure. “Design-only
limit” means the reviewed implementation does not prove the stronger property.

> Navigation: [← Observability](observability.md) · [Index](../README.md) · [Next: Development →](../guides/development.md)

## Security boundary

- Every job is admitted transitively before container, checkout, cache mutation,
  runtime credential injection, service, or download side effects. The
  run-service access token needed to acquire and renew the job is extracted
  before admission. Admission itself is read-only, bounded, and rejects unsafe
  or unresolved action structure.
  `crates/velnor-runner/src/admission.rs:1-14`,
  `crates/velnor-runner/src/admission.rs:485-530`
- Packaged services force strict capability validation. The packaged release,
  compiled capability manifest, and microVM artifacts are checked for identity
  coherence before activation/start.
  `crates/velnor-runner/debian/velnor.env:78-90`,
  `crates/velnor-runner/debian/postinst:174-205`
- Execution has two materially different trust boundaries. Firecracker uses one
  jailed VMM per job and guest-local Docker; the Docker backend uses the host
  Docker socket and runs as root.
  `crates/velnor-runner/src/execution/firecracker.rs:1-22`,
  `crates/velnor-runner/src/execution/firecracker.rs:184-255`,
  `crates/velnor-runner/src/execution/docker.rs:1-17`,
  `crates/velnor-runner/debian/velnor-daemon.service:62-64`

- The Docker lease proxy injects ownership labels and cgroup parents and
  rejects privileged mode, host binds, capabilities, and devices by default.
  This is ownership/reclaim control, not full Docker Engine authorization or
  tenant isolation. `VELNOR_ALLOW_PRIVILEGED_OPTIONS=true` can widen options for
  trusted jobs and must be treated as an explicit host-trust decision.
  `crates/velnor-runner/src/docker_lease.rs:1125-1303`,
  `crates/velnor-runner/src/github_adapter.rs:686-840`

## Data classes and handling

| Data | Live handling | Durable handling | Contract and limit |
|---|---|---|---|
| Workflow/action inputs and action metadata | Read during admission through a read-only metadata source; metadata and context have byte, node, depth, visit, and time bounds. | Metadata cache may retain bounded metadata; admission itself must not create job side effects. | Received values are excluded from admission capability errors; unresolved runtime roots remain unresolved until execution. `crates/velnor-runner/src/admission.rs:350-365`, `crates/velnor-runner/src/admission.rs:466-530`, `crates/velnor-runner/src/admission.rs:1049-1155` |
| Credentials, tokens, and secret references | Runtime environment can carry `GITHUB_TOKEN`, runtime access tokens, OIDC request tokens, and secret-derived expressions to the executor. | Sanitized DTOs carry secret names only; control-store events reject secret keys and known token markers. Runner OAuth/JIT material, including an optional private key, can also be persisted in `runner.json` with mode `0600`. | The runtime token values are sensitive live job data. Sanitization and file mode do not mean the executor, process environment, or host filesystem is secret-free or encrypted. `crates/velnor-runner/src/runtime_env.rs:132-197`, `crates/velnor-runner/src/runtime_env.rs:330-448`, `crates/velnor-runner/src/config.rs:59-79,168-203`, `crates/velnor-control/src/store/records.rs:1508-1600`, `crates/velnor-model/src/sanitized.rs:1-7` |
| Repository URLs and identity | Repository/workflow/ref/SHA identity is validated and carried through admission, guest messages, provenance, and sanitized records. | Sanitized URLs strip userinfo; opaque credential-bearing schemes are rejected. | Identity is metadata, not a secret. `crates/velnor-model/src/sanitized.rs:42-60`, `crates/velnor-runner/src/admission.rs:618-713` |
| Workspace, artifacts, and command output | Job work roots, scratch disks, stdio, command files, and result exports are handled by the selected executor. Firecracker checks message identity, nonce, plan hash, and result digest. Checkout with `persist-credentials` can write auth into `.git/config`; cleanup is best effort. | Workspaces/artifacts may remain in runner storage until cleanup or operator action. | Result-integrity checks do not establish confidentiality or secure deletion. `crates/velnor-runner/src/execution/firecracker.rs:847-1050`, `crates/velnor-runner/src/storage.rs:37-75`, `crates/velnor-runner/src/checkout.rs:288-320,597-620` |
| CAS objects and trees | Content-addressed objects are streamed, size-bounded, digest-verified, and materialized without symlink traversal. | Objects use BLAKE3 paths; trees retain relative paths, digests, and modes; unreachable objects can be reclaimed. | CAS integrity and path safety are provided; encryption at rest is not established. `crates/velnor-cas/src/lib.rs:1-40`, `crates/velnor-cas/src/lib.rs:370-535`, `crates/velnor-cas/src/lib.rs:967-1084`, `crates/velnor-cas/src/lib.rs:2011-2058` |
| Persistent caches | Cache authority is derived from admitted cache-contract steps, trust scope, repository, workflow, job class, and bounded reservations. Workflow overrides are not accepted. | Canonical cache paths are trust-scoped, with current code still able to read an existing legacy path. | Trust scope is a sharing boundary, not cryptographic confidentiality. `crates/velnor-runner/src/runtime_env.rs:219-313`, `crates/velnor-runner/src/storage.rs:77-127` |
| Control state and lifecycle events | Store writes are instance-namespaced and state/event changes are atomic. | `/var/lib/velnor/state.db` contains durable lifecycle state and append-only events; query, log, and storage projections are currently created in memory and are not repopulated after service reopen. Raw job messages, credentials, masks, and logs are excluded by contract. | Event text/detail are bounded and reject secret keys, markers, control characters, and bidi controls. `crates/velnor-control/src/store/mod.rs:1-10`, `crates/velnor-control/src/store/mod.rs:40-40`, `crates/velnor-control/src/store/records.rs:1-5`, `crates/velnor-control/src/store/records.rs:1508-1633`, `crates/velnor-control/src/application.rs:220-254` |
| Action execution journal | Records action identity, producer/consumer references, state, timing, output digests, worker, and trust class; telemetry exposes digests and safe identifiers. | SQLite WAL with `synchronous=FULL`, checksummed immutable records, leases, consumers, retention, termination claims, and trust revocations. | The journal is an integrity/recovery record, not an output-content vault. Its physical path and host filesystem permissions remain deployment concerns. `crates/velnor-action-journal/src/lib.rs:1-7`, `crates/velnor-action-journal/src/lib.rs:95-116`, `crates/velnor-action-journal/src/lib.rs:269-377` |
| Logs, forensic records, and traces | Forensic lines include identity and messages; tracing emits JSONL and may export OTLP to the configured endpoint. | Rotating files live below the configured logs directory; forensic and trace rotation retains one `.1` generation. | The reviewed writers are best-effort and show no general secret-redaction filter. `crates/velnor-runner/src/slot_log.rs:1-12`, `crates/velnor-runner/src/slot_log.rs:77-181`, `crates/velnor-runner/src/telemetry.rs:1-13`, `crates/velnor-runner/src/telemetry.rs:101-159` |
| Attestation and provenance | Approved `dist` subjects are streamed and SHA-256 hashed; OIDC claims are validated; bundles are uploaded to GitHub and signed through public-good or private endpoints according to repository visibility. | Bundle files are written below a unique runner-temp subdirectory and their container paths are recorded. | Attestation discloses repository, workflow, ref/SHA, run identifiers, runner environment, and subject names/digests to its configured services. `crates/velnor-runner/src/attestation.rs:53-142`, `crates/velnor-runner/src/attestation.rs:252-320`, `crates/velnor-runner/src/attestation.rs:427-566` |
| Package configuration and installation data | Debian units load operator config and, where applicable, an operator-owned secrets file. Guardian intentionally omits secrets. | `/etc/velnor/secrets.env` is operator-owned and mode `0600`; package configuration migrates a recognized token but does not ship or overwrite the secrets file. | Several runner units run as root with bounded systemd protections and explicit writable paths. `crates/velnor-runner/debian/postinst:156-172`, `crates/velnor-runner/debian/velnor-daemon.service:20-80`, `crates/velnor-runner/debian/velnor-guardian.service:6-35` |

## Threat-control table

| Threat | Current control | Residual/design-only limit | Evidence |
|---|---|---|---|
| Workflow smuggles an unapproved action, ref, path, input, or nested composite | Closed manifest, exact SHA/ref rules, path/input validation, recursion/cycle/depth/node/visit limits, and admission before side effects. | The manifest is the policy authority; an approved capability can still be dangerous by design. | `crates/velnor-runner/src/manifest.rs:785-896`, `crates/velnor-runner/src/manifest.rs:906-1133`, `crates/velnor-runner/src/admission.rs:725-883`, `crates/velnor-runner/src/admission.rs:901-1068` |
| Malformed or oversized remote metadata causes resource exhaustion | Response, metadata, context, input, graph, depth, traversal, and deadline bounds. | Bounds are availability controls, not confidentiality controls. | `crates/velnor-runner/src/admission.rs:35-68`, `crates/velnor-runner/src/admission.rs:466-530`, `crates/velnor-runner/src/admission.rs:1272-1345` |
| Action replay or stale guest message crosses a job boundary | Guest identity includes job/isolation/generation/nonce/plan hash; duplicate steps, replay, and terminal ordering are rejected. | This proves protocol checks in the reviewed adapter, not a general host compromise defense. | `crates/velnor-runner/src/execution/firecracker.rs:350-435`, `crates/velnor-runner/src/execution/firecracker.rs:847-1050` |
| MicroVM guest escapes through host Docker or shared network | Jailed Firecracker, read-only rootfs, writable per-job disk, guest-local Docker socket, per-VM TAP/netns, and default-drop forwarding rule. | The source proves this configuration path, not kernel/KVM, image, guest-agent, or host-hardening security. | `crates/velnor-runner/src/execution/isolation.rs:50-133`, `crates/velnor-runner/src/execution/firecracker.rs:642-710`, `crates/velnor-runner/src/execution/net.rs:1-111` |
| Host-Docker job reaches the host or another job | Host Docker backend requires the host socket and verifies systemd-v2 job-slice CPU quota. The packaged standalone job unit has a control-group kill boundary, but normal controller-spawned workers are not proven to use that unit template. | This is a root/host-Docker trust boundary, and the reviewed check establishes CPU quota, not microVM-equivalent filesystem, memory, network, or socket isolation. | `crates/velnor-runner/src/execution/docker.rs:21-33`, `crates/velnor-runner/src/execution/docker.rs:119-203`, `crates/velnor-runner/debian/velnor-job@.service:6-24`, `crates/velnor-runner/src/node/controller.rs:2248-2290` |
| Cache poisoning or cross-trust reuse | Cache IDs and declarations are generated from admitted contracts; scope must be trusted and owned by the repository; storage adds trust scope/class. | Existing legacy cache paths remain readable when canonical paths do not exist; no encryption or content confidentiality is shown. | `crates/velnor-runner/src/runtime_env.rs:219-313`, `crates/velnor-runner/src/storage.rs:77-127` |
| Tampered CAS object or unsafe tree path | BLAKE3 verification, atomic publication, regular-file checks, no symlink/traversal paths, bounded tree/file/total sizes, and fail-closed reclaim inventory. | Digest integrity does not prove origin authenticity or secrecy. | `crates/velnor-cas/src/lib.rs:1-40`, `crates/velnor-cas/src/lib.rs:230-350`, `crates/velnor-cas/src/lib.rs:1035-1084`, `crates/velnor-cas/src/lib.rs:2011-2058` |
| Secret leaks into sanitized state or structured errors | Secret references retain names only; URLs strip userinfo; control events reject secret keys/markers; admission and capability violations redact received values; error envelopes expose stable class/reason/remediation fields. | These are construction/validation controls for named surfaces, not a universal sanitizer for arbitrary logs, child processes, or files. | `crates/velnor-model/src/sanitized.rs:1-7`, `crates/velnor-model/src/sanitized.rs:42-60`, `crates/velnor-model/src/sanitized.rs:104-128`, `crates/velnor-runner/src/admission.rs:264-328`, `crates/velnor-runner/src/manifest.rs:85-108`, `crates/velnor-model/src/error_envelope.rs:115-158` |
| Log or telemetry exfiltration | Files and OTLP are explicit, bounded, rotating sinks; control-store events have a separate sanitizer. | Slot forensics and tracing accept message data, are best-effort, and show no equivalent general redaction. OTLP sends to an operator-configured endpoint. | `crates/velnor-runner/src/slot_log.rs:77-181`, `crates/velnor-runner/src/telemetry.rs:1-13`, `crates/velnor-runner/src/telemetry.rs:140-159`, `crates/velnor-control/src/store/records.rs:1508-1600` |
| Crash, lease loss, or concurrent retention deletes the wrong control data | SQLite WAL/foreign keys, bounded immediate transactions, lease generation fencing, commit fence, exact ownership, and rollback before commit. | A committed deletion is durable; post-commit maintenance failure is reported as post-commit and must not be retried as rollback. | `crates/velnor-control/src/store/retention.rs:710-765`, `crates/velnor-control/src/store/retention.rs:911-916`, `crates/velnor-control/src/store/retention.rs:1192-1309` |
| Unbounded control-store growth | Event/job age and row limits, database budget, bounded prune batches, physical free-space reserve, checkpoint/vacuum limits, and admission reservations. | Retention is bounded and may defer; it is not a promise of immediate deletion or a filesystem-wide quota. | `crates/velnor-control/src/store/records.rs:186-210`, `crates/velnor-control/src/store/retention.rs:29-81`, `crates/velnor-control/src/store/retention.rs:177-205`, `crates/velnor-control/src/store/retention.rs:1323-1477` |
| Orphaned workspace/image consumes or destroys active data | Reclaim targets UUID job workspaces and dangling untagged images only; warm caches, volumes, builders, and shared microVM work roots are protected or skipped. | Reclaim depends on the backend/lease conditions; it does not scrub bytes. | `crates/velnor-runner/src/leftover_disk.rs:1-10`, `crates/velnor-runner/src/leftover_disk.rs:103-154`, `crates/velnor-runner/src/leftover_disk.rs:222-238`, `crates/velnor-runner/src/leftover_disk.rs:279-364` |
| Package upgrade mixes executable, manifest, or microVM identities | Post-install drains active units, takes the package lock, verifies artifact SHA-256 values and binary/manifest coherence, and never restarts the fleet itself. | Operational activation and service enablement remain operator-controlled. | `crates/velnor-runner/debian/postinst:4-15`, `crates/velnor-runner/debian/postinst:92-147`, `crates/velnor-runner/debian/postinst:174-247` |
| Detached action is incorrectly reused or physically terminated | Adoption requires identical action identity and adoptable policy; consumer attach/detach is durable; trust revocation bypasses retention; physical termination has one durable claim. | Supersession is disabled by default; the physical terminator is an external hook. | `crates/velnor-action-journal/src/supersession.rs:1-6`, `crates/velnor-action-journal/src/supersession.rs:32-90`, `crates/velnor-action-journal/src/supersession.rs:339-492`, `crates/velnor-action-journal/src/supersession.rs:774-891` |

## Storage and lifecycle rules

The packaged storage roots are `/var/cache/velnor/v1`, `/var/lib/velnor`,
`/run/velnor`, and `/var/log/velnor`; user-mode storage uses XDG locations and
does not default to `$HOME/.velnor`. The control database is
`/var/lib/velnor/state.db`. `crates/velnor-runner/src/storage.rs:37-75`,
`crates/velnor-control/src/store/mod.rs:40-40`

The lifecycle is:

1. Admission reads and validates the complete action closure.
2. Execution receives the admitted plan and may create workspaces, scratch
   disks, caches, artifacts, logs, and runtime credentials.
3. Durable control state/events and action state retain bounded sanitized
   records, digests, identities, leases, and events; query, log, and storage
   projections are currently transient after service construction. Content
   stores retain content only where their respective paths are used.
4. Teardown removes the exact executor resources. Orphan reclaim and control
   retention are separate bounded maintenance paths.

Firecracker teardown explicitly does not imply secure wipe. CAS reclaim removes
unreachable objects, and leftover-disk reclaim removes selected filesystem
objects/images; neither source establishes media sanitization.
`crates/velnor-runner/src/execution/firecracker.rs:402-435`,
`crates/velnor-cas/src/lib.rs:1035-1084`,
`crates/velnor-runner/src/leftover_disk.rs:184-226`

## Explicit design-only limits

- **No at-rest confidentiality claim.** The reviewed sources do not establish
  encryption for the control database, action journal, CAS, caches, workspaces,
  logs, attestation bundles, snapshot files, or writable disks. File modes and
  systemd path restrictions are not encryption.
  `crates/velnor-cas/src/lib.rs:2011-2058`,
  `crates/velnor-action-journal/src/lib.rs:269-312`,
  `crates/velnor-runner/src/telemetry.rs:25-73`
- **No secure-erasure claim.** Unlink, reclaim, teardown, rotation, and SQLite
  deletion are logical cleanup only. Recovery from storage media, snapshots,
  filesystem slack, backups, and host-level observability is outside this
  contract.
  `crates/velnor-runner/src/execution/firecracker.rs:402-435`,
  `crates/velnor-control/src/store/retention.rs:710-765`
- **No Docker/microVM equivalence claim.** The Docker backend is a root process
  with access to the host Docker socket; its reviewed boundary check is a
  cgroup CPU quota. Select Firecracker when the host-Docker trust boundary is
  unacceptable.
  `crates/velnor-runner/src/execution/docker.rs:1-17`,
  `crates/velnor-runner/src/execution/docker.rs:119-203`,
  `crates/velnor-runner/debian/velnor-daemon.service:62-80`
- **No secret-zeroization claim.** Runtime environment injection is intentional;
  the credential-free guest handshake is a precondition for snapshot creation,
  not proof that all secret-bearing memory, files, process environments, or
  logs have been erased.
  `crates/velnor-runner/src/runtime_env.rs:132-197`,
  `crates/velnor-runner/src/execution/firecracker.rs:508-585`,
  `crates/velnor-runner/src/execution/firecracker.rs:746-821`
- **No universal log-redaction claim.** Control-store event validation is
  stronger than the reviewed forensic/tracing writers. Do not send secrets or
  untrusted sensitive payloads to those sinks.
  `crates/velnor-control/src/store/records.rs:1546-1600`,
  `crates/velnor-runner/src/slot_log.rs:77-181`,
  `crates/velnor-runner/src/telemetry.rs:101-159`
- **No immediate-global-retention claim.** Control retention has bounded defaults
  and may defer under lease, deadline, busy, reserve, or maintenance failure;
  it does not govern CAS, cache, workspace, log, package, or external service
  retention.
  `crates/velnor-control/src/store/retention.rs:177-205`,
  `crates/velnor-control/src/store/retention.rs:1323-1477`,
  `crates/velnor-runner/src/leftover_disk.rs:222-238`
- **No complete network-policy claim for every backend.** The reviewed default-
  drop TAP/netns policy is the microVM path. The Docker source does not provide
  an equivalent per-job network-control statement.
  `crates/velnor-runner/src/execution/net.rs:1-111`,
  `crates/velnor-runner/src/execution/docker.rs:119-203`
- **No universal guest credential-free claim.** The guest handshake rejects the
  specifically checked GitHub/Actions environment names, but it does not scan
  files, memory, or every possible credential variable. Guest-local Docker is
  root-owned, so isolation relies on the VM boundary; guest CPU/memory limits
  are not established by the reviewed jailer path.
  `crates/velnor-runner/src/execution/guest_agent.rs:39-64`,
  `crates/velnor-runner/src/execution/guest_image.rs:84-103`,
  `crates/velnor-runner/src/execution/firecracker.rs:203-225,654-686`
- **No vsock confidentiality or peer-authentication claim.** The host/guest
  protocol checks identity, nonce, and digest values over AF_VSOCK, but the
  reviewed protocol does not encrypt or independently authenticate the peer.
  Trust depends on Firecracker and host isolation.
  `crates/velnor-runner/src/execution/guest_agent.rs:492-535`,
  `crates/velnor-model/src/vsock_protocol.rs:146-215`
- **No JIT-secret erasure claim.** Runner OAuth/JIT material, including an
  optional private key, can be persisted in `runner.json` with mode `0600`;
  file mode is not encryption. Checkout credential cleanup is best effort.
  `crates/velnor-runner/src/config.rs:59-79,168-203`,
  `crates/velnor-runner/src/checkout.rs:597-620`
- **No built-in physical supersession guarantee.** Supersession defaults off,
  retention is bounded, and the actual physical terminator is supplied outside
  this journal module.
  `crates/velnor-action-journal/src/supersession.rs:32-90`,
  `crates/velnor-action-journal/src/supersession.rs:138-142`
