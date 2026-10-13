# Runtime checking and proof

Status: implemented runtime behavior only. Source and tests are the authority. Plan prose, design sketches, and future acceptance criteria are not runtime proof.

The runtime rule is simple: validate before side effects, persist intent before
executing the matching side effect, and fail closed when identity, freshness,
integrity, or ownership cannot be proved.

> Navigation: [← Development](development-now.md) · [Index](index.md) · [Next: CI estate contract →](ci-estate-contract.md)

## Runtime path

1. The runner executes an unconditional admission preamble before parsing or dispatch.
2. Strict capability environment and the compiled manifest gate job admission.
3. Admission closes the complete action/workflow graph before execution.
4. Backend preflight proves the selected executor and persists backend-specific proof.
5. Controller, guardian, health, watchdog, registration, and heartbeat reconciliation prove live capacity.
6. Durable journals fence producers and retain completion intent until acknowledged or observed terminal.
7. CAS, metadata, quotas, leases, and reclaim protect stored results.
8. Logs, events, telemetry, and exit envelopes expose bounded, redacted outcomes.

## Check matrix

| Check | Code authority | Result / next action |
|---|---|---|
| Admission preamble | `crates/velnor-runner/src/lib.rs:75-82`; `crates/velnor-runner/src/service.rs:539-575` | Enforce strict capability policy and compiled-manifest integrity before parsing/dispatch. Any error stops the runner. |
| Capability environment | `crates/velnor-runner/src/args.rs:270-306`; tests `crates/velnor-runner/src/args.rs:321-375` | `VELNOR_CAPABILITY_VALIDATION` must be exactly `strict`; removed skip and diagnostic variables fail even when empty or false-looking. Do not echo received values. |
| Manifest references and paths | `crates/velnor-runner/src/manifest.rs:735-895`; `crates/velnor-runner/src/manifest.rs:1040-1133` | Reject duplicate identities, mutable refs, malformed SHAs, unsafe subpaths, unknown actions/inputs, and unresolved constrained inputs. Only the explicit transition-tag allowlist escapes full-SHA validation. |
| Transitively closed admission | `crates/velnor-runner/src/admission.rs:1-15`; `crates/velnor-runner/src/admission.rs:496-772` | Resolve roots, local actions, remote actions, reusable workflows, nested composites, defaults, and inputs before child validation or fetch. Reject on graph, metadata, context, or deadline bounds. |
| Admission rejection before metadata fetch | tests `crates/velnor-runner/src/admission.rs:1810-1851`; `crates/velnor-runner/src/admission.rs:1889-1937` | Invalid expressions, refs, paths, or native-action cases produce bounded read counts; unsupported nested remotes stop after the already-required root read. |
| Backend selection | `crates/velnor-runner/src/runner.rs:4219-4247` | Unreadable backend configuration fails closed. Selected backend determines socket/buildx requirements. |
| Execution preflight | `crates/velnor-runner/src/preflight.rs:14-145`; `crates/velnor-runner/src/preflight.rs:147-313` | Run required Docker, cgroup, workspace, image-tool, script, and marker checks. Nonzero required commands fail with exit/stderr context. MicroVM runs backend preflight plus a synthetic jailed guest-Docker probe; it never falls back to host Docker. |
| Preflight bypass boundary | `crates/velnor-runner/src/runner.rs:4219-4225`; `crates/velnor-runner/src/runner.rs:3733-3817` | Preflight is skipped only for non-executing paths or explicit `skip_preflight`. Executing daemon slots still require the persisted executor proof. |
| Packaged artifact integrity | `crates/velnor-runner/src/execution/artifacts.rs:286-420`; `crates/velnor-runner/src/execution/artifacts.rs:457-505` | Require coherent versions, pins, manifest entries, existence, and SHA-256 equality for every MicroVM artifact. Missing or mismatched artifacts fail closed. |
| Routing proof | `crates/velnor-runner/src/node/prove.rs:24-98`; `crates/velnor-runner/src/node/prove.rs:289-350` | Require valid policy and evidence with exact normalized evidence/policy and runner-group equality. Invalid or missing proof means routing is not ready. |
| Executor proof | `crates/velnor-runner/src/node/prove.rs:84-98`; `crates/velnor-runner/src/node/prove.rs:1200-1222` | Docker requires the executor marker; MicroVM requires generation-bound proof and jailed guest Docker. Host Docker is not accepted as MicroVM proof. |
| Slot session proof | `crates/velnor-runner/src/node/prove.rs:100-127`; `crates/velnor-runner/src/node/prove.rs:247-287` | Process existence alone is insufficient. Require a fresh, generation-matching heartbeat, positive sequence, and exact process identity; otherwise do not treat the slot as live. |
| Health publication | `crates/velnor-runner/src/node/health.rs:1-74` | Write health JSON through a temporary file and rename. The file is authoritative; the socket is an optional fast path with file fallback. |
| Watchdog and READY | `crates/velnor-runner/src/node/watchdog.rs:1-38`; tests `crates/velnor-runner/src/node/watchdog.rs:41-53` | Feed systemd only after a completed local cycle. An incomplete cycle sends neither READY nor watchdog ping. |
| Guardian boundary | `crates/velnor-runner/src/node/guardian.rs:1-49`; `crates/velnor-runner/src/node/guardian.rs:52-85` | Guardian uses the journal and health server, does not read GitHub credentials or open Docker, fences dead/stale slots, publishes health, and can exit after one cycle. |
| Derived health | `crates/velnor-model/src/node.rs:199-223`; `crates/velnor-model/src/node.rs:225-285` | Journal/control failure is `NotReady`; GitHub, routing, group, or capacity shortfall is `Degraded`; zero ready slots or permits is `NotReady`; stable alerts identify the failed proof. |
| Reconciliation loop | `crates/velnor-runner/src/node/controller.rs:390-449`; `crates/velnor-runner/src/node/controller.rs:631-837` | Reconcile bounded state, heartbeat, generations, permits, registration, executor/session proof, outboxes, and health. Remote timeout preserves local state and retries; errors remain fail-closed. |
| Registration identity | `crates/velnor-runner/src/node/controller.rs:1079-1201` | Compare durable local identity with the remote numeric identity. Missing or changed identity records loss, shuts down active jobs as required, and rebuilds instead of guessing. |
| Stale/fenced slot | `crates/velnor-runner/src/node/controller.rs:1960-2071`; `crates/velnor-runner/src/node/controller.rs:2097-2205` | Fence stale generations, TERM the exact proven actor, wait within the bound, and KILL only after exact re-proof. PID reuse is left untouched. |
| Event-first side effects | `crates/velnor-control/src/journal.rs:1-6`; `crates/velnor-control/src/journal.rs:480-516`; `crates/velnor-control/src/journal.rs:1052-1128` | Commit the matching intent event before returning a side-effect command. A crash before commit has neither intent nor command; a crash after commit leaves recoverable intent. |
| Node completion outbox | `crates/velnor-runner/src/node/complete.rs:34-177` | Commit `CompletionIntended`, claim one send, send, and commit remote acknowledgement/terminal observation. A rejected durable transition never sends; unacknowledged outbox rows survive worker loss. |
| Node journal reducer | `crates/velnor-control/src/journal.rs:525-942` | Accept transitions only for the exact slot, job, generation, phase, owner, payload, and outbox state. Replay mismatches, stale generations, and illegal phases are rejected. |
| Control journal integrity | `crates/velnor-control/src/journal.rs:951-1030`; `crates/velnor-control/src/journal.rs:1118-1127` | Require supported schema, WAL, `synchronous=FULL`, foreign keys, and SHA-256 event checksums. Schema/checksum/state corruption stops recovery or writing. |
| Action-event integrity | `crates/velnor-action-journal/src/lib.rs:320-377` | Store canonical JSON plus checksum; recovery verifies checksum, JSON, action digest, state, and record invariants in sequence. |
| Action lease acquisition | `crates/velnor-action-journal/src/lib.rs:613-733` | Acquire generation 1, or CAS-take over an abandoned/expired lease with an incremented generation. Active unexpired is `LeaseBusy`; released is terminal; stale CAS is fenced. |
| Action lease renewal/fencing | `crates/velnor-action-journal/src/lib.rs:735-813`; `crates/velnor-action-journal/src/lib.rs:576-610` | Renew only with exact action key, owner, generation, active state, unexpired deadline, and heartbeat cadence. Any mismatch, expiry, or stale token is fenced. |
| Action completion fence | `crates/velnor-action-journal/src/lib.rs:454-531` | Insert complete record and release the exact live lease in one transaction. A stale producer cannot publish after a newer generation takes over. |
| Action abandon/expiry | `crates/velnor-action-journal/src/lib.rs:815-857` | Release, abandon, and expiry are durable state transitions; expiry increments fencing generation once. Next wake-up is reconstructed from SQLite deadlines. |
| Structured compiler action failure/cancel | `crates/velnor-runner/src/compiler_action.rs:208-343`; `crates/velnor-runner/src/compiler_action.rs:364-467`; test `crates/velnor-runner/src/compiler_action.rs:1409-1418` | Cache hit warms output but physical compilation still runs. Failure, cancellation, identity drift, output failure, or heartbeat failure abandons the producer lease; only revalidated output can publish. |
| Explicit job cancellation | `crates/velnor-runner/src/runner.rs:7088-7142`; `crates/velnor-runner/src/execution/mod.rs:175-203` | A matching `JobCancel` message sets the cancellation path; malformed cancellation is treated conservatively as cancellation. Execution cancels the selected backend. Renewal failure is not itself a cancellation signal. |
| GitHub run cancellation | `crates/velnor-control/src/github.rs:46-80`; `crates/velnor-control/src/github.rs:112-125`; `crates/velnor-runner/src/protocol.rs:616-619` | Inspect authoritative run status first. Completed runs are not canceled again; accepted terminal statuses are 202/409/404; ambiguous transport is `Operation`, not success. |
| Supersession/adoption | `crates/velnor-action-journal/src/supersession.rs:339-426` | When enabled and the action is adoptable, attach/adopt only identical Running/Complete state; lease contention otherwise waits. Physical lifetime is separate from logical consumer lifetime. |
| Detach/reap/termination | `crates/velnor-action-journal/src/supersession.rs:428-573`; `crates/velnor-action-journal/src/supersession.rs:639-892` | Live consumers continue. Trust revocation, non-adoptable work, disabled supersession, failed work, or expired retention terminates through a durable claim; transient termination failure restores a short retry deadline. |
| CAS object integrity | `crates/velnor-cas/src/lib.rs:1-6`; `crates/velnor-cas/src/lib.rs:412-535`; `crates/velnor-cas/src/lib.rs:635-801` | Write temp data, sync, atomically install, and verify digest on reads. Existing corrupt objects are rejected, not reused. Full and streaming reads are bounded. |
| CAS tree/path integrity | `crates/velnor-cas/src/lib.rs:230-346`; `crates/velnor-cas/src/lib.rs:804-865`; `crates/velnor-cas/src/lib.rs:967-1006` | Reject unsafe paths, duplicate/descendant conflicts, symlinks, oversized manifests, excessive depth/nodes/files, and digest/size mismatches. |
| CAS reclaim | `crates/velnor-cas/src/lib.rs:1035-1083`; `crates/velnor-cas/src/lib.rs:1099-1253` | Bound roots and inventory, validate the complete inventory before deletion, mark reachable blobs/trees, then delete only unreachable objects. Malformed roots or storage leave CAS unchanged. |
| Cache quota and publication | `crates/velnor-cache-service/src/lib.rs:613-695`; `crates/velnor-cache-service/src/lib.rs:697-731` | Reclaim before admission/publication, reserve quota, account result and metadata bytes, enforce quota, then use journal completion as the fence. Failed publication reclaims attempted CAS objects. |
| Filesystem cache GC | `crates/velnor-runner/src/cache.rs:1-62`; `crates/velnor-runner/src/cache.rs:142-174` | Shared/exclusive scope locks protect active work; destructive GC requires `--yes`. `--force-no-lease-check` is an explicit unsafe diagnostic bypass and emits a warning. |
| Durable storage budgets | `crates/velnor-control/src/store/retention.rs:792-828`; `crates/velnor-control/src/store/retention.rs:911-958`; `crates/velnor-control/src/store/retention.rs:1705-1737` | Count DB, WAL, reservations, and requested bytes. Reject overflow or invariant corruption; commit only with the exact owner/generation/expiry lease fence. |
| Log redaction | `crates/velnor-control/src/logs.rs:1-25`; `crates/velnor-control/src/logs.rs:38-105`; `crates/velnor-control/src/logs.rs:198-289` | Redact literal and normalized secret forms before storage, reject encoded/ambiguous secret syntax fail-closed, and enforce bounded records, buffers, subjects, and secret registry. |
| Log cursor integrity | `crates/velnor-control/src/logs.rs:108-180`; `crates/velnor-control/src/logs.rs:291-340` | Cursor binds identity, generation, sequence, and fingerprint. Ahead is invalid; expired or changed history requires a conflict/resnapshot. |
| Sanitized DTOs | `crates/velnor-model/src/sanitized.rs:1-129` | URLs strip userinfo; invalid or opaque values are rejected/degraded without echoing secrets; secret references contain names only. |
| Durable control events | `crates/velnor-control/src/journal.rs:363-478`; `crates/velnor-control/src/journal.rs:1118-1127` | Events are immutable, typed, generation-bearing, and checksummed. They are the durable audit/recovery stream; telemetry is not a substitute. |
| Telemetry | `crates/velnor-model/src/telemetry.rs:1-6`; `crates/velnor-model/src/telemetry.rs:433-601`; `crates/velnor-model/src/telemetry.rs:662-765` | Validate schema, IDs, bounded fields, event contracts, digests, accounting, and secret markers. Emit bounded observations separately from control state. |
| Telemetry file sink | `crates/velnor-model/src/telemetry.rs:795-970`; `crates/velnor-model/src/telemetry.rs:1003-1128` | Read/write bounded JSONL with locks, cursors, fingerprints, rotation, and validation. Sink failure is best effort and disables the file sink; it does not change durable control state. |
| Forensic runner logs | `crates/velnor-runner/src/slot_log.rs:1-13`; `crates/velnor-runner/src/slot_log.rs:73-167`; `crates/velnor-runner/src/telemetry.rs:1-12` | Additive bounded rotating logs and trace output are diagnostic only; append failure never changes runner behavior. |
| Public failure class | `crates/velnor-model/src/error_envelope.rs:1-159`; tests `crates/velnor-model/src/error_envelope.rs:165-216` | Map errors to stable classes/codes: success 0, condition 1, usage 2, authorization 3, unavailable 4, timeout 5, conflict 6, transport 7, operation 8, interrupted 130. Unknown envelope fields are rejected. |
| API/client failure mapping | `crates/velnorctl/src/http.rs:948-1044`; `crates/velnor-client/src/http.rs:719-731` | Preserve typed HTTP/API meaning: bad request usage, auth authorization, unavailable/overload transport, conflict conflict, unsupported/invalid operation/usage as defined by the mapping. |
| Remote ambiguity | `crates/velnor-control/src/github.rs:12-43` | Authorization is authorization; missing remote is unavailable; transport or invalid response is operation/not authoritative. Do not claim mutation success from ambiguity. |
| Job command result | `crates/velnor-runner/src/executor.rs:827-864`; `crates/velnor-runner/src/executor.rs:1036-1057`; `crates/velnor-runner/src/executor.rs:1265-1330` | Preserve the command/step exit code in execution results. This is distinct from the public API/control `ExitClass` mapping. |

