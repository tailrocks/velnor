# Plan 067: Serve the versioned Unix-socket control API

> **Executor instructions**: Apply `tailrocks-axum-best-practices` and
> `tailrocks-rust-best-practices` if available. Verify current official Axum and
> Tower documentation before using APIs. Keep Axum types out of model/domain
> crates and handlers thin.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-control crates/velnor-client crates/velnor-model crates/velnor-runner/src/runner.rs crates/velnor-runner/debian`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plan 066
- **Category**: architecture, security
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

`velnorctl` must not make internal files and logs a public compatibility
contract. A versioned local API provides one typed boundary for CLI, future TUI,
monitoring, and remote control while allowing daemon internals to evolve.

## Current state

- No socket server or daemon command channel exists.
- Drain is a process-global atomic toggled only by SIGTERM/SIGINT at
  `runner.rs:804-879`.
- systemd creates `/run/velnor` but current units expose no control socket.
- Existing state spans `runner.json`, `in-flight-job.json`, forensic logs,
  reservation/lease files, Docker, systemd, and GitHub.

## Scope

- `crates/velnor-control/src/http/**`, server lifecycle and application ports
- `crates/velnor-client/src/**`
- API DTOs in `velnor-model`
- daemon composition seam needed to own server lifetime
- Unix read socket `/run/velnor/<instance>/control.sock` and mutation socket
  `/run/velnor/<instance>/admin.sock`

**Out of scope**: TCP/HTTPS, remote auth, CLI command families, lifecycle
implementation beyond typed no-op/test ports.

## API contract

Read routes: `/v1/info`, `/v1/instances`, `/v1/slots`, `/v1/jobs`,
`/v1/events`, and `/v1/storage`. Streams: `/v1/watch` and
`/v1/logs`. Mutation routes: instance cordon/uncordon/drain/scale, slot recycle,
and reconcile target. Unimplemented mutations return a stable `501` error; they
must never return false success.

## Steps

### 1. Build inward-facing application ports

Define narrow query/watch/log/mutation traits in `velnor-control`. HTTP adapters
convert typed input to one use case and typed output. No handler reads files,
runs systemctl/Docker, or calls GitHub directly.

**Verify**: dependency scan proves model/control domain modules do not depend on
Axum, Tower, or HTTP types.

### 2. Implement stable transport and errors

Use separate request/response DTOs, deny unknown fields for mutations, bound
body/concurrency/timeout, propagate request IDs, mark sensitive headers before
tracing, catch panics, and return one versioned safe error envelope. Never emit
filesystem internals, upstream bodies, credentials, or stack traces.

**Verify**: Tower service tests cover success, 404/405, malformed ID/query/body,
unknown fields, timeout, load shed, panic, and safe internal error mapping.

### 3. Enforce Unix authorization

Accept only canonical instance names matching `[a-z0-9][a-z0-9_-]{0,63}`; reject
separators and dot components. Create the path relative to an owned directory
file descriptor with fixed umask/modes and no symlink following. `lstat` every
existing component; unlink only an owned socket at the exact final path, never a
symlink, non-socket, or foreign-owned object.

Use two sockets so supplementary-group ambiguity cannot widen mutation access.
Read routes exist on `control.sock`, owned by group `velnor`; mutation routes
exist only on `admin.sock`, owned by `velnor-admin`; root has full access.
Mutation requests on the read socket are absent/405, not header-upgraded.
Authorize with trusted Unix peer credentials plus socket permissions, never a
caller-supplied header or PID-only lookup. Plan 067's runtime/test setup verifies
both groups exist and fails closed; Plan 076 owns their packaged sysusers/group
creation. Refuse startup if any boundary cannot be enforced.

**Verify**: real-socket tests under distinct test identities prove read allowed,
mutation absent on the read socket, admin mutation routed only on the admin
socket, and unauthorized requests leave no event or state change. Cover
supplementary groups, PID reuse, symlink/non-socket/foreign-owner paths, restart,
and bind/unlink races.

### 4. Own lifecycle and streaming

Server binds before daemon readiness, publishes bind failure, stops admission on
shutdown, drains tracked requests/streams to a deadline, and joins every task.
Watch/log streams have bounded buffers, slow-consumer behavior, cancellation,
and one complete JSON object per JSONL item.

**Verify**: deterministic tests cover disconnect, lag, shutdown, blocked reader,
and no detached task after server exit.

### 5. Implement typed client

Client negotiates `/v1/info`, validates API compatibility, propagates deadlines
and request IDs, decodes stable errors, and supports streaming cancellation. A
canonical instance endpoint is the directory URI
`unix:///run/velnor/<instance>`; read/stream calls select `control.sock` and
mutation calls select `admin.sock`. Never retry a denied mutation against the
read socket or silently downgrade admin to read behavior.

**Verify**: client/server contract tests cover supported and unsupported API
versions, reconnect, timeout, and stream closure.

### 6. Mandatory fixture integration

Against `tailrocks/velnor-actions-fixture`, cancel old fixture runs and delete
only stale validation registrations; prove
clean state. Start daemon with temporary socket/state paths, dispatch a fresh
hold scenario, and query `/v1/info`, slots, jobs, and watch while active. Cancel
through GitHub; check only the new run every at most 60 seconds until terminal.

**Verify**: API observes active then canceled/teardown states; read-only access
cannot mutate; no orphan socket/task/registration remains.

## Done criteria

- [ ] All listed v1 routes and streams have explicit status/error contracts.
- [ ] Unix peer authorization is tested with real sockets.
- [ ] Axum handlers are thin and domain crates are transport-free.
- [ ] Graceful shutdown joins all server work.
- [ ] `rtk mise run check` and fresh fixture hold/cancel run pass.

## STOP conditions

- Runtime cannot reliably obtain Unix peer identity.
- Authorization falls back to request headers.
- Handler must parse internal log/file formats directly.
- API work expands to remote HTTPS in this task.

## Maintenance notes

All future endpoint changes require versioned DTO tests. Remote support in Plan
080 wraps the same application ports; it must not fork semantics.
