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

One NDJSON record per scenario run, `velnor.bench.result.v1`:

```
schema, run_id, recorded_at_unix_ms, scenario, family, driver, runnability,
environment { velnor.bench.environment.v1 — every field mandatory },
observations[ { total_ms, stages_ms{}, checkout_phases_ms{}, resources{}, git{} } ],
summaries { total_ms, stages_ms{}, checkout_phases_ms{}, lane_ms{},
            cpu_user_us, cpu_system_us, max_rss_bytes, block_input_ops,
            block_output_ops, process_count, docker_invocations,
            cache_hits, cache_misses, bytes_copied, bytes_downloaded,
            bytes_reused },
notes[]
```

Each summary carries `samples, min, max, mean, variance, p50, p95, p99`.

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

## Instrumentation the harness consumes, and what is still owed

### 1. The per-job process census — delivered, and read here

`crates/velnor-runner/src/docker/metrics.rs` now counts every host `docker`
process a job spawns. The count is taken at the one seam that sees them all:
`crates/velnor-runner/src/executor.rs:479`, inside the concrete `CommandRunner`,
so no call site carries a counter. Each invocation is classified into a
`DockerOp` (`crates/velnor-runner/src/docker/deadline.rs`) and the per-class
count and summed latency are reported by a guard that fires however the job
ends.

Those counters are process globals inside the runner, so the seam another
process reads them through is the `tracing` event the guard emits, which the
runner's file layer writes to `<config-base>/logs/trace.jsonl`. `runnertrace`
is that reader, and `velnor-bench census --trace <trace.jsonl>` prints one
`JobDockerCensus` per job plus a summary of processes per job under the same
percentile policy as every other statistic. A trace with no job scope in it is
an error, never a zero.

The per-invocation events are `DEBUG`; without a filter that admits
`velnor.docker` at debug level a trace carries only the per-job totals, so the
per-class figure is a *sum over the job*, not a sample, and no percentile over
individual calls is computed from it.

### 2. Per-phase checkout spans and GIT_TRACE2 counters — still owed

`crates/velnor-runner/src/checkout.rs` still emits no `tracing` span per phase
— mirror lock wait, mirror fetch, workspace fetch, workspace checkout, mtime
normalization — and still sets no `GIT_TRACE2_EVENT` on the git processes it
spawns. Neither `checkout.rs` nor `git_mirror.rs` contains a single `tracing`
call. Until they do, `Observation::checkout_phases_ms` cannot be filled from a
real job, whatever driver runs it.

`checkout_replay` exists because of that gap, and is careful about what it
claims. It replays, against a real synthetic repository, the exact git argv the
baseline (`2858e92`) and the current tip (`22c1b95`) build — a full
`+refs/*:refs/*` mirror fetch under the exclusive lock followed by a workspace
fetch from the mirror, versus a wanted-revision fetch followed by hard-linking
the object files. That is a measurement of two command sequences, not an
execution of `checkout.rs`, so it is deliberately **not** a row of the scenario
matrix, and every record it emits carries that caveat in `notes`.

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

## Usage

```
velnor-bench --velnor-repo . --fixture-repo ../velnor-actions-fixture probe
velnor-bench --velnor-repo . list
velnor-bench --velnor-repo . --network-egress \
  run --scenario docker/existing-image --iterations 20 --output bench.ndjson
velnor-bench census --trace /etc/velnor/logs/trace.jsonl
velnor-bench --velnor-repo . checkout-replay --iterations 20 --legs 6 \
  --pull-refs 250 --output checkout.ndjson
```

`--github-credentials` and `--network-egress` are asserted by the operator, not
inferred: a probe cannot tell a valid token from a cached one, nor a warm image
from a reachable registry.