## What is actually proven

Admission proves a closed, bounded graph and immutable capability identity. It does not merely validate the top-level workflow: nested local/remote actions and reusable workflows are resolved under the admission bounds before their execution paths are trusted. Positive closure and rejection-before-fetch behavior are covered by the admission tests at `crates/velnor-runner/src/admission.rs:1723-1808` and `crates/velnor-runner/src/admission.rs:1810-1937`.

Execution proof is layered. Backend preflight proves the configured executor; artifact verification proves MicroVM inputs; routing evidence proves the selected external identity; the session proof adds fresh generation-bound heartbeat and exact process identity; reconciliation turns those facts into durable registration/readiness state. A process that merely exists is not a ready slot.

The node control journal is the recovery authority for runner ownership and job completion. Its reducer rejects stale generations and illegal transitions; its outbox preserves completion until the remote result is acknowledged or terminal state is observed. The separate action journal is the producer/consumer authority for structured compiler-cache actions. Do not generalize that crate into proof that every arbitrary runner action is cache-backed.

Storage proof has two layers: CAS content-addressed integrity and durable accounting/lease fencing. CAS verifies bytes and tree structure; the cache service and retention store decide whether bytes may be admitted, published, retained, or reclaimed. Reclaim validates the complete inventory before deleting anything.

