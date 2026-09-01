# Velnor glossary

Short definitions for newcomers. The linked documents contain the full
behavior and proof limits.

> Navigation: [← Index](../index.md) · **Glossary** · [Next: Architecture →](architecture.md)

| Term | Meaning in Velnor |
| --- | --- |
| Admission | The bounded, read-first validation stage that closes the job/action graph and checks capabilities before execution side effects. |
| Backend | The selected execution implementation: `docker` or `microvm`. Selection is explicit; there is no fallback. |
| Broker | The GitHub Actions Runner V2 service endpoint that delivers control/job messages to a runner session. |
| CAS | Content-addressed storage. Velnor stores blobs/trees by verified BLAKE3 digest and reclaims unreachable content. |
| Control plane | Node-local services and durable lifecycle/event state plus bounded projections used to inspect resources, logs, telemetry, and lifecycle intent. Query, log, and storage projections are currently not repopulated after service reopen. |
| Generation | A fencing number attached to a slot, job, lease, or isolation identity. Older generations cannot claim current ownership. |
| Guest agent | The Velnor process inside a Firecracker guest. It receives a validated plan over vsock and executes it using guest-local Docker. |
| JIT runner | A GitHub runner configuration created just before use. Velnor treats registration as ephemeral and renews/recycles it explicitly. |
| Lease | A time-bounded ownership token for remote job work, action production, cache publication, or storage reservation. |
| Plan | The normalized, backend-neutral representation of a job after GitHub payload parsing and admission. |
| Reconciliation | A bounded comparison of desired state, local state, child processes, heartbeats, registrations, and durable outboxes, followed by exact owned actions. |
| Runner V2 | The upstream GitHub Actions runner protocol family used by Velnor for broker, run-service, timeline, credentials, and completion behavior. |
| Run service | The GitHub endpoint used to acquire a delivered job, renew its lease, and publish completion. |
| Slot | One ready runner worker. Each slot is an independently supervised OS process with a generation-bound heartbeat. |
| Trust scope | The operator-selected boundary controlling whether a job may use shared Docker, secrets, caches, and other capabilities. |
| Outbox | Durable completion intent awaiting remote send/acknowledgement. It makes a crash after local commit recoverable without guessing whether the remote call happened. |
| vsock | Host/guest transport used by the Firecracker execution path for readiness, plan delivery, stdio, results, and completion acknowledgements. |
| `CURRENT` | Implemented and supported by relevant source/tests, or backed by clearly identified current evidence. |
| `FUTURE` | Planned, incomplete, live-gated, design-only, or otherwise not a current capability claim. |
| `HISTORICAL` | Dated evidence tied to an older version/tree. Useful context; not current behavior. |
| `NOW` | Local label for a current observable command result; equivalent to `CURRENT` for that command. |
| `RESERVED` | Syntax or an internal model exists, but the public path is unavailable or fails closed. |
| `PROTOCOL` | An implemented external wire contract. |
| `INTERNAL` | An application or model contract that is not necessarily a public route. |
| `DESIGN-ONLY` | A stated design or limit that the implementation does not prove. |
| `OPEN/UNPROVEN` | No completion or capability claim is made. |

Start with [architecture](architecture.md) for the relationships among these
terms, then [execution](../guides/execution.md) for the job lifecycle.
