# C2 prereq map — quota/ceiling inventory + native admission + mediated lease API

Target: unbounded containers + one host-wide global N (plans/bastion-three-provider-ci/spec.md §4, §C2).
Verdicts: **REMOVE** = delete; **REFACTOR** = keep role/file, change semantics; **KEEP** = needed as-is.
Workspace: velnor3 @ /Users/donbeave/Projects/tailrocks/velnor-project/velnor3. Read-only survey; no repo edits.

## 1. Quota / ceiling code (25 items)

### 1a. Slot-division budget model → per-container `--cpus`/`--memory` + build-env partitions

- Q1 `crates/velnor-runner/src/container/host_budget.rs:1-64` module docs + `JOB_SLICE_CGROUP:58`, `SCHEDULER_MEMORY_PERCENT=85:64`. The whole "host budget ÷ slots" model. **REMOVE**
- Q2 `host_budget.rs:97-124` `HostBudget` / `SlotBudget` structs (`cpus`, `docker_cpu_milli`, `memory_bytes`). **REMOVE**
- Q3 `host_budget.rs:125-230` `observe`/`observe_host` + cgroup readers (`cpu.max:690`, `cpuset.cpus.effective:699`, `memory.max:708`, `parse_cpu_max:725`, `parse_cpu_list:740`). **REMOVE** (sizing use; cgroup *observation* for identity/cleanup lives in `docker/facts.rs`, unaffected)
- Q4 `host_budget.rs:211` `per_slot`, `:304` `capped_by_container_cpus`, `:324` `docker_cpu_option`, `:337` `docker_memory_option`, `:457` `notice`, `:546` `cpu_milli_from_cpus`, `:557` `format_cpu_milli`, `:587` `observe_slots` (test-only). **REMOVE**
- Q5 `host_budget.rs:356-368` `job_env` → injects `CARGO_BUILD_JOBS`, `MAKEFLAGS=-j`, `MBX_SCHEDULER_CPUS`, `MBX_SCHEDULER_MEMORY`. The disguised partition. **REMOVE**
- Q6 `host_budget.rs:382-456` `own_buildkit_entitlement`, `buildkit_size_summed` + `host_budget.rs:504-544` `BuildkitSize::{with_fallback,driver_opts}` (`cpu-period`/`cpu-quota`/`memory=`). **REMOVE**
- Q7 `crates/velnor-runner/src/container.rs:70-110` `append_flags_without_limits` (strips workflow `--cpus`/`--memory` when derived cap exists), `:112-148` `parse_memory_options`. **REMOVE**
- Q8 `container.rs:184-199` `JobContainerSpec.slot_count` + `:322-352` `slot_budget` / `own_buildkit_entitlement` / `buildkit_size_summed`. **REMOVE** budget fns; struct itself REFACTOR (drop `slot_count`)
- Q9 `container.rs:256-260` `BUDGET_ENV` override list + `:475-500` `append_resource_budget` (called `:716` from `start_args:543`) + `:415-462` `--cpus`/`--memory` emission with `min(declared, share)`. **REMOVE**
- Q10 `container.rs:284-317` `declared_container_cpus` / `declared_container_memory` (tightest operator/workflow limit). **REMOVE** (nothing to narrow against once unbounded)
- Q11 `crates/velnor-runner/src/service.rs:190-198` `--job-cpus`/`VELNOR_JOB_CPUS`, `--job-memory`/`VELNOR_JOB_MEMORY` daemon flags. **REMOVE**
- Q12 `crates/velnor-runner/src/runner.rs:6341-6360` `job_resource_options` + threading `:6036-6037`, `:7820` `resource_policy_label`, `:8426`, `:8476`, `:10527`, `:10556`, `:11061`, `:11180`. **REMOVE**
- Q13 `crates/velnor-runner/debian/velnor.env:55-56` `VELNOR_JOB_CPUS=4`, `VELNOR_JOB_MEMORY=12g` (+ comment `:53-54`); `debian/velnor-daemon.service:24-25`; `debian/velnor-daemon@.service:21-22` (same defaults). **REMOVE**
- Q14 `crates/velnorctl/tests/job_resource_flags_single_source.rs:1-120` (pins deleted flags). **REMOVE**
- Q15 `container.rs` budget tests `:2295-2666` (`--cpus`/`--memory`/`CARGO_BUILD_JOBS`/`MBX_SCHEDULER_*` assertions) + `host_budget.rs` tests `:860-1200` + `runner.rs:19520-19530` `job_resource_options_are_daemon_policy_flags`. **REMOVE**

