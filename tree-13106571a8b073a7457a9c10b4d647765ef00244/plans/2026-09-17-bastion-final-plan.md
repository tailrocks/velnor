# Bastion final plan: Velnor-controlled, Docker-native three-provider CI

**Authority:** This plan replaces the nine supplied plans, prompts, and evidence appendices as the single implementation plan. It incorporates the final user instruction above every attachment. Evidence snapshots remain historical facts; superseded architecture is not an implementation option.

**Target:** `root@37.27.110.241`, bastion, Debian 13, AMD EPYC 9454P, 48 physical cores / 96 logical CPUs, approximately 128 GB RAM. The supplied 128494 MiB corresponds to approximately 125.48 GiB. These are operator-reported facts, not a fresh host inspection.

**Status:** Consolidated specification with ordered acceptance gates and a paste-ready execution prompt. No source changes, deployment, test execution, or subagent implementation are claimed by these documents. Read-only live checks and the nine complete attachments inform the evidence section.

**Rollout:** Velnor → Jackin → ChainArgos → repeatable generic onboarding. `tailrocks/velnor-apt` is a necessary Velnor delivery dependency, not a fourth parallel application migration.

**Documents in this plan:**

- `plans/2026-09-17-bastion-final-plan.md` — this specification (sections 1–12 plus the retained-inventory annex)
- `plans/2026-09-17-bastion-final-plan-gates.md` — ordered checklist and acceptance gates A–G
- `plans/2026-09-17-bastion-final-plan-goal.md` — paste-ready `/goal` execution prompt

## 1. Authority and resolved conflicts

The attachment order used is v1 specification/evidence/prompt, then v2 specification/evidence/prompt, then the Velnor-first audit/specification/prompt. The latest user instruction wins over every attachment, including contradictory sentences in the bootstrap documents.

| Earlier proposal | Final decision |
| --- | --- |
| Six KVM worker VMs, QEMU, libvirt, remote provisioners | Removed. One Docker-native Debian host; no virtualization layer or new remote worker abstraction. |
| Independent Go scale-set controller or SDK sidecar | Removed. Scale Set control is implemented in Rust inside Velnor; upstream Go is reference material only. |
| Official self-hosted capacity remains available while all of Velnor is stopped | Removed. Both local modes depend on Velnor management. Already-running official runner containers should survive recoverable controller restarts, but new local provisioning depends on it. Hosted recovery stays independent. |
| Repository/organization worker pools and reservations | Removed. GitHub registration scopes remain where required, but all local work shares one host-wide capacity authority. |
| Per-slot budgets, resource classes, inherited job-slice ceilings | Removed from this deployment and the final execution model. No Velnor-imposed CPU/RAM quotas or per-slot build throttling. |
| Custom DinD rewrite for native Velnor | Removed. Preserve native Docker execution and its job-scoped API mediation/ownership semantics. |
| Direct release-asset or local `.deb` installation | Removed. Production installation and upgrades use the signed `tailrocks/velnor-apt` repository only. |
| Preserve old `github/velnor/both` schema through aliases or compatibility paths | Removed. One explicit provider-set model; complete breaking migrations at each repository's rollout gate. |
| Stop after Velnor and require another planning exercise | Removed. The same campaign proceeds through Jackin and ChainArgos, but only after each predecessor is independently qualified. |
| Rebuild workflow tooling in each normal CI job | Removed. Immutable attested products, publish-before-pin, one candidate build per source closure/platform. |

Historical tooling versions, failures, paths, and unit inventories remain useful discovery and regression inputs. They are not deployment pins or proof that a failure still exists.

## 2. Preserved evidence and limited live refresh

### 2.1 Immutable audit baselines

| Repository | Audited source SHA | Audited generator pin | Preserved inventory |
| --- | --- | --- | --- |
| `tailrocks/velnor` | `3353310c7648fca22698b6c0f4a69ab245127786` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 17 verification units; GitHub-hosted + Velnor defaults |
| `jackin-project/jackin` | `92f347ac39fbf0d6f9853168e2896a6c60522924` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 9 workflows; 40 units: 36 Rust, 1 Bun, 1 Docker, 2 Swift; hosted-only defaults |
| `ChainArgos/java-monorepo` | `235e479b150aeb949bc8a5190fba5b84f6303c80` | `1279c4f92c97b75dc4cc627f122e119f8a5eae16` | 11 workflows; 71 units: 37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs; Velnor-only defaults |

Source: original evidence appendix, repository sections; v2 preserves these as architecture-neutral facts. The full supplied ChainArgos unit inventory and Velnor unit IDs are retained in the annex below.

**Velnor CI:** The bootstrap audit records `35129353335` failing Policy on `generated-tree` against `b9c3156c...`, while Planning succeeded. Source `velnor-runner` version was `0.1.275`, with no matching `v0.1.275` release found at that inspection. Roughly two minutes were spent building the policy runtime from source. Do not confuse a crate version, a runtime-product release, an official runner release, and an installable daemon package.

**Runtime race:** PR #904 at `62a74bf58993c8c073d47274897f0db88cbd6159` had run `35136207272` fail Planning because product closure `f1f88c200e5b3b82` for requested revision `48a66ad7d56636f8bfa6069fbaf810d0089fef39` was not available until later. Reuse verified useful work; do not copy the race. PR #901's ruleset-403 fallback is a separate historical issue, not proof of the current root cause.

**Jackin:** Run `35114867283` exposed Swift-on-Ubuntu failures (`desktop xcframework requires macOS (Apple Silicon)` and `swift: command not found`) and a desktop test reading a deleted `release.yml`. Default nextest excluded `dind_e2e`, `session_send_e2e`, `usage_broker_e2e`, and `load_options_e2e`.

**ChainArgos:** Run `35094895601`, Policy job `104789709640`, recorded two failed rules: `generated-tree` and `trusted-runners`. All 71 units were skipped. The trust findings concerned maintenance/nightly jobs. The complete inspected tree lacked `.github/actions/` despite references to `report-velnor-ci-outcomes`. Rust Docker builds had the wrong context and omitted bake targets.

**Upstream:** `actions/scaleset` reference `fb56300503fd21caa788feeb85c63071d15155c6` dated 2026-09-15 moved acquisition responsibility into the scaler. Older release `v0.4.0` resolved to `6ce025902cd964747a078c2aabe7340ebc667eca`; do not mix its interface with current main. Official runner baseline was `v2.337.0`, with container behavior inspected at source `80bb1fb827fa44d489263061e71ef4adba7ad8cd`. These are historical reference identities, never a substitute for digest resolution and conformance testing.

### 2.2 What was refreshed while consolidating

