# Observability and evidence

Status: current implementation map, reviewed 2026-09-01. Velnor has several
observation planes with different durability and disclosure rules. A log, event,
telemetry record, health file, or green test is not automatically proof that a
job or fleet is healthy.

> Navigation: [← Operator guide](operator-now.md) · [Index](index.md) · [Next: Security and data →](security-and-data.md)

## Five observation planes

| Plane | Purpose | Current storage/transport |
| --- | --- | --- |
| Lifecycle events | Explain resource/job state transitions and recovery intent. | Sanitized SQLite event windows; bounded cursors and resnapshot on expiry. |
| Job logs | Show step output and failure context. | Runner forensic files, Results Service uploads, live WebSocket feed, and a bounded local log service. |
| Performance telemetry | Explain queue, tool, cache, compile, link, test, artifact, lease, and retention timings. | Versioned NDJSON with an opaque cursor; bounded ring/file; optional OTLP export. |
| Health/metrics | Prove current control, registration, capacity, executor, and process state. | Atomic health and controller-metrics snapshots plus systemd status. |
| Evidence reports | Capture reproducible fixture/host/package observations. | `velnor-tools`, shell wrappers, test fixtures, and dated Markdown/TSV records. |

The implementation intentionally keeps these planes separate. Do not use a raw
log line as a lifecycle transition, or a telemetry timestamp as a completion
acknowledgement.

## Job observation sequence

```text
RunQueued
  → job.acquired + sanitized admission row
  → RunAdmitted
  → capacity/lease/step/backend telemetry
  → step timelines + live/uploaded logs
  → job.transition.* + completion outcome
  → durable completion outbox ack / terminal observation
```

The runner records queue/admission/capacity facts before execution; executor
events cover tool preparation, cache lookup, compile/link/test, and artifact
materialization. Reporting failures are visible and best-effort; they do not
silently turn an execution failure into success. `crates/velnor-runner/src/runner.rs:5310-5400,5985-6028,6601-6902`,
`crates/velnor-runner/src/executor.rs:1189-1337`,
`crates/velnor-control/src/store/records.rs:2173-2258`.

## Telemetry contract

Each envelope contains:

```text
schema_version, run_id, action_key_digest?, lane, repo, trust_domain,
event, ts_logical, ts_wall, fields
```

Event-specific fields include queue duration, tool/cache timings, compiler/link/
test exit data, artifact digest, wait state, plan counts, lease generation,
consumer count, retention deadline, and trust-revocation reason. The schema is
`velnor.telemetry.v1`; records are NDJSON and pages return `records`,
`next_cursor`, and `dropped_before`. `crates/velnor-model/src/telemetry.rs:433-472,605-709`;
`schemas/velnor.telemetry.v1.json:1-20`.

Default limits are a 4,096-record ring and an 8 MiB telemetry file. Rotation
advances the epoch; it is not a long-term archive. Valid in-memory records can
remain available when file writing fails. `crates/velnor-model/src/telemetry.rs:779-815,1047-1127,1147-1221`.

Runner tracing is a separate JSONL sink at `<config>/logs/trace.jsonl`, rotated
at 32 MiB with one `.1` generation. With the `otel` feature and
`VELNOR_OTLP_ENDPOINT`, spans can be exported over HTTP; exporter failure does
not fail the runner. `crates/velnor-runner/src/telemetry.rs:1-12,101-159`.

## Logs and wire destinations

The same job output has intentionally different formats:

| Destination | Contract |
| --- | --- |
| Live Results Service WebSocket | Raw masked lines; GitHub renders the timestamp column. |
| Uploaded step/job blob | UTC round-trip timestamp with seven fractional digits, one space, then masked content. |
| Timeline feed | Raw masked content. |
| Local forensic file | Velnor-owned diagnostic format; files are `lifecycle.log`, `broker.log`, `registry.log`, and `daemon.log`. |

Mixing raw feed lines with timestamped blob lines changes GitHub UI/download
semantics. Runner forensic logs rotate at 32 MiB with one `.1` file.
`crates/velnor-runner/src/slot_log.rs:1-12,77-181`;
`docs/interface-reference.md` (log channel contract).

