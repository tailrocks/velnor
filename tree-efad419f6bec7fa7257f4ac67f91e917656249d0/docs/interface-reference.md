# Velnor interface reference

Source snapshot: 2026-09-01. Citations use repository-relative `path:line`.

Status markers:

- `CURRENT` — implemented and reachable from the current source path.
- `RESERVED` — syntax or model exists, but the current path fails closed, is unavailable, or is only a plan.
- `PROTOCOL` — current external wire contract implemented by the runner/client.
- `INTERNAL` — current application port or model contract, not necessarily a public route.

> Navigation: [← Integrations](integrations.md) · [Index](index.md) · [Next: Runner protocol →](runner-protocol-reference.md)

## Identity and limits

| Surface | Current contract |
| --- | --- |
| CLI | `velnorctl`; command metadata is generated from the live clap tree. `crates/velnorctl/src/lib.rs:29-30`, `1317-1374` |
| Control transport | `velnor-client/v1`; API version `v1`. `crates/velnor-client/src/lib.rs:9-10`, `crates/velnor-client/src/http.rs:150-170` |
| Unix endpoint | `unix:///run/velnor/<instance>`; instance is lowercase alphanumeric, `_` or `-`, 1–64 chars. `crates/velnor-client/src/unix.rs:22-92`, `118-131` |
| Query page | Default 100; accepted limit 1–1000. `crates/velnorctl/src/http.rs:686-706`, `crates/velnor-control/src/query.rs:14-15`, `78-155` |
| HTTP body | Admin mutation body limit 64 KiB. `crates/velnorctl/src/http.rs:112-124` |
| HTTP telemetry cursor | `v1.<epoch>.<sequence>`, max 128 bytes. `crates/velnorctl/src/http.rs:787-855` |
| Runner | Runner version `2.337.0`; user agent `actions-runner/2.337.0 (velnor)`. `crates/velnor-runner/src/protocol.rs:29-38` |
| Capability manifest | Version `10`; max 4096 steps and 256 inputs. `crates/velnor-runner/src/manifest.rs:17-24`, `1572-1653` |
| Vsock | Protocol version `6`; max payload 1 MiB; stdout stream `1`, stderr stream `2`. `crates/velnor-model/src/vsock_protocol.rs:12-21` |

## CLI

### Global flags

These flags apply before every top-level command.

| Flag | Value/default | Status and behavior |
| --- | --- | --- |
| `--context NAME` | optional | `CURRENT`; select a named local context. |
| `-o, --output FORMAT` | `table` | `CURRENT`; `table`, `wide`, `json`, `yaml`, `jsonl`, `name`. |
| `--instance NAME` | env `VELNOR_INSTANCE`, then `default` | `CURRENT`; selects the Unix daemon instance. |
| `--repo REPO` | optional | `CURRENT`; global repository input where a command consumes it. |
| `--selector SELECTOR` | optional | `CURRENT`; comma-separated equality terms. |
| `--field-selector SELECTOR` | optional | `CURRENT`; same selector grammar, separate field namespace. |
| `--since SINCE` | optional | `CURRENT`; RFC3339 or relative `45s`, `10m`, `2h`, `1h30m`. |
| `--timeout SECONDS` | optional | `CURRENT`; whole-command deadline. Timeout is exit class `timeout`. |
| `--no-color` | false | `CURRENT`; forces color off. |
| `-v, --verbose` | repeatable, normal | `CURRENT`; normal, verbose, debug, trace; 3+ repeats saturate at trace. |

Definitions and parsing are in `crates/velnorctl/src/lib.rs:84-190`.

### Command table