The selected GitHub connector returned the same Velnor main SHA above; PR #904 remained open at the recorded head; `velnor-apt/.github-gen/NO_WORKFLOWS_REQUIRED.md` still stated that APT publication primitives were absent; and upstream Scale Set main still resolved to `fb563005...`. These reads support the priority of the recorded work, not claims that all historical failures were rerun or that bastion is deployed.

The APT repository declaration still explicitly omits publication/update workflow families. The generator must implement the missing capability; a documentation-only exemption is not acceptable for this campaign. Do not remove the omission notice until generated coverage genuinely replaces it.

Refresh all main refs, PR status, run attempts/logs, releases, runner permissions, image digests, and host state at execution. Record corrections as `old observation → new evidence → consequence`, without changing this target architecture.

## 3. Architecture and product boundaries

```text
GitHub Actions
  ├─ github-hosted ───────────────────── GitHub-managed machines
  │
  └─ bastion: Debian + Docker Engine
       └─ APT-installed Velnor Rust control plane
            ├─ protected credentials + GitHub registration adapters
            ├─ durable journal + global queue/capacity authority
            ├─ lifecycle supervision + reconciliation + telemetry
            ├─ native adapter
            │    └─ existing Velnor execution → native Docker job containers
            └─ Scale Set adapter
                 └─ official one-job runner container + private DinD
```

There is one local product and one authority over job capacity, not necessarily one OS process. Preserve useful guardian/controller/job-process isolation within Velnor. Do not add an independently operated controller stack, Go controller runtime component, Kubernetes, VM, or per-repository daemon.

The official lane runs the unmodified official Actions runner; Velnor does not interpret its job steps. The native lane remains Velnor's own interpreter/executor and existing Docker backend. Management may share Docker client code, credential refresh, journaling, and cleanup primitives, but protocol-specific sessions and runner identities remain distinct.

Supported local operations must be packaged product behavior, not ad hoc host scripts that become an alternate runner implementation. Refactor freely to remove old control models; preserve native execution semantics, not obsolete architecture.

## 4. Explicit providers, capabilities, and result identity

Canonical provider IDs are exactly:

```text
github-hosted
github-self-hosted
velnor
```

Implement one provider-set schema across configuration, scanning, IR, plans, validation, `runs-on`, bootstrap, caching, artifacts, policy, dispatch, UI names, and aggregation. Remove the legacy `Github/Velnor/Both` model, ambiguous aliases, deprecation branches, and implicit provider inference from `runner.environment` or label substrings.

Illustrative final configuration contract — to implement and validate, not claimed current syntax:

```toml
[workflow]
providers = ["github-hosted", "github-self-hosted", "velnor"]
automatic_providers = ["github-hosted", "github-self-hosted", "velnor"]
default_dispatch_providers = ["github-hosted", "github-self-hosted", "velnor"]
```

Platform, trust, and capabilities are separate typed fields. Capabilities include Docker, nested privileged Docker, Buildx/Compose, Testcontainers, services with readiness, browser binaries, and native macOS/arm64. They do not carry CPU/RAM resource classes. Unknown selectors and unsupported capabilities fail explicitly; they never default to hosted or disappear silently.

Run one affected/full planner, then fan each selected eligible Linux unit out to all three providers with the same source, command/profile/features, fixtures, and test expectations. Display names must expose provider, for example `Rust · velnor-runner / github-self-hosted / bastion`. Use disjoint dedicated local selectors for official and native engines. A selector is routing, not authorization or a three-way fanout instruction.

Define result identity as:

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

The expected result set is fixed before execution. An expected missing, skipped, cancelled, timed-out, failed, duplicate-conflicting, or identity-mismatched result cannot pass. Platform/trust exclusions and genuinely unaffected units are declared by the trusted planner before expansion, not reclassified after failure. Diagnostic subsets are visible reduced coverage, never successful full qualification.

Each provider must actually execute its assigned verification. A hosted success or artifact cannot certify a local lane. Preserve legitimate compiler cache reuse; never substitute another lane's test report. Disable matrix fail-fast for qualification.

Planning, authoritative policy, recovery, and required-result monitoring stay hosted. External mutations—signing, package/registry publication, release/tag creation, deployments, Renovate writes, issues, destructive cache maintenance—have one writer. Retain actual required external contexts, including DCO; disabled application releases stay disabled.

## 5. Workflow generation and immutable runtime products

### 5.1 Sole generator

`velnor-workflow` owns every `.github/workflows` file in every repository this campaign changes, including runtime-product publishers, source releases, APT publication, control/watchdog jobs, and maintenance. It also owns or explicitly validates referenced local actions and generated manifests.

No hand YAML, copied historical workflow bodies, hidden repository-local templates, arbitrary command strings, raw-YAML escapes, or repository-name special cases. Reuse audited canonical repository executables only through defined typed capability contracts. A typed APT hook is not permission to add a general shell escape hatch.

Required mechanical checks: exact regeneration, ownership inventory including unexpected files, local-reference resolution, structured policy, actionlint, provider expansion, native-platform routing, and fail-closed aggregation. Rename fixture repositories and verify unchanged semantics to detect name-based special cases.

Each repository migrates directly to the final schema at its gate. Repositories awaiting rollout may continue using their previous immutable generator release; that is a staged deployment, not a compatibility parser in the new product. Make one-time configuration/state migration explicit; do not retain two runtime models.

### 5.2 Publish-then-pin without self-reference

Let `R` be the already published trusted runtime and `C` a candidate generator source closure.

1. Normal planning/policy and committed generated files remain bound to `R` while `C` is developed.
2. Build/test `C` once per full source closure/platform. Candidate output is checked in disposable test trees; it is not a new trusted pin.
3. Verify that the closure includes every build-affecting local dependency, build script/input, locked dependency, compiler/target/features/profile, and relevant configuration. Do not assume a hash of one crate covers a growing workspace.
4. Merge reviewed source and publish/attest the approved `C` product through a generated trusted hosted producer. Reuse a candidate binary only when source and build provenance are exact; a changed merge closure requires one new build.
5. Confirm all required platform assets and trusted attestations exist and verify before exposing the product as consumable.
6. A follow-up promotion commit atomically updates pin/product metadata and the entire generated tree using that verified product.

Changing generator source does not force committed workflows to be rendered by unpublished source. This resolves the bootstrap document's tension between immediate regeneration and publish-before-pin. Final pin/tree consistency remains mandatory.

Consumers verify producer repository, full closure, source identity, OS/architecture, features/profile, manifest and binary digests, trusted signer workflow/ref, and binary self-report. A cache hit does not waive integrity checks; a PR-provided checksum does not establish trust. The producing repository and consuming repository are different identities.

