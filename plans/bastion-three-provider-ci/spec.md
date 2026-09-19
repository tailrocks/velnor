# Bastion target-state specification: Velnor-controlled, Docker-native three-provider CI

Status: **authoritative target state** — branch `docs/bastion-final-plan` (PR `tailrocks/velnor#912`).
Date: 2026-09-17.
Repository: `tailrocks/velnor`.
Target: `root@37.27.110.241` (bastion), Debian 13, AMD EPYC 9454P, 48 physical cores / 96 logical CPUs, approximately 128 GB RAM (operator-reported 128494 MiB ≈ 125.48 GiB; re-verify by read-only host inspection at execution).

Rollout order: `tailrocks/velnor` → `jackin-project/jackin` → `ChainArgos/java-monorepo` → repeatable generic onboarding. `tailrocks/velnor-apt` (signed feed at `https://velnor-apt.tailrocks.com/`) is a Velnor delivery dependency, not a fourth parallel application migration. Each repository migrates only after its predecessor is independently qualified.

Documents in this campaign:

- `plans/bastion-three-provider-ci/spec.md` — this specification: target state only (§1–§9).
- `plans/bastion-three-provider-ci/work-plan.md` — ordered implementation steps A0–G2.
- `plans/bastion-three-provider-ci/checklist.md` — acceptance checklist, 1:1 with work-plan step IDs.
- `plans/bastion-three-provider-ci/evidence.md` — retained audit facts as revalidation inputs and known issues.
- `goal.md` (this directory) — paste-ready `/goal` execution prompt.

This specification governs implementation. Historical audit facts live in the evidence document; they are regression and discovery inputs, never deployment pins and never proof that a failure still exists.

---

## 1. Architecture and product boundaries

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

There is one local product and one authority over job capacity, not necessarily one OS process. Useful guardian/controller/job-process isolation inside Velnor is preserved. There is no independently operated controller stack, no Go controller runtime component, no Kubernetes, no VM, and no per-repository daemon.

The official lane runs the unmodified official Actions runner; Velnor does not interpret its job steps. The native lane is Velnor's own interpreter/executor with its existing Docker backend. Management may share Docker client code, credential refresh, journaling, and cleanup primitives, but protocol-specific sessions and runner identities stay distinct.

Supported local operations are packaged product behavior, not ad hoc host scripts that would become an alternate runner implementation. Old control models may be refactored away freely; native execution semantics are preserved.

## 2. Explicit providers, capabilities, and result identity

Canonical provider IDs are exactly:

```text
github-hosted
github-self-hosted
velnor
```

One provider-set schema is implemented across configuration, scanning, IR, plans, validation, `runs-on`, bootstrap, caching, artifacts, policy, dispatch, UI names, and aggregation. There are no ambiguous aliases, no deprecation branches, and no implicit provider inference from `runner.environment` or label substrings.

Illustrative final configuration contract — to implement and validate, not claimed current syntax:

```toml
[workflow]
providers = ["github-hosted", "github-self-hosted", "velnor"]
automatic_providers = ["github-hosted", "github-self-hosted", "velnor"]
default_dispatch_providers = ["github-hosted", "github-self-hosted", "velnor"]
```

Platform, trust, and capabilities are separate typed fields. Capabilities include Docker, nested privileged Docker, Buildx/Compose, Testcontainers, services with readiness, browser binaries, and native macOS/arm64. They do not carry CPU/RAM resource classes. Unknown selectors and unsupported capabilities fail explicitly; they never default to hosted and never disappear silently.

One affected/full planner runs, then each selected eligible Linux unit fans out to all three providers with the same source, command/profile/features, fixtures, and test expectations. Display names expose the provider, for example `Rust · velnor-runner / github-self-hosted / bastion`. Official and native engines use disjoint dedicated local selectors. A selector is routing, not authorization and not a three-way fanout instruction.

Result identity is defined as:

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

