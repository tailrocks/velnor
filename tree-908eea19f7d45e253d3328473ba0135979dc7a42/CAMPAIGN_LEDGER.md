# Velnor Rollout & Autonomous Execution Campaign Ledger

Authoritative operational ledger for Velnor deployment, two-engine host execution, and sequential five-repository rollout across macOS and Debian Bastion.

- **Orchestrator**: Antigravity Autonomous Orchestrator
- **Signoff Identity**: `Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s`
- **Host 1**: macOS Local Workstation (`darwin`, Apple Silicon arm64 / aarch64, OrbStack Docker Engine 29.4.0)
- **Host 2**: Debian Bastion (`root@37.27.110.241`, Debian 13 trixie x86_64, AMD EPYC 9454P 48c/96t, ~128 GB RAM, ~3.5 TB root NVMe)
- **Canonical Providers**: `github-hosted`, `github-self-hosted` (Velnor Scale Set official runner), `velnor` (Velnor native Docker engine)

---

## Consumer Rollout Order (Strictly Sequential)

1. `https://github.com/donbeave/essential-mac` (FIRST live consumer rollout on macOS)
2. `https://github.com/ChainArgos/jackin-agent-brown` (macOS consumer #2)
3. `https://github.com/ChainArgos/cloudflare-tofu` (macOS consumer #3)
4. `https://github.com/ChainArgos/github-terraform` (macOS consumer #4)
5. `https://github.com/ChainArgos/java-monorepo` (macOS consumer #5, LAST on macOS)

**Gate Progression**:
All 5 repositories must qualify on macOS local host BEFORE operational bastion deployment begins.
Once macOS gate completes, bastion qualifies against the exact same ordered list:
1. `essential-mac` -> 2. `jackin-agent-brown` -> 3. `cloudflare-tofu` -> 4. `github-terraform` -> 5. `java-monorepo` (LAST on bastion).

---

## Rollout Gates & Execution Ledger

| Gate | Scope | Status | Author | Verifier | Done Criteria & Evidence |
|---|---|---|---|---|---|
| **G0** | Current Truth & Access Inventory | **PASSED** (2026-09-21) | Subagent `2a6b424a` | Subagent `818188a6` / `fbefe9fe` | 100% verified admin/write permissions across 5 consumer repos + 3 product repos; OrbStack facts verified (18 cores, 121.7 GiB VM, cgroups v2); Bastion SSH & hardware verified (EPYC 9454P, 96 vCPUs, 125 GiB RAM, 0 Docker); fleet capacity deadlock identified (0 runners, 2 offline dogfood slots). |
| **G1** | Preflight, Base Image & Product Prerequisites | **COMPLETE** (2026-09-21) | Velnor Core Team | macOS, Protocol, Generator, Supply-Chain Reviewers | Pinned Scale Set protocol & runner image; Mac OrbStack Docker engine resolution; unified `PermitLedger` SQLite allocator with `max_jobs = N` FIFO ordering; Scale Set DinD workspace mount coherence verified; NativePermitGuard cleaning lifecycle verified; macOS RAII `PowerAssertionGuard` verified (100% nextest pass); Homebrew packaging with `launchd` supervision merged in `tailrocks/homebrew-velnor` PR #4 (`Formula/velnorctl.rb`). Pushed to `integrate/apple-ci-s2` (commit `5ec52f7c`). |
| **G2** | Action Metadata & Runner Binary Fixes | **COMPLETE** (2026-09-21) | Scale Set Specialist | Official-Engine / Protocol Reviewers | Runner binary fixes and protocol stabilization landed on branch `integrate/apple-ci-s2`: <br>• Remote action dot subpaths fix (`e9d22219`)<br>• Curl raw request body written to file for strict `Content-Length` header (`bbeda8fd`)<br>• Scale set registration ISO 8601 UTC timestamp formatting for ASP.NET deserialization (`a490b999`)<br>• Scale set runner `--tail 2000` log expansion & running job marker recognition to avoid premature timeouts (`5d6fec78`). |
| **G3** | Consumer #1 (`donbeave/essential-mac` on macOS) | **IN PROGRESS** (2026-09-21) | Native & Scale Set Specialists | Integration & Capacity Reviewers | **Native mode passed green** (Run `35589648167`, all Rust eligible jobs passed on `velnor`). **Combined mode active/in-progress** (Run `35590330572`: `github-self-hosted` scale set passed green, `velnor` native passed green, `github-hosted` macOS running concurrently under shared permit authority). |
| **G4** | Consumers #2, #3, #4 ChainArgos (`jackin-agent-brown`, `cloudflare-tofu`, `github-terraform`) | **IN PROGRESS** (2026-09-21) | Consumer Specialist | Stack & Shared Runtime Reviewers | Scale set daemon active with `max_jobs = 4` on scope `ChainArgos/jackin-agent-brown` (PID `27421`). Jobs actively executing and queued: <br>• `jackin-agent-brown` (PR #241, Run `35587944563`): Active runner processing jobs (`docs-prek / velnor` succeeded, Docker jobs active).<br>• `cloudflare-tofu` (PR #5, Run `35587981747`): Queued.<br>• `github-terraform` (PR #13, Run `35587968574`): Queued. |
| **G5** | Consumer #5 (`java-monorepo` on macOS) | **IN PROGRESS** (2026-09-21) | Java & Multi-language Specialist | Java, Rust, Docker Reviewers | All 71 historical units (37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs) queued/running across 3 providers (PR #2063, Run `35587996113` and policy run `35587994050`). Unblocks Bastion upon completion. |
| **G6** | Locked Bastion APT Deployment | **PREPARED** (2026-09-21) | Bastion & APT Specialist | Debian/Docker & Package Reviewers | Deployment pipeline prepared and dry-run verified (`deploy-bastion-g6.sh`, `playbooks/deploy-bastion-g6.yml`, `docs/APT_REPOSITORY.md`, commit `59a20dfb`). Bastion hardware safeguard verified (`/dev/nvme1n1` 3.5 TB secondary NVMe untouched & unmounted). Signed APT repository `https://velnor-apt.tailrocks.com` (GPG `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`) and transaction lock `/run/velnor/package-transaction.lock` configured. Ready for execution once macOS gates complete. |
| **G7** | Bastion 5-Consumer Replay (Java Monorepo LAST) | **PENDING G6** | Bastion Placement Specialist | Host-Placement & Parity Reviewers | Sequential replay on Debian Bastion: `essential-mac` -> `jackin-agent-brown` -> `cloudflare-tofu` -> `github-terraform` -> `java-monorepo` (LAST). Full 3-provider CI. |
| **G8** | Final Operational Acceptance & Onboarding | **PENDING G7** | Campaign Orchestrator | Independent Final Verifier | Deterministic verification of both modes on both hosts, drain/recovery tests, immutable provenance, reproducible onboarding documentation without per-repo pools. |

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

---

## Active Scale-Set Daemons on macOS Local Workstation

Both daemons run concurrently under Mach-O supervision on Apple Silicon arm64, sharing the centralized SQLite `PermitLedger` at `/Users/donbeave/.velnor-store/permit-ledger.db` with host-level capacity `max_jobs = 4`:

### Daemon 1: Scope `donbeave/essential-mac`
- **PID**: `9662`
- **Binary**: `/Users/donbeave/Projects/github/velnor/target/debug/velnor-runner daemon`
- **Target URL**: `https://github.com/donbeave/essential-mac`
- **Runner Name**: `velnor`
- **Labels**: `self-hosted,velnor,velnor-target-mvp,ubuntu-26.04,ubuntu-latest,hetzner-sentry-ci`
- **Config / State**: `/Users/donbeave/.velnor-store/scaleset-essential-mac`
- **Permit Ledger**: `/Users/donbeave/.velnor-store/permit-ledger.db`
- **Max Jobs Ceiling**: `4` (`--max-jobs 4`)
- **Execution Target Image**: `velnor/job-ubuntu:26.04`

### Daemon 2: Scope `ChainArgos/jackin-agent-brown`
- **PID**: `27421`
- **Binary**: `/Users/donbeave/Projects/github/velnor/target/debug/velnor-runner daemon`
- **Target URL**: `https://github.com/ChainArgos/jackin-agent-brown`
- **Runner Name**: `velnor-chainargos`
- **Labels**: `self-hosted,velnor,velnor-target-mvp,ubuntu-26.04,ubuntu-latest`
- **Config / State**: `/Users/donbeave/.velnor-store/scaleset-chainargos`
- **Permit Ledger**: `/Users/donbeave/.velnor-store/permit-ledger.db`
- **Max Jobs Ceiling**: `4` (`--max-jobs 4`)
- **Execution Target Image**: `velnor/job-ubuntu:26.04`

---

## Shared Host Capacity Authority (`PermitLedger`) Live State

The unified SQLite permit ledger enforces strict oldest-observed FIFO ordering across both engines (`Native` and `ScaleSet`) without overcommit:

- **Database Path**: `/Users/donbeave/.velnor-store/permit-ledger.db`
- **Configured Limit**: `max_jobs = 4` across the physical host.
- **Current Allocations Observed**:
  - `native/<uuid>`: `running` (PID active, executing inside container `velnor-job-...`)
  - `scaleset/...`: `provisioning` (DinD container & runner container starting)
  - `scaleset/...`: `provisioning` (DinD container & runner container starting)
  - `scaleset/...`: `acquiring` (waiting on slot allocation / backoff)
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
   - Storage: 3.5 TB root NVMe. **Secondary NVMe (`/dev/nvme1n1`, 3.5 TB) is strictly UNTOUCHED and unmounted**.
   - Docker: Currently uninstalled (`command not found`). Will be provisioned via official signed Debian trixie repository in Gate G6.
   - APT Deployment: Verified dry-run holding `/run/velnor/package-transaction.lock`, signed repository `https://velnor-apt.tailrocks.com` with GPG key `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`.

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

5. **Essential-Mac Consumer Audit (Consumer #1)**:
   - Portable units: CLI core, schema validation, package validation, portable cargo checks/tests.
   - Non-macOS compile defect diagnosed in `src/cmd/clone_workspaces.rs` lines 1312-1335 (unreachable statement and undeclared variables on `not(target_os = "macos")`). Fix is required for Linux Docker execution.
   - Native units: Swift helpers (`native/input-sources.swift`, `native/otp-handler.swift` requiring Carbon/AppKit frameworks) run on GitHub-hosted macOS runners (`macos-14`, `macos-15-intel`).