A missing product is a precise producer defect, not a signal to retry forever or compile in the consumer. Normal Planning/Policy/unit bootstrap has no cargo/source-build fallback. Do not select the daemon package through an unqualified `releases/latest` response: runtime products and daemon releases are separate release identities.

The initial generated publisher can be bootstrapped by a reviewed, controlled one-time maintainer build of the generator. That is an explicit tooling producer, not a production binary installation, ordinary CI fallback, or handwritten workflow. No PR-controlled code runs in privileged policy/publication contexts.

## 6. Global capacity, queueing, and unbounded execution

### 6.1 One host-wide permit ledger

Both local modes share exactly one `max_jobs=N`; GitHub-hosted jobs do not consume it. No fixed repository, organization, or engine reservation, priority, weight, or separate hardware pool exists.

A permit counts one top-level job lifecycle, including its runner, DinD, and nested service/build/test containers. Jackin's nested 20-capsule test remains one top-level job; its descendants still require ownership and cleanup.

Hold the permit from the point capacity is committed for acquisition/assignable readiness until terminal work and owned cleanup are confirmed. Include reserved, acquiring, provisioning, idle-assignable, running, cleaning, and uncertain states in the single count. Offered but unacquired work is queued demand, not occupied capacity. Capacity cannot exceed N after crash recovery or duplicate delivery. A cleanup failure must retain a visible reservation rather than releasing fictitious capacity.

Every native and Scale Set acquisition/readiness path uses this authority. In particular, do not let pre-registered native idle slots reserve all N indefinitely while the official lane waits. Observe demand separately; create/enable assignable native capacity just in time, using the existing native protocol with admission integrated around it. Where its current signals cannot support this, implement and verify a narrow read-only demand adapter, not a replacement executor or a second scheduler.

Start with no permanently reserved warm-idle inventory; pre-pull images instead. Registration/session metadata is not itself a compute pool. A future idle runner capable of taking a job must be counted and may not bypass FIFO/capacity. Persist generation-fenced grants and reject stale mutations. Reconcile before advertising capacity after restart; do not erase occupied work by resetting a semaphore.

### 6.2 Oldest observed eligible work first

Order eligible demand by durable `first_seen_at` plus an immutable sequence/tie-breaker across all local modes/scopes. Redelivery retains original age. Do not grant a younger job before an older eligible job when Velnor controls that choice. Ineligible/cancelled/blocked work has an explicit reason; an unavailable scope need not block unrelated eligible work.

This governs admission, not completion or actual start order. GitHub controls delivery and runner assignment; observed order is not global GitHub submission order. Native assignments, upstream redelivery, and assignment to any eligible runner can differ from local observation. Never promise strict end-to-end FIFO or a particular acquired job receiving a particular JIT runner.

### 6.3 No CPU/RAM ceilings

The final deployment applies no Docker `NanoCpus`/CPU quota/cpuset restriction, memory/reservation/swap ceiling, systemd `CPUQuota`, `MemoryMax`, or `MemoryHigh` on the workload ancestry. No slot-divided `CARGO_BUILD_JOBS`, MBX/BuildKit entitlement, Gradle worker count, or artificial heap ceiling is injected as a disguised resource partition.

Remove package-created quota drop-ins and preflight assumptions that require them; prevent upgrades from recreating them. Refactor configuration, runtime, BuildKit/service handling, package hooks, and tests together. Do not keep the old budget model solely for backward compatibility. Cgroups remain useful for identity, process ownership, observation, and cleanup without ceilings. Do not introduce per-job disk/PID quotas from v1.

Prove this for native jobs, official runners, DinD, and representative nested descendants through Docker HostConfig, effective cgroup ancestry (`cpu.max`, `memory.max`, `memory.high`, swap/cpuset settings), package units/drop-ins, and effective build environment. Inspect inherited limits, not only emitted Docker flags.

Keep correctness-required test serialization: Jackin's Docker E2E group and ChainArgos's RustFS group are test semantics, not a CPU partition. Revisit performance settings through tests rather than overriding every intentional serial group.

Unlimited resource entitlement does not imply infinite memory or guaranteed host isolation. No plan claims per-job OOM containment on this shared kernel. Report OOM/pressure, preserve diagnostics, and tune N; do not silently restore quotas or disable OOM handling.

### 6.4 Tune N for completed work

Benchmark a representative full and mixed workload, initially testing values such as 16, 24, 32, 48, 64 and higher only while stable useful throughput improves. These are experiments, not five simultaneous pools or promised capacity. Use a measured native-only provisional N before Scale Set, then remeasure mixed modes and requalify after Jackin/ChainArgos.

Keep source, logical plan, image/toolchain identity, and cold/warm conditions comparable. Measure jobs/minute, end-to-end time-to-green, queue and provisioning delay, compile/test/cache/cleanup phases, CPU, available memory/pressure/OOM, disk/inodes/IO latency, Docker/BuildKit/cache-lock contention, and failures. Do not intentionally destabilize unrelated jobs with a benchmark. Only host-level N changes; containers remain unbounded. Prefer highest stable useful throughput, not the largest integer or CPU utilization alone.

## 7. Rust Scale Set adapter and official-runner lifecycle

### 7.1 Protocol contract

Pin an audited upstream revision and implement in Rust: scope/group/set registration and reconciliation, App credential/token refresh, session creation/renewal, long polling, `lastMessageID`, capacity reporting, initial statistics, empty poll responses, `JobAvailable`, explicit `AcquireJobs`, JIT config, start/completion events, acknowledgement, and shutdown/recovery.

At the preserved main reference, the handler owns acquisition; returning success allows acknowledgement. Main and v0.4.0 differ. Add recorded/sanitized protocol fixtures and real upstream conformance tests, not only mocks that reproduce the implementation's assumptions.

Processing order:

1. Apply lifecycle/completion observations idempotently and reconcile durable occupied state.
2. Submit offered eligible work to the global oldest-observed queue; validate trust before grant.
3. Reserve a global permit and persist acquisition intent before `AcquireJobs`.
4. Record the actual returned acquired request IDs. Reconcile partial or uncertain responses; never assume every requested ID succeeded or spend a permit twice.
5. Persist JIT/provisioning intent and make runner creation idempotent through stable operation/ownership IDs.
6. Acknowledge only after replay-safe effects are durable. Acknowledgement need not wait for job completion. Keep offer validity/session boundaries explicit: do not acknowledge away the only reference to deferred work without proving later acquisition/re-offer semantics.
7. Use authoritative `Statistics.TotalAssignedJobs` for population convergence, including waiting and running assignments. Do not count capped message batches, double-count statistics plus reservations, or treat local tickets as GitHub assignment truth.
8. Derive each session's capacity advertisement from shared grants and existing commitments. Never let every listener independently spend N; verify upstream total-capacity versus free-capacity semantics and races.
9. Reconcile on idle polls too, with backoff/rate-limit handling and bounded calls. Unknown events after restart must trigger reconciliation, not panic.