The expected result set is fixed before execution. An expected missing, skipped, cancelled, timed-out, failed, duplicate-conflicting, or identity-mismatched result cannot pass. Platform/trust exclusions and genuinely unaffected units are declared by the trusted planner before expansion, not reclassified after failure. Diagnostic subsets are visible reduced coverage, never successful full qualification.

Each provider actually executes its assigned verification. A hosted success or artifact cannot certify a local lane. Legitimate compiler cache reuse is preserved; another lane's test report is never substituted. Matrix fail-fast is disabled for qualification.

Planning, authoritative policy, recovery, and required-result monitoring stay hosted. External mutations — signing, package/registry publication, release/tag creation, deployments, Renovate writes, issues, destructive cache maintenance — have one writer. Actual required external contexts are retained, including DCO; disabled application releases stay disabled.

## 3. Workflow generation and immutable runtime products

### 3.1 Sole generator

`velnor-workflow` owns every `.github/workflows` file in every repository this campaign changes, including runtime-product publishers, source releases, APT publication, control/watchdog jobs, and maintenance. It also owns or explicitly validates referenced local actions and generated manifests.

No hand YAML, copied historical workflow bodies, hidden repository-local templates, arbitrary command strings, raw-YAML escapes, or repository-name special cases. Audited canonical repository executables are reused only through defined typed capability contracts. A typed APT hook is not permission to add a general shell escape hatch.

Required mechanical checks: exact regeneration, ownership inventory including unexpected files, local-reference resolution, structured policy, actionlint, provider expansion, native-platform routing, and fail-closed aggregation. Fixture repositories are renamed to verify unchanged semantics and detect name-based special cases.

Each repository migrates directly to the final schema at its gate. Repositories awaiting rollout may continue using their previous immutable generator release; that is a staged deployment, not a compatibility parser in the new product. One-time configuration/state migration is explicit; two runtime models are never retained.

### 3.2 Publish-then-pin without self-reference

Let `R` be the already published trusted runtime and `C` a candidate generator source closure.

1. Normal planning/policy and committed generated files stay bound to `R` while `C` is developed.
2. `C` is built/tested once per full source closure/platform. Candidate output is checked in disposable test trees; it is not a new trusted pin.
3. The closure includes every build-affecting local dependency, build script/input, locked dependency, compiler/target/features/profile, and relevant configuration. A hash of one crate is never assumed to cover a growing workspace.
4. Reviewed source is merged and the approved `C` product is published/attested through a generated trusted hosted producer. A candidate binary is reused only when source and build provenance are exact; a changed merge closure requires one new build.
5. All required platform assets and trusted attestations exist and verify before the product is exposed as consumable.
6. A follow-up promotion commit atomically updates pin/product metadata and the entire generated tree using that verified product.

Changing generator source does not force committed workflows to be rendered by unpublished source. Final pin/tree consistency is mandatory.

Consumers verify producer repository, full closure, source identity, OS/architecture, features/profile, manifest and binary digests, trusted signer workflow/ref, and binary self-report. A cache hit does not waive integrity checks; a PR-provided checksum does not establish trust. The producing repository and the consuming repository are different identities.

A missing product is a precise producer defect, not a signal to retry forever or to compile in the consumer. Normal Planning/Policy/unit bootstrap has no cargo/source-build fallback. The daemon package is never selected through an unqualified `releases/latest` response: runtime products and daemon releases are separate release identities.

The initial generated publisher may be bootstrapped by a reviewed, controlled one-time maintainer build of the generator. That is an explicit tooling producer, not a production binary installation, not an ordinary CI fallback, and not a handwritten workflow. No PR-controlled code runs in privileged policy/publication contexts.

## 4. Global capacity, queueing, and unbounded execution

### 4.1 One host-wide permit ledger

Both local modes share exactly one `max_jobs=N`; GitHub-hosted jobs do not consume it. There is no fixed repository, organization, or engine reservation, priority, weight, or separate hardware pool.

