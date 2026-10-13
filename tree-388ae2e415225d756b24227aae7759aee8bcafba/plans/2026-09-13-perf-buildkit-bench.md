# Plan 2026-09-13: perf FAILs — BuildKit CPU sizing + bench VelnorJob driver

Base: origin/main tip 3657157f. Branch: r0-perf-bk. Worktree: /tmp/velnor-perf.

## FAIL 1: derived slot budget never reaches buildkitd

`JobContainerSpec::slot_budget` (host/slots, capped by declared `--cpus`) sizes
the job container (`--cpus`/`--memory`) and `CARGO_BUILD_JOBS`/`MAKEFLAGS`/
`MBX_SCHEDULER_*`, but `native_docker_setup_buildx` builds `--driver-opt`
only from static `resource_options`, and persistent shared builders get
disk-only reclaim (pressure prune, horizon) with no CPU/memory sizing.

Fix:

- `host_budget.rs`: `SlotBudget::buildkit_size(holders)` — builder ceiling =
  slot share x holders, capped at host CPU / scheduler memory pool
  (host x 85%). `BuildkitSize::driver_opts()` renders
  `cpu-period`/`cpu-quota`/`memory`. Unobservable budget yields no sizing.
- `container.rs`: `JobContainerSpec::buildkit_size(holders)` — derived size
  with declared `--memory` narrowing (aggregate entitlement), same
  narrow-only rule the container path uses.
- `buildkit.rs`: `builder_holder_count` (locked claim read; unreadable =
  skip resize, never guess) and `resize_builder_daemon` (`docker update`
  on the daemon container, best-effort, missing = already gone).
  Document the sizing model in module docs.
- `executor.rs` setup: claim, count holders, create with derived
  `--driver-opt` (static `resource_options` survive only as the
  total-absence fallback); existing builders get best-effort resize to
  the current holder count. Post: shrink on release when holders remain.
  All-or-nothing precedence, documented.
- Regression test: existing `buildx create` argv test asserts the derived
  opts (computed from the same host observation); new `host_budget` unit
  tests for share x holders, host capping, unobservable = empty.

## FAIL 2: VelnorJob bench driver unconditionally bails

`drivers::build` bails for `Driver::VelnorJob`, so all lifecycle /
persistent-host rows are unrunnable schema. Remote dispatch (registered
runner + GitHub credentials) stays unimplemented and honestly unrun.

Fix: local `velnor-job` driver (`drivers::velnor_job`) for the 8
lifecycle/persistent-host rows, measuring only what local dispatch really
does — no simulation, per crate rule 1:

- Ready (iteration workspace ready), Admission (real
  `HostCapacity::probe` + `DiskPressure` admit/refuse), Capacity (real
  `docker info` reachability + `probe_with_docker` promisable floor),
  CheckoutStart (workspace materialization), DockerSetup .. Teardown
  (real container lifecycle with per-job network).
- BrokerDelivery / AcquiredPayload / Checkout stay unobserved (no broker,
  no git remote locally) and are named in the record notes.
- `concurrent-slots`: N overlapping container lifetimes, phase-window
  stages. `trust-partition`: trusted + untrusted labelled pair, per-stage
  max. `persistent-host/job-N`: N-1 unmeasured warmups in `prepare`.
  `after-gc`: warmups + scoped owned-object GC (never host-wide prune).
- New `Requirement::VelnorJobLocalDriver` (code-available, like the
  remote flag); only the 8 rows move to `VELNOR_JOB_LOCAL =
  [local-driver, docker-daemon]`. Rust/docker/fault rows untouched.
- `drivers::build` routes VelnorJob to the new module; unimplemented
  (remote-only) ids keep the existing bail message.
- Update the three runnability tests that pin the old contract; README
  documents the local driver and its limits.
- Proof: `velnor-bench run` lifecycle + persistent-host rows with
  `--iterations 3`, plus focused unit tests.

## Verification

fmt, `clippy -D warnings`, affected tests
(`velnor-runner` container/buildkit/executor, `velnor-bench`), live bench
runs against local Docker. Sign commit, push `r0-perf-bk`, open PR to
main (no force-push, no other branches), remove worktree.