A database transaction cannot atomically include GitHub acquisition and Docker creation. Durable intent, idempotency, generation fencing, and reconciliation close those crash windows. Promise no local duplicate execution from a replayed assignment; do not claim exactly-once external side effects across GitHub retries.

### 7.2 Homogeneous official workers

Each official scale-set worker in the initial trusted Linux profile gets a pinned official Actions runner image and a private pinned DinD daemon. Use one homogeneous Docker-capable profile rather than guessing a job's runner requirements from an acquired request ID. GitHub may assign any eligible pending job to any idle runner in that set. Explicit typed authorization still controls privileged nested workloads.

Pin official runner, DinD, and local job/toolchain images by validated digest. Never deploy `latest`. Record runner version separately from Velnor/package/protocol reference. Verify the official image's actual tool content; it is not assumed to contain the hosted image's full catalog. Keep image updates a tested generated product path, respecting current runner update requirements.

Lifecycle:

```text
observed → eligible → reserved → acquire-intent → acquired/uncertain
→ provision-intent → DinD ready → runner connected → running
→ terminal → diagnostic export → owned cleanup → permit released
```

Reconcile each boundary, including success, error, cancellation, lost acknowledgement, runner death, daemon death, and management restart. Do not delete a scale set on a normal service restart; decommission is explicit. Preserve active work across ordinary controller restart where supported, otherwise surface explicit failure and cleanup—not a lost job or false success.

### 7.3 Private Docker semantics

Never mount bastion's raw `/var/run/docker.sock`, an alternate unrestricted host socket, or an unrestricted host Docker proxy into any job. The physical management socket belongs only to Velnor management. A job-private DinD socket may appear at `/var/run/docker.sock` inside the job; prove its actual source/daemon identity.

Use a private Unix socket, no public Docker TCP API. Share runner/DinD network namespace so localhost published ports behave as expected, while inner container-job services retain their own bridge/DNS semantics. Merely putting two outer containers on the same bridge does not share loopback.

Make bind sources visible at identical absolute paths to the runner and its daemon: checkout/workspace, `_work`, shared `TMPDIR`, necessary HOME/capsule paths, action externals, tool cache, and file-command directories. Match ownership. Do not expose the host root or broad credential directories to achieve convenience. Docker evaluates bind sources on the daemon's filesystem, so client-only paths are insufficient.

Every runner, DinD data directory/socket, network, volume, workspace, and nested job object gets a recorded ownership identity. Export logs before removing ephemeral resources. Two official jobs must be able to use the same inner names/ports without colliding. Native Velnor must achieve required isolation through its existing mediation/namespace semantics, not a newly imposed DinD rewrite.

### 7.4 Native Velnor boundary

Keep native Docker spawning, action semantics, job-scoped API lease proxy, object tracking, streams/cancellation, and safe retained caches. Its mediated host-backed Docker API is not private DinD and must not be described as such. Retain its protection against cross-job Docker objects; never replace it with a raw host socket.

Only control-plane integration, admission, credential renewal, unbounded policy, and concrete conformance bug fixes modify this path. Do not switch native jobs to the official runner or build a parallel execution backend.

## 8. Trust, credentials, host setup, and caches

Bastion is a trusted-tier shared-kernel CI host, not an adversarial multi-tenant sandbox. Privileged DinD increases host risk even without a raw management socket. No VM isolation or independent-local-failure-domain claim is made.

Enforce trust outside PR-editable YAML: controller-side event/source/ref checks and verified runner-group/workflow restrictions where available. Same-repository origin or a requested label is not sufficient. Cover forks, bot PRs, same-repo PRs, main, schedules, dispatch, tags, and merge-group explicitly. Test label spoofing and reusable-workflow input substitution. Default untrusted fork execution stays hosted, never privileged on bastion. Never execute untrusted checkout under privileged `pull_request_target`.

Use protected Velnor management credential providers, preferably GitHub Apps with per-installation scope. Keep App keys, administration tokens, signing keys, host SSH keys, and telemetry-write credentials out of jobs. Refresh tokens in the running provider, not by overwriting an EnvironmentFile and assuming process environment changes. Only required ephemeral runner/session material reaches official runners; native jobs receive their intended job credentials, not management authority. Keep publisher credentials separate and hosted. Authentication scopes do not allocate compute.

Host provisioning is idempotent and non-destructive. Read OS/CPU/NUMA, RAM, disks/signatures, mounts, inodes, Docker/cgroup driver, systemd units, running jobs, sockets, routes/firewall, and existing state first. Preserve SSH and recovery access. Leave the second NVMe untouched; no automatic formatting, RAID, or repartition. Do not use RAM-backed `/tmp` as if it were extra disk/RAM.

Install Docker and required Buildx/Compose tooling through an approved pinned package setup; verify cgroup v2/systemd driver against native preflight. No libvirt/KVM/QEMU. Keep Velnor configuration under `/etc/velnor`, durable state under `/var/lib/velnor`, runtime sockets/locks under `/run/velnor`, deliberate cache storage under `/var/cache/velnor`, and owned scratch on the existing filesystem. These are managed product paths, not repository-specific scripts.

Keep inner service ports and control APIs private. Permit required outbound GitHub/registry/package endpoints without assuming a frozen three-host allowlist. No public webhook receiver is required for the Scale Set long-poll design. Do not alter the host public route merely to add container networks.

Maintain fresh workspaces and private official daemon state with explicit durable caches. Namespace compiled artifacts by repository numeric ID, trust, provider, platform, image/toolchain/ABI, options, and dependency/source compatibility. No simultaneously writable shared Cargo target, Gradle home, or official DinD data directory. Preserve native Velnor's proven lease-aware stores; distinguish reusable content from unsafe mutable working trees. Export/import official DinD BuildKit cache before deletion. Untrusted inputs cannot publish trusted executable caches.

No host-wide `docker system prune`. Reclaim only owned inactive resources; protect active leases and state. Observe disk/inode pressure and apply intentional cache retention without resurrecting v1 per-job quotas. Preserve normal parallel main runs; PR supersession may cancel only the same PR's older attempt, never sibling providers or unrelated branches.

### 8.5 Server-setup reference paths in java-monorepo ansible-configs

Bastion host provisioning follows the proven `ChainArgos/java-monorepo` Ansible patterns below. Links are pinned to the audited ChainArgos source SHA `235e479b150aeb949bc8a5190fba5b84f6303c80`; re-resolve each path against live `main` at execution and record any drift as `old observation → new evidence → consequence`.