### 1b. systemd slice quotas + quota drop-ins

- Q16 `crates/velnor-runner/debian/velnor-jobs.slice:4-19` (`AssertPathExists` drop-in pin, `MemoryHigh=90%`, `MemoryMax=95%`, `MemorySwapMax=0`, `TasksMax=4096`). **REFACTOR** → keep slice for identity/cleanup; delete ceilings + drop-in pin (+`TasksMax`, a PID quota). `CPUWeight=100`/`IOWeight=100` are shares, not ceilings — C2 author decides, default drop.
- Q17 `crates/velnor-runner/debian/postinst:23-56` `write_host_scaled_jobs_cpu_quota` (`CPUQuota=95%` drop-in) + `:250-279` effective-quota verification (`systemctl cat/show`, `busctl CPUQuotaPerSecUSec`). **REMOVE**
- Q18 `crates/velnor-runner/debian/postrm:4-5,70-125` drop-in removal + worker/slice proofs ("keeping CPU quota" fail-closed chain). **REFACTOR** (keep generic unit cleanup, delete quota-removal logic)
- Q19 `debian/velnor-control.slice:7-9` (`MemoryMin=64M`, `CPUWeight=200`, `TasksMax=256`) — control plane, not workload ancestry; `MemoryMin` is protection. **KEEP**
- Q20 `debian/velnor-job@.service:10` `Slice=velnor-jobs.slice`, `debian/velnor-slot@.service:12` `Slice=velnor-control.slice` — placement/identity, no ceilings. **KEEP**

### 1c. Runtime quota verification (preflight / boundary proofs)

- Q21 `crates/velnor-runner/src/execution/docker.rs:282-300` `verify_docker_job_cgroup_boundary[_with_image]` + `:360-430` expected-quota (`online×95`) + `systemctl cat CPUQuota` check + `:522-580` `systemd_slice_state`/`parse_systemd_duration_usec` + `:12-14` `JOB_CGROUP_DROPIN` + `:35-37` quota comment. **REMOVE**
- Q22 `execution/docker.rs:74-99` `DockerResourceCapabilities` + `:151-165` `validate_docker_resource_projection` (exact-match `NanoCpus=500000000, Memory=67108864`) + `:119-147` macOS capability gate inside `validate_docker_isolation`. **REMOVE** quota proofs; **KEEP** the Linux systemd-driver/cgroup-v2 *mode selection* (`DockerIsolationMode`, driver≠ceiling)
- Q23 `crates/velnorctl/src/local_diagnostics.rs:1290-1377` macOS `--cgroup-parent/--cpus 0.5/--memory 64MiB` probe + `NanoCpus/Memory` inspect (`:1323`, `:1374`) + remediation `:1506-1535`. **REMOVE** (proves ceilings exist; needs unbounded-compatible replacement or deletion)
- Q24 Quota fixtures/tests: `execution/mod.rs:523-567,700-707` (`CPUQuota=95%` stubs), `execution/tests.rs:315-406`, `preflight.rs:512-531` (test double), `tests/node_arch.rs:271-289`, `tests/jobs_slice.rs:34-146`. **REFACTOR** → invert to assert absence / never-recreated
- Q25 Docs stating the budget model: `content/docs/operations/storage-and-resources.mdx:63-100`, `guides/execution.mdx:199,274-275`, `troubleshooting.mdx:159`, `reference/integrations.mdx:98`, `operations/security-and-data.mdx:67,77`. **REFACTOR**

### 1d. BuildKit / MBX / Gradle / heap partition injection

