# Interface reference

This is the current operator and local API reference. Velnor is research software. The control API is local-only and read-only today.

## Quick reference

```sh
velnorctl status [--json]
velnorctl get runners
velnorctl get slots
velnorctl get jobs
velnorctl get runs
velnorctl events
velnorctl logs SUBJECT
velnorctl telemetry
```

Use `velnorctl --help` for the installed binary's complete parser metadata.

## CLI

Global options:

| Option | Meaning |
| --- | --- |
| `--instance NAME` | Select the daemon instance. Defaults to `VELNOR_INSTANCE`, then `default`. |
| `--context NAME` | Select a saved local context. |
| `-o, --output FORMAT` | `table`, `wide`, `json`, `yaml`, `jsonl`, or `name`. |
| `--selector TERMS` | Comma-separated equality filters. |
| `--field-selector TERMS` | Equality filters in the field namespace. |
| `--since VALUE` | RFC3339 or a relative duration such as `10m`. |
| `--timeout SECONDS` | Command deadline. |
| `--no-color` | Disable color. |
| `-v` | Increase verbosity, up to trace. |

### Supported read commands

| Command | Use |
| --- | --- |
| `get RESOURCE` | List `hosts`, `instances`, `slots`, `runners`, `jobs`, `runs`, `queue`, `events`, `reservations`, or `leases`. |
| `describe RESOURCE/NAME` | Read one resource. |
| `events` | Read lifecycle events. |
| `logs SUBJECT` | Read bounded local logs. `--source`, `--cursor`, and `--tail` are supported. `--step` and `--failed` are accepted but currently have no effect. |
| `telemetry` | Read bounded telemetry with `--after` and `--limit`. |
| `wait RESOURCE` | Wait for a condition using snapshot/watch/resnapshot behavior. |
| `top TARGET` | Inspect resource collections for `host`, `instances`, `slots`, `jobs`, or `storage`; this is not a metrics backend. |
| `run list` | List runs. |
| `run view RUN_ID` | Read a run. |
| `run logs RUN_ID` | Read run logs. |
| `run watch RUN_ID` | Watch events; current implementation does not filter events by run identity, so use it with care. |
| `status` | Local health/status check; `--json` emits machine-readable output. |
| `doctor` | Probe registered runners. |
| `preflight` | Check configured execution prerequisites. |
| `capabilities export` | Export the compiled capability manifest. |
| `capabilities check JOB_MESSAGE.json` | Validate a sanitized job-message JSON dump against that manifest. |
| `storage paths` / `storage status` | Inspect local storage paths and status. |
| `api-resources` | Print the resource catalog. |
| `version`, `man`, `completion SHELL` | Version and CLI metadata helpers. |

`cache du` and `cache gc` operate on the local cache when their required local paths are available. Storage commands such as `storage du`, `storage gc`, `storage history`, `storage reservations`, `storage leases`, and `storage explain-pressure` currently return unavailable.

### Registration and local setup commands

```sh
velnorctl configure \
  --url https://github.com/OWNER/REPO \
  --pat "$GITHUB_TOKEN" \
  --name velnor \
  --labels velnor,velnor-target-mvp \
  --config-dir /var/lib/velnor/config
```

`configure` supports repository, organization, and enterprise URLs. Relevant options are:

| Option | Meaning |
| --- | --- |
| `--url` | GitHub repository, organization, or enterprise URL. |
| `--pat` | Registration credential; may come from `GITHUB_TOKEN`. Never put it in a shared command history. |
| `--name` | Runner name. |
| `--labels` | Comma-separated labels. Empty labels become `self-hosted,velnor`. |
| `--target-mvp-labels` | Add the target MVP Linux label set. |
| `--target-mvp-arm-label` | Add `ubuntu-24.04-arm`. |
| `--pool-id`, `--pool-name` | Select the runner group. Organization and enterprise scope require an explicit group rather than implicit `Default`. |
| `--replace` | Replace an existing local registration. |
| `--dry-run` | Validate registration inputs without writing credentials. |
| `--config-dir` | Store runner identity/configuration in this directory. |

Labels are sorted and deduplicated. macOS labels are rejected; execution is Linux-only. The target MVP label additions are `hetzner-sentry-ci`, `ubuntu-24.04`, `ubuntu-latest`, and `velnor-target-mvp`.

Other current setup commands are `auth status`, `auth check`, `context list/current/use/set/delete`, `config view/validate/diff/sources`, `instance init/install/apply/delete`, `adapter list/describe/check`, `workflow check`, `remove`, `status`, and `preflight`. `instance` and `workflow check` are local planning/input checks, not proof of a remote mutation or complete workflow compatibility.

`run cancel`, `run rerun`, `run download`, `run dispatch`, and `run open` are unavailable. `reconcile`, lifecycle commands (`cordon`, `uncordon`, `drain`, `resume`, `restart`, `recycle`, `scale`), and `diagnostics bundle` are also unavailable through the current control adapter. `release` is reserved for package lifecycle tooling.

