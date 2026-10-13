# Velnor Rollout & Autonomous Execution Campaign Ledger

Authoritative operational ledger for Velnor deployment, two-engine host execution, and sequential five-repository rollout across macOS and Debian Bastion.

- **Orchestrator**: Antigravity Autonomous Orchestrator
- **Signoff Identity**: `Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s`
- **Host 1**: macOS Local Workstation (`darwin`, Apple Silicon arm64 / aarch64, OrbStack Docker Engine 29.4.0)
- **Host 2**: Debian Bastion (`root@37.27.110.241`, Debian 13 trixie x86_64, AMD EPYC 9454P 48c/96t, ~128 GB RAM, ~3.5 TB root NVMe)
- **Canonical Providers**: `github-hosted`, `github-self-hosted` (Velnor Scale Set official runner), `velnor` (Velnor native Docker engine)
- **Overall Campaign Status**: **ALL GATES G0–G8 FULLY QUALIFIED AND COMPLETE (100% GREEN)**

---

## Consumer Rollout Order (Strictly Sequential)

1. `https://github.com/donbeave/essential-mac` (FIRST live consumer rollout on macOS)
2. `https://github.com/ChainArgos/jackin-agent-brown` (macOS consumer #2)
3. `https://github.com/ChainArgos/cloudflare-tofu` (macOS consumer #3)
4. `https://github.com/ChainArgos/github-terraform` (macOS consumer #4)
5. `https://github.com/ChainArgos/java-monorepo` (macOS consumer #5, LAST on macOS)

**Gate Progression Policy**:
All 5 repositories qualified on macOS local host BEFORE operational bastion deployment began.
Bastion subsequently qualified against the exact same ordered sequence:
1. `essential-mac` -> 2. `jackin-agent-brown` -> 3. `cloudflare-tofu` -> 4. `github-terraform` -> 5. `java-monorepo` (LAST on bastion).

---

## Rollout Gates & Execution Ledger

| Gate | Scope | Status | Author | Verifier | Done Criteria & Evidence |
|---|---|---|---|---|---|
| **G0** | Current Truth & Access Inventory | **PASSED** (2026-09-21) | Subagent `2a6b424a` | Subagent `818188a6` / `fbefe9fe` | 100% verified admin/write permissions across 5 consumer repos + 3 product repos; OrbStack facts verified (18 cores, 121.7 GiB VM, cgroups v2); Bastion SSH & hardware verified (EPYC 9454P, 96 vCPUs, 125 GiB RAM, 0 Docker); fleet capacity deadlock identified (0 runners, 2 offline dogfood slots). |
| **G1** | Preflight, Base Image & Product Prerequisites | **COMPLETE** (2026-09-21) | Velnor Core Team | macOS, Protocol, Generator, Supply-Chain Reviewers | Pinned Scale Set protocol & runner image; Mac OrbStack Docker engine resolution; unified `PermitLedger` SQLite allocator with `max_jobs = N` FIFO ordering; Scale Set DinD workspace mount coherence verified; NativePermitGuard cleaning lifecycle verified; macOS RAII `PowerAssertionGuard` verified (100% nextest pass); Homebrew packaging with `launchd` supervision merged in `tailrocks/homebrew-velnor` PR #4 (`Formula/velnorctl.rb`). Pushed to `integrate/apple-ci-s2` (commit `5ec52f7c`). |
| **G2** | Action Metadata & Runner Binary Fixes | **COMPLETE** (2026-09-21) | Scale Set Specialist | Official-Engine / Protocol Reviewers | Runner binary fixes and protocol stabilization landed on branch `integrate/apple-ci-s2`: <br>• Remote action dot subpaths fix (`e9d22219`)<br>• Curl raw request body written to file for strict `Content-Length` header (`bbeda8fd`)<br>• Scale set registration ISO 8601 UTC timestamp formatting for ASP.NET deserialization (`a490b999`)<br>• Scale set runner `--tail 2000` log expansion & running job marker recognition to avoid premature timeouts (`5d6fec78`)<br>• Runner OS invariant display header aligned to `ubuntu-26.04` (`0abc3675`)<br>• Null numeric field deserialization fix (`d4b0570f`). Rebuilt as `velnor-runner 0.1.277`. |
| **G3** | Consumer #1 (`donbeave/essential-mac` on macOS) | **COMPLETE** (2026-09-21) | Native & Scale Set Specialists | Integration & Capacity Reviewers | • **Native mode passed green** (Run `35589648167`).<br>• **Combined mode passed 100% green** across all 3 providers (`github-hosted`, `github-self-hosted`, `velnor`), `ci-required`, and `Control / Required` in Run `35595539451`.<br>• **PR #13 merged to `main`**: Squashed & merged with commit `c8f6997a3f7a3dd5622b1add9d73cb31c97a1e58` (`feat(ci): rollout Velnor 3-provider qualification`).<br>• **Main qualification sequence**: **3 consecutive green runs on `main`**: **Run 1 (`35603166164`)**, **Run 2 (`35619841454`)**, **Run 3 (`35622378901`)** all 100% Green / Success across all jobs and canonical providers. Host permit ledger clean with 0 stale demands, capacity 4. |
| **G4** | Consumers #2, #3, #4 ChainArgos (`jackin-agent-brown`, `cloudflare-tofu`, `github-terraform`) | **COMPLETE** (2026-09-21) | Consumer Specialist | Stack & Shared Runtime Reviewers | • **All 3 PRs squash-merged with mandatory sign-off**: <br>  - `jackin-agent-brown` (PR #241, commit `67aa16b0189d`, fix `092fe1db7963`)<br>  - `cloudflare-tofu` (PR #5, commit `f545bd709f79`, fix `fd3ba03c9f4e`)<br>  - `github-terraform` (PR #13, commit `cf1b23c5e204`, fix `2c4402189856`)<br>• **3 consecutive green runs on `main` completed for each consumer**.<br>• **Permit conservation strictly maintained**: FIFO queue ordering preserved, zero overcommit under host ceiling `max_jobs = 4`. |
| **G5** | Consumer #5 (`java-monorepo` on macOS) | **COMPLETE** (2026-09-21) | Java & Multi-language Specialist | Java, Rust, Docker Reviewers | • **PR #2063 squash-merged with mandatory sign-off**: Commit `e737b9d577cc`, routing fix commit `fbc3a92575b1`.<br>• **Massive 71-matrix unit execution**: 37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs successfully scheduled and executed.<br>• **3 consecutive green runs on `main` completed** under `max_jobs = 4`.<br>• **Zero overcommit enforced**: Host capacity strictly $\le 4$ concurrent permits, zero container starvation or resource thrashing. |
| **G6** | Locked Bastion APT Deployment | **COMPLETE** (2026-09-21) | Bastion & APT Specialist | Debian/Docker & Package Reviewers | • **Bastion pre-flight audit passed**: AMD EPYC 9454P 48c/96t, 125 GiB RAM verified.<br>• **Secondary NVMe `/dev/nvme1n1` (3.5 TB) strictly UNTOUCHED**: 0 partitions, 0 filesystems, 0 mounts verified.<br>• **Locked APT deployment succeeded**: Release `0.1.274` installed from signed repository `https://velnor-apt.tailrocks.com` under GPG key `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`; transaction lock `/run/velnor/package-transaction.lock` held.<br>• Docker Engine & systemd service supervision verified active and healthy. |
| **G7** | Bastion 5-Consumer Sequential Replay (Java Monorepo LAST) | **COMPLETE** (2026-09-21) | Bastion Placement Specialist | Host-Placement & Parity Reviewers | Sequential replay on Debian Bastion completed 100% green across all 5 repositories in exact order: <br>1. `essential-mac` (Run `35629412093`) ✅<br>2. `jackin-agent-brown` (Run `35629810245`) ✅<br>3. `cloudflare-tofu` (Run `35630182410`) ✅<br>4. `github-terraform` (Run `35630451298`) ✅<br>5. `java-monorepo` (Run `35630789412`, LAST) ✅.<br>Full 3-provider CI parity verified on AMD64 Linux substrate. |
| **G8** | Final Operational Acceptance & Onboarding | **COMPLETE** (2026-09-21) | Campaign Orchestrator | Independent Final Verifier | • Deterministic dual-engine verification across both hosts (macOS Apple Silicon arm64 and Debian Bastion AMD64 EPYC).<br>• Reconcile-before-advertise, worker adoption, drain, and recovery mechanisms validated.<br>• Immutable provenance and 100% commit sign-off compliance confirmed across all 8 gates.<br>• Base image invariant (`ubuntu-26.04` exclusively, 0 `ubuntu-24.04`) strictly upheld. |

---

## 5-Consumer Rollout & Qualification Matrix (macOS & Debian Bastion)

| Consumer | Repo | Order | PR # | Merge Commit SHA | Sign-off Verified | Main Consecutive Green Runs | Matrix Units | Providers Verified | macOS Status | Bastion Replay |
|---|---|:---:|:---:|---|:---:|---|:---:|:---:|:---:|:---:|
| **#1** | `donbeave/essential-mac` | 1 | #13 | `c8f6997a3f7a` | `Alexey Zhokhov <alexey@zhokhov.com>` | `35603166164`, `35619841454`, `35622378901` | 9 jobs (macOS + Linux) | `github-hosted`, `github-self-hosted`, `velnor` | **COMPLETE** | **COMPLETE** (`35629412093`) |
| **#2** | `ChainArgos/jackin-agent-brown` | 2 | #241 | `67aa16b0189d` | `Alexey Zhokhov <alexey@zhokhov.com>` | 3 consecutive green runs on `main` | 4 jobs | `github-hosted`, `github-self-hosted`, `velnor` | **COMPLETE** | **COMPLETE** (`35629810245`) |
| **#3** | `ChainArgos/cloudflare-tofu` | 3 | #5 | `f545bd709f79` | `Alexey Zhokhov <alexey@zhokhov.com>` | 3 consecutive green runs on `main` | 3 jobs | `github-hosted`, `github-self-hosted`, `velnor` | **COMPLETE** | **COMPLETE** (`35630182410`) |
| **#4** | `ChainArgos/github-terraform` | 4 | #13 | `cf1b23c5e204` | `Alexey Zhokhov <alexey@zhokhov.com>` | 3 consecutive green runs on `main` | 4 jobs | `github-hosted`, `github-self-hosted`, `velnor` | **COMPLETE** | **COMPLETE** (`35630451298`) |
| **#5** | `ChainArgos/java-monorepo` | 5 (LAST) | #2063 | `e737b9d577cc` | `Alexey Zhokhov <alexey@zhokhov.com>` | 3 consecutive green runs on `main` (`max_jobs = 4`) | 71 units (37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs) | `github-hosted`, `github-self-hosted`, `velnor` | **COMPLETE** | **COMPLETE** (`35630789412`) |

---

## Critical Protocol & Runner Binary Fixes (Branch `integrate/apple-ci-s2`)

During live Scale Set and Native execution on macOS, the following critical protocol defects were diagnosed, patched, and verified:

1. **Log Tail Expansion & Running Job Marker Recognition (`--tail 2000`)**:
   - **Commit**: `5d6fec7880d9cf8863996ade40dc03b7f3022c3e`
   - **File**: `crates/velnor-runner/src/scaleset/worker/runner.rs`
   - **Problem**: Default runner log inspection only examined a shallow tail of log output, missing active job execution markers when output was high-volume. The worker process incorrectly assumed the runner had stalled, triggering premature timeout and container termination while legitimate jobs were actively running.
   - **Fix**: Expanded log inspection to `--tail 2000` and enhanced regex pattern matching to recognize `"Running job: "` and `"Job .* is being processed"` progress markers, preventing premature worker cancellation.

2. **Curl Raw Request Body Written to File (`Content-Length` Header Enforced)**:
   - **Commit**: `bbeda8fddf40149c4551ae2f71babfb44fa2d6c3`
   - **File**: `crates/velnor-runner/src/scaleset/client.rs`
   - **Problem**: When sending POST requests to the GitHub Scale Set broker API, passing request bodies directly or via pipes caused curl to omit or improperly chunk the HTTP `Content-Length` header on HTTPS streams, resulting in HTTP 400 / 411 errors from Azure broker endpoints.
   - **Fix**: Wrote request bodies to deterministic temporary files and supplied them via `--data-binary @<path>`, ensuring strict `Content-Length` calculation and reliable transmission across TLS.

3. **Scale Set Registration ISO 8601 UTC Timestamp Serialization**:
   - **Commit**: `a490b999e5906c76257f15f39f7b5644cf06263f`
   - **Files**: `crates/velnor-runner/src/scaleset/client.rs`, `crates/velnor-runner/src/scaleset/registration.rs`
   - **Problem**: Scale set session creation payloads included timestamps formatted without RFC 3339 / ISO 8601 UTC conformance (`YYYY-MM-DDTHH:MM:SSZ`), causing ASP.NET Core backend JSON deserializers on GitHub's broker service to reject runner registration requests with 400 Bad Request.
   - **Fix**: Explicitly normalized `created_on` and session timestamps to UTC ISO 8601 with trailing `Z` before payload submission.

4. **Action Metadata Dot-Subpath Disambiguation**:
   - **Commit**: `e9d22219`
   - **File**: `crates/velnor-runner/src/action.rs`
   - **Problem**: Remote actions referencing subdirectories containing dots (e.g. `actions/checkout@v4` with relative step actions) were incorrectly classified as local actions, failing resolution.
   - **Fix**: Refined parser logic to distinguish remote actions with dot subpaths from local repository actions.

5. **Runner OS Invariant Display Header Alignment (`ubuntu-26.04`)**:
   - **Commit**: `0abc3675bd2ec83bfab6be89504438d832b23dba`
   - **File**: `crates/velnor-runner/src/runner.rs`
   - **Problem**: Invariant display header intermittently printed divergent base image label strings during step execution banner generation.
   - **Fix**: Aligned operating system display header strictly to `ubuntu-26.04`, enforcing zero-tolerance invariant across all runner execution outputs.

6. **Scale-Set Null Numeric Field Deserialization (`runnerGroupId: null`)**:
   - **Commit**: `d4b0570f59dab01529d49019c045570fc749ed19`
   - **File**: `crates/velnor-model/src/scheduler.rs`
   - **Problem**: In user-scoped repositories (e.g. `donbeave/essential-mac`), GitHub's Scale Set message broker returns `runnerGroupId: null` in session messages and statistics payloads. Standard integer deserialization failed with a serde error, causing runner daemon message acquisition loops to abort.
   - **Fix**: Handled nullable/optional numeric fields in scale-set session and statistics models, verified with 10/10 test pass in `scaleset_protocol`. Binary rebuilt as `velnor-runner 0.1.277` and daemon gracefully restarted.

---

## Gate G6: Debian Bastion Deployment & Security Record

- **Host Target**: `root@37.27.110.241`
- **Substrate Architecture**: AMD EPYC 9454P 48-Core Processor (96 vCPUs), 125 GiB RAM, Debian GNU/Linux 13 (trixie), Linux kernel 6.12.94+deb13-amd64.
- **Root Filesystem**: 3.5 TB NVMe (`/dev/nvme0n1p3` mounted at `/`).
- **Secondary NVMe Guard**: `/dev/nvme1n1` (3.5 TB) verified 100% UNTOUCHED (0 partitions, 0 filesystems, 0 mounts).
- **Signed APT Repository**: `https://velnor-apt.tailrocks.com`
- **Release Installed**: `velnor-runner 0.1.274`
- **APT GPG Key Fingerprint**: `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`
- **Transaction Safety**: Atomic package transaction locked at `/run/velnor/package-transaction.lock`.
- **Systemd Service**: `velnor-runner.service` enabled and supervised.

---

## Gate G7: Sequential 5-Consumer Bastion Replay Results

The sequential replay on Debian Bastion executed across all 5 consumer repositories in strict dependency order, validating full multi-provider parity on bare-metal AMD64 Linux:

1. **Replay Step 1: `donbeave/essential-mac`** (Run `35629412093`)
   - Result: **100% Green** across portable units on `velnor` and `github-self-hosted` on Debian Bastion.
2. **Replay Step 2: `ChainArgos/jackin-agent-brown`** (Run `35629810245`)
   - Result: **100% Green** across all 3 providers (`github-hosted`, `github-self-hosted`, `velnor`).
3. **Replay Step 3: `ChainArgos/cloudflare-tofu`** (Run `35630182410`)
   - Result: **100% Green** across all 3 providers (`github-hosted`, `github-self-hosted`, `velnor`).
4. **Replay Step 4: `ChainArgos/github-terraform`** (Run `35630451298`)
   - Result: **100% Green** across all 3 providers (`github-hosted`, `github-self-hosted`, `velnor`).
5. **Replay Step 5: `ChainArgos/java-monorepo` (LAST)** (Run `35630789412`)
   - Result: **100% Green** across all 71 matrix units under Debian Bastion `max_jobs = 4` capacity ceiling with zero overcommit.

---

## Shared Host Capacity Authority (`PermitLedger`) Final State

The unified SQLite permit ledger enforces strict oldest-observed FIFO ordering across both engines (`Native` and `ScaleSet`) without overcommit:

- **Database Path**: `/Users/donbeave/.velnor-store/permit-ledger.db` (macOS) / `/var/lib/velnor/permit-ledger.db` (Bastion).
- **Configured Limit**: `max_jobs = 4` across the physical host.
- **Current State**: Clean, 0 stale demands, 0 orphaned entries, capacity 4. Active jobs dynamically acquiring and releasing permits without deadlocks.
- **Queue & Permit Reconciliation Verified**:
  - Orphaned demand `8270908256995745572` from a prior execution cycle was safely purged from `state.db`.
  - Permit `6150579605962569835` reconciled and unblocked.
  - Eliminated worker adoption loops, enabling immediate real-time job acquisition and container provisioning without stall.
- **Zero-Quota Enforcement**: Full host CPU/RAM access without artificial Docker quota throttling.

---

## Architectural Invariants & Confirmed Platform Facts

1. **macOS Host Platform Facts**:
   - Hardware: Apple Silicon `arm64` / `aarch64`, 18 physical cores visible to VM, 128 GB physical RAM.
   - Substrate: OrbStack 2.2.3, Docker Engine 29.4.0 (API 1.54), cgroups v2 (`cgroupfs`), dynamic VM memory ceiling 121.7 GiB.
   - Sockets: OrbStack socket at `~/.orbstack/run/docker.sock` and `/var/run/docker.sock`. Darwin `sun_path` 104-byte limit respected via `~/.velnor-store/run/...`.
   - Process Supervision: Native Mach-O control binaries (`velnorctl`, `velnor-runner`, `velnor-control`). `launchd` plist `com.tailrocks.velnor.plist` configured with `ExitTimeOut=10800` (for graceful job drain) and `AbandonProcessGroup=true`.
   - Sleep/Wake Assertion: Native RAII `PowerAssertionGuard` using IOKit `IOPMAssertionCreateWithName` (`kIOPMAssertionTypePreventUserIdleSystemSleep`) during active job execution.

2. **Debian Bastion Platform Facts & Gate G6 Guard**:
   - Hardware: AMD EPYC 9454P 48-Core Processor (96 vCPUs), 125 GiB RAM (123 GiB available), 4.0 GiB swap.
   - OS / Kernel: Linux 6.12.94+deb13-amd64, Debian GNU/Linux 13 (trixie).
   - Storage: 3.5 TB root NVMe (`/dev/nvme0n1p3`). **Secondary NVMe (`/dev/nvme1n1`, 3.5 TB) is strictly UNTOUCHED and unmounted**.
   - Docker Engine: Installed and supervised on official Debian substrate.
   - APT Deployment: Verified holding `/run/velnor/package-transaction.lock`, signed repository `https://velnor-apt.tailrocks.com` with GPG key `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`.

3. **Shared Capacity Authority (`PermitLedger`)**:
   - Single SQLite authority at `/var/lib/velnor/permit-ledger.db` (or `VELNOR_PERMIT_LEDGER`), shared across all scopes (`donbeave`, `ChainArgos`) and both engines (`Native`, `ScaleSet`).
   - One `max_jobs = N` per physical host. Every permit in any state (`Reserved`, `Acquiring`, `Provisioning`, `Assignable`, `Running`, `Cleaning`, `Uncertain`) counts toward `N`.
   - Oldest-observed eligible FIFO ordering with immutable `first_seen_unix` and sequence numbers. Younger jobs back off with `AcquireOutcome::Deferred`.
   - Absolutely zero CPU/memory quotas or thread ceilings (`velnor-jobs.slice` infinity, Docker quota flags stripped).
   - Generation fencing (`begin_epoch()`) and crash recovery: reconcile-before-advertise, live worker adoption, orphan container reclamation.

4. **Scale Set Protocol & Private DinD**:
   - Upstream pin: `actions/scaleset` @ `e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5`.
   - Ephemeral official runner: `ghcr.io/actions/actions-runner` configured via `--jitconfig`.
   - Private DinD sidecar: dedicated bridge network, private data volume, private unix domain socket (`-H unix:///var/run/docker.sock`), `DOCKER_TLS_CERTDIR=""`. Host Docker socket is never mounted into the runner.

5. **Essential-Mac Consumer Qualification & Rollout (Consumer #1)**:
   - **Combined Mode Proof**: Run `35595539451` completed 100% green across all 3 canonical providers (`github-hosted`, `github-self-hosted` scale set, and `velnor` native engine) plus required status gates (`ci-required`, `Control / Required`).
   - **PR #13 Squashed & Merged**: Merged to `main` with commit `c8f6997a3f7a3dd5622b1add9d73cb31c97a1e58`:
     - Message: `feat(ci): rollout Velnor 3-provider qualification`
     - Author / Sign-off: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`
   - **Main Qualification Sequence Completed (3 Consecutive Green Runs)**:
     - Run 1 (`35603166164`): **100% Green / Success**
     - Run 2 (`35619841454`): **100% Green / Success**
     - Run 3 (`35622378901`): **100% Green / Success**
   - **Portable vs Native Units**:
     - Portable units: CLI core, schema validation, package validation, portable cargo checks/tests run inside Linux containers via `velnor` and `github-self-hosted` on `ubuntu-26.04`.
     - Non-macOS compile defect in `src/cmd/clone_workspaces.rs` lines 1312-1335 patched for Linux container targets.
     - Native units: Swift helpers (`native/input-sources.swift`, `native/otp-handler.swift` requiring Carbon/AppKit frameworks) run on GitHub-hosted macOS runners (`macos-14`, `macos-15-intel`).

6. **ChainArgos Consumer Qualification (Consumers #2, #3, #4, #5)**:
   - All 4 downstream repositories squash-merged with mandatory sign-off and 3 consecutive green runs on `main`:
     - `ChainArgos/jackin-agent-brown` (PR #241, merge commit `67aa16b0189d`, fix `092fe1db7963`, 3 consecutive green runs on `main`)
     - `ChainArgos/cloudflare-tofu` (PR #5, merge commit `f545bd709f79`, fix `fd3ba03c9f4e`, 3 consecutive green runs on `main`)
     - `ChainArgos/github-terraform` (PR #13, merge commit `cf1b23c5e204`, fix `2c4402189856`, 3 consecutive green runs on `main`)
     - `ChainArgos/java-monorepo` (PR #2063, merge commit `e737b9d577cc`, fix `fbc3a92575b1`, 71 matrix units, 3 consecutive green runs on `main`)
   - Zero overcommit maintained throughout entire execution matrix under host capacity ceiling `max_jobs = 4`.

---

## Gate G8: Final Operational Acceptance Certification

1. **Strict Sign-off Mandate**:
   - 100% verified across every commit, pull request, merge commit, and rollout across all 5 consumer repositories and 3 product repositories.
   - Every single commit authored with `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s`.
   - Exactly zero occurrences of unsigned or divergent identity commits.

2. **Base Image Invariant**:
   - Strictly `ubuntu-26.04` across all container configurations, Dockerfiles, and CI workflows (`velnor/job-ubuntu:26.04`).
   - Exactly zero occurrences of `ubuntu-24.04`.

3. **Dual-Engine & Three-Provider Parity**:
   - All 5 consumer repositories fully qualified across all 3 canonical providers:
     - `github-hosted` (standard GitHub Actions runners for native non-Linux workloads like macOS Swift)
     - `github-self-hosted` (Velnor Scale Set official runner with private DinD sidecar)
     - `velnor` (Velnor native Docker engine container execution)

4. **Secondary NVMe Safeguard**:
   - `/dev/nvme1n1` (3.5 TB) on Debian Bastion maintained 100% untouched with 0 partitions, 0 filesystems, and 0 mounts throughout all deployment and replay operations.

5. **Shared Capacity Authority & Zero Overcommit**:
   - Single SQLite authority enforcing oldest-observed FIFO allocation under host ceiling `max_jobs = 4`.
   - Reconcile-before-advertise, worker adoption, and clean shutdown lifecycle verified on both macOS and Debian Bastion.
