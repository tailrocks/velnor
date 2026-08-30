# unit-collector

`unit-collector` is the bounded first slice of TASK-004. It reads Cargo's
newline-delimited JSON messages from stdin and writes:

- `units.jsonl`: one record for each `compiler-artifact` or
  `build-script-executed` message;
- `summary.md`: the deterministic top 10 ranking by `wall_ms`.

It does not launch Cargo, set `CARGO_TARGET_DIR`, inject a compiler wrapper, or
use human-readable log lines as primary evidence.

```text
cargo check --message-format=json-render-diagnostics \
  | unit-collector --out units.jsonl --summary summary.md --mode check
```

Cargo artifact messages provide an explicit `fresh` decision, so the collector
maps `false` to `actual`, `true` to `fresh`, and missing values to `unknown`.
Timing fields are read only from structured `wall_ms`/`cpu_ms` values when
present; otherwise the output records zero and the summary states that timing
was unavailable. Absolute paths in caller-supplied fields are replaced with a
stable marker before serialization.

The library also exposes `OutputSnapshot`, `OutputEvidenceManifest`, and
`OutputEvidence`. A caller can capture declared `compiler-artifact.filenames`
and `executable` outputs before and after the observed operation, then pass the
path-free manifest through `CollectOptions::with_output_evidence`. Snapshots
contain only a BLAKE3 byte fingerprint and modification time. Missing,
unreadable, contradictory, or incomplete evidence is `unknown`; no Cargo
process or human-readable diagnostic is launched or consulted. Supply the
structured Cargo version with `--cargo-version` or
`CollectOptions::with_cargo_version`; omitted or malformed versions remain
explicitly unknown.

Fan-out attribution is available through the explicit `FanoutInput` API. A
caller supplies path-free `UnitId` values from collected records, a
`DependencyGraphInput` whose edges point from upstream dependency to
downstream unit, and an `InvalidationContext` naming the structured root(s).
`attribute_downstream_fanout` then adds direct, reachable, and unique observed
downstream counts to each matching record. It does not infer edges or
causality from message order or logs. Missing roots, missing graph data,
duplicate nodes or edges, unknown endpoints, cycles, and other contradictory
context produce `unknown` metrics; a known zero is reserved for a validated
graph with no downstream units. Fan-out fields are omitted when unknown so
older JSONL consumers can continue to read the additive record format.

Committed structured Cargo JSON fixtures under `tests/fixtures/` cover fresh,
touched-source, and dependency-bump passes. The real Parallax ordinary-PR
attribution report remains required before TASK-004 can be marked complete.