## Design-only or explicitly non-proof boundaries

- The runner scaffold is a temporary migration path, not a compatibility promise: `crates/velnor-runner/src/lib.rs:58-62`.
- Supersession is implemented but default-disabled (`enabled: false`); its adopt/retain/reap behavior is not active unless configured: `crates/velnor-action-journal/src/supersession.rs:32-53`.
- Controller phase CPU buckets are literal zero placeholders. Only aggregate controller CPU is live: `crates/velnor-runner/src/node/controller.rs:451-489`.
- `observe_session` is a legacy liveness helper. Current slot proof is `observe_slot_session` with fresh heartbeat and identity: `crates/velnor-runner/src/node/prove.rs:100-127`.
- Universal full-SHA enforcement is not true during the explicit fixture transition allowlist: `crates/velnor-runner/src/manifest.rs:741-765`.
- `skip_preflight` and `--force-no-lease-check` are real explicit bypasses. They are not proof; their use must be treated as an operator-selected weaker mode: `crates/velnor-runner/src/runner.rs:4219-4225`; `crates/velnor-runner/src/cache.rs:147-168`.
- Diagnostic logs and telemetry are bounded observability. They cannot establish readiness, ownership, completion, or storage durability: `crates/velnor-runner/src/slot_log.rs:73-91`; `crates/velnor-model/src/telemetry.rs:795-844`.
- A design statement or acceptance target without a corresponding runtime authority above is not implemented behavior. This document intentionally records no roadmap behavior as live proof.