| Command | Current syntax / command flags | Status |
| --- | --- | --- |
| `man` | Generate command man pages. | `CURRENT` |
| `completion SHELL` | Generate shell completion. | `CURRENT` |
| `version` | No command flags. | `CURRENT` |
| `api-resources` | Print the versioned resource catalog. | `CURRENT` |
| `explain FEATURE` | Explain a resource/field/action feature. | `CURRENT` |
| `get RESOURCE` | `hosts`, `instances`, `slots`, `runners`, `jobs`, `runs`, `queue`, `events`, `reservations`, `leases`; query flags: `--selector`, `--field-selector`, `--page-token`, `--limit`, `--since`. | `CURRENT` |
| `describe RESOURCE/NAME` | One resource identity. | `CURRENT` |
| `logs RESOURCE` | `--source`, `--cursor`, `--step`, `--failed`, `--tail`. `--step` and `--failed` are currently parsed but ignored. | `CURRENT`; only supported filters affect the request. |
| `telemetry` | `--after`, `--limit`. | `CURRENT` |
| `events` | Query flags from `get`. | `CURRENT` |
| `top TARGET` | Targets: `host`, `instances`, `slots`, `jobs`, `storage`. | `CURRENT` |
| `wait RESOURCE` | `--for CONDITION`. | `CURRENT` |
| `reconcile TARGET` | Targets: `runners`, `jobs`, `docker`, `storage`; `--dry-run`, `--yes`, `--plan-id`, `--reason`. | `RESERVED`; no reviewed plan is available. |
| `cordon TARGET` | `--reason`, `--idempotency-key`, optional `--expected-version`. | `RESERVED`; HTTP mutation adapter is not installed. |
| `uncordon TARGET` | Same lifecycle flags. | `RESERVED`; HTTP mutation adapter is not installed. |
| `drain TARGET` | Same lifecycle flags. | `RESERVED`; HTTP mutation adapter is not installed. |
| `resume TARGET` | Same lifecycle flags. | `RESERVED`; HTTP mutation adapter is not installed. |
| `restart TARGET` | Same lifecycle flags. | `RESERVED`; HTTP mutation adapter is not installed. |
| `recycle TARGET` | Same lifecycle flags. | `RESERVED`; HTTP mutation adapter is not installed. |
| `scale TARGET` | Required `--slots`, `--reason`, `--idempotency-key`; optional `--expected-version`. | `RESERVED`; HTTP mutation adapter is not installed. |
| `cache du` | Store-size report; inherited cache path/budget flags. | `CURRENT` |
| `cache gc` | `--dry-run`, `--yes`, `--force-no-lease-check`, `--keep-newest-targets` (3), `--max-age-days` (30), optional `--max-size-bytes`. | `CURRENT` |
| `capabilities` | Runtime validation/export command. | `CURRENT` |
| `configure` | GitHub registration: `--url`, `--pat`, `--name`, `--labels`, `--target-mvp-labels`, `--target-mvp-arm-label`, `--replace`, `--pool-id`, `--pool-name`, `--dry-run`, `--config-dir`. | `CURRENT` |
| `doctor` | `--url`, `--name` (default `velnor`), `--slots` (1), `--pat`. | `CURRENT` |
| `preflight` | `--config-dir`, `--work-dir`, `--docker-host-work-dir`, `--docker-image` (`velnor/job-ubuntu:26.04`), `--require-buildx` (true). | `CURRENT` |
| `remove` | `--pat`, `--local-only`, `--slots` (1), `--config-dir`. | `CURRENT` |
| `status` | `--config-dir`, `--slots` (1), `--check-target-mvp`, `--json`, `--state-dir`. | `CURRENT` |
| `daemon` | See daemon flag table below. | `CURRENT`; starts control and admin sockets with the daemon lifetime. |
| `storage paths` | Inspect canonical paths. | `CURRENT` |
| `storage status` | Inspect storage status. | `CURRENT` |
| `storage du` | Store usage. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `storage gc` | Storage GC plan/execute syntax. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `storage history` | Storage history. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `storage reservations` | Reservations. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `storage leases` | Leases. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `storage explain-pressure` | Pressure explanation. | `RESERVED`; CLI returns `control.api.unavailable`. |
| `run list` | Resource query flags. | `CURRENT`; reads `runs`, but current handler discards those flags and uses default query parameters. |
| `run view RUN_ID` | Numeric run id. | `CURRENT`; reads `runs`. |
| `run watch RUN_ID` | Watch run events. | `CURRENT`; current handler passes `RUN_ID` as an event cursor and does not filter events by run identity. |
| `run logs RUN_ID` | Read run logs. | `CURRENT` |
| `run cancel RUN_ID` | Cancel. | `RESERVED`; no authoritative service route. |
| `run rerun RUN_ID` | Rerun. | `RESERVED`; no authoritative service route. |
| `run download RUN_ID` | Download. | `RESERVED`; no authoritative service route. |
| `run dispatch WORKFLOW` | Required `--repo`; `--reference` defaults to `main`. | `RESERVED`; no authoritative service route. |
| `run open RUN_ID` | Open run. | `RESERVED`; no authoritative service route. |
| `config` | `view`, `validate`, `diff`, `sources`. | `CURRENT`; current implementation resolves built-in data only and reports the other source names as metadata. |
| `context` | `list`, `current`, `use NAME`, `set NAME --endpoint`, `delete NAME`. | `CURRENT`; local context file. |
| `auth` | `status`, `check`. | `CURRENT`; `check` can return a condition failure for unproven permissions. |
| `instance` | `init`, `install`, `apply`, `delete NAME`. | `CURRENT` shape; operations emit a local plan, not a remote mutation. |
| `capability` | `list`, `explain FEATURE`, `check --job-dump PATH`, `export`. | `CURRENT`; compiled strict manifest. |
| `adapter` | `list`, `describe FEATURE`, `check FEATURE`. | `CURRENT`; native adapter metadata/checks. |
| `workflow check` | Required `--repo`, `--reference`, `--workflow`. | `CURRENT`; basic input validation plus control-API reachability, not full workflow compatibility proof. |
| `diagnostics bundle` | `--archive PATH`. | `RESERVED`; no authoritative bundle route. |
| `canary` | `--timeout-seconds` (60), `--fixture`, optional `--report PATH`. | `CURRENT`; fixture or external queue-to-completion probe. |
| `release` | No parser variant. | `RESERVED` namespace for package production. |

