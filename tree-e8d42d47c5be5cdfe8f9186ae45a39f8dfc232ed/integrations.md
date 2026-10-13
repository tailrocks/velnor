# External integrations

Status: current source inventory, reviewed 2026-09-01. Presence of a client or
URL builder proves implementation intent and protocol shape; it does not prove
that credentials, permissions, network access, or a live service are available.

> Navigation: [← Execution now](execution-now.md) · [Index](index.md) · [Next: Interface reference →](interface-reference.md)

## Integration map

```text
GitHub.com / GHES
  ├─ REST: JIT registration, runner groups, runners, queued jobs, runs
  ├─ Runner V2 broker: session, message poll, acknowledgement
  ├─ Run Service: acquirejob, renewjob, completejob
  ├─ Results Service: Twirp step updates, signed log/summary blobs, WebSocket feed
  ├─ Actions cache: v1 Twirp and v2 Results Cache Service
  └─ OIDC + repository APIs: provenance/attestation

Node-local
  ├─ Docker Engine: HTTP/1.1 over a per-job Unix-socket lease proxy
  ├─ Firecracker: JSON/HTTP over a jailed Unix socket
  ├─ Firecracker guest: AF_VSOCK host/guest protocol
  └─ Git: HTTPS smart protocol v2 and local bare mirrors

Release infrastructure
  ├─ GHCR: multi-architecture OCI job image and BuildKit registry cache
  ├─ Fulcio/Rekor or GitHub-hosted signing: provenance signatures
  └─ signed APT repository: external publication/installation path
```

## GitHub Actions control path

