# Plan 080: Remote Contexts and Fleet Views

> **Executor instructions**: Extend existing C006–C061 commands over remote
> transport. Do not implement or combine leaf commands in this plan.

## Status

`P3 — pending; depends on Plan 079 and the stable local v1 control API.`

## Drift check at implementation start

Confirm the local API version, resource/watch semantics, context schema, Unix authorization model, and real host/instance topology. Inspect current network and trust boundaries before selecting the exact HTTPS and mutual-authentication implementation. This task may adapt transport details to the architecture, but may not add a scheduler or central source of truth.

## Why this task exists

The supplied design includes remote multi-host contexts after the local operator experience is stable. Remote support must carry the same versioned resources and mutation semantics over an authenticated channel, while each Velnor host remains authoritative for its own daemon state and GitHub remains authoritative for workflow runs and queues.

## Command surface

Use the existing global and context commands:

```text
velnorctl --context <name> get hosts|instances|slots|runners|jobs|events
velnorctl --context <name> describe <resource>/<name>
velnorctl --context <name> logs <resource>/<name>
velnorctl --context <name> top host|instances|slots|jobs|storage
velnorctl --context <name> wait <resource>/<name> ...

velnorctl context set <name> --endpoint https://<endpoint>
velnorctl context use <name>
velnorctl context delete <name>
```

Do not add commands beyond the supplied resource, context, and fleet-view examples.

## Required behavior

- Reuse the exact versioned v1 API models used on the Unix socket.
- Authenticate remote endpoints explicitly with HTTPS and mutually authenticated credentials or an equivalently explicit mechanism approved after inspection.
- Store only credential references in context configuration, never private key or bearer-token contents.
- Distinguish `host`, `instance`, stable `slot`, and ephemeral `runner` exactly as the resource model defines.
- Merge host-local Velnor data with GitHub-owned run/queue data only at the client layer and label every source `LOCAL`, `GITHUB`, or `MERGED`.
- Report per-host partial failures; never hide an unreachable host behind an apparently healthy aggregate.
- Preserve read versus admin authorization and require operator reason/audit events for remote mutation.
- Version negotiation must fail clearly on an incompatible API; do not parse implementation files or fall back to SSH/shell commands.

## Implementation steps

1. Add a remote transport to `velnor-client` that implements the same trait as the Unix client and preserves streaming backpressure, cancellation, deadlines, and structured errors.
2. Add an Axum HTTPS listener as a separately configured control-server surface. Reuse the same thin handlers/services as the Unix route; do not duplicate domain logic.
3. Implement authenticated identity mapping to read-only and admin roles. Validate certificate/credential rotation and revocation behavior.
4. Extend context validation, auth status/check, config sources, and diagnostics metadata for remote endpoints and credential references.
5. Extend the existing `host` resource with multi-host aggregation for existing
   get/describe/logs/events/top/wait operations. Give objects globally
   unambiguous identities while preserving original instance and slot names.
6. Bound fan-out concurrency and apply per-host and overall deadlines. Return stable partial-result metadata in machine output and non-zero status when requested completeness is not achieved.
7. Implement remote watch/log reconnect with resource versions/cursors so a connection loss does not silently skip or duplicate state. Make resume limits explicit.
8. Keep all mutation endpoints narrow and typed. Cordon, drain, resume, restart, recycle, scale, and reconcile must retain the same safe defaults as local operation.
9. Add audit events on both client-request and host-execution sides without copying credentials or workflow secrets.
10. Document trust boundaries, port exposure, certificate ownership, API compatibility, and local-only fallback behavior.

## Tests

- Transport-test protocol parity between Unix and HTTPS clients for every v1 resource and mutation.
- Test mutual authentication, expired/revoked credentials, read-only denial, hostname mismatch, version mismatch, reconnect, timeout, and partial-host failure.
- Test that handler bodies remain thin and domain behavior is identical across transports.
- Test aggregate sorting, global identity, source labels, duplicate instance names, fan-out limits, watch resume, and exit statuses.
- Run all Rust tests with `cargo nextest run`; never use `cargo test`.

## Mandatory fixture integration validation

1. Pin the exact `tailrocks/velnor-actions-fixture` commit.
2. Create at least two authenticated test contexts representing distinct Velnor hosts/instances and route a fixture matrix across both without changing workflow semantics.
3. Cancel all old active fixture runs and delete stale runner registrations before dispatch. Monitor only the newly returned run ID.
4. While jobs run, prove fleet `get`, `describe`, `logs --follow`, `events --watch`, `top`, and `wait` identify the correct host, instance, stable slot, ephemeral runner, job, and source.
5. Disconnect one remote endpoint during a controlled fixture execution. Prove partial results are explicit, stream resumption is ordered, the job remains safe, and the reconnected view converges.
6. Prove a read-only remote identity can inspect but cannot cordon, drain,
   recycle, scale, or reconcile. Package-version management remains outside the
   remote API. Prove the admin identity can perform one safe cordon/uncordon
   cycle without interrupting the active fixture job.
7. Check state within 60 seconds and diagnose any unchanged or queued state before two minutes. Store no rendered GitHub HTML.

## Done when

- Local and remote clients expose the same v1 semantics.
- Fleet views are source-labelled, partial-failure-aware, authenticated, and stable under reconnect.
- Fixture execution across two contexts remains correct through observation and a controlled endpoint loss.
- No SSH fallback, hidden filesystem parsing, or central scheduler was introduced.

## Stop condition

Stop after the supplied remote contexts and multi-host inspection/control behavior works. Do not add global scheduling, queue ownership, autoscaling policy, web UI, TUI, or a hosted control-plane product.