The exhaustive command registry and dispatch status are in `crates/velnorctl/src/lib.rs:192-278`, `448-558`, `930-1020`. Argument structs are in `crates/velnorctl/src/commands.rs:11-432`.

### Daemon and runtime flags

| Command | Flags |
| --- | --- |
| `daemon` | `--state-db` (`VELNOR_STATE_DB`), `--config-dir`, `--url`, `--pat` (`GITHUB_TOKEN`), `--name`, comma-delimited `--labels`, `--target-mvp-labels`, `--target-mvp-arm-label`, `--replace`, `--pool-id`, `--pool-name` (`VELNOR_POOL_NAME`), `--routing-policy-file`, `--dry-run-registration`, `--slots` (1), `--max-idle-slot-age-seconds`, `--once`, `--idle-timeout-seconds`, `--complete-noop`, `--execute-scripts`, `--dry-run-jobs`, `--dump-job-message`, `--docker-image` (`velnor/job-ubuntu:26.04`), `--job-cpus` (`2`), `--job-memory` (`4g`), `--trust-scope` (`public`), `--emergency-reserve-bytes` (10 GiB), `--job-peak-bytes` (4 GiB), `--node-action-image` (`velnor/node-actions:latest`), `--work-dir`, `--docker-host-work-dir`, `--skip-preflight`, `--require-docker-socket`. |
| `cache` | `--work-dir`, `--config-dir`, `--budget-targets-bytes` (200 GiB, `VELNOR_BUDGET_TARGETS_BYTES`), `--budget-caches-bytes` (50 GiB, `VELNOR_BUDGET_CACHES_BYTES`), `--budget-artifacts-bytes` (20 GiB), `--budget-cargo-bytes` (20 GiB), `--budget-mise-bytes` (20 GiB). |
| `configure` | `--url`, `--pat` (`GITHUB_TOKEN`), `--name`, comma-delimited `--labels`, `--target-mvp-labels`, `--target-mvp-arm-label`, `--replace`, `--pool-id`, `--pool-name`, `--dry-run`, `--config-dir`. |
| `doctor` | `--url`, `--name` (`velnor`), `--slots` (1), `--pat` (`GITHUB_TOKEN`). |
| `preflight` | `--config-dir`, `--work-dir`, `--docker-host-work-dir`, `--docker-image` (`velnor/job-ubuntu:26.04`), `--require-buildx` (true). |
| `remove` | `--pat` (`GITHUB_TOKEN`), `--local-only`, `--slots` (1), `--config-dir`. |
| `status` | `--config-dir`, `--slots` (1), `--check-target-mvp`, `--json`, `--state-dir`. |

Runtime definitions: `crates/velnorctl/src/runtime.rs:18-87`, `336-418`, `445-518`, `587-638`, `666-727`. Daemon wiring and socket ownership: `crates/velnorctl/src/runtime.rs:89-165`.

## Unix control API

### Endpoint and authorization

| Socket | Path | Mode/group | Routes |
| --- | --- | --- | --- |
| Control | `/run/velnor/<instance>/control.sock` | `0660`, group `velnor` | Read/query plane. |
| Admin | `/run/velnor/<instance>/admin.sock` | `0660`, group `velnor-admin` | Info plus reserved mutation plane. |

The endpoint parser rejects query/fragment components and path traversal. The server validates peer credentials; root, owner, or an allowed group may pass, while missing/incomplete credentials are denied. `crates/velnor-client/src/unix.rs:29-92`, `crates/velnorctl/src/http.rs:141-191`, `468-510`.

### Routes

| Method/path | Request | Response/status | Status |
| --- | --- | --- | --- |
| `GET /v1/info` on control | none | `{apiVersion, schemaVersion, mutations:false}` | `CURRENT` |
| `GET /v1/info` on admin | none | Same info; mutations currently false. | `CURRENT` |
| `GET /v1/{resource_kind}` | `selector`, `fieldSelector`, `pageToken`, `since`, `limit`; default limit 100 | Resource page with `resources`, optional `nextPageToken`. | `CURRENT` |
| `GET /v1/watch` | `resourceKind`, `afterVersion`, `limit`; default limit 100 | Versioned event items. | `CURRENT` |
| `GET /v1/logs/{subject}` | `source`, `cursor`, `limit`; default limit 100 | Log items with cursor/source/sequence/message. | `CURRENT` |
| `GET /v1/telemetry` | `after`, `limit`; default limit 100 | Records, `nextCursor`, `droppedBefore`. | `CURRENT` |
| `POST /v1/instances/{instance}/{operation}` | JSON mutation body below | Would return operation id/phase; current server returns HTTP 501 with error reason `operation.unsupported`. | `RESERVED` |