A permit counts one top-level job lifecycle, including its runner, DinD, and nested service/build/test containers. Jackin's nested 20-capsule test is one top-level job; its descendants still require ownership and cleanup.

The permit is held from the point capacity is committed for acquisition/assignable readiness until terminal work and owned cleanup are confirmed. Reserved, acquiring, provisioning, idle-assignable, running, cleaning, and uncertain states are all included in the single count. Offered but unacquired work is queued demand, not occupied capacity. Capacity cannot exceed N after crash recovery or duplicate delivery. A cleanup failure retains a visible reservation rather than releasing fictitious capacity.

Every native and Scale Set acquisition/readiness path uses this authority. Pre-registered native idle slots must not reserve all N indefinitely while the official lane waits. Demand is observed separately; assignable native capacity is created/enabled just in time using the existing native protocol with admission integrated around it. Where its current signals cannot support this, a narrow read-only demand adapter is implemented and verified — not a replacement executor and not a second scheduler.

There is no permanently reserved warm-idle inventory at start; images are pre-pulled instead. Registration/session metadata is not itself a compute pool. A future idle runner capable of taking a job is counted and must not bypass FIFO/capacity. Generation-fenced grants are persisted and stale mutations are rejected. State is reconciled before capacity is advertised after restart; occupied work is never erased by resetting a semaphore.

### 4.2 Oldest observed eligible work first

Eligible demand is ordered by durable `first_seen_at` plus an immutable sequence/tie-breaker across all local modes/scopes. Redelivery retains original age. A younger job is never granted before an older eligible job when Velnor controls that choice. Ineligible/cancelled/blocked work carries an explicit reason; an unavailable scope need not block unrelated eligible work.

This governs admission, not completion or actual start order. GitHub controls delivery and runner assignment; observed order is not global GitHub submission order. Native assignments, upstream redelivery, and assignment to any eligible runner can differ from local observation. Strict end-to-end FIFO is never promised, and no particular acquired job is promised a particular JIT runner.

### 4.3 No CPU/RAM ceilings

The final deployment applies no Docker `NanoCpus`/CPU quota/cpuset restriction, memory/reservation/swap ceiling, systemd `CPUQuota`, `MemoryMax`, or `MemoryHigh` on the workload ancestry. No slot-divided `CARGO_BUILD_JOBS`, MBX/BuildKit entitlement, Gradle worker count, or artificial heap ceiling is injected as a disguised resource partition.

Package-created quota drop-ins and preflight assumptions that require them are removed; upgrades must not recreate them. Configuration, runtime, BuildKit/service handling, package hooks, and tests are refactored together. The old budget model is not kept for backward compatibility. Cgroups remain useful for identity, process ownership, observation, and cleanup without ceilings. No per-job disk/PID quotas are introduced.

Quota-free execution is proven for native jobs, official runners, DinD, and representative nested descendants through Docker HostConfig, effective cgroup ancestry (`cpu.max`, `memory.max`, `memory.high`, swap/cpuset settings), package units/drop-ins, and effective build environment. Inherited limits are inspected, not only emitted Docker flags.

Correctness-required test serialization is kept: Jackin's Docker E2E group and ChainArgos's RustFS group are test semantics, not a CPU partition. Performance settings are revisited through tests rather than by overriding every intentional serial group.

Unlimited resource entitlement does not imply infinite memory or guaranteed host isolation. No per-job OOM containment on this shared kernel is claimed. OOM/pressure is reported, diagnostics are preserved, and N is tuned; quotas are never silently restored and OOM handling is never disabled.

### 4.4 Tune N for completed work

A representative full and mixed workload is benchmarked, initially testing values such as 16, 24, 32, 48, 64 and higher only while stable useful throughput improves. These are experiments, not five simultaneous pools or promised capacity. A measured native-only provisional N comes before Scale Set, then mixed modes are remeasured and N is requalified after Jackin/ChainArgos.

