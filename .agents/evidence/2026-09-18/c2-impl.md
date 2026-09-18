# C2-PREREQ implementation evidence

Branch: `feat/c2-unbounded-global-n` (from `108f43aa`, latest `origin/docs/bastion-final-plan` at start).
Commit: `deb9e204` (signed off, pushed to origin).
Diff: 36 files, +3049/−3384. No legacy shims; obsolete tests removed or inverted to assert absence.

## REMOVE — unbounded (all 18 map items)

- `container/host_budget.rs` deleted (1283 lines); `slot_count`/`resource_options` fields, `BUDGET_ENV`,
  all budget fns gone. Emission strips 11 quota flags unconditionally (`QUOTA_FLAGS`), job + service.
- No `CARGO_BUILD_JOBS`/`MAKEFLAGS`/`MBX_SCHEDULER_*`/`VELNOR_JOB_BUDGET` injection; workflow's own
  spellings pass through untouched. (No Gradle/heap partition code existed — verified by grep.)
- `velnor-jobs.slice` identity-only (no AssertPathExists, no CPU/Memory/Tasks directives).
- `postinst` deletes the stale `10-host-cpu.conf` and proves effective absence
  (`CPUQuotaPerSecUSec`/`MemoryMax`/`MemoryHigh` all `infinity`); never writes a quota.
- `postrm` keeps generic unit cleanup (enumerate/mask/disable/stop/proofs, verified ordering),
  quota logic deleted. `velnor.env` + both daemon units: job-cap env removed, `VELNOR_MAX_JOBS` documented.
- `--job-cpus/--job-memory` flags + env removed from `service.rs`, `args.rs`, `velnorctl runtime/host`;
  `job_resource_flags_single_source.rs` deleted. `JobAdmission.resource_policy` is now `"unbounded"`.
- BuildKit: entitlements, `resize_builder_daemon`, `buildx --driver-opt` sizing, setup/resize/post-shrink
  all removed; claims record identity only. `github_adapter` quota flags stripped at admission both lanes.
- Nested lease creates: 16 quota `HostConfig` keys stripped pre-gate; `--shm-size` explicitly allowed
  (shared-memory sizing, not a ceiling — same call at admission and emission).

## REFACTOR — one host-wide max_jobs=N ledger

- New `velnor-control/src/permit_ledger.rs` (SQLite, `permit-ledger.db` next to state db):
  lanes `Native` + `ScaleSet` (D1 hook), 7 counted states, generation-fenced grants (release
  unfenced), reconcile-before-advertise, idempotent acquire, crash-redelivery adoption by dead pid,
  sweep of dead unprotected uncertain natives only. Reconcile never deletes.
- New `velnor-runner/src/permit_guard.rs`: `NativePermitGuard` (acquire/commit, running transition,
  release-on-drop, uncertain-on-cleanup-failure), `TeardownPermitRelease`, pid-liveness probe,
  `resolve_max_jobs` (explicit N, else slots, never 0), path resolution.
- Native wiring: acquire beside the durable intent in `handle_v2_message` (full/unreadable → skip,
  broker redelivers); marker carries `permit_holder` + `permit_ledger`; teardown thread releases
  after confirmed cleanup; 3 cleanup-failure paths retain uncertain; recovery releases; daemon
  startup sets N → epoch → reconcile vs markers → sweep (fail-closed).
- `execution/docker.rs` + `local_diagnostics.rs` + fixtures inverted: Linux slice must be loaded with
  all ceilings `infinity`; macOS probe is placement-only and asserts `NanoCpus=0, Memory=0`.
- `jobs_slice.rs`/`node_arch.rs` rewritten to assert absence/never-recreated (incl. executable
  drop-in-removal test). Docs (4 mdx) rewritten to unbounded + ledger.

## Deliberate boundaries (for D1 / parent review)

- Journal `PermitReserved` kept as slot-PROCESS liveness (spawn/register/ready/drain), not capacity;
  idle slots hold no ledger permit. Full idle-assignable JIT + demand adapter + oldest-observed queue
  belong to D1's shared allocator (work-plan D1 action 6 owns that proof).
- `velnor-control.slice` weights/TasksMax kept (control plane, not workload).
- Multi-daemon transient: a second daemon's sweep can free a crashed daemon's marker'd row before
  that daemon restarts (bounded, self-heals via recovery no-op release). Single-daemon bastion unaffected.
- Doctor/diagnostics carry no quota asserts (Q32 verified no-op). Disk budgets, serial groups,
  mediated lease API, trust knobs untouched (all KEEP items).

## Tests (all green)

- `cargo test -p velnor-runner -p velnor-control -p velnorctl`: all pass —
  runner lib 2147 (+4 ignored), control lib 276, all integration binaries green (incl. rewritten
  `jobs_slice` 4/4, `node_arch` 23/23), ctl suites green.
- `cargo test -p velnor-runner --lib --features test-support`: 2216 pass, incl. new
  `full_permit_ledger_skips_acquisition_without_calling_github` (0 GitHub hits, no row spent)
  and fixed `transient_acquire_failure_keeps_broker_session_alive` (temp ledger, occupancy returns to 0).
- New: ledger 9 (`permit_ledger.rs`), guard 6 (`permit_guard.rs`), unbounded emission
  (`container.rs` ×2 + service case, `github_adapter.rs`, `docker_lease.rs`, execution absence ×2).
- `cargo clippy --all-targets` on the three crates: 0 warnings. `cargo fmt` applied. `sh -n` on
  postinst/postrm clean.