Router construction and handlers: `crates/velnorctl/src/http.rs:99-124`, `660-908`. Client request shapes: `crates/velnor-client/src/http.rs:24-118`, `150-291`.

Minimal read request:

```sh
curl --unix-socket /run/velnor/default/control.sock \
  http://localhost/v1/info
```

Response:

```json
{"apiVersion":"v1","schemaVersion":1,"mutations":false}
```

The admin socket returns the same `mutations:false` response today. A local
socket request needs filesystem permission for the socket’s group; this example
does not bypass peer authorization.

The client default request timeout is 10 seconds. Requests require the `/v1/` path prefix, carry a local `X-Request-Id`, bound response size, and accept only 2xx responses. Mutation calls first require admin info with `mutations:true`; the current server advertises false, so the client rejects the operation before posting. `crates/velnor-client/src/http.rs:120-147`, `244-335`.

Mutation body:

```json
{
  "operation": "cordon|uncordon|drain|resume|restart|scale|recycle|reconcile",
  "reason": "operator reason",
  "idempotencyKey": "unique key",
  "expectedVersion": 12,
  "slots": 4
}
```

If the mutation adapter is enabled, `operation` must match the path, reason and
idempotency key must be non-empty and max 512 bytes, and scale must be 1–4096.
The current HTTP route returns `operation.unsupported` before validating this
body; these rules are the internal lifecycle contract, not currently reachable
public validation. `crates/velnorctl/src/http.rs:858-946`,
`crates/velnor-control/src/lifecycle.rs:289-347`.

### API error mapping

| HTTP | Envelope class | Exit code | Meaning |
| --- | --- | ---: | --- |
| 400 | `usage` | 2 | Invalid JSON, query, or field. |
| 403 | `authorization` | 3 | Peer or operation denied. |
| 404 | `unavailable` | 4 | Resource/service absent. |
| 409 | `conflict` | 6 | Version/idempotency/operation conflict. |
| 500 | `operation` | 8 | Operation failure. |
| 501 | `operation` | 8 | Declared operation unsupported. |
| 503 | `transport` | 7 | Bounded application concurrency shed. |

Machine envelope fields are `class`, `code`, `reason`, optional `requestId`, optional `remediation`; unknown fields are rejected. Exit classes also include `success` 0, `condition` 1, `timeout` 5, and `interrupted` 130. `crates/velnorctl/src/http.rs:948-1046`, `crates/velnor-model/src/error_envelope.rs:6-81`, `115-159`.

## Control application ports

`ApplicationServices` wires query, event/watch, logs, lifecycle, storage, and telemetry services. In the current production bundle, lifecycle/events/telemetry use durable paths, while query, logs, and storage projections are newly in-memory and empty after reopening; normalized readers are not yet wired. Test construction is also in-memory. `crates/velnor-control/src/application.rs:19-102`, `220-254`.

### Query, watch, logs, telemetry

| Port | Request | Result |
| --- | --- | --- |
| `QueryPort` | resource kind, selectors, page token, limit, since | Sorted resource page; page token binds generation, filters, limit, and offset. |
| `WatchPort` | resource kind, after version, limit | Ordered versioned events. |
| `LogPort` | subject, source, cursor, limit | Ordered log items. |
| `TelemetryPort` | after cursor, limit | Telemetry page and cursor metadata. |

Query selectors support `name`, `metadata.name`, `kind`, `resourceKind`, and `source`; comma-separated `key=value` terms are ANDed. Page tokens use `v1:<generation>:<fingerprint>:<offset>` and stale/filter-mismatched tokens fail closed. `crates/velnor-control/src/ports.rs:48-163`, `214-236`; `crates/velnor-control/src/query.rs:78-155`, `223-245`.

### Lifecycle

`MutationPort` accepts `cordon`, `uncordon`, `drain`, `recycle`, `resume`, `restart`, `scale`, and `reconcile`. Durable mutation records replay the same idempotency key, reject an unexpected version, and return an operation id/phase. Desired states are `cordoned`, `ready`, `draining`, `recycling`, `scaling`, and `reconciling`. `crates/velnor-control/src/ports.rs:165-242`, `crates/velnor-control/src/lifecycle.rs:133-247`.