- Q26 `crates/velnor-runner/src/buildkit.rs:1090-1131` `resize_builder_daemon` (sole `docker update --cpus/--memory` writer; only other `"update"` is arity table `docker/deadline.rs:334`). **REMOVE** resize; **KEEP** builder lifecycle (`stop/start/prune/remove`, claim files) unbounded
- Q27 `crates/velnor-runner/src/executor.rs:12209-12270` `buildx_driver_resource_options` (`--memory`→`memory=`, `--cpus`→`cpu-period/cpu-quota`) + `buildx_driver_options` merge + call sites `:4610`, `:5208`, `:5291-5292` + tests `:16958-17011`, `:19859`, `:19785`. **REMOVE** sizing; keep builder setup/teardown
- Q28 `container.rs:491-499` `MBX_CACHE_DIR/TARGET_ROOT/GC_*` (`MBX_GC_MAX_TOTAL_SIZE=50GiB` etc.). Disk-hygiene GC bounds, not CPU/RAM partitions — outside the removal clause. **KEEP** (C2 author to confirm)
- Q29 Gradle worker count / JVM heap ceiling injection: **ABSENT** — no `ORG_GRADLE`/`GRADLE_OPTS`/`Xmx`/`MaxRAM` injection in `crates/velnor-runner/src` (only workflow detector `crates/velnor-workflow/src/scan/gradle.rs` + workflow `NODE_OPTIONS` passthrough `container.rs:2297` + upstream blocklists `script_step.rs:1150`, `workflow_command.rs:51`, both upstream parity). Nothing to remove. **KEEP**
- Q30 Workflow-supplied quota flags: `github_adapter.rs:850-875` untrusted `safe_container_option` allowlist currently **permits** `--cpus/--cpu-quota/--cpuset-*/--memory/...`; `executor.rs:14180-14200` is arity-only (`docker_run_option_takes_value`). **REFACTOR** → strip quota flags from the allowlist so no HostConfig ceiling can arrive via workflow (quota-free proof covers HostConfig); arity table **KEEP**

### 1e. Disk / PID: no per-job quotas exist (keep as-is)

- Q31 `capacity.rs:50-56` `DEFAULT_EMERGENCY_RESERVE_BYTES`/`DEFAULT_JOB_PEAK_BYTES` + `host_capacity.rs:55-133,191-300` (`probe`, `DiskPolicy/DiskPressure/DiskAction`) + `runner.rs:3559,3646,3673-3691,6733` disk admission. Admission-time *refusal*, not cgroup ceilings — spec-compliant. **KEEP**

## 2. Native admission path (8 items)

"Native" = Velnor-native daemon/slot path (per-slot JIT V2 + journal permits), vs the official-runner / Scale-Set lane.
`max_jobs` / `assignable` / `MAX_JOBS`: **zero hits in `crates/`** (exist only in `plans/bastion-three-provider-ci/`). Global N is greenfield.

- N1 Permit ledger — `crates/velnor-control/src/journal.rs`: `FleetState` (`admission_blocked/version:143-145`, `desired_ready:146`, `capacity_declared/invalid:154-158`), `SlotRecord.permit_held:372`, `advertised_capacity:240`, events `DesiredCapacity:511`/`PermitReserved:514`/`ReadyAttempt`/`Assigned`/`JobOwned`, `SideEffect::AdvertiseCapacity:711`, reducer gates `:771-960`, `set/clear_admission_blocked:1820,1859`, `read_admission_state:1953`; model `crates/velnor-model/src/node.rs:144-147,386-455` (`desired_ready_slots`, `capacity_permits`, `missing_permit`). The durable single-count authority — closest thing to the future global-N ledger. **KEEP + REFACTOR** (extend to host-wide N incl. reserved/acquiring/provisioning/assignable/running/cleaning/uncertain)
- N2 Assignable-capacity creation — `runner.rs:3255` + `reserve_capacity_permits:4717-4752` (`DesiredCapacity{ready:--slots}` + N `PermitReserved` at daemon start); `node/controller.rs:1119,1238` `RegisterRunner` → `runner.rs:4756` `jit_configure_one_slot` → `controller.rs:1400` `ReadyAttempt`. Today pre-registers all N at boot — violates §4 just-in-time rule. **REFACTOR** (create/enable assignable capacity JIT against the global ledger + read-only demand adapter)
- N3 Admission gates around acquisition — `runner.rs:2750` `effective_capacity_blocked`, `:2927` `wait_for_capacity_block_signal_in`, drain/cordon reducer gates, provisional `JobOwned` flow (`journal.rs:944-1160`). **KEEP**
- N4 Existing native protocol (register/acquire) — per-slot JIT V2 (`velnor-model/src/scheduler.rs:20-38`), `protocol.rs` JIT/broker/run-service calls, controller ready/assigned/owned cycle. Reused as-is per §4 ("not a replacement executor"). **KEEP**
- N5 Disk admission (part of native admission) — same sites as Q31. **KEEP**
- N6 `capacity.rs:393-420` `ScopeLease::acquire:655-693` (+`Drop:693`) — cache-store mutex, not capacity; callers `cache.rs:2011,2795,2819,3025`, `runner.rs:7988`, `leftover_disk.rs:976,983`. **KEEP**
- N7 N-config surface — `service.rs:152-154` `--slots`, `velnor.env:33` `VELNOR_SLOTS=4`, daemon units `--slots ${VELNOR_SLOTS}`, `velnor-controller@.service:20` `--desired-ready`, `velnor-model/src/configuration.rs:50-51,66-67`, `velnor-control/src/config.rs:36`. Per-daemon today; global-N knob goes here. **REFACTOR**
- N8 `crates/velnor-runner/src/admission.rs` (transitively-closed *action* admission, `MAX_ADMISSION_*` bounds) — name collision only, not capacity admission. **KEEP** untouched