Source, logical plan, image/toolchain identity, and cold/warm conditions stay comparable. Measured: jobs/minute, end-to-end time-to-green, queue and provisioning delay, compile/test/cache/cleanup phases, CPU, available memory/pressure/OOM, disk/inodes/IO latency, Docker/BuildKit/cache-lock contention, and failures. Benchmarks must not intentionally destabilize unrelated jobs. Only host-level N changes; containers remain unbounded. The highest stable useful throughput wins, not the largest integer or CPU utilization alone. Never present all-green parity across different hosted hardware as a fair performance comparison; compare same-host before/after only.

## 5. Rust Scale Set adapter and official-runner lifecycle

### 5.1 Protocol contract

An audited upstream revision is pinned and implemented in Rust: scope/group/set registration and reconciliation, App credential/token refresh, session creation/renewal, long polling, `lastMessageID`, capacity reporting, initial statistics, empty poll responses, `JobAvailable`, explicit `AcquireJobs`, JIT config, start/completion events, acknowledgement, and shutdown/recovery.

Recorded/sanitized protocol fixtures and real upstream conformance tests are required, not only mocks that reproduce the implementation's assumptions.

Processing order:

1. Apply lifecycle/completion observations idempotently and reconcile durable occupied state.
2. Submit offered eligible work to the global oldest-observed queue; validate trust before grant.
3. Reserve a global permit and persist acquisition intent before `AcquireJobs`.
4. Record the actual returned acquired request IDs. Partial or uncertain responses are reconciled; never assume every requested ID succeeded or spend a permit twice.
5. Persist JIT/provisioning intent and make runner creation idempotent through stable operation/ownership IDs.
6. Acknowledge only after replay-safe effects are durable. Acknowledgement need not wait for job completion. Offer validity/session boundaries stay explicit: never acknowledge away the only reference to deferred work without proving later acquisition/re-offer semantics.
7. Use authoritative `Statistics.TotalAssignedJobs` for population convergence, including waiting and running assignments. Never count capped message batches, double-count statistics plus reservations, or treat local tickets as GitHub assignment truth.
8. Derive each session's capacity advertisement from shared grants and existing commitments. Never let every listener independently spend N; upstream total-capacity versus free-capacity semantics and races are verified.
9. Reconcile on idle polls too, with backoff/rate-limit handling and bounded calls. Unknown events after restart trigger reconciliation, not panic.

A database transaction cannot atomically include GitHub acquisition and Docker creation. Durable intent, idempotency, generation fencing, and reconciliation close those crash windows. No local duplicate execution from a replayed assignment is promised; exactly-once external side effects across GitHub retries are not claimed.

### 5.2 Homogeneous official workers

Each official scale-set worker in the initial trusted Linux profile gets a pinned official Actions runner image and a private pinned DinD daemon. One homogeneous Docker-capable profile is used rather than guessing a job's runner requirements from an acquired request ID. GitHub may assign any eligible pending job to any idle runner in that set. Explicit typed authorization still controls privileged nested workloads.

Official runner, DinD, and local job/toolchain images are pinned by validated digest. `latest` is never deployed. Runner version is recorded separately from Velnor/package/protocol reference. The official image's actual tool content is verified; it is not assumed to contain the hosted image's full catalog. Image updates go through a tested generated product path, respecting current runner update requirements.

Lifecycle:

```text
observed → eligible → reserved → acquire-intent → acquired/uncertain
→ provision-intent → DinD ready → runner connected → running
→ terminal → diagnostic export → owned cleanup → permit released
```

Each boundary is reconciled, including success, error, cancellation, lost acknowledgement, runner death, daemon death, and management restart. A scale set is never deleted on a normal service restart; decommission is explicit. Active work is preserved across ordinary controller restart where supported, otherwise explicitly failed with cleanup — never a lost job or false success.

### 5.3 Private Docker semantics

