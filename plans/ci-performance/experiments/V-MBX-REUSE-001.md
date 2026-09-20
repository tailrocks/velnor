# V-MBX-REUSE-001: compiler reuse and cache write authority

Status: pinned-source diagnosis; controlled experiment and independent review
pending. Not a completed optimization iteration.

## Observed mechanism

The generated Rust workflow uses Mr. Boxington 1.12.0 and action commit
`867fc530102eec5b756075d70d850dc8330d2272` (v1.4.0), objects mode, per-unit
compatibility keys and `save-on-workflow-dispatch: true`.

Pinned MBX source is `a35ac252988359ea8882b5a2766e767e005409c0` (v1.12.0).
Its [compiler path](https://github.com/jdx/mr-boxington/blob/a35ac252988359ea8882b5a2766e767e005409c0/crates/mbx/src/rustc.rs)
increments `unconsulted` when no usable prediction exists and no action lookup
was attempted. Compilation then proceeds. Thus “not looked up” cannot be
treated as successful compiler reuse or dismissed as unrelated operations.
It is distinct from a consulted cache miss and from explicit bypasses.

The [export implementation](https://github.com/jdx/mr-boxington/blob/a35ac252988359ea8882b5a2766e767e005409c0/crates/mbx-cache-store/src/lib.rs)
includes predictions from every completed command in an export group and
deduplicates by invocation identity. The pinned action creates a unique
`MBX_CACHE_EXPORT_GROUP` per run/attempt and exports that group. Directory
transport does not inherently discard prediction metadata. Investigate actual
imported metadata and invocation identity before blaming object transport.

The [action save policy](https://github.com/jdx/mr-boxington-action/blob/867fc530102eec5b756075d70d850dc8330d2272/src/lib.ts)
permits default-branch pushes and explicitly enabled manual dispatches.
Pull-request runs do not save through this action. Repeated PR runs therefore
cannot seed a new compatibility namespace merely by reporting a nominal
runtime “write” mode. The actual action authority must be recorded separately.

## Architectural diagnosis

Current telemetry collapses archive hits, compiler lookups and effective save
authority into misleading warm/cold summaries. A fallback restore can be
reported cold, and a warm archive does not prove usable compiler predictions.
New MBX namespaces also need a compatible authorized producer before PR warm
reuse can be measured. This is a producer/consumer and observability boundary,
not evidence that required compilation should be suppressed.

## Alternatives and experiment design

1. Retain objects mode and seed an isolated namespace through an existing safe
   manual verification path. Repeat identical inputs, then source and lockfile
   changes. Include seed, restore, export and transfer costs.
2. Compare target-state mode against objects mode under identical platform,
   toolchain, source and trust constraints. Preserve Cargo fingerprint checks;
   measure archive transport and unavoidable linking separately.
3. Investigate an authorized persistent compiler backend with stable path and
   prediction identities. Compare contention, network cost and trust isolation
   against ephemeral archive import; do not silently change runner hardware.

MBX's [session statistics](https://github.com/jdx/mr-boxington/blob/a35ac252988359ea8882b5a2766e767e005409c0/crates/mbx/src/session/stats.rs)
provide structured version-4 reports: lookups, hits, misses, unconsulted,
bypasses, compiler time, bytes, materialization and remote timings. Capture
each command/session separately. Reusing one report path across nested task
commands may overwrite earlier evidence; a final report alone is insufficient.

Required next evidence: actual imported prediction counts and cache-save
decisions, at least five controlled observations per important condition,
compiler invocation/download evidence, and independent result review. No warm
cache, zero-recompile, speedup or cache-strategy acceptance follows from these
source reads.

## Concrete telemetry contradiction

Job [106029138322](https://github.com/tailrocks/velnor/actions/runs/35492230871/job/106029138322)
reports `No mbx cache found` at `05:40:46.5166507Z`. Its final telemetry reports
`cache_outcomes.mbx = prefix`. The action exports `cache-hit = false` both when
no archive exists and when a prefix archive was restored; it exports no matched
key. The source-owned reporter incorrectly turns false into prefix, and also
uses compiler-log phrases to infer archive state. Those are different layers.

The truthful bounded repair is to report a non-exact/unknown archive result
unless actual restore evidence distinguishes miss from fallback. Compiler
statistics must remain separate. Inspecting this job's raw log proves its
archive miss; this cannot be generalized from the false boolean alone.

Retained timestamped excerpts are
`../observations/velnor-35492230871-collector-order-cache.txt` and the prior
`../observations/velnor-35491248265-collector-order-cache.txt`.
Both commands together report 104 unconsulted compiler invocations, one hit,
zero misses and five bypasses. The candidate runs Clippy before nextest;
the prior run reverses those two commands. Both pass 42 tests. Checks take
approximately 21 versus 14 seconds in these single samples. This apparent
successful-path regression needs repeated controlled measurements; it is
not accepted as noise or as a speedup.