Marker: `INTERNAL` current port; `RESERVED` public HTTP/CLI mutation path because the adapter returns unsupported before invoking this port. `crates/velnorctl/src/http.rs:868-908`.

### Storage

`StorageService` validates object id/owner/scope, caps the catalog at 100,000 objects, snapshots scope accounting, creates bounded GC plans with a 900-second TTL, and requires confirmation plus digest/version revalidation for execution. Unknown physical bytes make the aggregate physical-byte total unknown. `crates/velnor-control/src/storage.rs:20-24`, `49-211`.

Marker: `INTERNAL` service; only CLI storage `paths` and `status` are currently reachable through `velnorctl`. `crates/velnorctl/src/lib.rs:525-538`.

## Resource model

All resources use camelCase fields and flatten the shared `ResourceMeta`. `AnyResource` is a tagged union with `resourceKind` and PascalCase variant tags; identity is `<kind>:<name>`. `crates/velnor-model/src/resources.rs:1-12`, `250-315`.

| Resource kind | Payload fields beyond shared metadata |
| --- | --- |
| `Host` | `hostname`, optional `agentVersion`, `labels`. `crates/velnor-model/src/resources.rs:14-29` |
| `Instance` | `host`, `version`, optional `uptimeMs`, `slotsConfigured`, `slotsBusy`. `crates/velnor-model/src/resources.rs:31-49` |
| `Slot` | `host`, `index`, `slotKind`, `phase`, optional `job`. `crates/velnor-model/src/resources.rs:51-72` |
| `RunnerRegistration` | `labels`, `ephemeral`, `online`. `crates/velnor-model/src/resources.rs:74-88` |
| `Job` | `repository`, `run`, `workflow`, `headBranch`, `queuedMs`, `durationMs`, `conclusion`. `crates/velnor-model/src/resources.rs:90-117` |
| `Run` | `repository`, `number`, `headSha`, `headBranch`, `event`, `status`, `conclusion`, `url`. `crates/velnor-model/src/resources.rs:119-145` |
| `QueueEntry` | `position`, `job`, `waitMs`. `crates/velnor-model/src/resources.rs:147-162` |
| `Event` | `sequence`, `occurredAt`, `eventKind`, `subject`, `detail`. `crates/velnor-model/src/resources.rs:164-185` |
| `Reservation` | `slot`, `purpose`, `expiresAt`. `crates/velnor-model/src/resources.rs:187-200` |
| `Lease` | `holder`, `ttlMs`, `expiresAt`. `crates/velnor-model/src/resources.rs:202-216` |
| `Capability` | `key`, `supported`, `details`. `crates/velnor-model/src/resources.rs:218-232` |
| `Adapter` | `adapter`, `version`, `actions`. `crates/velnor-model/src/resources.rs:234-248` |

## Rendering and CLI metadata

| Format | Contract |
| --- | --- |
| `table` | Human table to stdout; warnings to stderr. |
| `wide` | Table plus `SOURCE`, `REASON`, `LAST-TRANSITION`. |
| `json` | Pretty, versioned resource collection. |
| `yaml` | Versioned resource collection. |
| `jsonl` | One JSON object per line. |
| `name` | Unversioned `<kind>:<name>`. |

Formats and color policy: `crates/velnor-render/src/lib.rs:19-119`, `630-679`. Narrow resource columns are:

| Resource | Columns |
| --- | --- |
| Host | `NAME HOSTNAME AGENT LABELS` |
| Instance | `NAME HOST VERSION UPTIME SLOTS BUSY` |
| Slot | `NAME HOST INDEX CLASS PHASE JOB` |
| RunnerRegistration | `NAME LABELS EPHEMERAL ONLINE` |
| Job | `NAME REPO RUN WORKFLOW QUEUED DURATION CONCLUSION` |
| Run | `NAME REPO NUMBER BRANCH EVENT STATUS CONCLUSION` |
| QueueEntry | `NAME POSITION JOB WAIT` |
| Event | `NAME SEQ OCCURRED EVENT SUBJECT` |
| Reservation | `NAME SLOT PURPOSE EXPIRES` |
| Lease | `NAME HOLDER TTL EXPIRES` |
| Capability | `NAME KEY SUPPORTED` |
| Adapter | `NAME ADAPTER VERSION ACTIONS` |

Column projections: `crates/velnor-render/src/lib.rs:224-464`. CLI schema metadata exposes binary/version, global flags, command names/about, and per-command flag metadata including invocation strings. `crates/velnor-model/src/cli_meta.rs:9-59`.

## Execution and action contracts

### Execution backend

`execution.toml` contains an `[execution]` table with `backend = "docker"` or `backend = "microvm"`. Unknown, missing, malformed, or unsupported configuration fails closed. MicroVM failure does not fall back to Docker; host-Docker-socket maintenance is Docker-only. `crates/velnor-model/src/execution.rs:8-17`, `86-220`.