Bastion's raw `/var/run/docker.sock`, an alternate unrestricted host socket, or an unrestricted host Docker proxy is never mounted into any job. The physical management socket belongs only to Velnor management. A job-private DinD socket may appear at `/var/run/docker.sock` inside the job; its actual source/daemon identity is proven.

A private Unix socket is used, with no public Docker TCP API. Runner and DinD share network namespace so localhost published ports behave as expected, while inner container-job services retain their own bridge/DNS semantics. Merely putting two outer containers on the same bridge does not share loopback.

Bind sources are visible at identical absolute paths to the runner and its daemon: checkout/workspace, `_work`, shared `TMPDIR`, necessary HOME/capsule paths, action externals, tool cache, and file-command directories. Ownership matches. The host root or broad credential directories are never exposed for convenience. Docker evaluates bind sources on the daemon's filesystem, so client-only paths are insufficient.

Every runner, DinD data directory/socket, network, volume, workspace, and nested job object gets a recorded ownership identity. Logs are exported before ephemeral resources are removed. Two official jobs must be able to use the same inner names/ports without colliding. Native Velnor achieves required isolation through its existing mediation/namespace semantics, not a newly imposed DinD rewrite.

### 5.4 Native Velnor boundary

Native Docker spawning, action semantics, job-scoped API lease proxy, object tracking, streams/cancellation, and safe retained caches are kept. Its mediated host-backed Docker API is not private DinD and must not be described as such. Its protection against cross-job Docker objects is retained; it is never replaced with a raw host socket.

Only control-plane integration, admission, credential renewal, unbounded policy, and concrete conformance bug fixes modify this path. Native jobs are never switched to the official runner and no parallel execution backend is built.

## 6. Trust, credentials, host setup, and caches

Bastion is a trusted-tier shared-kernel CI host, not an adversarial multi-tenant sandbox. Privileged DinD increases host risk even without a raw management socket. No VM isolation or independent-local-failure-domain claim is made.

Trust is enforced outside PR-editable YAML: controller-side event/source/ref checks and verified runner-group/workflow restrictions where available. Same-repository origin or a requested label is not sufficient. Forks, bot PRs, same-repo PRs, main, schedules, dispatch, tags, and merge-group are covered explicitly. Label spoofing and reusable-workflow input substitution are tested. Default untrusted fork execution stays hosted, never privileged on bastion. Untrusted checkout is never executed under privileged `pull_request_target`.

Protected Velnor management credential providers are used, preferably GitHub Apps with per-installation scope. App keys, administration tokens, signing keys, host SSH keys, and telemetry-write credentials stay out of jobs. Tokens refresh in the running provider, not by overwriting an EnvironmentFile and assuming process environment changes. Only required ephemeral runner/session material reaches official runners; native jobs receive their intended job credentials, not management authority. Publisher credentials stay separate and hosted. Authentication scopes do not allocate compute.

Host provisioning is idempotent and non-destructive. OS/CPU/NUMA, RAM, disks/signatures, mounts, inodes, Docker/cgroup driver, systemd units, running jobs, sockets, routes/firewall, and existing state are read first. SSH and recovery access are preserved. The second NVMe is left untouched; no automatic formatting, RAID, or repartition. RAM-backed `/tmp` is never used as if it were extra disk/RAM.

Docker and required Buildx/Compose tooling are installed through an approved pinned package setup; cgroup v2/systemd driver is verified against native preflight. No libvirt/KVM/QEMU. Velnor configuration lives under `/etc/velnor`, durable state under `/var/lib/velnor`, runtime sockets/locks under `/run/velnor`, deliberate cache storage under `/var/cache/velnor`, and owned scratch on the existing filesystem. These are managed product paths, not repository-specific scripts.

Inner service ports and control APIs stay private. Required outbound GitHub/registry/package endpoints are permitted without assuming a frozen three-host allowlist. No public webhook receiver is required for the Scale Set long-poll design. The host public route is never altered merely to add container networks.