## Unix control API

Velnor exposes two Unix sockets per instance:

| Socket | Path | Role |
| --- | --- | --- |
| Control | `/run/velnor/<instance>/control.sock` | Read-only queries, watches, logs, and telemetry. |
| Admin | `/run/velnor/<instance>/admin.sock` | Intended mutation socket; currently read-only because mutations are false. |

Sockets normally use mode `0660` with groups `velnor` and `velnor-admin`. Peer credentials are checked. Root, the socket owner, or an allowed group may access a permitted socket.

```sh
curl --unix-socket /run/velnor/default/control.sock http://localhost/v1/info
```

```json
{"apiVersion":"v1","schemaVersion":1,"mutations":false}
```

### Routes

| Method/path | Use |
| --- | --- |
| `GET /v1/info` | API/schema version and mutation capability. |
| `GET /v1/{resourceKind}` | Resource page. Supports `selector`, `fieldSelector`, `pageToken`, `since`, and `limit`. Default limit is 100; maximum is 1000. |
| `GET /v1/watch` | Bounded event page. Supports `resourceKind`, `afterVersion`, and `limit`. |
| `GET /v1/logs/{subject}` | Bounded logs. Supports `source`, `cursor`, and `limit`. |
| `GET /v1/telemetry` | Bounded telemetry. Supports `after` and `limit`; returns `nextCursor` and `droppedBefore`. |
| `POST /v1/instances/{instance}/{operation}` | Reserved mutation shape; currently returns HTTP 501 and `operation.unsupported`. |

Watch, log, and telemetry cursors are bounded by retention. An expired cursor requires a fresh snapshot. The default client request timeout is 10 seconds.

### Errors

| HTTP | Class | Meaning |
| ---: | --- | --- |
| 400 | `usage` | Invalid request. |
| 403 | `authorization` | Peer or operation denied. |
| 404 | `unavailable` | Resource or service absent. |
| 409 | `conflict` | Version, idempotency, or operation conflict. |
| 500 | `operation` | Operation failed. |
| 501 | `operation` | Operation is not implemented. |
| 503 | `transport` | Bounded server concurrency unavailable. |

The JSON error envelope contains `class`, `code`, `reason`, and optional `requestId` and `remediation`. CLI exit classes include success (`0`), condition (`1`), usage (`2`), authorization (`3`), unavailable (`4`), timeout (`5`), conflict (`6`), transport (`7`), operation (`8`), and interrupted (`130`).

## Resources and limits

Resource names are `<kind>:<name>`. Available kinds are `Host`, `Instance`, `Slot`, `RunnerRegistration`, `Job`, `Run`, `QueueEntry`, `Event`, `Reservation`, `Lease`, `Capability`, and `Adapter`.

The API uses camelCase fields. Query pages default to 100 items and accept at most 1000. Watches retain at most 4096 events. Logs are bounded to 16,384 records or 16 MiB. Telemetry is bounded and rotates; it is not an archive.

## Backend and capability contract

Select exactly one backend in `execution.toml`:

```toml
[execution]
backend = "docker" # or "microvm"
```

Missing, unknown, or malformed values fail closed. There is no Docker/Firecracker fallback. Docker requires the host Docker socket and the Docker preflight. MicroVM requires KVM, verified Firecracker/jailer artifacts, and its guest/vsock path; it rejects host Docker-socket use.

Admission validates trust policy, the compiled capability manifest, and the complete action closure before checkout, downloads, leases, credentials, or execution. Unsupported actions, unapproved references, invalid inputs, and unsafe trust combinations are rejected.

Ordinary action references must be full 40-character commit SHAs unless explicitly allowed by the compiled manifest. The manifest includes approved native checkout/cache behavior and a finite allowlist of actions; it is the authority for a particular build. Inspect it with `capabilities export` and test a job message with `capabilities check`.

## Credential boundaries

The operator PAT is used for registration, runner/group lookup, and runner cleanup. It is not a job credential.

GitHub supplies job-scoped credentials in the acquired job payload. Velnor injects them only after admission, including the job token, Actions runtime token, OIDC material, and cache/results credentials. Non-trusted jobs that carry user secrets are rejected where the selected trust policy requires it. MicroVM guests must be credential-free at readiness; credentials are delivered only for the job session.

Packaged installations keep the operator token in `/etc/velnor/secrets.env` with mode `0600`; it is not passed in process arguments.

## Known visibility limits

The read API is local Unix-socket access only. There is no Prometheus endpoint, dashboard, SSE endpoint, or HTTP health endpoint. Query, log, and storage projections may be empty after reopening because their current application wiring is not fully rehydrated; durable lifecycle events are the stronger recovery signal. Controller phase CPU buckets are currently zero. Forensic and tracing log writers are best-effort.