### Action model

| Type | Values/fields |
| --- | --- |
| Digest | Lowercase 64-hex BLAKE3-256; canonical JSON recursively sorts object keys and preserves array order. |
| Platform | `os`, `arch`, optional `abi`. |
| Trust | `untrusted`, `trusted`, `release`. |
| Execution policy | trust class, network, privileged, timeout, adoptable (default true). |
| Action kind | `checkout`, `source-classification`, `toolchain`, `dependency-resolution`, `compile`, `test-compile`, `test-execute`, `lint`, `format`, `package`, `container-build`, `service-snapshot`, `integration-test`, `artifact-verify`, `sign`, `publish`, `benchmark`, `aggregate`. |
| Action key | command/input-root/image/toolchain/platform/environment/dependency-output digests plus execution policy. |
| Action state | `planned`, `waiting`, `leased`, `running`, `publishing`, `complete`, `failed`, `abandoned`. |

`crates/velnor-action-model/src/lib.rs:17-222`, `224-336`.

### Capability manifest

Current compiled action entries:

| Repository/action key | Current allowance |
| --- | --- |
| `tailrocks/velnor-actions` | Approved composite actions under `actions/run-gate`, `actions/cache-contract`, `actions/aggregate`. |
| `jackin-project/jackin-role-action` | Approved composite action. |
| `fsfe/reuse-action` | Approved composite action. |
| `actions/checkout` | Native checkout plus approved refs. `__native` is allowed only here. |
| `actions/cache` | `restore` and `save` subpaths. |
| `actions/attest-build-provenance` | Approved refs and constrained subject paths. |
| `actions/create-github-app-token` | Approved action. |
| `actions/upload-artifact` | Approved action. |
| `actions/github-script` | Approved action. |
| `actions/download-artifact` | Approved action. |
| `actions/upload-pages-artifact` | Approved action. |
| `actions/configure-pages` | Approved action. |
| `actions/deploy-pages` | Approved action. |
| `dorny/paths-filter` | Approved action. |
| `jdx/mise-action` | Approved action. |
| `mozilla-actions/sccache-action` | Approved action. |
| `kunobi-ninja/kache-action` | Approved action. |
| `rui314/setup-mold` | Approved action. |
| `extractions/setup-just` | Approved action. |
| `swatinem/rust-cache` | Approved action. |
| `crazy-max/ghaction-github-runtime` | Approved action. |
| `renovatebot/github-action` | Approved action. |
| `docker/setup-buildx-action` | Approved action. |
| `docker/login-action` | Approved action. |
| `docker/metadata-action` | Approved action. |
| `docker/build-push-action` | Approved action. |
| `docker/bake-action` | Approved action. |
| `hadolint/hadolint-action` | Approved action. |
| `docker/setup-qemu-action` | Approved action. |
| `sigstore/cosign-installer` | Approved action. |

All ordinary action refs must be full 40-hex SHAs. The only mutable-ref transition allowlist is the explicit plan-041 set; it is a temporary integrity exception, not a general interface. Reusable workflows are server-expanded and require full-SHA refs. Manifest entries and reusable workflows: `crates/velnor-runner/src/manifest.rs:344-765`. Validation, input constraints, and attestation checks: `crates/velnor-runner/src/manifest.rs:779-895`, `1063-1133`, `1288-1412`.

Reusable workflows currently declared:

- `jackin-project/jackin-role-action/.github/workflows/publish.yml`, inputs `jackin-version`, `registry`, `runner-amd64`, `runner-arm64`, `runner-merge`, `publish`.
- `tailrocks/velnor-actions/.github/workflows/package-signer.yml`, inputs `artifact-name`, `subject-path`, `source-ref`.

Input rules are `any`, literal, required literal, forbidden, or predicate. Violations carry step/repository/action ref/field/received/accepted/manifest version, with received values redacted in display. `crates/velnor-runner/src/manifest.rs:69-125`.

## Runner job message

The runner job message type is `PipelineAgentJobRequest`. `AgentJobRequestMessage` accepts the provider job envelope with:

| Group | Fields |
| --- | --- |
| Identity/timing | `messageType`, `jobId`, `jobDisplayName`, `jobName`, `requestId`, `lockedUntil`, `queueTime`. |
| Plan/timeline | `plan`, `timeline`. |
| Inputs | `variables`, `mask`, `resources`, `steps`. |
| Job context | `environmentVariables`, `defaults`, `jobContainer`, `jobServiceContainers`, `jobOutputs`, `workspace`, `contextData`, `actionsEnvironment`, optional `billingOwnerId`, `actionsDependencies`. |