## Health and metrics

Health combines control/journal/GitHub/routing/group state, desired/actual/
registered capacity, permits, executor readiness, queue/outbox age, canary,
backend, and lifecycle state. Health JSON is written atomically; the socket is
an optional fast path with file fallback. A journal/control failure is
`NotReady`; a GitHub/routing/group/capacity shortfall is `Degraded`; zero ready
slots or permits is `NotReady`. `crates/velnor-model/src/node.rs:136-285`;
`crates/velnor-runner/src/node/health.rs:1-74`.

`controller-metrics.json` is a current snapshot containing process counts,
reconcile p95, journal transaction/WAL bytes, and CPU buckets. `top` is not a
metrics backend: it queries resource collections and renders them as a live
view. `crates/velnor-runner/src/node/controller.rs:451-493`;
`crates/velnorctl/src/commands.rs:122-136`.

## Retention and durability

| Store | Current bound | Important limit |
| --- | --- | --- |
| Control events | 30 days / 100,000 rows; bounded prune batches. | Retention may defer under leases, deadlines, reserve, or maintenance failure. |
| Terminal jobs | 90 days / 20,000 durable rows. | This bounds durable store rows, not raw log/content retention; API projections may still be empty after service recreation. |
| Control database | 512 MiB ceiling with SQLite WAL accounting. | Physical filesystem free space and external backups are separate. |
| Telemetry | 4,096 records and 8 MiB file by default. | Rotation drops old generations; no archive is promised. |
| Forensic/trace logs | 32 MiB active file plus one `.1`. | Writers are best effort and are not a universal secret-redaction boundary. |
| Action journal | Checksummed WAL records, leases, consumers, and separate supersession retention. | Integrity/recovery record, not an output-content vault. Job-completion outbox belongs to the control journal, not this action journal. |

Sources: `crates/velnor-control/src/store/retention.rs:177-249`,
`crates/velnor-control/src/journal.rs:64-105,352-444`,
`crates/velnor-action-journal/src/lib.rs:95-124`,
`crates/velnor-action-journal/src/supersession.rs:24-30,492-674`.

## Operator surfaces

```sh
velnorctl get events --limit 100
velnorctl logs RUN_OR_JOB --tail 200
velnorctl telemetry --limit 100
velnorctl top host
velnorctl wait instance/default --for Ready --timeout 60
velnorctl status --json
```

The read API is `GET /v1/watch`, `GET /v1/logs/{subject}`, and
`GET /v1/telemetry`; cursors are bounded and opaque. `wait` resnapshots if a
watch cursor expires. `crates/velnor-client/src/http.rs:150-242`;
`crates/velnorctl/src/http.rs:660-821`;
`crates/velnorctl/src/lib.rs:776-831,1277-1306`.

Fixture and host evidence is produced by `velnor-tools fixture-readiness`,
`fixture-report`, `scripts/live_host_doctor.sh`, and `scripts/target_verify.sh`. Fixtures mock
GitHub and use synthetic files; they do not prove live service behavior.
`crates/velnor-tools/src/main.rs:1048-1210,1257-1330,2732-2797`;
`scripts/live_host_doctor.sh:6-76`.

## Current gaps

- Query and log services are instantiated but not repopulated from normalized
  rows after service recreation; storage catalog projection is likewise
  in-memory. `crates/velnor-control/src/application.rs:30-52,220-254`.
- Diagnostics has a bounded/redacted model, but the CLI currently reports the
  bundle as unavailable. `crates/velnor-control/src/diagnostics.rs:1-148`;
  `crates/velnorctl/src/lib.rs:1005-1020`.
- Action-journal telemetry conversion exists, but a production telemetry sink
  hookup is not established by the inspected source.
- Compile telemetry currently reports `metrics_known=false` with zeroed
  counters; no production critical-path emitter was found.
  `crates/velnor-runner/src/executor.rs:1282-1313`.
- Control-event sanitization is stronger than the forensic/tracing writers.
  Do not send secrets or untrusted sensitive payloads to those sinks. See
  [security and data](security-and-data.md).
