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
| **G1** | Product Prerequisites for FIRST Mac Pilot | **PASSED** (2026-09-21) | Velnor Core Team | macOS, Protocol, Generator, Supply-Chain Reviewers | Pinned Scale Set protocol & runner image; Mac OrbStack Docker engine resolution; unified `PermitLedger` SQLite allocator with `max_jobs = N` FIFO ordering; Scale Set DinD workspace mount coherence verified; NativePermitGuard cleaning lifecycle verified; macOS RAII `PowerAssertionGuard` verified (100% nextest pass); Homebrew packaging with `launchd` supervision merged in `tailrocks/homebrew-velnor` PR #4 (`Formula/velnorctl.rb`). Pushed to `integrate/apple-ci-s2` (commit `5ec52f7c`). |
| **G2** | `essential-mac` on macOS / Scale Set FIRST | **IN PROGRESS** | Scale Set Specialist | Official-Engine / Mac Integration Reviewer | Live proof of `essential-mac` eligible units in official runner containers via Velnor Scale Set with private DinD. Docker conformance & portable checks pass. Native macOS units run on GitHub-hosted macOS. |
| **G3** | `essential-mac` on macOS / Native SECOND, then Combined | PENDING | Native Engine Specialist | Native, Capacity, Result Reviewers | Live proof of `essential-mac` on native Velnor Docker backend. Then both modes together with shared N capacity authority, PR + 3 consecutive green main runs. |
| **G4** | Next Three Consumers on macOS (Sequential) | PENDING | Consumer Specialist | Stack Reviewer + Shared Runtime Reviewer | Sequential rollout: `jackin-agent-brown` -> `cloudflare-tofu` -> `github-terraform`. Generated workflows, both local modes + hosted, PR + 3 green main runs each. |
| **G5** | `java-monorepo` LAST on macOS | PENDING | Java & Multi-language Specialist | Java, Rust, Docker, Frontend, Coverage Reviewers | All 71 historical units (37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs) across 3 providers. Testcontainers, GraalVM, amd64 emulation labeled, PR + 3 green main runs. Unblocks Bastion. |
| **G6** | Docker + APT-only Velnor on Bastion | PENDING | Bastion & APT Specialist | Debian/Docker & Package Reviewers | Official Debian repo Docker install, signed APT repository publication (`velnor-apt`), APT-only install holding `/run/velnor/package-transaction.lock`, healthy systemd service, shared N, no quotas. |
| **G7** | Bastion Consumer Replay (Java Monorepo LAST) | PENDING | Bastion Placement Specialist | Host-Placement, Repository, Parity Reviewers | Bastion native Velnor first, then Scale Set mode. Sequential replay: `essential-mac` -> `jackin-agent-brown` -> `cloudflare-tofu` -> `github-terraform` -> `java-monorepo` (LAST). Full 3-provider CI. |
| **G8** | Final Operational Acceptance & Onboarding | PENDING | Campaign Orchestrator | Independent Final Verifier | Deterministic verification of both modes on both hosts, drain/recovery tests, immutable provenance, reproducible onboarding documentation without per-repo pools. |

---

## Architectural Invariants & Confirmed Platform Facts

1. **macOS Host Platform Facts**:
   - Hardware: Apple Silicon `arm64` / `aarch64`, 18 physical cores visible to VM, 128 GB physical RAM.
   - Substrate: OrbStack 2.2.3, Docker Engine 29.4.0 (API 1.54), cgroups v2 (`cgroupfs`), dynamic VM memory ceiling 121.7 GiB.
   - Sockets: OrbStack socket at `~/.orbstack/run/docker.sock` and `/var/run/docker.sock`. Darwin `sun_path` 104-byte limit respected via `~/.velnor-store/run/...`.
   - Process Supervision: Native Mach-O control binaries (`velnorctl`, `velnor-runner`, `velnor-control`). `launchd` plist `com.tailrocks.velnor.plist` configured with `ExitTimeOut=10800` (for graceful job drain) and `AbandonProcessGroup=true`.
   - Sleep/Wake Assertion: Native RAII `PowerAssertionGuard` using IOKit `IOPMAssertionCreateWithName` (`kIOPMAssertionTypePreventUserIdleSystemSleep`) during active job execution.

2. **Debian Bastion Platform Facts**:
   - Hardware: AMD EPYC 9454P 48-Core Processor (96 vCPUs), 125 GiB RAM (123 GiB available), 4.0 GiB swap.
   - OS / Kernel: Linux 6.12.94+deb13-amd64, Debian GNU/Linux 13 (trixie).
   - Storage: 3.5 TB root NVMe. **Secondary NVMe identified and strictly UNTOUCHED**.
   - Docker: Currently uninstalled (`command not found`). Will be provisioned via official signed Debian trixie repository in Gate G6.

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