`plan` carries scope/type/version/id/group/artifact URI/location/definition/owner. `timeline` carries id/change id/location. Resources contain service endpoints, repositories, and containers. `crates/velnor-runner/src/job_message.rs:8-177`.

Each `ActionStep` carries type/id/name/display fields, enabled/condition/continue-on-error/timeout, context, reference, environment, and inputs. References are repository, container registry, or script; numeric `1/2/3` and accepted string spellings decode to those types. `crates/velnor-runner/src/job_message.rs:179-295`.

## GitHub runner protocol

### Scope and registration

Supported scopes are enterprise, organization, and repository. Hosted GitHub uses `github.com`/`api.github.com`; GHES uses `/api/v3/` API paths. The client constructs JIT-config, runner, group, queued-job, workflow-run-cancel, and repository/org endpoints from the validated scope. `crates/velnor-runner/src/protocol.rs:459-590`.

JIT registration uses name, runner group id, labels, and work folder; the response carries runner metadata and an encoded JIT config. Decoded settings include agent/pool/server/work-folder, v2-flow, ephemeral, and update policy; credentials are scheme/data or OAuth JWT material. `crates/velnor-runner/src/protocol.rs:651-894`, `1017-1041`, `1133-1326`.

Auth endpoints require HTTPS except loopback test endpoints. Tokens, private keys, and response bodies are redacted by the protocol error/reporting helpers. `crates/velnor-runner/src/protocol.rs:249-270`.

### Service clients and lifecycle

| Client | Current operations |
| --- | --- |
| `BrokerClient` | Create/delete session, poll runner message, acknowledge runner request. Poll classes: empty, message, error; error classes include authentication, forbidden, missing session, conflict, rate-limited, client, server, transport. |
| `RunServiceClient` | Acquire job, renew job, complete job; acquisition retries are bounded, and completion retries transient statuses while treating deterministic 4xx as terminal. |
| `DistributedTaskClient` | Agent pools, agents, agent/session/message lifecycle, renew/finish requests, job-completed event, timeline records, and feed append. |
| `RegistrationClient` | JIT config, runner-group lookup, runner listing, queued-job listing. |
| `FeedStreamClient` | Connect, send log lines, ping; per-call append remains as a compatibility surface, while connect/send is preferred. Removal remains future migration work. |
| `TwirpResultsClient` | Update steps; upload step/job logs and step summary through signed blob URLs and metadata calls. |

Protocol client methods and retry/status classification: `crates/velnor-runner/src/protocol.rs:1694-1767`, `1858-2157`, `2246-2550`, `3693-4106`, `4310-4380`.

### Provider DTOs and enums

| DTO/enum | Contract |
| --- | --- |
| Agent pool | `id`, `name`, `isHosted`, `isInternal`. |
| Agent/session | Agent id/name/version/OS/max parallelism/ephemeral/update policy/labels/authorization/properties; session id plus encryption key. |
| Agent message | message id/type/body/IV. Job request type is `RunnerJobRequest`. |
| Job result | Plan/job ids, conclusion, outputs, step results, annotations, telemetry, environment URL, billing owner, infrastructure failure category. |
| Timeline | Job/task records with ids, parent, type, times, state/result, worker/order/ref, and error/warning/notice counts. |
| Task result | `succeeded`, `failed`, `canceled`, `skipped`, `abandoned`. |
| Runner status | `online`, `busy`, `offline`. |
| Step status/conclusion | In progress `3`, completed `6`; conclusions unknown `0`, success `2`, failure `3`, cancelled `4`, skipped `7`. |

DTOs and enum spellings: `crates/velnor-runner/src/protocol.rs:2951-3670`, `3849-3885`.

## Lifecycle event/state vocabulary

Event reasons are a closed 17-token set: `readiness.ready`, `readiness.degraded`, `drain.started`, `drain.completed`, `slot.state_changed`, `registration.missing`, `registration.offline`, `registration.stale_busy`, `job.acquired`, `job.waiting`, `job.started`, `job.completed`, `job.canceled`, `job.rejected`, `capacity.pressure`, `gc.started`, `gc.completed`. `crates/velnor-model/src/lifecycle.rs:33-148`.

Job states are `queued`, `acquired`, `waiting`, `started`, `completed`, `canceled`, `rejected`; terminal states are completed/canceled/rejected. Legal transitions are queued→acquired, acquired→waiting, waiting→started, started→completed/canceled/rejected, and acquired/waiting→canceled/rejected. Other edges, including terminal transitions, fail. `crates/velnor-model/src/lifecycle.rs:163-215`, `228-260`.

## Vsock execution protocol

### Messages