| Integration | What Velnor uses | Source |
| --- | --- | --- |
| GitHub REST | GitHub.com and GHES URL roots; runner groups, JIT configs, runner listing/deletion, queued jobs, run inspection/cancel/rerun/dispatch. | `crates/velnor-runner/src/protocol.rs:459-647,915-965,1133-1426`; `crates/velnor-control/src/github.rs:46-148` |
| JIT registration | OAuth client-credentials with a JWT bearer assertion; `generate-jitconfig`; name, labels, runner group, and scope. | `crates/velnor-runner/src/protocol.rs:915-965,1133-1158` |
| Runner V2 broker | `POST /session`, `GET /message`, and `POST /acknowledge`; empty success means idle, while non-2xx is an error. | `crates/velnor-runner/src/protocol.rs:1858-1946`; [job flow](execution-now.md#from-broker-message-to-admission) |
| Run Service | `POST /acquirejob`, `/renewjob`, and `/completejob`; delivery is acquired before execution and completion is retried with durable intent. | `crates/velnor-runner/src/protocol.rs:2007-2207`; `crates/velnor-runner/src/node/complete.rs:34-177` |
| Results Service | VSS/Twirp step updates, signed step/job log and summary uploads, and a persistent WebSocket live-feed connection. | `crates/velnor-runner/src/protocol.rs:2246-2490,3693-4414`; `crates/velnor-runner/src/runner.rs:6601-6902` |
| Runner protocol authority | Velnor pins `actions/runner` `v2.337.0`; upstream source remains the behavior authority for protocol changes. | [runner protocol reference](runner-protocol-reference.md); `crates/velnor-runner/src/protocol.rs:29-38` |

GitHub supplies the workflow/job payload, including `SystemVssConnection`,
job-scoped result credentials, variables, masks, endpoints, actions, steps,
services, workspace, and context. Velnor must not substitute an operator token
for the job credential. `crates/velnor-runner/src/job_message.rs:8-177`,
`crates/velnor-runner/src/github_adapter.rs:204-266`.

## Cache and artifact services

| Integration | Role | Boundary |
| --- | --- | --- |
| Actions Cache v1/v2 | Serve or consume GitHub cache protocol when configured. | Bearer tokens are hashed into tenant namespaces. `VELNOR_ACTIONS_CACHE_URL` enables the service; `VELNOR_ACTIONS_CACHE_BIND` controls its listener, defaulting to `127.0.0.1:17933`. `crates/velnor-runner/src/gha_cache.rs:1-26,222-287,772-802` |
| Velnor CAS | Store immutable blobs/trees by BLAKE3 digest. | Writes are atomic and reads verify digests; tree paths reject traversal/symlink hazards. `crates/velnor-cas/src/lib.rs:1-40,370-535,967-1084` |
| Compiler cache | Select Kache, sccache, or off according to admitted policy. | Trust scope and job ownership bound the cache; microVM plans force compiler cache off. `crates/velnor-runner/src/container.rs:70-96`, `crates/velnor-runner/src/github_adapter.rs:39-105` |
| GHCR | Publish/pull the multi-architecture workload image and use registry-backed BuildKit cache in release CI. | OCI digests and release metadata are verified; GHCR availability is external. `.github/workflows/release.yml:358-443,810-827`; `crates/velnor-runner/src/release.rs:655-700` |

MicroVM cache movement uses digest-verified blobs over vsock. It does not use
host-directory passthrough, virtio-fs, or the host Docker socket.
`crates/velnor-runner/src/execution/cache_transport.rs:1-68`.

## Node-local engines

| Integration | Protocol and ownership | Source |
| --- | --- | --- |
| Docker Engine | Host backend talks to `/var/run/docker.sock`; jobs use a Velnor lease proxy that injects ownership labels, forces the jobs cgroup, and rejects privileged/host-bind/device escapes. | `crates/velnor-runner/src/execution/docker.rs:20-37`; `crates/velnor-runner/src/docker_lease.rs:1-16,1089-1303` |
| Firecracker | Jailed VMM uses JSON/HTTP over a per-job API Unix socket; artifacts and versions are pinned and hash-verified. | `crates/velnor-runner/src/execution/unix_api.rs:1-68`; `crates/velnor-runner/src/execution/artifacts.rs:81-118,461-506` |
| Guest agent | AF_VSOCK only. Identity, readiness, plan delivery, stdio, cancellation, results, and teardown are versioned and fenced by nonce/plan hash/generation. | `crates/velnor-runner/src/bin/velnor-guest-agent.rs:1-37`; `crates/velnor-model/src/vsock_protocol.rs:12-253` |
| Git | Checkout uses HTTPS smart protocol v2. Optional bare mirrors use exclusive locks; credentials and URLs are not persisted in the mirror. | `crates/velnor-runner/src/checkout.rs:360-470,543-674`; `crates/velnor-runner/src/git_mirror.rs:1-61,102-155` |

## Provenance and release services

Attestation uses GitHub OIDC and repository APIs, restricts subjects to approved
distribution files, hashes subjects with SHA-256, and writes a provenance bundle.
Signing can use public-good Fulcio/Rekor or GitHub-hosted signing endpoints.
`crates/velnor-runner/src/attestation.rs:53-145,252-320,427-557`.

Release CI builds native amd64/arm64 binaries and Debian packages, a
multi-platform GHCR image, and guest artifacts. The release record binds source,
binary, Debian, OCI, manifest, and package identities; the signed APT repository
is the publication authority. `.github/workflows/release.yml:103-176,356-454,810-827`;
`crates/velnor-runner/src/release.rs:655-692,1577-1604`.

## Credentials and trust

- Daemon registration uses `GITHUB_TOKEN`/`VELNOR_URL`; packaged installation
  keeps the token in operator-owned `/etc/velnor/secrets.env` with mode `0600`,
  never in argv. `crates/velnor-runner/debian/velnor.env:4-15`,
  `crates/velnor-runner/debian/velnor-daemon.service:16-32`.
- Job execution credentials come from GitHub's job endpoint data and are
  injected only after admission; the run-service token used to acquire/renew
  is extracted earlier. Non-trusted jobs containing user secrets are rejected when
  the selected backend requires stronger trust. `crates/velnor-runner/src/runtime_env.rs:108-197`,
  `crates/velnor-runner/src/runner.rs:6155-6167`.
- MicroVM guest readiness requires absence of job credentials; the host sends
  the plan over vsock after that handshake. `crates/velnor-runner/src/execution/guest_agent.rs:19-82`.

## Deliberately absent integrations

No implementation evidence exists for S3/object storage, Redis, PostgreSQL or
MySQL, Kubernetes, SSH-based runner transport, or an external message queue.
Do not add those names to architecture diagrams until a source-owned adapter,
configuration contract, test surface, and data/security review exist.