Workspaces stay fresh and official daemon state stays private, with explicit durable caches. Compiled artifacts are namespaced by repository numeric ID, trust, provider, platform, image/toolchain/ABI, options, and dependency/source compatibility. No simultaneously writable shared Cargo target, Gradle home, or official DinD data directory. Native Velnor's proven lease-aware stores are preserved; reusable content is distinguished from unsafe mutable working trees. Official DinD BuildKit cache is exported/imported before deletion. Untrusted inputs cannot publish trusted executable caches.

No host-wide `docker system prune`. Only owned inactive resources are reclaimed; active leases and state are protected. Disk/inode pressure is observed and intentional cache retention is applied without per-job quotas. Normal parallel main runs are preserved; PR supersession may cancel only the same PR's older attempt, never sibling providers or unrelated branches.

### 6.1 Server-setup reference paths in java-monorepo ansible-configs

Bastion host provisioning follows the proven `ChainArgos/java-monorepo` Ansible patterns below. Pin each path to the audited ChainArgos source SHA at execution start, re-resolve against live `main`, and record any drift as `old observation → new evidence → consequence`.

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

Reference permalink root: `https://github.com/ChainArgos/java-monorepo/tree/<audited-sha>/ansible-configs` (audited SHA recorded in the evidence document). These playbooks are the starting pattern for idempotent, non-destructive bastion provisioning; bastion-specific Velnor packaging, APT repository wiring, and systemd units are delivered through the Velnor package and the generated workflows in this campaign, not by forking these playbooks into an alternate runner implementation.

## 7. APT delivery pipeline and package operation

The production source is the signed repository documented by `tailrocks/velnor-apt` at `https://velnor-apt.tailrocks.com/`, not a release download installed locally. The final package carries the reviewed Velnor management/native/Scale Set code and the required operational binaries/units. No untracked binary replacement or separately deployed controller.

Generic typed APT capabilities are implemented: source repository and exact release identity, package, architecture set, stable/preview suites, signer fingerprint and secret references, release-coherence verification, repository assembly, publication records, previous-version retention, GitHub Pages artifact/deployment, and channel-update tasks where actually required. Suite behavior is preserved through one final implementation rather than compatibility wrappers. `apt-repository` is never a descriptive label producing only docs.

`scripts/verify-release.sh` is audited and reused through a narrowly defined typed verifier contract; reusable logic is generalized where appropriate. Configuration never executes arbitrary shell/YAML. Generic behavior is proven with a renamed fixture package/repository. All APT CI/publication/required-result workflows are generated, replacing the omission notice and stale direct-install documentation.

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

Complete existing release contracts are kept, including native arm64 payload generation, unless an explicit correctly versioned product change removes an actual dependency. Building/staging a kernel/rootfs is not running a VM. Genuine KVM-runtime tests stay a declared separately qualified platform capability; VMs are never provisioned on bastion and a Docker substitute is never counted as passing them.

Source release and APT publication run through generated hosted recovery/publisher paths so broken local Velnor cannot block the package needed to repair it. Verification may run multiple ways; signing/deployment has one owner. Mutation of the same feed/tree is serialized, not all repository CI. An older publication must not roll back a newer candidate unintentionally.

Before install, the public signing-key fingerprint is authenticated against a separately trusted project reference; then repository-scoped `Signed-By` is configured. The key from the download URL is never blindly trusted. Signed metadata, candidate version, architecture, package hash, source/record, and repository origin are verified. APT's metadata signature/checksum chain and a separate artifact attestation are distinct checks.

The package lock and release activation contract is retained: `/run/velnor/package-transaction.lock`, compatible package/manifest/binary record, and `release verify-installed` before starting services. Affected jobs are drained, config/secrets are preserved, and package installation is never assumed to start the fleet.

There is no unattended bastion package updater in this repository. After C1,
`velnor-runner` is held so ordinary `apt upgrade` does not float it to another
candidate. An operator updates it by exact version through the hold-aware
transaction below; hold detection, unhold, APT install, and re-hold stay under
one exclusive package lock.

