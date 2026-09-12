# velnor-bench

The benchmark harness for Velnor. It replaces `scripts/benchmark/benchmark.sh`,
a bash script with five embedded Python heredocs that never invoked Velnor,
Docker, or a job — every number it produced was a statement about `cargo`.

## Rules this crate enforces in code

1. **Nothing is simulated.** A scenario either drives real work or is reported
   as unrun with the missing requirement named. There is no driver that
   fabricates a stage timing.
2. **Environment identity is mandatory.** A record whose environment block is
   missing or partial fails to deserialise (`record::tests`). A fact the host
   cannot answer is `{"unavailable": {"reason": ...}}`, which is a different
   thing from a field that was never captured.
3. **Internal and external latency never mix.** Stages are attributed to
   `velnor_model::telemetry::TelemetryLane`, the same `Github` / `Velnor` split
   the runner already uses.
4. **A statistic is only emitted where the sample supports it.** See below.

## Honest runnability contract

Every matrix row names `velnor-job` as its preferred driver, but a
`VelnorJob` row remains **unrun** until the real runner-owned VelnorJob driver
exists. A registered runner, GitHub credentials, a fixture checkout, Docker,
network access, or any other external prerequisite does not authorize a
preferred measurement by itself. Those capabilities can establish that the
environment is ready; they cannot make an unimplemented driver runnable.

Where the matrix declares an honest fallback, the result is explicitly marked
`degraded` and records the fallback driver plus the missing preferred-driver
requirements. For Docker rows, `docker-direct` is such a fallback: it measures
real container work, but not Velnor job dispatch, broker delivery, acquisition,
or admission. It must never be presented as a preferred VelnorJob measurement.
Rows without an honest fallback remain unrun.

## Percentile policy

A quantile `q` is only distinguishable from the sample maximum when the sample
contains at least `1 / (1 - q)` observations:

| quantile | minimum n |
| --- | --- |
| p50 | 2 |
| p95 | 20 |
| p99 | 100 |

Below that threshold the field is `{"unsupported": {"samples": n, "required": r}}`
rather than a number. A summary of fewer than three observations is refused
outright, so a single run is never reported. The replaced script ran `n = 5`
and printed its maximum as a "p95".

## Output schema

One NDJSON record per scenario run, `velnor.bench.result.v2`:

```text
schema, run_id, recorded_at_unix_ms, scenario, family, driver, runnability,
environment { velnor.bench.environment.v1 — every field mandatory },
observations[ { total_ms, stages_ms{}, checkout_phases_ms{}, resources{}, git{},
                docker_census{ by_class{ label: {count, latency_ms} } },
                fault? {class, injected, contained, residue[], detail} } ],
summaries { total_ms, stages_ms{}, checkout_phases_ms{}, docker_census{ label: {count, latency_ms} },
            lane_ms{},
            cpu_user_us, cpu_system_us, max_rss_bytes, block_input_ops,
            block_output_ops, process_count, docker_invocations,
            cache_hits, cache_misses, bytes_copied, bytes_downloaded,
            bytes_reused },
notes[], context{}
```

`observations[].git` is tagged evidence, not an unqualified counter object:
`{"status":"not_measured"}` for drivers without Git tracing,
`{"status":"no_git_trace_observed"}` only when an armed trace slot
survives without any Trace2 event, and
`{"status":"observed","counters":{...},"successful":true|false}` when
complete Trace2 lifecycles were observed. Concurrent workers may emit
`status: "mixed"` with observed counters and explicit worker counts. A
complete non-zero Git lifecycle remains observed with `successful: false`;
malformed, incomplete, or missing evidence fails the current Cargo-direct run.
The state means “no Git Trace2 process observed”; it is not a claim that a
child could not have exited before Trace2 initialization.

The result schema remains v2. Deserializers accept the historical v2
discriminator `{"status":"no_git_process"}` as an input alias; new output
always uses `no_git_trace_observed`.

Each summary carries `samples, min, max, mean, variance, p50, p95, p99`.
`docker_census` labels are the runner's closed `DockerOp` vocabulary; validation
rejects any other label and any census whose total disagrees with
`resources.docker_invocations`. `fault` is present exactly on `fault/*` rows and
must name a catalogue class that actually triggered. `context` carries the
comparison dimensions (`--context key=value`): runner identity, trust class,
image tag — what differs between two runs being compared.

Two companion schemas: soak reports (`velnor.bench.soak.v1`, from `soak`) and
comparisons (`velnor.bench.comparison.v1`, from `compare`).

`environment` records CPU model (the brand string, not the architecture — the
replaced script recorded `platform.processor()`, which is the architecture),
core count, RAM, architecture, OS, kernel, filesystem type of the working root,
Docker server version, the negotiated client API version, storage driver,
BuildKit version, Velnor commit, fixture commit, rustc and cargo versions, mbx
version, job image digest, and the runner configuration including slot count.

## Drivers

| driver | observes | notes |
| --- | --- | --- |
| `velnor-job` | every stage | the authoritative driver; needs a registered runner and dispatch credentials |
| `docker-direct` | docker setup, create, start, first user command, completion, teardown | real containers, no broker or acquisition |
| `cargo-direct` | first user command only | a build measured on the host; the scope of the replaced script, and never a claim about Velnor |

`BenchRecord::validate` rejects any record carrying a stage outside its
driver's observable set, so a container-only measurement can never be read as a
claim about job acquisition latency.

## Instrumentation boundary