| Kind | Payload |
| ---: | --- |
| 1 `GuestReady` | `isolationId`, `generation`, Docker health, absence of job credentials, session challenge. |
| 2 `DeliverPlan` | Job/isolation/generation, execution nonce, plan SHA256, plan bytes. |
| 3 `ImportBlob` | Digest SHA256 and bytes. |
| 4 `StepStarted` | Step id. |
| 5 `StepCompleted` | Step id, exit code, skipped. |
| 6 `Stdio` | Stream (`1` stdout or `2` stderr), bytes. |
| 7 `CommandFile` | Path and bytes. |
| 8 `Annotation` | Text. |
| 9 `Cancel` | No payload. |
| 10 `Telemetry` | CPU millis and memory bytes. |
| 11 `ResultExport` | Digest SHA256 and bytes. |
| 12 `TeardownAck` | Job/isolation/generation, execution nonce, plan SHA256. |
| 13 `JobCompleted` | Conclusion and exit code. |
| 14 `GuestIdentity` | Isolation id, generation, restored. |
| 15 `PrepareSnapshot` | No payload. |
| 16 `SnapshotReady` | No payload. |

The execution nonce binds a domain prefix, challenge, job id, isolation id, plan digest, and generation. `crates/velnor-model/src/vsock_protocol.rs:23-49`, `55-146`.

### Frame

```text
u16 version (big-endian)
u16 kind (big-endian)
u32 payload_len (big-endian)
payload bytes
32-byte SHA-256 checksum
```

The decoder rejects unsupported versions/kinds, oversized payloads, checksum mismatch, truncation, and trailing bytes; one exact frame is consumed. `crates/velnor-model/src/vsock_protocol.rs:148-253`, `572-586`.

## Log channel contract

The same job output has different wire shapes by destination:

| Destination | Current line shape |
| --- | --- |
| Live WebSocket feed | Raw masked content. The GitHub UI adds its timestamp column. |
| Uploaded step/job log blob | `.NET` round-trip timestamp with exactly seven fractional digits, one space, then masked content: `YYYY-MM-DDTHH:MM:SS.fffffffZ <content>`. |
| V1 timeline feed | Raw masked content. |
| Step metadata | RFC3339 values in fields; never prefix these values onto log lines. |
| Local console mirror | Velnor-local formatting; it is not rendered by GitHub. |

`live_feed_lines` must remain separate from `blob_log_lines`; mixing them
duplicates timestamps in the live UI or leaks them into downloaded content.
Guard tests are `live_feed_lines_are_raw_and_blob_lines_are_timestamped` and
`unix_now_iso8601_is_github_strippable` in
`crates/velnor-runner/src/runner.rs`. Changes to these helpers, their call
sites, or feed/upload clients require the guard tests and a fresh side-by-side
fixture comparison.

## Current/reserved boundary

| Boundary | Marker | Evidence |
| --- | --- | --- |
| CLI parsing, schema metadata, local config/context/auth/instance plans | `CURRENT` | `crates/velnorctl/src/lib.rs:1133-1266`, `1317-1374` |
| Control read API: info, resources, watch, logs, telemetry | `CURRENT` | `crates/velnorctl/src/http.rs:99-110`, `660-821` |
| Application query/watch/log/telemetry ports | `CURRENT` / `INTERNAL` | `crates/velnor-control/src/application.rs:19-102`, `crates/velnor-control/src/ports.rs:214-236` |
| Lifecycle mutation service | `CURRENT` / `INTERNAL` | `crates/velnor-control/src/lifecycle.rs:133-247` |
| Lifecycle HTTP/CLI mutations | `RESERVED` | Adapter returns unsupported and HTTP 501. `crates/velnorctl/src/http.rs:868-908` |
| Storage service and GC model | `CURRENT` / `INTERNAL` | `crates/velnor-control/src/storage.rs:49-211` |
| Public storage `du/gc/history/reservations/leases/explain-pressure` | `RESERVED` | `crates/velnorctl/src/lib.rs:525-538` |
| Run list/view/watch/logs | `CURRENT` | `crates/velnorctl/src/lib.rs:930-969` |
| Run cancel/rerun/download/dispatch/open | `RESERVED` | No authoritative service route. `crates/velnorctl/src/lib.rs:970-975` |
| Reconciliation commands | `RESERVED` | No reviewed plan. `crates/velnorctl/src/lib.rs:908-927` |
| Diagnostics bundle | `RESERVED` | No authoritative bundle route. `crates/velnorctl/src/lib.rs:1005-1020` |
| Strict action manifest and provider runner protocol | `CURRENT` / `PROTOCOL` | `crates/velnor-runner/src/manifest.rs:735-895`, `crates/velnor-runner/src/protocol.rs:1858-2157` |
| Vsock execution framing/messages | `CURRENT` / `PROTOCOL` | `crates/velnor-model/src/vsock_protocol.rs:12-253` |