```sh
# VERSION is the independently verified candidate from the signed repository.
: "${VERSION:?Set the verified exact Debian package version}"
export VERSION
install -d -m 0750 /run/velnor
apt-get update
apt-cache policy velnor-runner
/usr/bin/flock --exclusive --nonblock --no-fork /run/velnor/package-transaction.lock /bin/bash -euo pipefail -c '
  holds=$(apt-mark showhold)
  was_held=0
  if printf "%s\n" "$holds" | grep -qx velnor-runner; then
    was_held=1
  fi
  rehold_runner() {
    rc=$?
    trap - EXIT
    set +e
    status=$(dpkg-query -W -f="\${Status}" velnor-runner 2>/dev/null)
    if [ "$was_held" = 1 ] || printf "%s\n" "$status" | grep -Eq " (installed|unpacked|half-configured|half-installed)$"; then
      apt-mark hold velnor-runner || rc=1
    fi
    exit "$rc"
  }
  trap rehold_runner EXIT
  if [ "$was_held" = 1 ]; then
    apt-mark unhold velnor-runner
  fi
  apt-get install "velnor-runner=${VERSION}"
  apt-mark hold velnor-runner
  trap - EXIT
'
dpkg-query -W velnor-runner
```

Activation, drain, and health commands are derived from the implemented package/help and verified. Working `velnorctl` mutation verbs are never invented. Production is never deployed via `dpkg -i`, `apt install ./file.deb`, a copied executable, disabled signature verification, or altered packaged files.

A bootstrap package is followed by a Scale Set package. Unbounded policy/global native admission is built early enough that the bootstrap package can activate it before the first bastion job. This is a source dependency running in parallel, not permission to run bounded jobs until later. Subsequent feature fixes also arrive through APT.

Breaking schema changes never justify compatibility shims. A previous coherent package with its matching config/state recovery snapshot is retained only when restoration is proven; a package downgrade alone is never claimed to read a newer state schema. Otherwise the tested forward-recovery path is documented. All package recovery stays APT-only.

## 8. Hosted result verifier and fault contract

One generated hosted authority reports the required result for each run/attempt, preserving `ci-required` where required. It excludes itself and telemetry from the workload set. It starts after planning without waiting in `needs` for every local job; otherwise queued work can prevent outage detection.

Authenticated outbound Velnor health records are provided, bound to repository/source/run/attempt, provider, sequence, freshness, occupied permits, and provisioning progress. A separate least-privilege telemetry writer is not a new runner controller. The hosted verifier validates identity and freshness; missing data means unavailable, not ordinary backlog. A hostname printed by a job is not placement evidence.

GitHub runner/job metadata is correlated with management provisioning IDs, actual official/native engine versions, image digests, selected tests and JUnit counts, cache/timing reports, and cleanup receipts. Artifacts are preserved outside disposable containers. All API pages are enumerated and run attempts are distinguished. A later reporter or cancellation must not overwrite an already failed required result with success; reruns revalidate exact identity and outcome provenance.

Initial measurable operating targets, not claims of current performance: reserve-to-connected within 180 seconds; owned cleanup within 120 seconds; free-capacity provisioning stall diagnosed within five minutes; full local outage reflected as failed/incomplete within ten minutes. Explicit execution/full-run deadlines are tuned against cold baselines. Legitimate FIFO backlog and unmet workflow dependencies do not equal a provisioning stall. Qualification waves persist when required by hosted job lifetimes or matrix limits; units are never omitted to fit one run.

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

Faults run against identified canaries and test fixtures. Shared Docker restart/host reboot uses an isolated coordinated window preserving unrelated work and SSH. No destructive disk operations, broad pruning, deliberate whole-host OOM, or unrelated network outage is implied by "break fast."

## 9. Repository-specific obligations after the Velnor gate

### 9.1 Jackin, second