The runner records per-job host-Docker metrics at its `CommandRunner` seam,
and the benchmark consumes them through two trusted bridges. In both cases
the harness reuses the runner's own types and vocabulary; it never recounts
at its own call sites and never invents a second classifier.

### 1. The runner's per-job Docker census

`crates/velnor-runner/src/executor.rs` defines `trait CommandRunner` as the
single choke point for every host process spawn. The product-side
`crates/velnor-runner/src/docker/metrics.rs` scope records invocation count,
closed operation class, latency, timeout, and exit status, emits totals on
all job exits, and exposes the same counters machine-readably through
`docker::metrics::snapshot()`.

The harness side (`census::DockerCensus`) derives the per-iteration census
from its recorded invocations using the runner's own `docker::classify`, so
there is exactly one classifier and one twelve-label vocabulary. Until the
`velnor-job` driver exists this census covers the invocations the harness
itself spawns — a strict subset of a real job's census, stated on the record.
When the driver lands, the same per-class record shape is filled from the
runner's `velnor.docker` totals instead.

### 2. Per-phase checkout spans and GIT_TRACE2 counters

The runner emits one `tracing` span per checkout phase — mirror lock wait,
mirror fetch, workspace fetch, workspace checkout (wall-clock since BC-14
deleted mtime normalization) — and the trace file layer records span close
events with busy/idle timings (`crates/velnor-runner/src/telemetry.rs`).
`trace::checkout_phases_from_trace` parses those close records back into
`CheckoutPhase` timings; the span-name table is pinned on the runner side by
a test that runs a real checkout (`checkout_emits_the_four_bench_phase_spans`).
No new sink was needed.

Byte and ref counters do **not** need a runner change: `gittrace` sets
`GIT_TRACE2_EVENT` on the Git processes Cargo spawns and reads the documented
event JSON back. Each worker arms its owned trace slot with a unique marker
before timing. Marker-only output is explicit `no_git_trace_observed`; missing,
overwritten, empty, malformed, or incomplete output fails closed. For jobs the
runner drives, the runner must set the same variable on its Git children for
those counters to appear.

## Coverage inherited from the deleted script

| removed bash scenario | replacement |
| --- | --- |
| `cold_check` | `rust/cold` |
| `warm_check`, `fresh_worktree_separate_target_warm` | `rust/warm` |
| `noop` | `rust/noop` |
| `fresh_worktree_separate_target_cold` | `rust/cold` |
| `cross_worktree_shared_target_reuse` | `rust/fresh-worktree-same-commit` |
| `small_source_edit` | `rust/small-source-edit` |
| `dependency_manifest_touch` | `rust/manifest-touch` |
| `dependency_graph_change` | `rust/lockfile-update` |
| `build`, `warm_build` | `rust/cargo-build` |
| `nextest` | `rust/nextest` |
| `clippy` | `rust/clippy` |
| `native_sys_build` | `rust/native-sys` |
| `parallel_independent_jobs_N` | `rust/concurrent-jobs` |

Two behavioural differences, both deliberate:

* `rust/native-sys` builds a package that `cargo metadata` reports as a real
  `*-sys` crate. The script's "full" mode built the whole workspace and called
  it a native-sys measurement.
* `rust/lockfile-update` performs a real `cargo update`, so it declares
  `network-egress` and is reported as unrun without it. The script's offline
  variant only appended a path dependency, which is a different measurement.

## Fault catalogue, soak, and comparison

The matrix holds 38 rows: the original 33 plus `lifecycle/trust-partition`
and four `fault/*` rows. `fault::FAULT_CATALOGUE` names all 27 failure
classes across the process, HTTP, filesystem, and signal seams. Four are
measurable today through `docker-direct` — the harness injects a real fault
(SIGKILL mid-step, failing user command, absent object, network conflict)
into a real container lifecycle and asserts the real containment; each
observation carries the `FaultOutcome`, and `run` exits nonzero when any
outcome is uncontained (the record is still written). The other 23 classes
need the `velnor-job` driver and stay declared-but-unrun. Scripted
process-seam injection for runner-internal paths lives in the runner's
`fault_injection.rs` decorator, proven against the real checkout.

`soak` repeats a scenario round after round and samples owned-object residue
(the verdict: zero every round) plus scratch growth and timing drift (evidence,
no threshold yet). `compare` divides two same-scenario, same-driver records
into per-quantile ratios, gated on sample size like the percentiles — a p95
ratio needs n>=20 on both sides — and surfaces the records' `--context`
differences so the reader sees what was compared. That is also the
trust-partition protocol: run `lifecycle/trust-partition` once per trust
class with `--context trust-class=...` and compare the admission, capacity,
and checkout stages. Velnor-vs-actions/runner is the same operation with
`--context runner=...` once both dispatch drivers exist.

## Usage

```shell
velnor-bench --velnor-repo . --fixture-repo ../velnor-actions-fixture probe
velnor-bench --velnor-repo . list
velnor-bench --velnor-repo . --network-egress \
  run --scenario docker/existing-image --iterations 20 --output bench.ndjson
velnor-bench --velnor-repo . \
  run --scenario fault/step-command-fails --iterations 5 --output faults.ndjson
velnor-bench --velnor-repo . \
  soak --scenario docker/existing-image --rounds 10 --output soak.ndjson
velnor-bench --velnor-repo . \
  compare --baseline bench-a.ndjson --candidate bench-b.ndjson
```

`--github-credentials` and `--network-egress` are asserted by the operator, not
inferred: a probe cannot tell a valid token from a cached one, nor a warm image
from a reachable registry.