| Playbook or doc | Path in `ChainArgos/java-monorepo` | Use for bastion |
| --- | --- | --- |
| Base server setup | `ansible-configs/install-base.yml` | UTC timezone, APT base packages, shell/terminfo baseline for a new Debian server |
| Docker Engine install | `ansible-configs/install-docker.yml` | Pinned Docker APT repository, `docker-ce` + Buildx/Compose plugins, daemon logging config, service enablement |
| Docker host variant | `ansible-configs/install-docker-selene.yml` | Host-specific Docker install variant; compare before choosing bastion's Docker shape |
| Inventory pattern | `ansible-configs/hosts.ini` | Ansible inventory structure for dedicated Hetzner servers |
| Collection dependencies | `ansible-configs/requirements.yaml` | Galaxy collection prerequisites for every playbook run |
| Playbook index | `ansible-configs/README.md` | Which playbook owns which host, controller bootstrap, secret handling via `fnox`/`op` |
| Debian upgrade runbook | `ansible-configs/docs/upgrade-debian.md` | Manual Debian upgrade procedure |
| Package updates | `ansible-configs/update-packages.yml` | Routine APT package refresh |
| Debian upgrade play | `ansible-configs/upgrade-debian.yml` | Automated Debian release upgrade |

Reference permalink root: [ansible-configs at the audited SHA](https://github.com/ChainArgos/java-monorepo/tree/235e479b150aeb949bc8a5190fba5b84f6303c80/ansible-configs). These playbooks are the starting pattern for idempotent, non-destructive bastion provisioning; bastion-specific Velnor packaging, APT repository wiring, and systemd units are delivered through the Velnor package and the generated workflows in this plan, not by forking these playbooks into an alternate runner implementation.

## 9. APT delivery pipeline and package operation

The production source is the signed repository documented by `tailrocks/velnor-apt` at `https://velnor-apt.tailrocks.com/`, not a release download installed locally. The final package carries the reviewed Velnor management/native/Scale Set code and the required operational binaries/units. No untracked binary replacement or separately deployed controller.

Implement generic typed APT capabilities: source repository and exact release identity, package, architecture set, stable/preview suites, signer fingerprint and secret references, release-coherence verification, repository assembly, publication records, previous-version retention, GitHub Pages artifact/deployment, and channel-update tasks where actually required. Preserve suite behavior through one final implementation rather than compatibility wrappers. `apt-repository` cannot be a descriptive label producing only docs.

Audit and reuse `scripts/verify-release.sh` through a narrowly defined typed verifier contract; generalize reusable logic where appropriate. Configuration must not execute arbitrary shell/YAML. Prove generic behavior with a renamed fixture package/repository. Generate all APT CI/publication/required-result workflows, replacing the omission notice and stale direct-install documentation.

Pipeline:

```text
verified source/tag → coherent amd64 + arm64 products and record
→ independent digest/source/manifest/OCI/provenance verification
→ staged APT index and retained previous version
→ signed InRelease/Release metadata + publication record
→ generated hosted single-writer Pages deployment
→ independently verified live APT candidate
→ locked exact-version APT install on bastion
→ verify-installed/activation → start → real job
```

Keep complete existing release contracts, including native arm64 payload generation, unless an explicit correctly versioned product change removes an actual dependency. Building/staging a kernel/rootfs is not running a VM. Genuine KVM-runtime tests stay a declared separately qualified platform capability; do not provision VMs on bastion or count a Docker substitute as passing them.

Source release and APT publication run through generated hosted recovery/publisher paths so broken local Velnor cannot block the package needed to repair it. Verification may run multiple ways; signing/deployment has one owner. Serialize mutation of the same feed/tree, not all repository CI. An older publication must not roll back a newer candidate unintentionally.

Before install, authenticate the public signing-key fingerprint against a separately trusted project reference; then configure repository-scoped `Signed-By`. Do not blindly trust whatever key comes from the download URL. Verify signed metadata, candidate version, architecture, package hash, source/record, and repository origin. APT's metadata signature/checksum chain and a separate artifact attestation are distinct checks.

Retain the package lock and release activation contract from the original audit: `/run/velnor/package-transaction.lock`, compatible package/manifest/binary record, and `release verify-installed` before starting services. Drain affected jobs, preserve config/secrets, and do not assume package installation starts the fleet.

After these prerequisites, the root installation transaction has this shape:

```sh
# VERSION is the independently verified candidate from the signed repository.
: "${VERSION:?Set the verified exact Debian package version}"
install -d -m 0750 /run/velnor
apt-get update
apt-cache policy velnor-runner
/usr/bin/flock --exclusive --nonblock --no-fork \
  /run/velnor/package-transaction.lock \
  apt-get install "velnor-runner=${VERSION}"
dpkg-query -W velnor-runner
```

Derive activation, drain, and health commands from the implemented package/help and verify them. Do not invent working `velnorctl` mutation verbs. Never use `dpkg -i`, `apt install ./file.deb`, a copied executable, disabled signature verification, or altered packaged files to deploy production.

Use a bootstrap package followed by a Scale Set package. Build unbounded policy/global native admission early enough that the bootstrap package can activate them before the first bastion job. This is a source dependency running in parallel, not permission to run bounded jobs until later. Subsequent feature fixes also arrive through APT.

Breaking schema changes do not justify compatibility shims. Retain a previous coherent package with its matching config/state recovery snapshot only when restoration is proven; never claim a package downgrade alone can read a newer state schema. Otherwise document the tested forward-recovery path. All package recovery remains APT-only.

## 10. Hosted result verifier and fault contract

One generated hosted authority reports the required result for each run/attempt, preserving `ci-required` where required. It excludes itself and telemetry from the workload set. It starts after planning without waiting in `needs` for every local job; otherwise queued work can prevent outage detection.

Provide authenticated outbound Velnor health records, bound to repository/source/run/attempt, provider, sequence, freshness, occupied permits, and provisioning progress. A separate least-privilege telemetry writer is not a new runner controller. The hosted verifier validates identity and freshness; missing data means unavailable, not ordinary backlog. A hostname printed by a job is not placement evidence.

Correlate GitHub runner/job metadata with management provisioning IDs, actual official/native engine versions, image digests, selected tests and JUnit counts, cache/timing reports, and cleanup receipts. Preserve artifacts outside disposable containers. Enumerate all API pages and distinguish run attempts. A later reporter or cancellation must not overwrite an already failed required result with success; reruns revalidate exact identity and outcome provenance.

Initial measurable operating targets, not claims of current performance: reserve-to-connected within 180 seconds; owned cleanup within 120 seconds; free-capacity provisioning stall diagnosed within five minutes; full local outage reflected as failed/incomplete within ten minutes. Tune explicit execution/full-run deadlines against cold baselines. Legitimate FIFO backlog and unmet workflow dependencies do not equal a provisioning stall. Persist qualification waves when required by hosted job lifetimes or matrix limits; never omit units to fit one run.

| Adversarial test | Required invariant and evidence | Independent verifier role |
| --- | --- | --- |
| Kill official runner or DinD; kill native job worker | No false success; diagnostic export; only owned resources removed; capacity released once after cleanup; next job clean | Docker/lifecycle verifier |
| Kill Velnor; restart before/after acquire, JIT, Docker create/start, running, completion or ACK | Durable reservations reconciled; active containers adopted or explicitly failed; no duplicate execution, lost job, or leaked permit | Protocol and recovery verifiers |
| Redeliver/duplicate/reorder events; return partial/uncertain AcquireJobs responses | Deduplication and correct returned-ID handling; no statistics double-count; first-seen age retained | Protocol verifier |
| Cancel queued or running work, including GitHub upstream reassignment | Correct terminal/withdrawn state; no new runner for stale demand; permit/accounting correct | Queue verifier |
| Two scopes and both engines race for final permits | Total occupied ≤ N; no independent N per scope, old eligible work not locally overtaken, no permanent idle-slot starvation | Capacity verifier |
| Docker daemon restart or network loss during poll/acquire/ACK/refresh | Visible degraded state, no unsafe new acquisition, bounded retry, recovery without orphan/identity confusion | Infrastructure/recovery verifier |
| Incorrect runtime digest, manifest, signer/ref or APT key/package/architecture/record | Rejected before execution/publication/install; no insecure fallback; previous trusted state intact | Supply-chain verifier |
| Orphans, partial deletion, unknown lifecycle events, stale control generation | Reconcile ownership; do not panic or broad-prune; unreconciled state never counts as free | Lifecycle verifier |
| Fork selector spoofing or protected-workflow input substitution | No bastion execution; hosted-only trust exclusion explicit | Trust verifier |
| Missing/skipped/wrong-provider test report or stale run attempt | Required aggregate fails, even with remaining providers green | Result verifier |
| Hidden ancestor limits or injected build budgets | Inspect real containers/cgroups and environment; quota-free policy demonstrated on descendants | Resource verifier |

Run faults against identified canaries and test fixtures. Shared Docker restart/host reboot uses an isolated coordinated window preserving unrelated work and SSH. No destructive disk operations, broad pruning, deliberate whole-host OOM, or unrelated network outage is implied by “break fast.”

## 11. Repository-specific obligations after the Velnor gate

### 11.1 Jackin, second

Preserve the 40-unit historical baseline or record a coverage-equivalent correction. Its baseline has 38 Linux-oriented units, hence 114 three-provider executions plus two genuine hosted macOS units, before explicit additional E2E/control jobs. Do not count 120 Linux executions by pretending Swift runs on Linux.

Fix generic native routing to actual Apple Silicon macOS. Preserve same-job local XCFramework production followed by Swift build/test. Reconcile the obsolete desktop `release.yml` assertion with generator-owned disabled/enabled release policy; do not restore legacy YAML or enable releases merely to pass it.

Explicitly execute `docker-e2e` with `e2e` enabled. Build the capsule Linux ELF from the tested source and set `JACKIN_CAPSULE_BIN`; no preview-release substitute. Prove Docker, Buildx, Compose where used, `script(1)` PTY support, nested privileged DinD, Java Testcontainers, TLS/no-proxy behavior, and temporary relay socket/file mounts. Preserve serial E2E groups; exercise the 20-capsule fixture fanout. No fixture weakening to make a lane green.

Historic tool declarations include Rust 1.97.1, Bun 1.3.14, Node 24.18.0, configured Ubuntu 26.04 and macOS 26. Re-resolve actual compatible tool/image versions; do not silently force a different Linux userspace or install Apple-only mise tools on Linux.

Add only authorization, generic scope entries where needed, and typed generator configuration. If a generic bug is discovered, fix/publish the generator or Velnor package, requalify Velnor for that change, then continue Jackin. No per-Jackin host architecture.

### 11.2 ChainArgos, third

Preserve 71 historical units or a reviewed coverage-equivalent correction: 213 three-provider executions before additional controls. Repair policy ownership/trust failures and missing generated local actions first.

Provide disposable job-local PostgreSQL for Flyway/jOOQ and tests. Sixteen Gradle units historically ran `flywayMigrate jooqCodegen`; inspect the actual Gradle graph to remove duplicate root/module execution only with equal task/test coverage. Both tools must use the same job database through actual build-supported variables (including `POSTGRESQL_DB_HOST/PORT` where declared). Never target production or shared default databases.

Run Rust Testcontainers PostgreSQL, RabbitMQ, Redis, and RustFS suites; explicitly select the intended nextest CI profile, retain the one-at-a-time RustFS group, and independently verify the effective configuration. Historical RustFS image is `rustfs/rustfs:1.0.0-beta.8`; PostgreSQL fixture includes `postgres:18-alpine`. Resolve and lock tested digests rather than assuming those tags are immutable. Separate fixture acceptance from live Ethereum/Base/blockchain RPC suites and label actual external failures.

Fix generic Rust Docker context to include root workspace inputs, `backend/`, and `scripts/`. The historic bake contract specifies root context and 12 service targets; building only the Dockerfile's final stage is insufficient. Do not run root Compose unchanged with fixed global names/ports/home mounts through a shared host namespace.

Preserve GraalVM Java 25/native-image (historical oracle-graalvm-25.0.3), Gradle wrapper 9.5.1, Rust 1.98.1, required native/protobuf tools, Node 24.20.0, Bun 1.3.14, and declared browser requirements unless a separately tested source change updates them. Audit Playwright/browser gates; standard frontend unit success alone is not browser coverage. Check the historical Micronaut Docker API `1.44` override against the selected daemon.

Keep mutation single-writer and application release-disabled policy unchanged. Adding access/scope configuration must reuse the same Velnor controller and global N.

### 11.3 Generic onboarding

After qualification, onboarding is: verify trust/access → authorize repository and add a declarative GitHub scope only if necessary → add typed config → select an already published generator pin → regenerate/check → run full qualification → record health inventory. No new daemon, VM, hardware pool, custom host script, or copied workflow. A new GitHub organization requires authorization/registration metadata, not reserved resources.

## 12. Execution ownership and speed

The main agent is an orchestrator only: maintains the ledger, assigns work, resolves dependencies/decisions, reviews evidence, and unblocks. Delegate actual research, designs, code edits, integrations, tests, infrastructure, package publication, deployment, and verification to subagents. A designated integration subagent performs source integration/regeneration; a designated infrastructure subagent is the only live bastion writer in its window.

Every module has a named author and a different independent verifier. Verifiers attempt to disprove the target invariants and rerun critical checks; author assertions, unchecked boxes, screenshots, or a PR body saying “all green” are not certification.

Parallel workstreams: current/live evidence; CI/policy; runtime products; provider schema/rendering; APT primitives/feed; package/systemd/unbounded mode; native/global admission; Rust Scale Set protocol; official runner/DinD; trust/credentials; caches/performance; hosted verification; later-repository read-only inventories. Give additional agents bounded tests, source audits, and adversarial review whenever write dependencies block them. No idle agent waits for a shared generator file it does not own.

Use isolated worktrees and explicit file ownership. Integrate small verified vertical changes continuously; no prolonged design freeze or giant final merge. Preserve unrelated work and repository-required merge/DCO policy. No backward-compatibility shims, aliases, dual models, or deprecation windows. Breaking code/config/API changes are preferred when they directly reach the target. Package lifecycle/state migration must still be explicit and correct.

Ledger fields: task ID, dependency, author, verifier, source/branch, finding, target invariant, evidence location, status, blocker, and next action. Evidence is append-only/versioned enough to survive compaction. Known external blockers do not stop independent work or authorize fake completion. Judge correctness and target fit, not effort/ROI.

## Annex — retained inventory and source provenance

This annex is evidence, not an additional plan. Historical versions and IDs below must be revalidated before use. The final architecture and gates above govern implementation.

### A. Velnor baseline unit IDs

The original evidence identifies these 17 units:

```text
bun-velnor
docker
docs
opentofu
rust-policy
rust-unit-collector
rust-velnor-bench
rust-velnor-client
rust-velnor-control
rust-velnor-model
rust-velnor-render
rust-velnor-runner
rust-velnor-tools
rust-velnor-workflow
rust-velnor-workflow-contract
rust-velnorctl
rust-production-topology
```

### B. ChainArgos baseline unit IDs

Complete generated unit inventory:

| Unit | Kind | Root |
| --- | --- | --- |
| bun-chainargos-docs | bun | `frontend/docs` |
| bun-chainargos-eventcatalog | bun | `frontend/eventcatalog` |
| bun-platform | bun | `frontend/platform` |
| bun-platform-prototype | bun | `frontend/platform-prototype` |
| docker-ansible-configs-config-selene-observability-compose-maple | docker | `ansible-configs/config/selene-observability/compose/maple` |
| docker-ansible-configs-config-selene-observability-compose-parallax | docker | `ansible-configs/config/selene-observability/compose/parallax` |
| docker-ansible-configs-config-selene-observability-compose-sentry-proxy | docker | `ansible-configs/config/selene-observability/compose/sentry-proxy` |
| docker-backend-rust | docker | `backend-rust` |
| docker-docker-containers-docker-dbt-fusion | docker | `docker-containers/docker-dbt-fusion` |
| docker-docker-containers-docker-jvm-base | docker | `docker-containers/docker-jvm-base` |
| docker-docker-containers-docker-kestra-backup | docker | `docker-containers/docker-kestra-backup` |
| docker-docker-containers-docker-kestra-playwright | docker | `docker-containers/docker-kestra-playwright` |
| docker-frontend-platform | docker | `frontend/platform` |
| docker-frontend-platform-prototype | docker | `frontend/platform-prototype` |
| docker-frontend-wallet-screening | docker | `frontend/wallet-screening` |
| docs | docs | `.` |
| gradle-backend | gradle | `backend` |
| gradle-backend-bitcoin-domain | gradle | `backend/bitcoin-domain` |
| gradle-backend-bitcoin-flyway | gradle | `backend/bitcoin-flyway` |
| gradle-backend-bitcoin-model | gradle | `backend/bitcoin-model` |
| gradle-backend-bitcoin-processor-app | gradle | `backend/bitcoin-processor-app` |
| gradle-backend-bitcoin-utils | gradle | `backend/bitcoin-utils` |
| gradle-backend-coingecko-common | gradle | `backend/coingecko-common` |
| gradle-backend-coingecko-price-scraper-job | gradle | `backend/coingecko-price-scraper-job` |
| gradle-backend-coingecko-scraped-pricing-import-job | gradle | `backend/coingecko-scraped-pricing-import-job` |
| gradle-backend-crypto-utils | gradle | `backend/crypto-utils` |
| gradle-backend-eth-domain | gradle | `backend/eth-domain` |
| gradle-backend-eth-flyway | gradle | `backend/eth-flyway` |
| gradle-backend-eth-model | gradle | `backend/eth-model` |
| gradle-backend-eth-processor-app | gradle | `backend/eth-processor-app` |
| gradle-backend-eth-transfer-validation-job | gradle | `backend/eth-transfer-validation-job` |
| gradle-backend-legacy-domain | gradle | `backend/legacy-domain` |
| gradle-backend-legacy-flyway | gradle | `backend/legacy-flyway` |
| gradle-backend-redshift-dump-job | gradle | `backend/redshift-dump-job` |
| gradle-backend-report-flyway | gradle | `backend/report-flyway` |
| gradle-backend-tailrocks-jooq-utils | gradle | `backend/tailrocks-jooq-utils` |
| gradle-backend-tailrocks-type | gradle | `backend/tailrocks-type` |
| gradle-backend-tailrocks-type-converters | gradle | `backend/tailrocks-type-converters` |
| gradle-backend-temp-domain | gradle | `backend/temp-domain` |
| gradle-backend-temp-flyway | gradle | `backend/temp-flyway` |
| gradle-backend-toolbox | gradle | `backend/toolbox` |
| gradle-backend-transfer-monitor-app | gradle | `backend/transfer-monitor-app` |
| gradle-backend-transfer-monitor-domain | gradle | `backend/transfer-monitor-domain` |
| gradle-backend-transfer-monitor-flyway | gradle | `backend/transfer-monitor-flyway` |
| gradle-backend-tron-domain | gradle | `backend/tron-domain` |
| gradle-backend-tron-flyway | gradle | `backend/tron-flyway` |
| gradle-backend-tron-model | gradle | `backend/tron-model` |
| gradle-backend-tron-processor-app | gradle | `backend/tron-processor-app` |
| gradle-backend-tron-transfer-validation-job | gradle | `backend/tron-transfer-validation-job` |
| gradle-backend-whitelabel-app | gradle | `backend/whitelabel-app` |
| gradle-backend-whitelabel-domain | gradle | `backend/whitelabel-domain` |
| gradle-backend-whitelabel-flyway | gradle | `backend/whitelabel-flyway` |
| gradle-backend-whitelabel-model | gradle | `backend/whitelabel-model` |
| node-wallet-screening | node | `frontend/wallet-screening` |
| rust-bitcoin-grpc-server | rust | `backend-rust/bitcoin-grpc-server` |
| rust-bitcoin-migration | rust | `backend-rust/bitcoin-migration` |
| rust-bitcoin-processor-app | rust | `backend-rust/bitcoin-processor-app` |
| rust-blockchain-explorer | rust | `backend-rust/blockchain-explorer` |
| rust-chainargos-scripts | rust | `scripts` |
| rust-coingecko-pricing-app | rust | `backend-rust/coingecko-pricing-app` |
| rust-eth-grpc-server | rust | `backend-rust/eth-grpc-server` |
| rust-eth-migration | rust | `backend-rust/eth-migration` |
| rust-eth-processor-app | rust | `backend-rust/eth-processor-app` |
| rust-legacy-grpc-server | rust | `backend-rust/legacy-grpc-server` |
| rust-legacy-migration | rust | `backend-rust/legacy-migration` |
| rust-lightdash-csv-delivery-app | rust | `backend-rust/lightdash-csv-delivery-app` |
| rust-processor-compare-app | rust | `backend-rust/processor-compare-app` |
| rust-processor-monitor-app | rust | `backend-rust/processor-monitor-app` |
| rust-tron-grpc-server | rust | `backend-rust/tron-grpc-server` |
| rust-tron-migration | rust | `backend-rust/tron-migration` |
| rust-tron-processor-app | rust | `backend-rust/tron-processor-app` |

### C. Jackin inventory boundary

The supplied evidence reports 40 units and their kind counts but does not provide a full per-unit table comparable to ChainArgos. Do not invent those IDs. Re-read the generated unit manifest at the audited SHA and current execution SHA, retain the counts and native/E2E obligations, and record every coverage change.

### D. Input provenance, in precedence order

Within each edition the specification, evidence and prompt were reconciled together; the final user instruction resolves remaining conflicts. The bootstrap audit was read before its specification and prompt. SHA-256 values below identify the exact supplied bytes and were re-verified against the consolidation inputs when this plan was committed.

| Source | File | SHA-256 |
| --- | --- | --- |
| S1 | `bastion-three-runner-spec.md` | `6280fc883c3d297a0f49366f9342acf1b395107c800357dcc6b8efa67eddb3a7` |
| S2 | `repository-and-upstream-evidence.md` | `d83a1494cfe3e20e11086e9de79a42501923d20b18124f22a18a704374b3e2fc` |
| S3 | `setup-three-runners-goal.md` | `015f89f26ddf9b5cd75981e282fd7bd4783bcd47652ee90ff434f98adc0ee109` |
| S4 | `bastion-three-runner-spec-v2.md` | `c538f2dbb17ad159310d8f6ab824355251968e2413b922212d31c204c2a17d2d` |
| S5 | `repository-and-upstream-evidence-v2.md` | `92ad2eb9bfb5349b360bd1e024a7b7aa35a66ebec19291967f5c4d7e860ffae5` |
| S6 | `setup-three-runners-goal-v2.md` | `b4882398c64fec8bf5ec8835c11afd75572db892a3f19d4141787804e4437b38` |
| S7 | `velnor-ci-cd-audit-2026-09-17.md` | `a6e99e93011ebecca1ea6618e55fe939d4cee4bcd688474ec2442367fcbd756e` |
| S8 | `velnor-first-bootstrap-spec.md` | `e9b50856179b04e7527f354e83c7aef12d4ea20d37219546d98c1bb13953c4b6` |
| S9 | `velnor-first-bootstrap-goal.md` | `a09988e7c30f7e70be3d0bb115556ae12ca2e0277b6cd3225119800fb95aca45` |

### Evidence locator

- Baseline SHAs/counts: S1 section 2; S2 repository sections; S5 source baseline and repository findings.
- Jackin native failures, E2E exclusions, same-source capsule, nested DinD, relay paths and 20 capsules: S2 Jackin sections “Existing failures” and “Real Docker acceptance requirements.”
- ChainArgos policy failures, PostgreSQL/Flyway/jOOQ, CI nextest/RustFS, Docker context and 12 bake targets, missing local action: S2 ChainArgos sections “Observed current failure,” “Docker and database correctness requirements,” and “Cache, reports, and side effects.”
- Native Velnor mediated Docker lease versus DinD, cgroup prerequisites, credential refresh gap, package lock and activation: S2 Velnor sections “Existing Docker behavior,” “Topology feasibility and credential gap,” and “Packaging and operator procedure.” These facts survive even though the VM deployment advice in the same source does not.
- Exact Scale Set acquisition, statistics, replay, assignment mapping, namespace/path/externals requirements: S2 “Verified scale-set design research.”
- No VMs, native backend reuse, no quotas, rollout order: S4/S6, superseded only where S8/S9 move scale-set control into Velnor and combine capacity.
- APT capability gap, Policy drift, PR #904 publication race, source version versus release: S7; expanded in S8 sections 3–6.
- Final Rust dual mode/global N/publish-then-pin/APT contract: S8/S9, with current user instruction overriding their compatibility text and extending execution beyond Velnor.

### Focused live checks in this consolidation

- [Velnor main ref](https://api.github.com/repos/tailrocks/velnor/git/ref/heads/main): returned `3353310c7648fca22698b6c0f4a69ab245127786`.
- [PR #904](https://github.com/tailrocks/velnor/pull/904): returned open, head `62a74bf58993c8c073d47274897f0db88cbd6159`. Its description's green claims are not independently rerun evidence.
- [APT omission declaration](https://github.com/tailrocks/velnor-apt/blob/main/.github-gen/NO_WORKFLOWS_REQUIRED.md): returned blob `3172bb883ec343a676d82c2594cb1a399191bf07`, still declaring omitted APT workflows.
- [Scale Set main ref](https://api.github.com/repos/actions/scaleset/git/ref/heads/main): returned `fb56300503fd21caa788feeb85c63071d15155c6`.

### Official technical cross-checks

- [Docker bind mounts](https://docs.docker.com/engine/storage/bind-mounts/): paths are evaluated on the daemon host, not the client.
- [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/): unconstrained defaults and memory-exhaustion implications.
- [Debian trixie apt-secure](https://manpages.debian.org/trixie/apt/apt-secure.8.en.html): signed repository metadata/checksum trust chain.
- [Debian trixie sources.list](https://manpages.debian.org/trixie/sources.list%285%29): repository-scoped Signed-By key selection.
- [actions/scaleset](https://github.com/actions/scaleset): statistics-based demand and homogeneous ephemeral runners; use the pinned source contract for exact API behavior.

Published documentation and source can disagree, including scale-set label support. Use a dedicated unambiguous set selector and live conformance against the chosen API; do not propagate the older blanket “one label only” rule. No additional controller or ARC deployment is selected by these references.
