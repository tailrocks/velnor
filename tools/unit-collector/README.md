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

Committed structured Cargo JSON fixtures under `tests/fixtures/` cover fresh,
touched-source, and dependency-bump passes. The real Parallax ordinary-PR
attribution report remains required before TASK-004 can be marked complete.