The 40-unit historical baseline is preserved or a coverage-equivalent correction is recorded. Its baseline has 38 Linux-oriented units, hence 114 three-provider executions plus two genuine hosted macOS units, before explicit additional E2E/control jobs. 120 Linux executions are never counted by pretending Swift runs on Linux.

Generic native routing to actual Apple Silicon macOS is fixed. Same-job local XCFramework production followed by Swift build/test is preserved. The obsolete desktop `release.yml` assertion is reconciled with generator-owned disabled/enabled release policy; legacy YAML is not restored and releases are not enabled merely to pass it.

`docker-e2e` is explicitly executed with `e2e` enabled. The capsule Linux ELF is built from the tested source and `JACKIN_CAPSULE_BIN` is set; no preview-release substitute. Docker, Buildx, Compose where used, `script(1)` PTY support, nested privileged DinD, Java Testcontainers, TLS/no-proxy behavior, and temporary relay socket/file mounts are proven. Serial E2E groups are preserved; the 20-capsule fixture fanout is exercised. No fixture weakening to make a lane green.

Historic tool declarations include Rust 1.97.1, Bun 1.3.14, Node 24.18.0, configured Ubuntu 26.04 and macOS 26. Actual compatible tool/image versions are re-resolved; a different Linux userspace is never silently forced and Apple-only mise tools are never installed on Linux.

Only authorization, generic scope entries where needed, and typed generator configuration are added. If a generic bug is discovered, the generator or Velnor package is fixed/published, Velnor is requalified for that change, then Jackin continues. No per-Jackin host architecture.

### 9.2 ChainArgos, third

71 historical units are preserved or a reviewed coverage-equivalent correction: 213 three-provider executions before additional controls. Policy ownership/trust failures and missing generated local actions are repaired first.

Disposable job-local PostgreSQL is provided for Flyway/jOOQ and tests. Sixteen Gradle units historically ran `flywayMigrate jooqCodegen`; the actual Gradle graph is inspected to remove duplicate root/module execution only with equal task/test coverage. Both tools use the same job database through actual build-supported variables (including `POSTGRESQL_DB_HOST/PORT` where declared). Production or shared default databases are never targeted.

Rust Testcontainers PostgreSQL, RabbitMQ, Redis, and RustFS suites run; the intended nextest CI profile is explicitly selected, the one-at-a-time RustFS group is retained, and the effective configuration is independently verified. Historical RustFS image is `rustfs/rustfs:1.0.0-beta.8`; PostgreSQL fixture includes `postgres:18-alpine`. Tested digests are resolved and locked rather than assuming those tags are immutable. Fixture acceptance is separated from live Ethereum/Base/blockchain RPC suites and actual external failures are labeled.

The generic Rust Docker context is fixed to include root workspace inputs, `backend/`, and `scripts/`. The historic bake contract specifies root context and 12 service targets; building only the Dockerfile's final stage is insufficient. Root Compose is never run unchanged with fixed global names/ports/home mounts through a shared host namespace.

GraalVM Java 25/native-image (historical oracle-graalvm-25.0.3), Gradle wrapper 9.5.1, Rust 1.98.1, required native/protobuf tools, Node 24.20.0, Bun 1.3.14, and declared browser requirements are preserved unless a separately tested source change updates them. Playwright/browser gates are audited; standard frontend unit success alone is not browser coverage. The historical Micronaut Docker API `1.44` override is checked against the selected daemon.

Mutation single-writer and application release-disabled policy stay unchanged. Added access/scope configuration reuses the same Velnor controller and global N.

### 9.3 Generic onboarding

After qualification, onboarding is: verify trust/access → authorize repository and add a declarative GitHub scope only if necessary → add typed config → select an already published generator pin → regenerate/check → run full qualification → record health inventory. No new daemon, VM, hardware pool, custom host script, or copied workflow. A new GitHub organization requires authorization/registration metadata, not reserved resources.