## 3. Mediated lease API for native Docker — never the raw socket (9 items)

Raw host socket is never bind-mounted into jobs (asserted `container.rs:2977-2983,4424,4441,4451`). All guest Docker flows through:

- L1 Identity constants — `docker_lease.rs:29-45` (`JOB_ID_LABEL`, `DAEMON_ID_LABEL`, `JOB_CGROUP_PARENT="velnor-jobs.slice"`, `JOB_CONTAINER_NAME_PREFIX`, `BUILDKIT_CONTAINER_NAME_PREFIX`). CgroupParent is placement/identity, not a ceiling. **KEEP**
- L2 Proxy server — `docker_lease.rs:2160-2480` (`DockerLeaseGuard`, `bind:2353`, `bind_to:2360`, `Drop:2378`, spawn `:2422`, `proxy_until_closed:2932`). The mediation itself. **KEEP**
- L3 Request mediation — `rewrite_docker_api_request:1212`, `transform_request_buffer:3456`, `inject_ownership_labels:1166`, `is_docker_object_create:984`, CgroupParent enforcement `:1470-1520` (force-set `:1518`, ambiguity fail-closed `:1491,:1512`). **KEEP**
- L4 Nested-create resource fields — `reject_unsafe_nested_host_controls:1555-1630`: `cpushares/cpuquota/cpuperiod/cpusetcpus/cpusetmems/memory/memoryreservation/memoryswap/nanocpus/oomkilldisable/pidslimit/shmsize` currently **pass through** (`:1599-1606 => false`). Nested lease creates could self-impose ceilings; §C2 proof covers nested descendants. **REFACTOR** (strip or explicitly allow; default strip)
- L5 Security policy in the same gate — privileged BuildKit-only `:1567-1573`, binds/mounts `:1590-1593`, host net/mode denies `:1562-1566`, GPU exact-shape `:1583`, restart-policy `:1577`. **KEEP**
- L6 Job-side wiring — `container.rs:26` `JOB_DOCKER_HOST`, `:247` `DockerLeasePaths`, `:1272` `docker_lease_paths`, `:1349` lease-mount `daemon_visible:/var/run/docker.sock`, `:655,:920` `DOCKER_HOST`, `:662-666,:925-929` `VELNOR_DOCKER_HOST_*`; `docker_lease.rs:562` `guest_docker_socket_host`; `executor.rs:2078-2079,2253,2263,5731` guard fields/handoff/abort. **KEEP**
- L7 Reclaim by ownership label — `docker_lease.rs:605-830` list/remove arg builders, `:840-877` `JobNetworkGuard`, `:1985-2160` `list/remove/reclaim_*`, `reclaim_orphan_jobs:2099`, `reclaim_daemon_orphan_jobs:2113`. **KEEP**
- L8 Host leg (runner→real daemon, not guest-visible) — `docker/engine.rs:151-200,345-367` endpoint resolution + socket candidates, `:177-178` `VELNOR_DOCKER_HOST`/`DOCKER_HOST` precedence; `docker/client.rs:865-1323` BuildKit-exclusion listing. **KEEP**
- L9 Socket presence checks (not ceilings) — `service.rs:236` `require_docker_socket`, `execution/docker.rs` preflight socket-exists, `preflight.rs:396-413` `DOCKER_HOST`/socket validation, `:277` lease-socket mount in preflight container args. **KEEP**

## Counts

| Section | Items | REMOVE | REFACTOR | KEEP |
|---|---|---|---|---|
| 1. Quota/ceiling (Q1–Q31) | 31 | 18 | 8 | 5 |
| 2. Native admission (N1–N8) | 8 | 0 | 3 | 5 |
| 3. Mediated lease API (L1–L9) | 9 | 0 | 1 | 8 |
| **Total** | **48** | **18** | **12** | **18** |

Split-verdict items (Q8, Q22, Q26, Q30) are counted once under REFACTOR; their KEEP halves are named inline.
