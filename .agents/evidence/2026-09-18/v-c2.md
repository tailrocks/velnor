# C2 verification — CERTIFIED

Branch `feat/c2-unbounded-global-n`, commit `deb9e204a3a871ec15aec1582088d75ebf620752`
(origin tip == deb9e204; DCO signed off; diff 36 files +3049/−3384 vs 108f43aa, matches impl claim).
Spec: `plans/bastion-three-provider-ci/spec.md` §4. Map: `/tmp/c2-prereq-map.md`. Impl: `/tmp/c2-impl.md`.
Method: independent re-verification in scratch worktree `/tmp/v-c2-wt` + clean clone `/tmp/v-c2-clone`
(pristine tree, tree hash `8f7404ad…` matches worktree; host_budget.rs absent, ledger files present).
No edits, no merges, no pushes.

## Disprove attempts (all failed = good)

- Quota strings (`NanoCpus|CPUQuota|MemoryMax|CARGO_BUILD_JOBS|--job-cpus|…`): every remaining hit is a
  strip-list, an absence/infinity assertion, or a comment. No emission site. `docker update` survives
  only as the arity-table word in `docker/deadline.rs:334`; `resize_builder_daemon` gone.
- `host_budget|HostBudget|SlotBudget|slot_budget`: zero hits. Remaining `slot_count` is daemon-slot
  identity plumbing (`args.rs`/`service.rs`/`runner.rs`), not budget; `JobContainerSpec` carries
  `slot_store_key` (store identity) + `options`, no budget fields.
- `resource_options|BUDGET_ENV|append_resource_budget|declared_container|buildkit_size|own_buildkit|
  driver_resource_options|buildx_driver`: zero hits. `resource_policy` is the pre-existing job-summary
  label; native admission now records `"unbounded"` (`runner.rs:7950`).
- `VELNOR_JOB_CPUS|VELNOR_JOB_MEMORY|job-cpus|job-memory`: zero hits. `VELNOR_JOB_PEAK_BYTES` kept =
  disk-admission estimate (KEEP Q31), not a cgroup ceiling.
- `CARGO_BUILD_JOBS` in `velnor-bench` is in `AMBIENT_CARGO_ENV_TO_REMOVE` (removed, not set); in
  `velnor-tools` it is a fixture asserting jackin's own workflow content (workflow-authored, and the
  generator in `velnor-workflow/src` emits none — grep clean). No `MAKEFLAGS`/`GRADLE_OPTS`/`Xmx`
  injection anywhere in runner/workflow/executor/buildkit.
- Emission backstop: `QUOTA_FLAGS` (11) stripped unconditionally in `container.rs` for job + service;
  admission strips the same 11 pre-trust-split in `github_adapter.rs` (both lanes, logged);
  nested lease creates strip 16 `HostConfig` keys pre-gate (`docker_lease.rs`). `--shm-size`/`ShmSize`
  explicitly allowed at all 3 layers (documented: shared-memory sizing, not a ceiling).
- `velnor-jobs.slice` identity-only (no CPU/Memory/Tasks directives). `postinst` defines AND calls
  `remove_stale_jobs_cpu_quota_dropin` (:34, :168), never writes a quota, proves all three effective
  properties read `infinity`. `postrm` has no quota logic. `velnor-control.slice` kept (control plane).
- Ledger is the single authority: `occupied()` = `COUNT(*)` over all rows (all 7 states × both lanes);
  `acquire` is generation-fenced, idempotent, `Full`/`NotConfigured`-closed; one `set_max_jobs` at
  daemon startup (`runner.rs:4780`); one CLI/env knob (`--max-jobs`/`VELNOR_MAX_JOBS`); lanes
  `Native` + `ScaleSet` (D1 hook); 7 states Reserved/Acquiring/Provisioning/Assignable/Running/
  Cleaning/Uncertain. `resolve_max_jobs`: explicit N, else slots, never 0. No second knob found.
- KEEP items intact: `MBX_GC_*` disk hygiene, `--cgroup-parent velnor-jobs.slice` + `JOB_DOCKER_HOST`
  + lease mediation/reclaim, disk admission (`DiskPolicy`/`DiskPressure`/reserves), `DockerIsolationMode`,
  serial/concurrency code untouched by the diff (no serial/concurr/e2e file in it), no per-job disk/PID
  quotas introduced (`--ulimit nofile` only).

## Rerun in clean clone (all green)

- `cargo test -p velnor-control`: lib 276 + all integration binaries pass.
- `cargo test -p velnor-runner --lib`: 2147 pass, 4 ignored.
- `cargo test -p velnor-runner --tests`: all binaries pass incl. `jobs_slice` 4/4, `node_arch` 23/23.
- `cargo test -p velnor-runner --lib --features test-support`: 2216 pass incl.
  `full_permit_ledger_skips_acquisition_without_calling_github` and
  `transient_acquire_failure_keeps_broker_session_alive`.
- `cargo test -p velnorctl`: all suites pass; `job_resource_flags_single_source.rs` confirmed deleted.
- `cargo clippy -p velnor-runner -p velnor-control -p velnorctl --all-targets`: 0 warnings.
- `cargo fmt --check`: clean. `sh -n` postinst/postrm: clean.
- Ledger unit tests: `permit_ledger` 9/9 pass.

(Note: `rtk cargo …` emitted no output in this sandbox and mise distrusted the fresh clone path;
used `mise trust` + plain `cargo`. Same toolchain, same tree.)

## Verdict: CERTIFIED

Commit deb9e204 implements `/tmp/c2-impl.md` against spec §4 + prereq map: unbounded execution
(no CPU/RAM/cpuset ceilings in emission, admission, packages, BuildKit, nested leases, or build env)
plus exactly one host-wide `max_jobs=N` ledger counting all states across both lanes, with KEEP
items preserved. No scope expansion detected (diff = 36 claimed files; docs + tests + ledger only).
