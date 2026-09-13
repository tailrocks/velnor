# Plan 2026-09-13: native Engine API slice 1 — read-only fast path (GOAL 17/58)

Base: origin/main tip 3448549c at fetch time (tip has since advanced to
bfefb5c7; this branch stays on the fetch-time base per task). Branch:
r0-engine-api. Worktree: /tmp/velnor-eng.

## Problem

Every host Docker control-plane query costs one `docker` subprocess
(BC-7: 12+N minimal, 52-72 representative per job). GOAL 17 asks for a
native Engine path judged on latency, process count, cancellation,
timeouts, error typing, and dependency cost. The BC-7 typed facade
(`docker/client.rs`), per-class deadlines, per-job metrics, and the
`DockerErrorCategory` taxonomy (#733) all landed before this slice, so
the migration has a routing point, a budget policy, a counter, and an
error vocabulary to plug into.

## Fix (this branch, strict scope)

New `docker/engine.rs`: minimal async Engine API client over
`/var/run/docker.sock` (the exact daemon both CLI seams force via
`DOCKER_HOST`), read-only GETs only — version, info, container inspect,
image inspect, network inspect, container list. Hand-rolled HTTP/1.1
 framing over `tokio::net::UnixStream`, `serde_json` parsing.

Zero new dependencies. tokio (`net`, `time`, `rt`, `io-util`) and
serde_json are already in the tree; hyper/hyper-util stay
server-featured on purpose — enabling their client sides would pull the
HTTP client machinery for six `Connection: close` GETs, and
`execution/unix_api.rs` already establishes hand-rolled socket HTTP as
the codebase shape. Unversioned paths are the version negotiation (the
daemon serves its native schema; nothing pins an old version), and any
skew surfaces as an `EngineError` answered by CLI fallback, never as a
misread value.

Faced routing (`Docker::engine_or_cli`): the seven single-object/daemon
queries (`container_readiness`, `container_running`, `container_id`,
`image_id`, `mapped_ports`, `daemon_cgroup`, `inspect_exit`) try the API
first under `min(class deadline, 5s)` and fall back to their historical
CLI query on ANY API failure — transport, timeout, status, framing,
JSON, schema. The CLI stays the arbiter whenever the API does not
affirmatively succeed, so surfaced results and the error taxonomy
(`NotFound`, `DockerTimeout`, `DockerCommandError` categories) are
exactly today's; timeouts cannot be false (the CLI confirms) and a
fallback costs exactly one subprocess — the same as before. Fallback
logs at `warn` with a closed-vocabulary `docker_api_fault` reason;
`EngineError` carries status codes and I/O strings only, never body
bytes (inspect carries `Config.Env`, and the sinks do no redaction).

Out of this slice, deliberately: container-list facade routing (reclaim
consumers parse CLI-format text through injected closures; typing that
is the next slice — the client endpoint plus tests land here),
`version`/`network` facade methods (no callers; would be dead code),
buildx queries (no Engine equivalent, stay CLI), connection reuse
(follow-up; a Unix connect is microseconds).

Two gates keep tests hermetic: the routing default is off under
`cfg(test)` (scripted tests stay meaningful on daemon hosts;
`VELNOR_DOCKER_ENGINE_API=0` is the production escape hatch), and the
job transport routes only for `is_host_process_runner()` runners — an
Engine answer is a host fact, so doubles never consult it even while a
routing test holds the process-global override.

Deadlines/cancellation: each transport call honors a bound within its
class (API ≤5s, CLI class deadline); degraded total ≤ class+5s. An
in-flight API call is ≤5s of unkillable socket wait, then the CLI child
(killable as today). No new unbounded wait.

## Live findings (OrbStack Engine 29.4.0 / API 1.54)

A temporary live probe (removed before the PR) asserted all seven
migrated queries API==CLI on live objects, twice. It caught two real
behaviors the mock-first design missed:

1. The daemon chunks these GETs (`Transfer-Encoding: chunked`), which
   would have left the fast path permanently dark behind fallback.
   Chunked decoding is implemented (slurp under the 8 MiB cap, pure
   in-memory decoder with unit cases for multi-chunk, extensions,
   trailers, truncations).
2. `/info` serves `CgroupVersion` as string `"2"`, not a number. The
   parser accepts both (the CLI template renders both identically).

Committed fixtures include a verbatim live State/NetworkSettings/Id
document (Config elided — the parser never reads it and Env carries
image data).

## Numbers

Subprocess count, representative 7-query facade sequence (committed
regression test `representative_sequence_is_identical_with_zero_subprocess_on_api`):

- before (engine off): 7 runner calls = 7 `docker` subprocesses, 0 API;
- after (engine on): 0 runner calls, 7 API servings, byte-identical
  typed values across legs.

Representative-job projection (BC-7's 2-service job, 52-72 processes):
preflight cgroup 1, mise-seed image id 1, service context 2×2,
readiness polls ~2-4×2 → ~10-14 subprocesses eliminated (≈15-25%).
Remainder by design or future slice: payload run/exec, lifecycle
create/start/stop/rm, reclaim listings (next slice), buildx plugin.

Latency, live container-inspect query, 25 iterations, macOS arm64
loaded dev machine via OrbStack socket:

- run 1: CLI facade p50 47.90ms (raw CLI 51.38) vs API facade p50
  1.14ms (raw API 1.22) — ~42x;
- run 2: CLI facade p50 53.06ms (raw CLI 62.10) vs API facade p50
  1.52ms (raw API 1.87) — ~35x.

Per migrated call: ~30-60ms becomes ~1-2ms. Per-class API latency now
lands in `Snapshot.api_classes` against the CLI histogram for the same
comparison in production.

## Tests

22 new, all mock-socket hermetic: 16 engine unit (framing incl.
chunked, all six parsers incl. live shape, statuses, timeout,
declared/slurp oversize caps, routing gate), 5 facade routing (7-query
before/after identity, status/timeout/missing fallback, buildx stays
CLI), 1 metrics (API counters rise without touching invocations).

Verification: `cargo fmt --check` clean; `cargo clippy -p velnor-runner
--all-targets -- -D warnings` clean; `docker::` 57/57 green across
repeated runs; facade-consumer modules (`execution::{cancel,docker}`,
`docker_lease`, `buildkit`) 179/179 green; full lib suite 1950 pass
with 3 failures in `checkout`/`node::cleanup` — the same pre-existing
macOS load-sensitive family (flock/lease/lock timing; proven flaky at
base 3448549c, where the failing set varies run to run). No doc
warnings added.

## Follow-ups (not this slice)

Typed container-list facade routing + reclaim migration; connection
reuse; version/network facade methods when a caller needs them;
consecutive-failure short-circuit if degraded-daemon +5s ever matters.
