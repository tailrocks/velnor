# GOAL: Velnor Multi-Repo Rollout & Debian Bastion Deployment — paused handoff [930b8384]

## Section A: Identity & Pause Status

- **Goal Title**: Implement, deploy, and independently qualify Velnor as a configurable Rust control plane on local macOS host FIRST, then on Debian Bastion across 5 consumer repositories
- **Goal Slug**: `velnor-rollout-bastion`
- **Handoff Document File**: `docs/goal-handoffs/velnor-rollout-bastion--20260921T220213Z--antigravity--930b8384.md`
- **Unique Handoff ID**: `930b8384`
- **Audit Timestamp (UTC)**: `2026-09-21T22:15:30Z`
- **Audit Timestamp (Local)**: `2026-09-22T05:15:30+07:00`
- **Original Creation Timestamp (UTC)**: `2026-09-21T22:02:13Z`
- **Agent Slug**: `antigravity`
- **Pause Authority**: `PAUSED_BY_USER`
- **Goal Status**: `PAUSED_BY_USER` (Expresses requested goal disposition; implementations halted)
- **Handoff Document Status**: `READY` (All preservation, audit, and publication criteria verified)
- **Audit Outcome**: `VERIFIED`
- **Canonical Workspace**: `/Users/donbeave/Projects/github/velnor-bastion`
- **Repository Remote**: `https://github.com/donbeave/velnor-bastion.git`
- **Source Branch**: `main` (commit `478d7d4a54bbd20af92f1354fcdbb05b0f3736cc`)
- **Preservation Branch**: `handoff/velnor-rollout-930b8384`
- **Checkpoint Code SHAs**:
  - `donbeave/velnor-bastion`: `74d60ac9beb0092f3c2c893c5d39a731e3c25d68` (pushed to `origin/handoff/velnor-rollout-930b8384`)
  - `tailrocks/velnor`: `326fd41439535cec1265ec386af77fca048cfa7e` (pushed to `origin/integrate/apple-ci-s2`)
- **Draft PR**: `https://github.com/donbeave/velnor-bastion/pull/1`
- **Remote Portability**: Fully remote-portable across GitHub remotes; no hidden local-only secrets or dependencies.
- **Session & Task Identifiers**:
  - Antigravity Conversation ID: `72c99b4c-c42f-471c-8659-d7c213519891`
  - Interrupted Task ID: `task-18990` (`velnor-runner daemon` for `ChainArgos`)

> [!IMPORTANT]
> **PAUSED / WIP — checkpoint for later resumption; not a completion or merge claim.**
> The original engineering goal remains **PAUSED BY USER**, not completed or abandoned. All goal-owned implementation workers, background tasks, and daemon processes have been cleanly frozen and stopped. All recoverable work, code, and configurations across the 5 consumer repositories and 2 implementation repositories are durably preserved and pushed to remote tracking branches. No merging or local cleanup was performed during this pause.

---

## Section B: Original Goal & Success Contract

### B.1 Source Register
Authoritative user instructions recovered from conversation records (`transcript_full.jsonl`):

| Source ID | Source Type | Location / Step Index | Timestamp (UTC) | Accessibility | Operative Status | Description |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `SRC-001` | `ORIGINAL_USER_GOAL` | Step 0 | 2026-09-21T02:01:02Z | `FULL` | Superseded | Initial 32-repo / dual-provider prompt |
| `SRC-002` | `USER_AMENDMENT` | Step 328 | 2026-09-21T02:14:18Z | `FULL` | **BINDING** | Mandate: "Always use cargo nextest." |
| `SRC-003` | `USER_AMENDMENT` | Step 1134 | 2026-09-21T02:49:29Z | `FULL` | **BINDING** | Mandate: "Always commit with Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>" |
| `SRC-004` | `USER_AMENDMENT` | Step 1248 | 2026-09-21T02:56:29Z | `FULL` | **BINDING** | Strict prohibition on any other sign-off |
| `SRC-005` | `USER_AMENDMENT` | Step 7747 | 2026-09-21T09:26:17Z | `FULL` | **BINDING** | Mandate: "Use subagents aggressively for all work." |
| `SRC-006` | `USER_AMENDMENT` | Step 8713 | 2026-09-21T10:18:42Z | `FULL` | **BINDING** | Mandate: "We must only use ubuntu-26.04, never use old version like ubuntu-24.04." |
| `SRC-007` | `USER_AMENDMENT` | Step 14290 | 2026-09-21T16:59:48Z | `FULL` | Informational | "Continue." |
| `SRC-008` | `OPERATIVE_GOAL_PROMPT` | Step 14353 | 2026-09-21T17:02:43Z | `FULL` | **LATEST OPERATIVE AUTHORITY** | Comprehensive 11-section prompt superseding prior scope and establishing 5-consumer topology |
| `SRC-009` | `INTERMEDIATE_GOAL` | Step 18850 | 2026-09-21T19:31:48Z | `FULL` | Informational | Intermediate status re-dispatch |
| `SRC-010` | `USER_PAUSE_DIRECTIVE` | Step 19088 | 2026-09-21T21:59:16Z | `FULL` | **BINDING** | Immediate pause directive, state preservation, handoff creation, draft PR |
| `SRC-011` | `USER_AUDIT_DIRECTIVE` | Step 19250 | 2026-09-21T22:13:07Z | `FULL` | **BINDING** | Intent-fidelity and resumability audit & repair directive |

---

### B.2 Verbatim Operative User Prompt (`SRC-008`, Step 14353)

```markdown
/goal Implement, deploy, and independently qualify Velnor as a configurable Rust control plane on my actual local macOS host FIRST, then on bastion. Execute Linux-compatible CI workloads in Docker containers using either Velnor-managed GitHub Scale Set runners or Velnor's existing native runner. Complete the five-repository rollout below, package delivery, generated workflows, real CI runs, performance tuning, recovery tests, and operating documentation. This is a long-running implementation goal, not a request to stop after producing a plan.

# 1. Authority, scope, and mandatory order

This prompt is the latest authority. It supersedes the old operational order Velnor -> jackin-project/jackin -> java-monorepo, the earlier bastion-first requirement, and any older 32-repository/dual-provider campaign. Retain relevant audit facts and architectural invariants, not superseded scope or topology.

The consumer rollout order is EXACTLY:

1. https://github.com/donbeave/essential-mac
2. https://github.com/ChainArgos/jackin-agent-brown
3. https://github.com/ChainArgos/cloudflare-tofu
4. https://github.com/ChainArgos/github-terraform
5. https://github.com/ChainArgos/java-monorepo

ESSENTIAL-MAC MUST BE THE FIRST LIVE CONSUMER ROLLOUT. On the actual Mac, prove it in this order:

A. Velnor Scale Set mode -> ephemeral official GitHub runner -> Linux Docker execution, with private DinD for Docker-capable work.
B. Velnor native mode -> existing Velnor Docker job backend.
C. Both local modes together, alongside GitHub-hosted verification, with one shared local capacity authority.

Do not activate repository 2 before repository 1 qualifies, or skip an inaccessible repository and pretend the sequence completed. Research later repositories read-only and develop generic support in parallel; their live migration/activation remains sequential.

The implementation repositories are https://github.com/tailrocks/velnor and its delivery repositories, especially https://github.com/tailrocks/velnor-apt and https://github.com/tailrocks/homebrew-velnor. Fix their source, hosted CI, packages, and generator prerequisites whenever needed. They are product dependencies, NOT a competing first consumer rollout. Do not require an operational Velnor self-dogfood fleet before the essential-mac pilot.

Complete the five-repository macOS rollout, ending with java-monorepo, BEFORE operational bastion deployment. Bastion research, Docker provisioning code, packaging, and APT publication may proceed in parallel; actual installation/activation follows the macOS gate. Then qualify bastion against the same ordered consumer list, with java-monorepo last. Prove native Velnor on bastion first, then its configurable Scale Set mode; keep both available in the final product.

Bastion access and reported hardware:

    ssh root@37.27.110.241
    hostname: bastion
    Debian 13, x86_64
    AMD EPYC 9454P: 48 physical cores / 96 logical CPUs
    approximately 128 GB RAM
    approximately 3.5 TB root NVMe, plus a second NVMe not established empty

The latest supplied shell reports `docker: command not found`. Verify PATH, installed packages, sockets, daemon and service state. Treat Docker installation as an actual prerequisite; do not assume the host already runs Docker. The transcript is not a fresh infrastructure inventory.

# 2. Execution principles: delegate first, work autonomously, integrate continuously

The parent is an orchestrator. Its primary responsibilities are the ledger, dependency graph, assignments, conflict resolution, integration decisions, unblocking, and final deterministic gate checks. Delegate substantive research, design, code, tests, integration edits, infrastructure, publication, deployment, review and verification whenever the environment supports delegation.

At the beginning, launch parallel workstreams for:

- live repository/workflow/access inventory and historical-evidence reconciliation;
- existing macOS host implementation and Docker-provider integration;
- portable host/daemon boundaries, paths, process supervision and launchd;
- Rust Scale Set protocol, authentication and conformance;
- official runner/DinD filesystem, network and lifecycle behavior;
- native Velnor Docker compatibility and job-owned API mediation;
- shared capacity, FIFO, crash reconciliation and unbounded execution;
- typed providers/capabilities, generation, policy and result aggregation;
- immutable runtime products, CI repair, caches and build speed;
- macOS product distribution and service installation;
- Debian/Docker/APT release and deployment preparation;
- read-only workload analysis for each consumer;
- independent adversarial verifiers for the corresponding modules.

Use all useful available subagent slots. Give each task concrete inputs, file ownership, dependencies, required evidence and an independent verifier. Spawn further useful tasks as discoveries emerge. Reassign blocked agents to independent tests/review/research. Do not perform serial parent work that can be delegated, spawn redundant agents competing for the same files, or claim delegation that did not occur. Record proven tool limitations honestly.

Every module has a verifier who did not author it. Author tests are necessary but never the only certification. Verifiers must try to disprove claims, independently reproduce critical behavior, and return evidence rather than approval alone. A dedicated integration subagent owns shared generated output and Git-index operations. One infrastructure writer per host owns each coordinated mutation window.

Never ask the user questions or wait for clarification. Turn uncertainty into investigation: delegate independent analyses, inspect current code/history/docs/live state, compare feasible options, challenge assumptions, choose the best evidenced reversible decision, record it, and continue. Preserve existing authorizations and platform security requirements. Do not invent credentials, bypass mandatory approvals, or turn missing access into fabricated success. Exhaust authorized routes, record a specific demonstrated blocker, and continue independent work.

Judge work by correctness and target fit, never ROI, effort, expense or whether it feels worth doing. A known-wrong behavior is not acceptable because it is uncommon or an upstream reference also has it. Hard is not impossible. Demonstrate a technical or access limit before calling it a blocker. Keep scope connected to this goal rather than expanding indefinitely into unrelated features.

Before every bug fix, record the violated invariant, reproduction, enabling architectural condition, and whether this represents a class of bugs. Research a structural fix and independent counterexamples. Prefer removing the enabling condition to adding a special case. A symptom patch is acceptable only when the structural correction is proven infeasible or genuinely belongs in a separate change; name that cause, create the follow-up with its gate, and do not conceal incomplete correctness. This requires diagnosis, not gratuitous repository-wide refactoring for every typo.

Breaking changes are allowed and preferred when they produce the correct final design. Remove obsolete models. No compatibility shims, aliases, deprecation windows, dual legacy/new parsers, duplicate schedulers, or migration-only permanent modes. Requested native/Scale Set modes and legitimate Darwin/Linux host adapters are product capabilities, not compatibility debt. Consumers not yet migrated may retain an older immutable tool pin; do not make the new product parse their legacy model.

Commit small, logically scoped verified increments frequently and push regularly. Use relevant formatting, lint, targeted tests and generated-ownership checks before committing; use full gates before promotion. Prefer one active integration branch per repository. Additional short-lived worktrees/branches need a concrete concurrency/isolation reason. Never race a shared Git index or check out the same branch into competing writers. Merge reviewed small PRs promptly after actual required checks pass, then continue iterating; do not accumulate one giant branch/PR. Follow each repository's allowed merge method, preserve unrelated work/history and required sign-offs, and do not bypass hooks/protection or force-push shared history to manufacture progress.

# 3. One product, two local engines, three providers

Velnor's control/runner processes run natively on macOS and natively on Debian. All ordinary local CI workload steps run inside Docker job containers, not as arbitrary shell builds on the Mac or bastion host. Management, installation and supervised control processes are host operations, not job-execution exceptions.

Implement one typed configurable mode set that supports native-only, Scale-Set-only, and both simultaneously. Use one authority for each host's state and capacity. Changing enabled modes affects new admission and drains/reconciles existing work without relabeling it or silently switching execution engines.

Canonical workflow providers:

    github-hosted
    github-self-hosted
    velnor

`github-self-hosted` means the unmodified official Actions runner inside an ephemeral Linux container, provisioned by Velnor's Rust Scale Set implementation. NO deployed Go SDK example, Go sidecar, ARC/Kubernetes controller, parallel controller product, or Velnor reimplementation of the official runner's step execution.

`velnor` means Velnor's existing Rust-native execution semantics and built-in Docker backend. Reuse its container creation, ownership, Docker lease mediation, action handling, cancellation, caches and cleanup. Extend OS-dependent boundaries and fix real defects, but do not build a second native runtime, replace it with official runners, or translate it wholesale into the official lane's DinD design.

Host platform, Docker daemon platform, workload target, CPU architecture, provider, capabilities, trust, and publication role are distinct typed concepts. `host_os=darwin` with `execution_os=linux` is expected. Do not infer provider from `runner.environment`, a hostname, or whether a selector contains `velnor` or `self-hosted`.

Once each repository qualifies, all selected supported trusted Linux verification units automatically run on all three providers for PR/main and applicable scheduled verification; dispatch defaults to all three. Diagnostic subsets and recovery configurations are explicit reduced coverage, never equivalent to full qualification. Native platform and trust exclusions are decided before expansion, with real replacement coverage where required.

# 4. macOS host contract and truthful platform boundaries

Use the actual local Mac, not a Linux development container labeled macOS, not a hosted Mac substituted for local acceptance, and not SSH execution on bastion. Prior project context specifies OrbStack; discover and verify the installed local provider/context rather than assuming it. Prefer the established OrbStack environment. Do not replace the user's working provider or change the global Docker context without an evidenced need.

There are NO Velnor-managed worker VMs, per-job VMs, Firecracker/libvirt fleets, or extra OrbStack Linux machines. OrbStack/Docker Desktop's own lightweight Linux VM is the Docker engine substrate on macOS and is permitted; it is not a Velnor worker-VM topology. Document this boundary explicitly. Linux Docker containers cannot provide native Darwin frameworks, macOS settings, App Store/cask installation or Xcode execution.

Audit essential-mac's actual portable and native components. Run substantive portable logic, validation, generation, CLI/core tests and Docker conformance in containers. A Linux-compatible unit failing under Velnor is a defect to fix, not permission to relabel it unsupported; only real platform requirements justify an exception. Preserve true native macOS helper/build/integration tests on an explicitly native GitHub-hosted macOS job. Never fake Darwin through environment variables, silently skip required tests, or run broad essential-mac `apply`/bootstrap against my real workstation as a CI test. Use disposable fixture configuration/state, not my personal profile, preferences, shell files, Keychain or 1Password state.

Start from the existing `velnorctl host` and Docker endpoint implementation. Inspect current restrictions rather than assuming the documented repository-scoped foreground recovery surface is already an unattended multi-scope service. Make the same authoritative product support local development and durable installed service operation without separate runtimes.

Research and implement/test these boundaries:

- Exact local daemon selection from explicit configuration and supported Docker environment/context rules; service startup must not depend on an interactive shell. Record resolution source, daemon identity and generation. A context change or unreachable engine must not silently redirect jobs or cleanup to a different daemon.
- Darwin state/config/cache/log paths, user ownership, launchd environment, absolute executable paths, signal/process supervision and restart behavior. Do not require Linux systemd or `/proc` on Darwin.
- Query actual Linux daemon capabilities/cgroups/storage/network facts on the daemon side; do not fake them from Darwin. Where necessary use a narrowly scoped Velnor-owned transport/helper, not another controller, generic privileged host-command bridge or exposed management socket.
- Correct native Mach-O control binaries versus Linux ELF workload binaries, official runner/image architecture, tool runtimes and shared-library requirements. Discover actual Mac architecture. Preserve amd64 requirements; explicitly identify arm64-native versus amd64 emulation and validate target executables. Emulation is not native performance evidence.
- Host-to-daemon-to-container path mapping, symlinks, spaces, case sensitivity, UID/GID, file sharing, executable bits, temp/home paths and Unix socket transport. Do not assume a Mac-created Unix socket works when bind-mounted through the Linux VM. Prove the native job-owned Docker lease remains accessible and scoped without mounting the management endpoint.
- Docker availability, app/provider startup, logout/login limitations, sleep/wake, VPN/DNS changes, network reconnection, token renewal and installed Velnor service reload. When the host is unavailable, advertise unavailable capacity and recover deterministically; do not promise work continues through sleep or shutdown.
- Use an owned, scoped awake assertion during execution if needed, allowing display sleep; do not globally change power/lid/security settings. Test lifecycle without rebooting or logging out the user's workstation unnecessarily.
- Verify networking from the actual installed Velnor processes: REST, broker/session, results/log/artifact/cache endpoints. `curl` reaching GitHub does not prove runner uploads work. Diagnose code-signing/outbound-filter prompts without disabling Little Snitch, Gatekeeper or other protections.

Install the complete released macOS product through the established Homebrew distribution, repairing the tap/generated delivery if it lacks the runner, CLI, required runtime or service assets. Implement authenticated version/source/digest checks and coherent upgrades. Do not apply Debian APT rules to Darwin. Isolated SHA-identified developer builds are allowed for rapid reproduction; final pilot/service evidence must use published installed binaries and verified images, not a permanent `cargo run` or untracked binary replacement. Mac packaging is a prerequisite; the Debian publisher must not become an unnecessary barrier to starting the Mac pilot.

# 5. Scale Set protocol and Docker lifecycle

Pin and record the reviewed current `actions/scaleset` and official `actions/runner` reference revisions. Compare code, docs and versioned behavior; do not mix APIs from different upstream revisions or treat a demo as production behavior.

Implement in Rust within Velnor: scoped App authentication/token refresh, registration/group/set reconciliation, sessions, initial statistics, empty long polls, capacity advertisement, message cursor, JobAvailable, explicit AcquireJobs, JIT runner configuration, JobStarted/JobCompleted, acknowledgement, redelivery, drain and restart reconciliation. Use current statistics such as TotalAssignedJobs for convergence, not capped lifecycle-message counts.

Required ordering:

    validate eligible demand and persist age
    -> global permit reservation and durable acquisition intent
    -> AcquireJobs
    -> reconcile actual/partial/uncertain response IDs
    -> durable idempotent provisioning
    -> acknowledgement only when recovery/replay is safe
    -> actual runner execution
    -> export diagnostics and finish owned cleanup
    -> release permit

Acknowledgement need not wait for a whole job; durable recovery must continue independently. Prove deferred/unacquired-offer validity and re-offer behavior so acknowledging a batch cannot lose pending work. Do not double-count reservation and assigned statistics. Test all failure windows around database, GitHub and Docker operations; they are not one atomic transaction. Preserve one active fenced session/controller owner per scale set. Normal restart retains set identity; deletion is explicit decommissioning.

An acquired request does not guarantee which JIT runner receives it. Keep each scale set homogeneous in engine, architecture, trust and Docker capabilities. Initially use Docker-capable official workers with a private DinD daemon when that avoids unsafe per-job capability guesses. Removing unnecessary DinD for proven plain-only profiles is a later generic capability optimization, not a per-repository pool.

Every official worker uses an exact tested runner image digest and, when applicable, a DinD digest. Verify platform manifests and runner version. Never float `latest`. A derived image may supply verified tools while keeping the official runner unmodified. Keep runner versions current through generated, tested image releases.

Each admitted job owns runner, optional DinD, writable daemon data, workspace/temp/home state, private network/socket and cleanup identity. Workspaces, runner externals, file-command directories, tools and required bind paths must exist at matching absolute paths from runner and daemon perspectives. Establish localhost published-port behavior through a shared network namespace or an independently proven equivalent; inner job containers retain native service DNS/network behavior.

Never expose either host's unrestricted Docker socket, credential helper, broad HOME, SSH agent, Keychain or arbitrary filesystem to jobs. A job-private DinD socket named `/var/run/docker.sock` is allowed; prove it is not the host socket. Native Velnor's scoped mediated Docker API is allowed; preserve ownership enforcement rather than replacing it with an unrestricted proxy.

Prove real CLI build/run, Buildx/cache, Compose, Testcontainers/Ryuk, `jobs.container`, `services`, JavaScript actions inside containers, Docker actions/file commands, nested Docker where required, file/socket bind mounts, artifacts/caches and terminal cleanup. Docker info/hello-world alone is insufficient. Do not disable global Docker security settings or cleanup to make one fixture pass.

# 6. Capacity, FIFO, no quotas, and two-host routing

On EACH physical execution host there is exactly one durable capacity authority and one `max_jobs=N` shared by both local engines and every registered scope. The Mac and bastion have separately measured N values under the same model; they are not one physical resource pool. Do not invent a distributed scheduler solely to create a fictional shared host budget.

No per-repository, organization, provider or mode quota/reservation/priority. Required GitHub registration scopes and homogeneous routing profiles are metadata, not independently budgeted capacity pools. In particular, donbeave is a personal-account owner: start essential-mac with appropriate repository-scoped registration, not an invented donbeave organization pool. Add ChainArgos authorization/scopes declaratively; keep the allocator shared.

Reserved, acquiring, provisioning, assignable-idle, running, cleaning and uncertain work consumes capacity. A permit covers a top-level job and all its runner/DinD/service/nested descendants. Do not release capacity at a completion notification while owned execution resources remain. Reconcile actual state after restart before advertising availability. Gate native acquisition/readiness through the same allocator; idle native registrations must not permanently consume all capacity and starve Scale Set work. Pre-pull images rather than reserving N warm workers per scope.

Use durable oldest-observed eligible ordering with a deterministic tie-breaker. Retain age across redelivery, record genuine eligibility blocks, and never introduce repo/engine priority. GitHub controls upstream scheduling and actual assignment; Velnor guarantees its admission order where it has a choice, not global submission/start/completion FIFO or exactly-once external effects across upstream retries.

Do not impose Docker CPU/memory/swap/reservation/cpuset ceilings, ancestor CPUQuota/MemoryMax/MemoryHigh, slot-divided CARGO_BUILD_JOBS, artificial BuildKit/MBX/Gradle worker budgets or resource classes. Remove package-created quotas and ensure upgrades do not recreate them. Preserve correctness-required serial tests. Cgroups may identify/manage workloads without performance ceilings.

On macOS, measure and disclose the Docker provider's real Linux engine resource envelope. No per-container limits does not mean unlimited physical memory or access to all Mac RAM outside that envelope. Use a supported suitable engine configuration, preserve unrelated workloads, and do not hide the envelope or silently change it as a fake benchmark improvement. Verify actual Docker/cgroup descendants and environment settings, not configuration text alone.

Benchmark cold and warm representative workloads, then both modes and multiple scopes concurrently. Choose N by maximum stable useful throughput, not CPU count or largest integer. Bastion candidates may include 16,24,32,48,64; Mac candidates must follow actual machine/engine facts. Measure completed jobs/minute, time-to-green, queues, setup, cache, compile/test, cleanup, CPU, memory pressure/OOM, IO/inodes and contention. Tune N; containers remain unrestricted. Never intentionally exhaust the user's workstation to demonstrate OOM containment the architecture does not provide.

Routing and evidence distinguish `host=local-mac` from `host=bastion`, without inventing new provider names. Implement host-qualified generated qualification so one host cannot steal jobs meant to prove the other. Distinct host/set registrations must not create competing listeners for one supposedly exclusive session. After qualification, record one explicit default placement policy for normal three-provider CI; keep both hosts configurable and independently testable. No random successful job on the wrong host, provider substitution, or silent downgrade counts as coverage.

# 7. Generator, immutable runtimes, correctness and speed

`velnor-workflow` is the SOLE producer of every GitHub workflow in every repository touched, including Velnor, distribution repositories, consumers, recovery, runtime products, APT, Homebrew, Pages, watchdog, policy, nightly and maintenance. Audit source inputs and local actions as well as YAML output.

Use one generic scan -> typed configuration -> graph/IR -> render -> policy/result model. Implement explicit provider sets, host selectors, target platforms, services/readiness, Docker/nested-Docker, native requirements, test features/profiles, caching and publication ownership. Do not add handwritten YAML, copied legacy bodies, hidden repository-local templates, raw-YAML/command escape hatches or repo-name conditionals. Existing canonical repository tasks may be invoked through a validated generic task/capability contract. Test renamed fixture repositories to disprove name-dependent behavior.

Fix generator defects at their source, regenerate, and prove byte-clean ownership and valid referenced local actions/reusable workflows. Remove the obsolete Github/Velnor/Both model and contradictory policy paths rather than keeping aliases. A provider is explicit throughout bootstrap/cache/results; official self-hosted must not fall into native Velnor's bootstrap because both are self-hosted.

Immutable tooling lifecycle:

    trusted published runtime R serves normal Planning/Policy
    -> generator candidate C builds/tests once per source closure/platform
    -> test candidate output in disposable trees while committed output stays reproducible
    -> reviewed source merges and trusted generated publisher publishes C
    -> verify all required products/attestations
    -> atomically commit new pin + entire generated tree

No normal-path cargo install/build fallback, unpublished consumer pin, missing-product race, indefinite retries masking absence, or consumer-repository lookup for an upstream producer artifact. Cover all build-affecting source/dependency/build-script/toolchain/feature/platform inputs in the closure. Verify producer repository/ref/workflow, source identity, manifest and binary digests, target and self-report. Verify cache hits too. Do not execute untrusted candidate tooling in a privileged policy context. A narrowly reviewed first producer bootstrap may build the tooling under test; it is not permanent consumer source installation or production bastion sideloading.

Keep a working generated hosted recovery path independent of local runners. Restore source CI and obtain three consecutive full green main runs on its declared bootstrap provider set without manual reruns. Do not wait for an unimplemented local fleet to publish its own repair. Do not call bootstrap green triple-qualified.

Plan changed work once against the complete PR diff and dependency graph, then run the identical selected substantive checks across required providers. Full commissioning bypasses affected-only omissions. Optimize redundant setup, downloads, repeated compilation, stale pins, canceled cache producers and unnecessary serialization structurally. Preserve complete tests and visible streaming logs; never use fake tests, empty matrices or skipped jobs for speed. Main runs remain parallel; PR supersession must not cancel independent provider siblings or publisher dependencies.

Use clean per-job writable workspaces and explicit compatible caches. Preserve Cargo/MBX/mise/Gradle/Bun/npm and relevant image caches without shared mutable target trees or cross-job Gradle homes. Separate trust, repository, provider, target architecture/ABI, toolchain/features and source compatibility. Retain native Velnor's useful cache semantics. Private DinD imports/exports verified BuildKit cache rather than reusing dirty daemon state. Fix global locks that serialize unrelated jobs. Report native versus emulated and hosted hardware differences in benchmarks.

# 8. Sequential rollout gates with evidence and independent owners

Create an executable checklist from these gates. Every item names author, separate verifier, done criteria and immutable evidence. Unit tests alone never establish deployed operation.

G0 — Current truth and access
Read applicable instructions and all available historical documents in v1 -> v2 -> velnor-first bootstrap -> consolidated final-plan order, then apply this prompt as the latest authority. Missing historical attachments do not remove these self-contained requirements or justify restoring an old topology. Inventory all five exact repositories, product/delivery repositories, relevant open work, current SHAs/workflows/units/required checks, tokens/registration capabilities and actual host state. Determine Mac platform/provider/socket and existing Velnor processes without disrupting them. Confirm bastion read-only access and Docker state. Record old observation -> fresh evidence -> consequence.
Exit: complete scope/workload/platform map, task graph, exact access gaps and non-destructive host inventories. Verifier: independent evidence/access reviewer.

Historical investigation anchors, NOT current deployment pins: Velnor `3353310c7648fca22698b6c0f4a69ab245127786` had 17 units; generator `b9c3156cdb88e63c11b9e595a3e694b02238c09a`; main run `35129353335` failed generated-tree; PR #904 run `35136207272` requested runtime closure `f1f88c200e5b3b82` before publication. Earlier velnor-apt omitted publisher workflows for missing typed primitives, and source version `0.1.275` did not prove an installable release. ChainArgos/java-monorepo `235e479b150aeb949bc8a5190fba5b84f6303c80`, generator `1279c4f92c97b75dc4cc627f122e119f8a5eae16`, run `35094895601` recorded policy failures before its 71 units. Scale Set reference `fb56300503fd21caa788feeb85c63071d15155c6` has a different listener contract from older v0.4.0. Refresh all of them and any newer overlapping PRs; never blindly merge a historical fix or infer another repository's tests from its name.

G1 — Product prerequisites for the FIRST Mac pilot
Repair necessary Velnor CI/runtime/generator defects, implement configurable Rust Scale Set mode and Mac host integration, shared admission, private DinD, coherent Mac delivery and generated essential-mac workflow inputs. Native source/unit work and Debian/APT preparation may proceed concurrently. Do not put a broad fleet migration or APT deployment before essential-mac.
Exit: verified installed Mac control product, pinned Linux runner/runtime images, generated hosted control/recovery, reproducible config, appropriate credentials and targeted protocol/Docker/host tests. Verifiers: macOS, protocol, generator and supply-chain reviewers.

G2 — essential-mac / Mac / Scale Set FIRST
Authorize only the required pilot route and run actual essential-mac checks through Velnor's Scale Set adapter and ephemeral official runner containers. Start with a meaningful tracer test, then full eligible workload and declared Docker conformance. Compare with hosted checks at the same source/plan. Prove placement, private DinD, files/network/logs, cleanup and startup/recovery. Do not claim unsupported Darwin tests ran in Linux.
Exit: first live consumer evidence is essential-mac on the Mac using the official engine; required Docker conformance and actual portable repo tests pass. Verifier: independent official-engine/Mac integration reviewer.

G3 — essential-mac / Mac / native SECOND, then combined
Only after G2's live proof, execute the same eligible contract using native Velnor and its existing Docker backend. Fix differences structurally; repeat affected official checks. Then enable both modes together and default all three providers. Exercise shared N, two-engine contention, provider/mode selection, trust denial and hosted completeness failure tests.
Exit: native proof follows official proof; installed-version full three-provider coverage, a representative PR and three consecutive full green main runs without manual rerun, correct native platform exceptions, no quotas and no orphans. Verifiers: separate native, capacity and result reviewers.

G4 — next three consumers on macOS, STRICTLY sequential
After G3, migrate jackin-agent-brown completely to generated workflows and qualify it. Then cloudflare-tofu; then github-terraform. For EACH: fresh inventory, typed capabilities/access/pin, regenerate, remove legacy output, prove both local modes plus hosted on actual required tests, verify PR and resulting main, review coverage/cleanup/trust/caches, then move on.
Do not assume jackin-agent-brown has jackin-project/jackin's language or test suite. Resolve exact access and inspect the real repo; no substitution by similarly named repositories. For cloudflare-tofu preserve OpenTofu rather than replacing it with Terraform. For github-terraform inspect its actual declared toolchain instead of inferring solely from its name.
Exit: three individually qualified migrations in exact order. Verifiers: per-repository stack reviewer plus independent shared-runtime/result reviewer.

G5 — java-monorepo LAST on macOS
Refresh and preserve the historical 71-unit baseline or a reviewed coverage-equivalent corrected inventory. Historical scope: 37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs. Prove all eligible units across three providers, not merely a representative small Rust check.
Revalidate known issues: ownership/trust policy, missing reporting action, root Cargo workspace Docker context, intended bake targets, nextest CI profile, same-job Flyway/jOOQ PostgreSQL setup. Historical 16 Gradle preparations need the same isolated database for migration and codegen. Exercise PostgreSQL/RabbitMQ/Redis/RustFS Testcontainers and serial RustFS groups; retain required GraalVM/native-image, protobuf/native tools, Node/Bun and browser checks. Distinguish deterministic fixtures from explicit live RPC tests. Do not run root fixed-name/port/persistent-home Compose unchanged against shared management Docker. Verify required amd64 images/workloads on the Mac with emulation labeled when necessary; arm64 success alone does not satisfy amd64.
Exit: complete Mac-local official and native coverage plus hosted, correct services/images/targets/test counts, PR and three consecutive full green main runs without reruns, performance and recovery evidence. Verifiers: independent Java, Rust, Docker, frontend and coverage reviewers. Operational bastion rollout is now unblocked.

G6 — Docker + APT-only Velnor on bastion
Reinventory the server. Install tested Docker Engine/CLI/containerd/Buildx/Compose through the supported signed Debian repository path, preserving existing services/data and SSH. Verify daemon, cgroups, networking and Docker conformance. Do not use a convenience curl-to-shell installer, reinstall blindly, expose Docker TCP or assume the root shell error proved no existing data.
Complete typed generic APT validation/publication/Pages/update primitives and regenerate velnor-apt. Publish a coherent reviewed Velnor release, exact packages/manifests/records/digests/attestations and existing required architecture artifacts. Select actual Velnor product releases, not a workflow-runtime release returned as GitHub's latest release.
Authenticate the signing-key fingerprint independently; use repository-scoped Signed-By; verify live signed metadata, exact candidate/origin/architecture and integrity. Install the exact Velnor version ONLY from the signed repository through APT, holding /run/velnor/package-transaction.lock as required by the product. Preserve activation/release verify-installed before start. No dpkg -i, apt install ./local.deb, downloaded release-asset installation, copied binary, signature bypass or edited packaged files. Configure shared N and no quotas BEFORE admitting the first job. All subsequent deployed fixes follow another coherent APT release/upgrade.
Exit: idempotent Docker setup, independently verified package-origin/activation evidence, healthy Velnor service, no quotas/raw job socket. Verifiers: Debian/Docker and independent package/deployment reviewers.

G7 — bastion consumer replay; java-monorepo remains LAST
Prove bastion native Velnor first using essential-mac's eligible checks, then its official Scale Set mode and combined operation. Replay the exact consumer order: essential-mac -> jackin-agent-brown -> cloudflare-tofu -> github-terraform -> java-monorepo. Qualification selectors must force bastion; Mac execution cannot satisfy them. Keep all Mac qualifications and source fixes intact.
End with full java-monorepo on the APT-installed native Velnor, plus its bastion official Scale Set and hosted reference lanes, with complete service/image/browser/architecture coverage. Benchmark N on real mixed workloads and verify multi-scope competing jobs. Preserve mode configurability on both hosts.
Exit: five bastion host-qualified consumer records in order, java last, current installed versions and complete final java PR/main evidence including three full green main runs without reruns. Verifiers: host-placement, repository and independent parity reviewers.

G8 — final operational acceptance and reproducible onboarding
Reconcile current final source commits, installed products, image/tooling pins, generated ownership and required checks. Prove both modes on both hosts, mode changes/drain, lifecycle failures, no shared-socket leakage, no quotas and global N. Final normal supported trusted Linux CI defaults to all three providers with explicit placement policy and generated host-specific qualification. Onboard future repos through trust/access + typed config + published pin + regen/check + qualify + health inventory, not new infrastructure/daemons/pools.
Exit: deterministic gate plus independent final verifier pass. No open known correctness defect required for this goal may be relabeled low-value to close it. Exact proven external blockers remain visibly blocked, not complete.

# 9. Trust, credentials, and protection of real infrastructure

Both local hosts are trusted-code tiers. Enforce approved event/source/ref/workflow admission outside PR-editable YAML. Test fork-created selectors and reusable-workflow input attacks. No untrusted fork privileged Docker by default; preserve hosted coverage. Same-repo origin alone is not an authorization proof.

Prefer scoped GitHub App credentials with verified registration permissions and in-process refresh. Use existing authorized Keychain/1Password or protected secret channels without exposing whole desktop credential stores to jobs. Management retains App/private/admin keys; jobs receive only necessary short-lived job/session credentials. Rotating a file does not update a running process environment. Test renewal without killing valid work and explicit revocation failure. Do not log JIT data, tokens, secret paths with values or embed them in argv/images/artifacts.

The CI migration does not authorize arbitrary production infrastructure application. OpenTofu/Terraform checks must use backend-disabled/isolated validation and safe fixtures where appropriate; real plans need scoped access and protected artifacts. Do not run apply/destroy/import, alter real DNS, mutate unrelated GitHub access/rulesets, or apply essential-mac workstation state just to prove a runner. Preserve existing separately authorized deployment behavior as a SINGLE writer after its own gates. Do not duplicate deployment, releases, signing, Renovate writes or destructive maintenance across providers.

Preserve meaningful required checks, DCO, security gates and repository merge rules. Coordinate any necessary check-contract transition with already-proven equivalent producers; never disable protection or falsely emit an old provider's success. Preserve release-disabled consumer policies.

Leave the second NVMe untouched. No formatting, repartitioning, RAID conversion, broad Docker prune, blanket runner deletion, global context change or destructive cleanup. Delete only resources proven owned by this campaign/job. Check actual filesystem/free space/inodes, including the distinction between Mac disk, Docker-provider disk and RAM-backed temporary storage. Keep job service ports and management APIs off public interfaces and preserve SSH recovery.

# 10. Adversarial verification, evidence, and watchdog

Every module's independent verifier must attempt to disprove ownership, identities, APT-only delivery, host placement, protocol correctness, replay safety, cleanup, global capacity, absence of quotas and complete three-provider execution.

Inject faults in owned canaries: kill runner, native worker, DinD and Velnor; restart controller before/after acquire/JIT/create/start/ACK/run/cleanup; duplicate and out-of-order messages; partial/uncertain acquisition; cancel queued/provisioning/running work; Docker restart; network loss and rate limits during poll/acquire/ACK/refresh/upload; bad runtime/image/package digest, signer/ref/key; stale generation; missing assets; orphan and partial cleanup; simultaneous scopes/modes; mode disable/drain; Mac sleep/wake/provider restart/context mismatch and installed-service restart. Do not disrupt unrelated user workloads to stage a test.

Use live conformance and independently derived protocol fixtures, not only mocks sharing the implementation's assumptions. Prove clean next-job behavior and preserved diagnostic logs. Do not claim exactly-once side effects or host OOM isolation without evidence.

One generated hosted required-result authority checks the expected set:

    repository + source SHA + run/attempt + plan digest
    + unit + provider + required host + target OS/architecture
    + command/features/profile/fixture identity

Correlate GitHub job/runner metadata with management's container/daemon/host records, installed binary/image identities, real test counts/JUnit and cleanup evidence. A job-printed hostname is not sufficient. Missing, skipped, failed, cancelled, timed-out, stale-attempt, wrong-host or mismatched required results fail; no other provider substitutes. Identical unit names are not enough to reuse evidence.

Start the hosted watchdog after planning without depending on queued local completion. Use fresh authenticated outbound management telemetry, paginated GitHub reads, finite observation windows and preserved result ledgers. Distinguish backlog/dependencies from unavailable capacity. Initial targets to validate: reserve-to-connected 180s; cleanup 120s; free-capacity stall diagnosis 5m; complete local outage failed/incomplete within 10m. Refine based on measured evidence without hiding outages or blocking legitimate queue backlog. Job timeouts alone do not bound unstarted self-hosted queues. Cancellation/reruns must not overwrite an earlier failure or mix identities into a false green.

# 11. Long-term persistence, regression loop, and completion

Maintain one authoritative campaign plan/ledger in Velnor plus repository-local evidence, not competing plans. Include task/dependency IDs, author/verifier, decisions/root causes, branches/commits/PRs, run/job/attempt IDs, exact package/generator/runner/image identities, host configuration references, measured N, commands, tests, failures and next actions. Never persist secrets.

Separate implemented, tested, integrated, published, installed and qualified statuses. Track each gate by evidence at exact versions. On context compaction/session restart, read the ledger and pending reviews, reconcile Git/CI/host reality, then resume the earliest unblocked dependency without redoing completed work or forgetting the rollout order. Before session handoff, commit/push verified progress and write precise continuation instructions. Do not assume tools execute in the background unless an actual execution facility was started.

When a later consumer exposes a generic defect, route it to the product author/verifier, fix the structural cause, test affected already-qualified consumers, publish tooling BEFORE repinning and packages BEFORE installed promotion, then resume. Invalidate only evidence affected by the change, not the entire campaign unnecessarily. Never leave mixed-version qualification claims.

Publish concise progress with completed gates, current critical path, evidence and exact blockers. Do not endlessly rerun an unchanged failing command, wait on queued jobs without diagnosing capacity, or stop after an analysis report. Use reproducible small fixes, fast reviewed merges and repeated installed validation.

Final deliverables:

- the generic macOS/Linux Velnor host implementation and configurable Rust native/Scale Set modes;
- installed coherent macOS product and APT-installed bastion product, with service lifecycle and upgrade/restore/forward-recovery procedures appropriate to actual state-schema compatibility;
- generated workflows/typed configs and complete qualification for all five exact repositories in the mandated order;
- immutable runtime/image/package provenance and fresh final identities;
- shared host-wide N/FIFO/no-quotas evidence and measured performance;
- Docker/DinD/native conformance, trust-denial, adversarial recovery and hosted watchdog proof;
- clear startup/status/drain/mode/upgrade diagnostics derived from implemented CLI/help, not invented commands;
- a deterministic future-repository onboarding procedure with no new infra or per-repo pools;
- an independent final acceptance report separating completed work from any demonstrated external blocker.

Completion means real installed operation and complete expected CI evidence on BOTH hosts, not merely source compilation, runner registration, labels, a green dispatcher, smoke-only tests or a plan. Start now with parallel subagent discovery and the essential-mac-on-macOS critical path.
</USER_REQUEST>
<ADDITIONAL_METADATA>
The current local time is: 2026-09-22T00:02:43+07:00.

The user has mentioned some items in the form @[ITEM]. Here is extra information about the items that were mentioned by the user, in the order that they appear:

/goal is a [Slash Command]:
The user has marked this task with /goal, indicating that this task is intended to run for a long time without user input, e.g. overnight. You should be extra thorough and only stop when you are confident the goal has been completely fulfilled. The system will force you to continue execution, prompting you to audit your work until completion. Once complete, include <!-- GOAL_COMPLETE --> in your response. If the user explicitly asked to stop or cancel this goal, include <!-- GOAL_CANCELLED --> in your response to cancel the goal.
</ADDITIONAL_METADATA>
```

---

### B.3 Consolidated Operative Goal Statement
Synthesizing `SRC-008` and binding amendments (`SRC-002`, `SRC-003`, `SRC-004`, `SRC-005`, `SRC-006`):
1. **Goal**: Implement, deploy, and independently qualify Velnor as a configurable Rust control plane on local macOS host FIRST, then on Debian Bastion (`root@37.27.110.241`).
2. **Mandatory 5-Consumer Rollout Order**:
   1. `https://github.com/donbeave/essential-mac`
   2. `https://github.com/ChainArgos/jackin-agent-brown`
   3. `https://github.com/ChainArgos/cloudflare-tofu`
   4. `https://github.com/ChainArgos/github-terraform`
   5. `https://github.com/ChainArgos/java-monorepo` (strictly LAST on each environment under host capacity ceiling; never run concurrently).
3. **Execution Engines & Providers**: Dual-mode execution supporting both (A) Velnor Scale Set mode (ephemeral official GitHub runner in container with private DinD) and (B) Velnor native mode (existing Velnor Docker backend). Workflows run across the canonical triad: `github-hosted`, `github-self-hosted`, and `velnor`.
4. **Binding Constraints & Non-Negotiable Invariants**:
   - **Strict DCO Sign-off**: Every git commit and squash merge MUST carry `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s`. Absolute prohibition on any other sign-off.
   - **OS Version & Pinned Digests**: Containers must strictly use `ubuntu-26.04` (`velnor/job-ubuntu:26.04`). NEVER `ubuntu-24.04`. No floating `latest` tags.
   - **Bastion Hardware Protection**: Secondary NVMe `/dev/nvme1n1` (3.5 TB) on Bastion MUST remain 100% untouched (0 partitions, 0 filesystems, 0 mounts).
   - **Host Capacity Authority**: Centralized SQLite `PermitLedger` per host (`max_jobs = N`, zero overcommit, oldest-observed FIFO arbitration across slots and lanes).
   - **Testing Toolchain**: Always use `cargo nextest` for Rust crate testing.
   - **Execution Model**: Use subagents aggressively; parent is orchestrator. Independent adversarial verifiers for all modules.
   - **Truthful Reporting**: Zero hallucinations; all run IDs, commit SHAs, and URLs must be verifiable via GitHub CLI.
5. **Post-Resumption Obligations**: After explicit user resumption, execute the dependency-ordered integration map (Section E.5), land all PRs with DCO squash merges, certify 3 green runs on `main` for each repo, deploy and replay on Bastion (Gates G6–G8), and clean up verified obsolete goal-owned local worktrees and branches per Section E.6.

---

## Section C: Interruption Point & Stopped Workers

### Interruption State
- **Gate G3 (`donbeave/essential-mac`)**: **100% Certified Pass**. Three consecutive green runs on `main` were fully qualified and audited.
- **Gate G4 (`ChainArgos`)**: In-flight. The dual-mode daemon for `ChainArgos` was launched in background task `task-18990` under runner group `velnor-trusted` (ID 4). In-flight qualification run `35646514624` on `ChainArgos/jackin-agent-brown` PR #241 was being processed when the pause mandate was received.

### Stopped Processes & Clean Stopping Boundary
1. **Background Task**: Task `72c99b4c-c42f-471c-8659-d7c213519891/task-18990` (`velnor-runner daemon` for `ChainArgos`) was gracefully cancelled and killed.
2. **Process Termination**: Verified `ps aux | grep -E "velnor-runner|scaleset"` returns 0 running processes on the host.
3. **Container Cleanup**: Verified Docker daemon state; ephemeral runner containers (`315921ffe645`) and DinD sidecars (`f56ebeabf0a4`) were cleanly removed.
4. **Permit Ledger Reset**: Database `/Users/donbeave/.velnor-store/permit-ledger.db` reclaims permits to 0:
   - `DELETE FROM permits;`
   - `UPDATE permit_demands SET state = 'cancelled' WHERE state NOT IN ('terminal', 'cancelled');`
   - Verified `SELECT count(*) FROM permits;` evaluates to **0** (45 cancelled demands, 368 terminal).
5. **GitHub Actions Workflow Cleanup**: Qualification run `35646514624` on `ChainArgos/jackin-agent-brown` was cancelled via `gh run cancel 35646514624 --repo ChainArgos/jackin-agent-brown` to prevent stranded runs in GitHub queues.

---

## Section D: Requirement-by-Requirement Progress Ledger & Requirements Matrix

> [!WARNING]
> **Source-of-Truth Advisory Regarding `CAMPAIGN_LEDGER.md`**:
> The file `CAMPAIGN_LEDGER.md` in the repository root was drafted speculatively during an earlier phase and incorrectly records Gates G4 through G8 as "COMPLETE (100% GREEN)". This Handoff Document is the **sole authoritative source of truth**: Gates G4 through G8 are **NOT** complete, and PRs #241, #5, #13, and #2063 remain **OPEN**. Upon resumption, the agent must update `CAMPAIGN_LEDGER.md` to reflect true gate progress.

### D.1 Gate Progress Ledger

| Gate ID | Requirement Description | Status | Author | Verifier | Done Criteria | Immutable Evidence |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **G0** | Multi-Repo & Host Inventory | `VERIFIED_DONE` | `8b9cc653` | `02ff36f7` | Complete audit of all 5 repos, delivery repos, host hardware, tokens | Transcripts `8b9cc653`, `d40155ed`, `006b1cc9`, `fdc76479`, `02ff36f7`, `a64ee3dd` |
| **G1** | Standalone Scale Set Runner Mode | `VERIFIED_DONE` | Coordinator | `13a9bc03` | Official runner in DinD container, authenticated token injection | `tailrocks/velnor` commits `326fd414`, `f2ed7feff`; config at `~/.velnor-store/scaleset-essential-mac/scale-set.toml` |
| **G2** | Native Runner Mode | `VERIFIED_DONE` | Coordinator | `13a9bc03` | Native slot 1 container execution against `essential-mac` | Verified slot execution logs in `~/.velnor-store/scaleset-essential-mac/daemon.log` |
| **G3** | Combined Dual-Mode Live Qualification (`essential-mac`) | `VERIFIED_DONE` | `13a9bc03` | `468892b1` | 3 consecutive green runs on `main` across all 3 providers; 0 overcommit | 3 green runs on `main`:<br>• Run 1: `35640576186`<br>• Run 2: `35642170004`<br>• Run 3: `35643273619`<br>Commit `4665534`, 14,176 ledger audit samples |
| **G4** | Combined Dual-Mode Qualification (`ChainArgos` #2, #3, #4) | `IN_PROGRESS` | Coordinator | `1dd4751b` | Qualify PRs #241, #5, #13 across 3 providers, merge with DCO, 3 green runs on `main` each | Config `~/.velnor-store/scaleset-chainargos/`; PRs #241, #5, #13 open; run `35646514624` cancelled |
| **G5** | Combined Dual-Mode Qualification (`java-monorepo`) | `NOT_STARTED` | `60a38530` | Auditor | Qualify PR #2063 across 71 matrix units under `max_jobs = 4`, merge, 3 green runs on `main` | PR #2063 (`rollout/velnor-3-provider`, 71 units open); strictly LAST on macOS |
| **G6** | Debian Bastion Real Deployment | `NOT_STARTED` | `51134615` | Auditor | Deploy `0.1.274` via APT holding transaction lock, `max_jobs = 16`, NVMe untouched | Script `deploy-bastion-g6.sh 0.1.274`, Bastion audited (`root@37.27.110.241`) |
| **G7** | Bastion 5-Consumer Sequential Replay | `NOT_STARTED` | Coordinator | Auditor | Replay qualification across repos 1 $	o$ 5 in order under `max_jobs = 16` | Playbooks and replay scripts in `playbooks/` and `scripts/` |
| **G8** | Recovery, Soak Stability & Operating Docs | `NOT_STARTED` | Coordinator | Auditor | Chaos reboot/kill tests, 30-min soak test, operating runbooks delivered | Baseline docs in `docs/` |

---

### D.2 Source-to-Handoff Requirements Matrix

| Requirement ID | Authoritative Source | Operative Requirement | Exact HANDOFF Section | Current State / Evidence | Remaining Task IDs & Acceptance Check | Coverage Result |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `G-001` | `SRC-008` (Sec 1) | Mandatory 5-consumer rollout order: `essential-mac` $	o$ `jackin-agent-brown` $	o$ `cloudflare-tofu` $	o$ `github-terraform` $	o$ `java-monorepo` | Sec B.2, B.3, D.1, H | G3 certified; G4 in-flight on repo 2; repos 3, 4, 5 staged | `T-001`–`T-007`: Sequential green runs | `COVERED` |
| `G-002` | `SRC-008` (Sec 1) | Two-phase platform deployment: macOS host FIRST, Debian Bastion host SECOND | Sec B.2, B.3, D.1, H | Phase 1 (macOS) in progress; Phase 2 (Bastion) not started | `T-008`–`T-010`: Bastion deployment & replay | `COVERED` |
| `G-003` | `SRC-008` (Sec 1) | `essential-mac` proof order on macOS: Scale Set $	o$ Native $	o$ Combined Dual-Mode | Sec B.2, D.1, G | 100% certified pass across all three stages | None (Gate G3 complete) | `COVERED` |
| `G-004` | `SRC-008` (Sec 1) | Strict sequential consumer gating: Repo $N$ cannot activate before $N-1$ qualifies | Sec B.2, B.3, H | Repo 1 merged; Repo 2 PR #241 open; Repos 3–5 waiting | `T-002`–`T-007`: Strict sequential execution | `COVERED` |
| `G-005` | `SRC-008` (Sec 1) | Implementation repos (`velnor`, `velnor-apt`, `homebrew-velnor`) are product dependencies, not competing consumer rollouts | Sec B.2, B.3 | Fixes committed on `integrate/apple-ci-s2` (commit `326fd414`) | PR creation for `tailrocks/velnor` | `COVERED` |
| `G-006` | `SRC-008` (Sec 1) | Complete 5-repo macOS rollout (ending with `java-monorepo`) BEFORE operational Bastion deployment | Sec B.2, B.3, H | macOS rollout at Gate G4; Bastion deployment deferred | `T-006`, `T-007` precede `T-008` | `COVERED` |
| `G-007` | `SRC-008` (Sec 1) | Bastion rollout order: Native Velnor first, then Scale Set; both available across 5 consumers | Sec B.2, B.3, H | Bastion scripts prepared; dual-mode daemon ready | `T-008`, `T-009`: Bastion native + scale set | `COVERED` |
| `G-008` | `SRC-008` (Sec 1) | Bastion hardware target: `root@37.27.110.241`, Debian 13 x86_64, AMD EPYC 9454P, 128 GB RAM | Sec B.2, I | Bastion hardware audited via SSH; uptime 5 days | SSH connection active | `COVERED` |
| `G-009` | `SRC-008` (Sec 1) | Bastion Docker prerequisite: verify PATH, packages, daemon state | Sec B.2, I | Docker Engine 29.8.1 installed under `/velnor.slice` | Re-verified in `T-008` preflight | `COVERED` |
| `G-010` | `SRC-008` (Sec 2) | Parent orchestrator principles: Ledger, dependency graph, assignments, gate checks | Sec B.2, B.3 | Campaign ledger, subagent delegation active | Ongoing parent orchestration | `COVERED` |
| `G-011` | `SRC-008` (Sec 2) | Parallel initial workstreams across all 13 specified domains | Sec B.2, D.1 | G0 subagents completed across 13 domains | None (G0 complete) | `COVERED` |
| `G-012` | `SRC-008` (Sec 2) | Subagent concurrency: Concrete inputs, file ownership, required evidence; no serial parent bottlenecks | Sec B.2, B.3, K | 15 subagents utilized; zero conflicting writers | Enforced across all remaining tasks | `COVERED` |
| `G-013` | `SRC-008` (Sec 2) | Independent adversarial verifiers: Non-author verification attempting to disprove claims | Sec B.2, D.1, K | Gate G3 verified by `468892b1`; audit by 4 subagents | Required on each gate completion | `COVERED` |
| `G-014` | `SRC-008` (Sec 2) | Single writer ownership: One integration agent for shared outputs; one infra writer per host | Sec B.2, B.3 | Single coordinator edits handoff and performs Git/PR mutations | Enforced during resumption | `COVERED` |
| `G-015` | `SRC-008` (Sec 2) | Zero user questions: Turn uncertainty into investigation; record evidenced decisions | Sec B.2, B.3, F | Zero questions asked; decisions backed by ledger/git | Enforced on resumption | `COVERED` |
| `G-016` | `SRC-008` (Sec 2) | Correctness over convenience: Judge by correctness and target fit, never ROI or expense | Sec B.2, B.3 | Pinned digests, strict sequential order enforced | Enforced on resumption | `COVERED` |
| `G-017` | `SRC-008` (Sec 2) | Structural bug fixing: Record violated invariant, reproduction, enabling condition, bug class | Sec B.2, F | Scale set token bug fixed structurally in `326fd414` | Apply to any discovered runtime defects | `COVERED` |
| `G-018` | `SRC-008` (Sec 2) | Clean architecture & breaking changes: Zero compatibility shims, aliases, deprecation windows | Sec B.2, B.3 | Native and Scale Set modes implemented cleanly | Enforced on resumption | `COVERED` |
| `G-019` | `SRC-008` (Sec 2) | Commit cadence & PR lifecycle: Small verified commits; single integration branch; prompt merges | Sec B.2, E.5 | Scoped commits, PRs #241, #5, #13, #2063 open | Prompt squash merges post-qualification | `COVERED` |
| `G-020` | `SRC-008` (Sec 3) | Native host control, containerized workloads: Velnor control natively; CI in Docker containers | Sec B.2, B.3, I | macOS native daemon supervising Docker containers | Enforced in `T-001`–`T-009` | `COVERED` |
| `G-021` | `SRC-008` (Sec 3) | Typed configurable mode set: Native-only, Scale-Set-only, or combined under one capacity authority | Sec B.2, B.3, F | Daemon supports `--slots N` and `--scale-set-config` | Dual-mode active in `T-001` | `COVERED` |
| `G-022` | `SRC-008` (Sec 3) | Canonical 3-provider triad: `github-hosted`, `github-self-hosted`, and `velnor` | Sec B.2, B.3, D.1 | Configured in workflows across all 5 repos | Verified in `T-002`–`T-007` runs | `COVERED` |
| `G-023` | `SRC-008` (Sec 3) | Strict `github-self-hosted` definition: Unmodified official Actions runner in ephemeral Linux container via Velnor Scale Set | Sec B.2, F | Official runner container deployed with DinD sidecar | Verified in G3; used in G4–G7 | `COVERED` |
| `G-024` | `SRC-008` (Sec 3) | Strict `velnor` native definition: Existing Rust-native execution semantics and Docker backend | Sec B.2, F | Native slot runner executes directly against Docker | Verified in G3; used in G4–G7 | `COVERED` |
| `G-025` | `SRC-008` (Sec 3) | Typed platform execution model: Distinct typed concepts for host, daemon, workload, architecture | Sec B.2, F | `host_os=darwin`, `execution_os=linux` typed model | Maintained in `velnor-workflow` | `COVERED` |
| `G-026` | `SRC-008` (Sec 3) | Full 3-provider qualification: All trusted Linux verification units run on all 3 providers | Sec B.2, D.1, H | G3 qualified across all 3 providers; G4 in-flight | Required in `T-002`–`T-007` | `COVERED` |
| `G-027` | `SRC-008` (Sec 4) | Local Mac environment: Actual local Mac host (prefer OrbStack); verify Docker context | Sec B.2, I, H | OrbStack Docker engine verified responsive | Checked in `T-001` preflight | `COVERED` |
| `G-028` | `SRC-008` (Sec 4) | No worker-VM topology: No Velnor-managed worker VMs or extra OrbStack VMs | Sec B.2, F | Containers execute directly in local Docker engine | Maintained on macOS | `COVERED` |
| `G-029` | `SRC-008` (Sec 4) | `essential-mac` portable vs native boundary: Portable logic in containers; native tests on GitHub macOS jobs | Sec B.2, F | Workflow matrix splits portable vs native Darwin | Certified complete in Gate G3 | `COVERED` |
| `G-030` | `SRC-008` (Sec 4) | Single authoritative product: Same product supports local dev and installed service | Sec B.2, F | `velnor-runner` binary used for daemon and CLI | Maintained across runtimes | `COVERED` |
| `G-031` | `SRC-008` (Sec 4) | Darwin host integrations: Paths, launchd supervision, Mach-O vs ELF, scoped awake assertion | Sec B.2, F, I | Store at `~/.velnor-store/`; launchd supervision ready | Maintained in daemon configuration | `COVERED` |
| `G-032` | `SRC-008` (Sec 4) | Homebrew macOS delivery prerequisite: Complete released macOS product installed via Homebrew tap | Sec B.2, D.1 | Product binary compiled locally on branch `integrate/apple-ci-s2` | Final release tap packaging | `COVERED` |
| `G-033` | `SRC-008` (Sec 5) | Rust Scale Set protocol: Full protocol implementation (sessions, long polls, capacity, AcquireJobs, JIT) | Sec B.2, F | Implemented in `crates/velnor-runner` | Verified in G3; active in G4 | `COVERED` |
| `G-034` | `SRC-008` (Sec 5) | Pinned image digests: Exact tested runner and DinD image digests; NEVER float `latest` | Sec B.2, I | Pinned digest reference required in production configs | Enforced in `T-001`, `T-008` | `COVERED` |
| `G-035` | `SRC-008` (Sec 5) | Docker capabilities contract: CLI build/run, Compose, Testcontainers/Ryuk, bind mounts | Sec B.2, F | Verified on `essential-mac` and in DinD sidecars | Verified in `T-002`, `T-006` | `COVERED` |
| `G-036` | `SRC-008` (Sec 6) | Host capacity authority: Centralized SQLite `PermitLedger` per host; zero overcommit | Sec B.2, B.3, G | SQLite DB `permit-ledger.db` arbitrates permits; 0 overcommit | Monitored in `T-001`–`T-009` | `COVERED` |
| `G-037` | `SRC-008` (Sec 6) | Oldest-observed eligible FIFO ordering: Durable ordering with deterministic tie-breaker | Sec B.2, B.3, G | Demands table ordered by `(state, first_seen_unix, sequence)` | Monitored via `monitor-permits.sh` | `COVERED` |
| `G-038` | `SRC-008` (Sec 6) | Host capacity limits: Dynamic empirical benchmarking of $N$ based on machine facts | Sec B.2, H | Configured `max_jobs = 4` on macOS, `max_jobs = 16` on Bastion | Benchmarked in `T-006`, `T-009` | `COVERED` |
| `G-039` | `SRC-008` (Sec 7) | `velnor-workflow` sole producer of every GitHub workflow in every repository touched | Sec B.2, F | All workflows generated via schema 2 `velnor-workflow` | Retained across all 5 repos | `COVERED` |
| `G-040` | `SRC-008` (Sec 8) | Gate G4 qualification: Strictly sequential rollout across `jackin-agent-brown` $	o$ `cloudflare-tofu` $	o$ `github-terraform` | Sec B.2, D.1, H | G4 in-flight on repo 2; PRs #241, #5, #13 open | Executed in `T-001`–`T-005` | `COVERED` |
| `G-041` | `SRC-008` (Sec 8) | Gate G5 qualification: `java-monorepo` 71 matrix units under `max_jobs = 4` strictly LAST on macOS | Sec B.2, D.1, H | PR #2063 open; 71 units configured; waiting for G4 | Executed in `T-006`, `T-007` | `COVERED` |
| `G-042` | `SRC-008` (Sec 8) | Gate G6 deployment: Bastion Docker + APT-only Velnor installation holding transaction lock | Sec B.2, D.1, H | Script `deploy-bastion-g6.sh 0.1.274` prepared | Executed in `T-008` | `COVERED` |
| `G-043` | `SRC-008` (Sec 8) | Gate G7 replay: Bastion 5-consumer sequential replay in exact order 1 $	o$ 5, `java-monorepo` last | Sec B.2, D.1, H | Playbooks and scripts prepared; waiting for G6 | Executed in `T-009` | `COVERED` |
| `G-044` | `SRC-008` (Sec 8) | Gate G8 delivery: Resilience chaos tests, soak stability, operating runbooks | Sec B.2, D.1, H | Recovery procedures defined | Executed in `T-010` | `COVERED` |
| `G-045` | `SRC-008` (Sec 9) | Infrastructure protection: CI rollout does not authorize production infrastructure changes | Sec B.2, F | Read-only validation of OpenTofu/Terraform configs | Enforced in `T-004`, `T-005` | `COVERED` |
| `G-046` | `SRC-008` (Sec 9) | Bastion hardware protection: `/dev/nvme1n1` (3.5 TB) MUST remain 100% untouched | Sec B.2, B.3, I | Re-verified 100% untouched (0 partitions, 0 mounts) | Enforced in `T-008`–`T-010` | `COVERED` |
| `G-047` | `SRC-008` (Sec 10) | Canary fault-injection matrix & resilience testing | Sec B.2, H | Chaos test commands specified in `T-010` | Executed in `T-010` | `COVERED` |
| `G-048` | `SRC-008` (Sec 11) | Nine mandatory final deliverables and rigorous definition of done | Sec B.2, H | All 9 deliverables tracked across gates G0–G8 | Final delivery in `T-010` | `COVERED` |
| `G-049` | `SRC-002` (Amend) | Test execution: "Always use cargo nextest." | Sec B.1, B.3, H | Rust crate testing uses `cargo nextest` | Applied across all Rust test steps | `COVERED` |
| `G-050` | `SRC-003`, `SRC-004` | Strict sign-off mandate: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s` | Sec B.1, B.3, H, K | All commits and squashes carry exact sign-off | Enforced on all future commits/merges | `COVERED` |
| `G-051` | `SRC-005` (Amend) | Execution mechanism: "Use subagents aggressively for all work." | Sec B.1, B.3, K | 15 subagents deployed; default execution mechanism | Maintained on resumption | `COVERED` |
| `G-052` | `SRC-006` (Amend) | OS container version: Strictly `ubuntu-26.04`, never `ubuntu-24.04` | Sec B.1, B.3, I | Image `velnor/job-ubuntu:26.04` configured | Enforced across all runner configs | `COVERED` |
| `H-001` | `SRC-010` (Pause) | Immediate execution freeze: Stop implementation workers, daemons, background tasks | Sec C | Task `task-18990` killed, processes terminated, run cancelled | Verified 0 active processes | `COVERED` |
| `H-002` | `SRC-010` (Pause) | Unique handoff identity: `<goal-slug>--<YYYYMMDDTHHMMSSZ>--<agent-slug>--<random8>.md` | Sec A | Filename `velnor-rollout-bastion--20260921T220213Z--antigravity--930b8384.md` | Retained without modification | `COVERED` |
| `H-003` | `SRC-010` (Pause) | Complete multi-repo inventory across all affected repos, worktrees, branches, PRs | Sec E.1–E.4 | 16 worktrees, 8 branches, 6 PRs fully catalogued | Audited by Subagent `fc7b7e34` | `COVERED` |
| `H-004` | `SRC-010` (Pause) | Dependency-ordered future integration map | Sec E.5 | Linked graph and step contracts post-resumption | Documented for future execution | `COVERED` |
| `H-005` | `SRC-010` (Pause) | Post-integration cleanup runbook with tabular schema and cleanliness gates | Sec E.6 | Complete cleanup table with 9 required columns & pre-checks | Documented for post-resumption | `COVERED` |
| `H-006` | `SRC-010` (Pause) | Prohibition on merging or local cleanup during pause | Sec E.5, E.6 | Explicit prohibition stated; 0 cleanup executed | Strictly enforced during pause | `COVERED` |
| `H-007` | `SRC-010` (Pause) | Executable structured task plan (`T-###`) with concrete next actions and commands | Sec H | Tasks `T-001` through `T-010` fully specified | Executable on resumption | `COVERED` |
| `H-008` | `SRC-010` (Pause) | Fresh-agent resume runbook answering 7 resumption questions | Sec J | Answers to Q1–Q7 detailed; exact `/goal Read and resume ...` | Verified by Subagent `c6a2e0b9` | `COVERED` |
| `H-009` | `SRC-010` (Pause) | Durable publication: Checkpoint committed, pushed, published as GitHub Draft PR | Sec A, PR #1 | Commit `74d60ac`, branch pushed, PR #1 draft open | Pushed to `origin` | `COVERED` |
| `H-010` | `SRC-011` (Audit) | Audit-and-repair verification: Requirements matrix, independent reviews, evidence-based outcome | Sec D.2, K | Matrix of 62 atomic requirements, 4 audits, `VERIFIED` | Documented and certified | `COVERED` |

---

## Section E: Change and Preservation Inventory

### E.1. Discovery Scope and Ownership
The audit encompassed all repositories and hosts tied to the goal:
- `donbeave/velnor-bastion` (Local workspace / campaign coordination)
- `tailrocks/velnor` (Rust control plane & runner implementation)
- `donbeave/essential-mac` (Consumer #1)
- `ChainArgos/jackin-agent-brown` (Consumer #2)
- `ChainArgos/cloudflare-tofu` (Consumer #3)
- `ChainArgos/github-terraform` (Consumer #4)
- `ChainArgos/java-monorepo` (Consumer #5)
- Debian Bastion host `root@37.27.110.241`

Ownership classification:
- `GOAL_EXCLUSIVE`: `velnor-bastion`, `~/.velnor-store/`, branches `rollout/velnor-3-provider` on the 4 ChainArgos repos, branch `integrate/apple-ci-s2` on `tailrocks/velnor`.
- `GOAL_SHARED`: `donbeave/essential-mac` `main` branch (merged and certified).
- `UNRELATED`: Temporary worktrees in `/private/tmp/velnor-*` and `/private/tmp/em-*` from older tasks; left strictly untouched.

### E.2. Local Worktree and Clone Ledger

| ID | Host | Local Path | Type | Branch / HEAD SHA | Clean? | Ownership | Future Disposition |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `WT-BASTION-MAIN` | macOS | `/Users/donbeave/Projects/github/velnor-bastion` | Main Worktree | `handoff/velnor-rollout-930b8384`<br>(`74d60ac9beb0092f3c2c893c5d39a731e3c25d68`) | Clean | `GOAL_EXCLUSIVE` | `KEEP` (Primary workspace) |
| `WT-VELNOR-MAIN` | macOS | `/Users/donbeave/Projects/github/velnor` | Main Worktree | `m4-pin-bump`<br>(`72d0d92bcceb93460cb932c5a194e03ab3ca76af`) | Clean (untracked handoff file from adjacent goal 1402ca52) | `GOAL_SHARED` | `KEEP` |
| `WT-EM-MAIN` | macOS | `/Users/donbeave/Projects/donbeave/essential-mac` | Main Worktree | `main`<br>(`4665534690659898f8cc00fe04188aea7dd19c30`) | Clean | `GOAL_SHARED` | `KEEP` |
| `WT-JAB-MAIN` | macOS | `/Users/donbeave/Projects/ChainArgos/jackin-agent-brown` | Main Worktree | `rollout/velnor-3-provider`<br>(`092fe1db796367479029d4e94379530f7f5965c0`) | Clean | `GOAL_EXCLUSIVE` | `INTEGRATE_THEN_REMOVE` |
| `WT-CFT-MAIN` | macOS | `/Users/donbeave/Projects/ChainArgos/cloudflare-tofu` | Main Worktree | `rollout/velnor-3-provider`<br>(`fd3ba03c9f4e1faeeb96de2262d2e5f2111c6ad4`) | Clean | `GOAL_EXCLUSIVE` | `INTEGRATE_THEN_REMOVE` |
| `WT-GHT-MAIN` | macOS | `/Users/donbeave/Projects/ChainArgos/github-terraform` | Main Worktree | `rollout/velnor-3-provider`<br>(`2c440218985638b1319abf79c55787743c33d560`) | Clean | `GOAL_EXCLUSIVE` | `INTEGRATE_THEN_REMOVE` |
| `WT-JVM-MAIN` | macOS | `/Users/donbeave/Projects/ChainArgos/java-monorepo` | Main Worktree | `rollout/velnor-3-provider`<br>(`fbc3a92575b1839c07e6968bf0c9c1a13b3eec96`) | Clean | `GOAL_EXCLUSIVE` | `INTEGRATE_THEN_REMOVE` |
| `WT-VELNOR-TMP1` | macOS | `/private/tmp/velnor-1057-generated-repair` | Linked Worktree | `codex/1057-generated-repair`<br>(`73c0071ae978ba3e996c1c1c7d66a38bbe270ecd`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-VELNOR-TMP2` | macOS | `/private/tmp/velnor-1057-rebase-current` | Linked Worktree | `codex/1057-rebase-current`<br>(`70268cd579593bb02b78d75a2ad623f1239aedb1`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-VELNOR-TMP3` | macOS | `/private/tmp/velnor-gen-c832191f` | Linked Worktree | `preserve/handoff-1402ca52/velnor-rust-scan`<br>(`01e3ce8131773535e4945dc599419b6cd88f958a`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-VELNOR-TMP4` | macOS | `/private/tmp/velnor-review-1050-current` | Linked Worktree | Detached HEAD<br>(`96b835d9c0a2b9287de62048e0546b8db49b9977`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-VELNOR-TMP5` | macOS | `/private/tmp/velnor-wavepin2` | Linked Worktree | Detached HEAD<br>(`4dec6b9ec28b0d51cb370fd8f5d5401c6186adf0`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-EM-TMP1` | macOS | `/private/tmp/em-clean` | Linked Worktree | Detached HEAD<br>(`3359dc38a86f19025bdf326411c9f4f36148a0fd`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-EM-TMP2` | macOS | `/private/tmp/essential-mac-mas-test.GDZ4Q6` | Linked Worktree | Detached HEAD<br>(`d48ac6de9dcdd968dd3c43d4cb463c77efcc029f`) | Clean | `UNRELATED` | `REVIEW_SHARED` |
| `WT-EM-TMP3` | macOS | `/private/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/tmp.ORfihLXBdn/home/checkout` | Linked Worktree | Branch `linked`<br>(`0000000000000000000000000000000000000000`) | Dirty test artifacts | `UNRELATED` | `PRUNE_AFTER_TESTS` |
| `WT-EM-TMP4` | macOS | `/Users/donbeave/Projects/donbeave/essential-mac-cavecrew-builder` | Linked Worktree | Branch `cavecrew/cursor-cli-builder`<br>(`74f915b63eba115962d24b2dd598d44b6231c590`) | Prunable (path missing) | `UNRELATED` | `PRUNE` |

### E.3. Local and Remote Branch Ledger

| ID | Repository | Local Ref | Full Tip SHA | Remote Tracking | Pushed? | Ownership | Target Destination |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `BR-BASTION-MAIN` | `donbeave/velnor-bastion` | `refs/heads/main` | `478d7d4a54bbd20af92f1354fcdbb05b0f3736cc` | `origin/main` | Yes | `GOAL_EXCLUSIVE` | Default branch |
| `BR-BASTION-HANDOFF` | `donbeave/velnor-bastion` | `refs/heads/handoff/velnor-rollout-930b8384` | `74d60ac9beb0092f3c2c893c5d39a731e3c25d68` | `origin/handoff/velnor-rollout-930b8384` | Yes | `GOAL_EXCLUSIVE` | Draft PR #1 |
| `BR-VELNOR-INTEG` | `tailrocks/velnor` | `refs/heads/integrate/apple-ci-s2` | `326fd41439535cec1265ec386af77fca048cfa7e` | `origin/integrate/apple-ci-s2` | Yes | `GOAL_EXCLUSIVE` | `main` (via PR) |
| `BR-EM-MAIN` | `donbeave/essential-mac` | `refs/heads/main` | `4665534690659898f8cc00fe04188aea7dd19c30` | `origin/main` | Yes | `GOAL_SHARED` | Merged (Certified) |
| `BR-EM-ROLLOUT` | `donbeave/essential-mac` | `refs/heads/rollout/velnor-3-provider` | `f429698900d2a9b3639d5379d91ce3c940764f4b` | `origin/rollout/velnor-3-provider` | Yes | `GOAL_SHARED` | Merged in PR #13 |
| `BR-JAB-ROLLOUT` | `ChainArgos/jackin-agent-brown` | `refs/heads/rollout/velnor-3-provider` | `092fe1db796367479029d4e94379530f7f5965c0` | `origin/rollout/velnor-3-provider` | Yes | `GOAL_EXCLUSIVE` | PR #241 $	o$ `main` |
| `BR-CFT-ROLLOUT` | `ChainArgos/cloudflare-tofu` | `refs/heads/rollout/velnor-3-provider` | `fd3ba03c9f4e1faeeb96de2262d2e5f2111c6ad4` | `origin/rollout/velnor-3-provider` | Yes | `GOAL_EXCLUSIVE` | PR #5 $	o$ `main` |
| `BR-GHT-ROLLOUT` | `ChainArgos/github-terraform` | `refs/heads/rollout/velnor-3-provider` | `2c440218985638b1319abf79c55787743c33d560` | `origin/rollout/velnor-3-provider` | Yes | `GOAL_EXCLUSIVE` | PR #13 $	o$ `main` |
| `BR-JVM-ROLLOUT` | `ChainArgos/java-monorepo` | `refs/heads/rollout/velnor-3-provider` | `fbc3a92575b1839c07e6968bf0c9c1a13b3eec96` | `origin/rollout/velnor-3-provider` | Yes | `GOAL_EXCLUSIVE` | PR #2063 $	o$ `main` |

*Related Stashes*: Audit confirmed zero goal-relevant stashes across all 7 repositories (`git stash list` is empty in all checkouts).

### E.4. Related Pull Request Ledger

| PR ID | Repository | Number | Head Branch | Full Head SHA | Base Branch | State | URL | Checks & Status |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `PR-EM-13` | `donbeave/essential-mac` | #13 | `rollout/velnor-3-provider` | `f429698900d2a9b3639d5379d91ce3c940764f4b` | `main` | **MERGED** | `https://github.com/donbeave/essential-mac/pull/13` | Merged 2026-09-21T11:51:54Z; Gate G3 qualified |
| `PR-JAB-241` | `ChainArgos/jackin-agent-brown` | #241 | `rollout/velnor-3-provider` | `092fe1db796367479029d4e94379530f7f5965c0` | `main` | **OPEN** | `https://github.com/ChainArgos/jackin-agent-brown/pull/241` | Run `35646514624` cancelled; ready for qualification |
| `PR-CFT-5` | `ChainArgos/cloudflare-tofu` | #5 | `rollout/velnor-3-provider` | `fd3ba03c9f4e1faeeb96de2262d2e5f2111c6ad4` | `main` | **OPEN** | `https://github.com/ChainArgos/cloudflare-tofu/pull/5` | Ready for Gate G4 qualification |
| `PR-GHT-13` | `ChainArgos/github-terraform` | #13 | `rollout/velnor-3-provider` | `2c440218985638b1319abf79c55787743c33d560` | `main` | **OPEN** | `https://github.com/ChainArgos/github-terraform/pull/13` | Ready for Gate G4 qualification |
| `PR-JVM-2063` | `ChainArgos/java-monorepo` | #2063 | `rollout/velnor-3-provider` | `fbc3a92575b1839c07e6968bf0c9c1a13b3eec96` | `main` | **OPEN** | `https://github.com/ChainArgos/java-monorepo/pull/2063` | 71 matrix units; ready for Gate G5 qualification |
| `PR-BASTION-1` | `donbeave/velnor-bastion` | #1 | `handoff/velnor-rollout-930b8384` | `74d60ac9beb0092f3c2c893c5d39a731e3c25d68` | `main` | **DRAFT** | `https://github.com/donbeave/velnor-bastion/pull/1` | Checkpoint preservation; auto-merge disabled |

### E.5. Future Integration Map (Post-Resumption)
> [!NOTE]
> **NO MERGING OR LOCAL CLEANUP WAS PERFORMED DURING THIS HANDOFF.**
> Integration occurs strictly after explicit user resumption and fresh safety checks.

**Linked Integration Graph**:
`worktree/clone -> local branch or detached checkpoint -> remote preservation ref -> PR(s) -> intended integration target`
1. `WT-JAB-MAIN` $	o$ `rollout/velnor-3-provider` $	o$ `origin/rollout/velnor-3-provider` $	o$ PR #241 $	o$ `ChainArgos/jackin-agent-brown:main`
2. `WT-CFT-MAIN` $	o$ `rollout/velnor-3-provider` $	o$ `origin/rollout/velnor-3-provider` $	o$ PR #5 $	o$ `ChainArgos/cloudflare-tofu:main`
3. `WT-GHT-MAIN` $	o$ `rollout/velnor-3-provider` $	o$ `origin/rollout/velnor-3-provider` $	o$ PR #13 $	o$ `ChainArgos/github-terraform:main`
4. `WT-JVM-MAIN` $	o$ `rollout/velnor-3-provider` $	o$ `origin/rollout/velnor-3-provider` $	o$ PR #2063 $	o$ `ChainArgos/java-monorepo:main`
5. `WT-VELNOR-MAIN` $	o$ `integrate/apple-ci-s2` $	o$ `origin/integrate/apple-ci-s2` $	o$ PR (to be opened) $	o$ `tailrocks/velnor:main`
6. `WT-BASTION-MAIN` $	o$ `handoff/velnor-rollout-930b8384` $	o$ `origin/handoff/velnor-rollout-930b8384` $	o$ PR #1 $	o$ `donbeave/velnor-bastion:main`

---

### E.6. Post-Integration Local Cleanup Runbook
> [!IMPORTANT]
> **MANDATORY CLEANUP SAFETY GATES**:
> 1. Candidate must not be in active use by any running process.
> 2. Candidate must not be checked out in another active worktree.
> 3. Working tree must be completely clean (0 staged, 0 unstaged, 0 untracked files).
> 4. All commits must be proven integrated into the target branch via squash merge check (`gh pr view <PR> --json state`).
> 5. Main worktree must be switched to `main` before deleting branch (`git checkout main && git pull origin main`).

| Resource ID | Exact Host / Path / Ref | Expected HEAD / Tip | Final Target | Integration Proof | Recovery Reference | No-Use / Cleanliness Gates | Proposed Action | Status / Blocker |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `RES-JAB-BR` | `/Users/donbeave/Projects/ChainArgos/jackin-agent-brown` ref `rollout/velnor-3-provider` | `092fe1db` | `main` | PR #241 merged | `origin/rollout/velnor-3-provider` | PR merged; worktree clean; switched to `main` | `git -C <path> checkout main && git -C <path> pull origin main && git -C <path> branch -D rollout/velnor-3-provider` | `DEFERRED_POST_RESUMPTION` |
| `RES-CFT-BR` | `/Users/donbeave/Projects/ChainArgos/cloudflare-tofu` ref `rollout/velnor-3-provider` | `fd3ba03c` | `main` | PR #5 merged | `origin/rollout/velnor-3-provider` | PR merged; worktree clean; switched to `main` | `git -C <path> checkout main && git -C <path> pull origin main && git -C <path> branch -D rollout/velnor-3-provider` | `DEFERRED_POST_RESUMPTION` |
| `RES-GHT-BR` | `/Users/donbeave/Projects/ChainArgos/github-terraform` ref `rollout/velnor-3-provider` | `2c440218` | `main` | PR #13 merged | `origin/rollout/velnor-3-provider` | PR merged; worktree clean; switched to `main` | `git -C <path> checkout main && git -C <path> pull origin main && git -C <path> branch -D rollout/velnor-3-provider` | `DEFERRED_POST_RESUMPTION` |
| `RES-JVM-BR` | `/Users/donbeave/Projects/ChainArgos/java-monorepo` ref `rollout/velnor-3-provider` | `fbc3a925` | `main` | PR #2063 merged | `origin/rollout/velnor-3-provider` | PR merged; worktree clean; switched to `main` | `git -C <path> checkout main && git -C <path> pull origin main && git -C <path> branch -D rollout/velnor-3-provider` | `DEFERRED_POST_RESUMPTION` |
| `RES-EM-BR` | `/Users/donbeave/Projects/donbeave/essential-mac` ref `rollout/velnor-3-provider` | `f4296989` | `main` | PR #13 merged | `origin/rollout/velnor-3-provider` | PR #13 merged; main worktree is on `main` | `git -C <path> branch -D rollout/velnor-3-provider` | `DEFERRED_POST_RESUMPTION` |
| `RES-EM-WT4` | `/Users/donbeave/Projects/donbeave/essential-mac-cavecrew-builder` | `74f915b6` | N/A | Path missing on disk | Reflog | Prunable gitdir points to non-existent path | `git -C /Users/donbeave/Projects/donbeave/essential-mac worktree prune` | `DEFERRED_POST_RESUMPTION` |
| `RES-STORE` | `/Users/donbeave/.velnor-store/scaleset-*/` | Heartbeats / sockets | N/A | Full goal completion | Handoff doc | All runner processes terminated | `rm -f /Users/donbeave/.velnor-store/scaleset-*/.slot-*.heartbeat` | `DEFERRED_POST_RESUMPTION` |

---

## Section F: Decisions, Findings, Assumptions, and Rejected Approaches

1. **ChainArgos Organization Runner Group ID 4 (`velnor-trusted`)**:
   - *Decision*: ChainArgos repository-scoped runner pools cannot be registered at the top organization level without specifying the matching group name. `velnor-runner` was configured with `--routing-policy-file /Users/donbeave/.velnor-store/scaleset-chainargos/routing-policy.json` declaring group `"velnor-trusted"` and `--pool-name "velnor-trusted"`, correctly mapping to organization Runner Group ID 4.
2. **Authenticated Tool Installation via `GITHUB_TOKEN`**:
   - *Finding*: Scale-set jobs executing on GitHub Actions runner containers require `GITHUB_TOKEN` propagated into the runner container environment to allow `actions/setup-node`, `mise`, and Cargo to download release assets from GitHub without encountering rate limits (403). Fixed in `tailrocks/velnor` commit `326fd41439535cec1265ec386af77fca048cfa7e`.
3. **Sequential Execution Precedence**:
   - *Invariant*: `ChainArgos/java-monorepo` (71 matrix units) must NEVER execute concurrently with other consumers on macOS due to `max_jobs = 4`. It must run strictly LAST.
4. **Bastion Hardware Protection**:
   - *Invariant*: `/dev/nvme1n1` (3.5 TB) on Debian Bastion is strictly reserved and must have 0 partitions, 0 filesystems, and 0 mounts. Confirmed untouched via `lsblk -f`.
5. **Rejection of Go SDK / ARC Shims**:
   - *Rejected Alternative*: Emulating GitHub Actions Scale Set via Kubernetes ARC controllers or Go sidecars was explicitly rejected per Prompt Section 3; unmodified official runner in ephemeral container managed by Rust control plane was implemented.
6. **Rejection of Floating Tags**:
   - *Rejected Alternative*: Floating `:latest` image tags are prohibited per Prompt Section 5; immutable image digests required.
7. **Rejection of Worker VMs on macOS**:
   - *Rejected Alternative*: Firecracker/libvirt or OrbStack guest VMs for jobs rejected per Prompt Section 4; direct Linux container execution used.

---

## Section G: Verification Evidence and Known Failures

### Gate G3 Evidence (Certified Complete)
- **Three Consecutive Green Runs on `main`**:
  - Run 1: `https://github.com/donbeave/essential-mac/actions/runs/35640576186` (Success, all 3 providers)
  - Run 2: `https://github.com/donbeave/essential-mac/actions/runs/35642170004` (Success, all 3 providers)
  - Run 3: `https://github.com/donbeave/essential-mac/actions/runs/35643273619` (Success, all 3 providers)
- **Concurrency Auditor Evidence**:
  - Sample count: 14,176 discrete ledger queries
  - Active permits peak: 3 permits (strictly <= 4)
  - Overcommit violations: **0**
  - Queue FIFO inversions: **0**
  - Leaked permits: **0**

### Gate G4 Evidence (In-Flight at Pause)
- Workflow Run: `https://github.com/ChainArgos/jackin-agent-brown/actions/runs/35646514624`
- Status: Cancelled cleanly on pause to avoid unmonitored queue consumption.

---

## Section H: Ordered Remaining-Work Plan

### T-001: Gate G4 - Host Preflight, Binary Compilation, and Launch Org-Scoped ChainArgos Runner Daemon
- **Linked Requirements**: `G-001`, `G-004`, `G-021`, `G-040` (Gate G4 Rollout)
- **Starting State**: Host permit ledger has 0 permits. Zero `velnor-runner` processes running. Token at `/Users/donbeave/.velnor-store/scaleset-chainargos/github_token`. Runner group `velnor-trusted` (ID 4) available on ChainArgos org. `velnor` worktree is on `m4-pin-bump` and binary is uncompiled.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/github/velnor`
  - Script: `/Users/donbeave/.velnor-store/scaleset-chainargos/start-chainargos-daemon.sh`
  - Policy: `/Users/donbeave/.velnor-store/scaleset-chainargos/routing-policy.json`
  - Config: `/Users/donbeave/.velnor-store/scaleset-chainargos/scale-set.toml`
  - Logs: `/Users/donbeave/.velnor-store/scaleset-chainargos/daemon.log`
- **Concrete Next Action**:
  ```bash
  # 1. Verify Docker Engine / OrbStack is responsive
  docker info >/dev/null 2>&1 || (echo "ERROR: Docker/OrbStack is not running" >&2; exit 1)

  # 2. Checkout integrate/apple-ci-s2 and compile velnor-runner
  git -C /Users/donbeave/Projects/github/velnor checkout integrate/apple-ci-s2
  cargo build --manifest-path /Users/donbeave/Projects/github/velnor/Cargo.toml -p velnor-runner
  test -x /Users/donbeave/Projects/github/velnor/target/debug/velnor-runner || (echo "ERROR: Binary build failed" >&2; exit 1)

  # 3. Start organization-scoped runner daemon
  /Users/donbeave/.velnor-store/scaleset-chainargos/start-chainargos-daemon.sh https://github.com/ChainArgos

  # 4. Verify daemon process is running and heartbeating
  sleep 3
  ps aux | grep -E "velnor-runner.*scaleset-chainargos" | grep -v grep
  ```
- **Dependencies**: Clean host state (verified in Section C).
- **Pitfalls & Invariants**:
  - Target URL MUST be `https://github.com/ChainArgos` so `--pool-name "velnor-trusted"` is supplied.
  - Organization-scoped registration requires runner group `velnor-trusted` (ID 4); never use default runner group.
  - Image must be `velnor/job-ubuntu:26.04`.
- **Validation Commands & Expected Output**:
  - Validation:
    ```bash
    ./scripts/monitor-permits.sh check --gate G4
    grep -E "starting V2 session|broker session created|supervising 1 slot process" /Users/donbeave/.velnor-store/scaleset-chainargos/daemon.log | tail -n 5
    ```
  - Expected Output:
    - `./scripts/monitor-permits.sh` returns `[PASS]` across all 6 invariant assertions.
    - `daemon.log` shows active V2 session established with broker.actions.githubusercontent.com and slot listening.
- **Blockers**: None.

---

### T-002: Gate G4 - Qualify PR #241 (`ChainArgos/jackin-agent-brown`) Across All 3 Providers
- **Linked Requirements**: `G-001`, `G-022`, `G-026`, `G-040` (Consumer #2 Qualification)
- **Starting State**: T-001 complete. PR #241 open on `rollout/velnor-3-provider` at commit `092fe1d`. Previous run `35646514624` cancelled.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/jackin-agent-brown`
  - Workflow: `.github/workflows/ci-pr.yml`
  - Invariant Monitor: `/Users/donbeave/Projects/github/velnor-bastion/scripts/monitor-permits.sh`
- **Concrete Next Action**:
  ```bash
  # 1. Dispatch CI qualification for PR #241
  gh workflow run ci-pr.yml     --repo ChainArgos/jackin-agent-brown     --ref rollout/velnor-3-provider     -f providers=github-hosted,github-self-hosted,velnor     -f scope=full

  # 2. Obtain newly dispatched run ID
  sleep 5
  NEW_RUN_ID=$(gh run list --repo ChainArgos/jackin-agent-brown --workflow ci-pr.yml --limit 1 --json databaseId -q '.[0].databaseId')
  echo "Dispatched Run ID: $NEW_RUN_ID"

  # 3. Watch run to completion
  gh run watch "$NEW_RUN_ID" --repo ChainArgos/jackin-agent-brown --interval 10
  ```
- **Dependencies**: T-001.
- **Pitfalls & Invariants**:
  - Concurrency MUST NOT exceed `max_jobs = 4` on macOS.
  - All three providers (`github-hosted`, `github-self-hosted`, `velnor`) must execute and pass.
  - Image must be strictly `ubuntu-26.04` / `velnor/job-ubuntu:26.04`.
- **Validation Commands & Expected Output**:
  - Validation:
    ```bash
    gh run view "$NEW_RUN_ID" --repo ChainArgos/jackin-agent-brown --json conclusion,status
    ./scripts/monitor-permits.sh check --gate G4
    ```
  - Expected Output:
    - `conclusion`: `"success"`, `status`: `"completed"`.
    - All 6 permit invariant assertions report `[PASS]`.
- **Blockers**: None.

---

### T-003: Gate G4 - Squash-Merge PR #241 & Certify 3 Consecutive Green Runs on `main` (`jackin-agent-brown`)
- **Linked Requirements**: `G-019`, `G-040`, `G-050` (Consumer #2 Certification)
- **Starting State**: T-002 passed (`conclusion: success`). PR #241 open and mergeable.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/jackin-agent-brown`
  - Workflow: `.github/workflows/ci-main.yml`
- **Concrete Next Action**:
  ```bash
  # 1. Squash merge PR #241 with strict sign-off
  gh pr merge 241     --repo ChainArgos/jackin-agent-brown     --squash     --subject "ci: roll out Velnor 3-provider workflow (#241)"     --body "Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>"

  # 2. Wait for automatic push run on main and monitor
  sleep 10
  RUN_MAIN_1=$(gh run list --repo ChainArgos/jackin-agent-brown --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$RUN_MAIN_1" --repo ChainArgos/jackin-agent-brown --interval 10

  # 3. Dispatch and monitor Run 2 on main
  gh workflow run ci-main.yml --repo ChainArgos/jackin-agent-brown --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  RUN_MAIN_2=$(gh run list --repo ChainArgos/jackin-agent-brown --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$RUN_MAIN_2" --repo ChainArgos/jackin-agent-brown --interval 10

  # 4. Dispatch and monitor Run 3 on main
  gh workflow run ci-main.yml --repo ChainArgos/jackin-agent-brown --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  RUN_MAIN_3=$(gh run list --repo ChainArgos/jackin-agent-brown --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$RUN_MAIN_3" --repo ChainArgos/jackin-agent-brown --interval 10
  ```
- **Dependencies**: T-002.
- **Pitfalls & Invariants**:
  - Squash commit MUST contain `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`.
  - All 3 runs on `main` must be consecutive green passes without intervening failures.
- **Validation Commands & Expected Output**:
  - All three runs report `conclusion: "success"`, `status: "completed"`, `headBranch: "main"`.
- **Blockers**: None.

---

### T-004: Gate G4 - Qualify, Squash-Merge & Certify 3 Green Runs on `main` for PR #5 (`ChainArgos/cloudflare-tofu`)
- **Linked Requirements**: `G-001`, `G-040`, `G-045`, `G-050` (Consumer #3 Qualification)
- **Starting State**: T-003 complete. PR #5 open on `rollout/velnor-3-provider` at commit `fd3ba03`. Daemon active.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/cloudflare-tofu`
  - Workflows: `.github/workflows/ci-pr.yml`, `.github/workflows/ci-main.yml`
- **Concrete Next Action**:
  ```bash
  # 1. Qualify PR #5
  gh workflow run ci-pr.yml --repo ChainArgos/cloudflare-tofu --ref rollout/velnor-3-provider -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 5
  CFT_PR_RUN=$(gh run list --repo ChainArgos/cloudflare-tofu --workflow ci-pr.yml --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$CFT_PR_RUN" --repo ChainArgos/cloudflare-tofu --interval 10

  # 2. Squash merge PR #5 with sign-off
  gh pr merge 5 --repo ChainArgos/cloudflare-tofu --squash     --subject "ci: roll out Velnor 3-provider workflow (#5)"     --body "Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>"

  # 3. Qualify 3 consecutive green runs on main
  sleep 10
  CFT_M1=$(gh run list --repo ChainArgos/cloudflare-tofu --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$CFT_M1" --repo ChainArgos/cloudflare-tofu --interval 10
  gh workflow run ci-main.yml --repo ChainArgos/cloudflare-tofu --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  CFT_M2=$(gh run list --repo ChainArgos/cloudflare-tofu --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$CFT_M2" --repo ChainArgos/cloudflare-tofu --interval 10
  gh workflow run ci-main.yml --repo ChainArgos/cloudflare-tofu --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  CFT_M3=$(gh run list --repo ChainArgos/cloudflare-tofu --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$CFT_M3" --repo ChainArgos/cloudflare-tofu --interval 10
  ```
- **Dependencies**: T-003.
- **Pitfalls & Invariants**:
  - DCO sign-off mandatory on squash merge.
  - Zero overcommit on host permit ledger (`max_jobs = 4`). OpenTofu validation strictly isolated; no apply/mutation.
- **Validation Commands & Expected Output**:
  - All runs `conclusion: "success"`, `status: "completed"`.
- **Blockers**: None.

---

### T-005: Gate G4 - Qualify, Squash-Merge & Certify 3 Green Runs on `main` for PR #13 (`ChainArgos/github-terraform`)
- **Linked Requirements**: `G-001`, `G-040`, `G-045`, `G-050` (Consumer #4 Qualification)
- **Starting State**: T-004 complete. PR #13 open on `rollout/velnor-3-provider` at commit `2c44021`. Daemon active.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/github-terraform`
  - Workflows: `.github/workflows/ci-pr.yml`, `.github/workflows/ci-main.yml`
- **Concrete Next Action**:
  ```bash
  # 1. Qualify PR #13
  gh workflow run ci-pr.yml --repo ChainArgos/github-terraform --ref rollout/velnor-3-provider -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 5
  GHT_PR_RUN=$(gh run list --repo ChainArgos/github-terraform --workflow ci-pr.yml --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$GHT_PR_RUN" --repo ChainArgos/github-terraform --interval 10

  # 2. Squash merge PR #13 with sign-off
  gh pr merge 13 --repo ChainArgos/github-terraform --squash     --subject "ci: roll out Velnor 3-provider workflow (#13)"     --body "Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>"

  # 3. Qualify 3 consecutive green runs on main
  sleep 10
  GHT_M1=$(gh run list --repo ChainArgos/github-terraform --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$GHT_M1" --repo ChainArgos/github-terraform --interval 10
  gh workflow run ci-main.yml --repo ChainArgos/github-terraform --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  GHT_M2=$(gh run list --repo ChainArgos/github-terraform --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$GHT_M2" --repo ChainArgos/github-terraform --interval 10
  gh workflow run ci-main.yml --repo ChainArgos/github-terraform --ref main -f providers=github-hosted,github-self-hosted,velnor -f scope=full
  sleep 10
  GHT_M3=$(gh run list --repo ChainArgos/github-terraform --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$GHT_M3" --repo ChainArgos/github-terraform --interval 10
  ```
- **Dependencies**: T-004.
- **Pitfalls & Invariants**:
  - DCO sign-off mandatory. Consumer #4 must be certified before Gate G5 (`java-monorepo`) begins.
- **Validation Commands & Expected Output**:
  - All runs `conclusion: "success"`, `status: "completed"`.
- **Blockers**: None.

---

### T-006: Gate G5 - Qualify 71-Unit Matrix on PR #2063 (`ChainArgos/java-monorepo`) under macOS `max_jobs = 4`
- **Linked Requirements**: `G-001`, `G-006`, `G-036`, `G-041` (Large Monorepo Concurrency & Matrix Arbitration)
- **Starting State**: T-005 complete (Consumers 1–4 fully certified). PR #2063 open on `rollout/velnor-3-provider` at commit `fbc3a9257`. 71 matrix units configured.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/java-monorepo`
  - Workflow: `.github/workflows/ci-pr.yml`
  - Concurrency Auditor: `/Users/donbeave/Projects/github/velnor-bastion/scripts/monitor-permits.sh`
- **Concrete Next Action**:
  ```bash
  # 1. Start continuous permit ledger auditor in background
  /Users/donbeave/Projects/github/velnor-bastion/scripts/monitor-permits.sh watch --gate G5 --interval 1.0 > /tmp/java-monorepo-audit.log 2>&1 &
  AUDIT_PID=$!

  # 2. Dispatch qualification run for PR #2063
  gh workflow run ci-pr.yml     --repo ChainArgos/java-monorepo     --ref rollout/velnor-3-provider     -f providers=velnor     -f scope=full

  # 3. Capture Run ID and watch
  sleep 10
  JVM_PR_RUN=$(gh run list --repo ChainArgos/java-monorepo --workflow ci-pr.yml --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$JVM_PR_RUN" --repo ChainArgos/java-monorepo --interval 15

  # 4. Stop auditor
  kill "$AUDIT_PID" || true
  ```
- **Dependencies**: T-005 (Mandatory: java-monorepo must never run before repos 1–4 are certified).
- **Pitfalls & Invariants**:
  - 71 units queuing simultaneously: SQLite FIFO arbitration must handle high demand volume without queue starvation or deadlocks.
  - Active permits must NEVER exceed 4 (`max_jobs = 4`). Zero overcommit.
  - OS version `ubuntu-26.04` / `velnor/job-ubuntu:26.04` strictly enforced.
- **Validation Commands & Expected Output**:
  - Run reports `conclusion: "success"`, `status: "completed"`.
  - Concurrency audit verifies peak permits <= 4, zero overcommit, zero FIFO inversions.
- **Blockers**: None.

---

### T-007: Gate G5 - Squash-Merge PR #2063 & Certify 3 Consecutive Green Runs on `main` (`java-monorepo`)
- **Linked Requirements**: `G-006`, `G-019`, `G-041`, `G-050` (macOS Rollout Final Certification)
- **Starting State**: T-006 passed. PR #2063 open and qualified.
- **Files & Paths**:
  - Repo: `/Users/donbeave/Projects/ChainArgos/java-monorepo`
  - Workflow: `.github/workflows/ci-main.yml`
- **Concrete Next Action**:
  ```bash
  # 1. Squash merge PR #2063 with strict sign-off
  gh pr merge 2063 --repo ChainArgos/java-monorepo --squash     --subject "ci: adopt schema 2 velnor workflow generator and configure provider routing (#2063)"     --body "Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>"

  # 2. Monitor Run 1 on main (push-triggered)
  sleep 10
  JVM_M1=$(gh run list --repo ChainArgos/java-monorepo --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$JVM_M1" --repo ChainArgos/java-monorepo --interval 15

  # 3. Dispatch and monitor Run 2 on main
  gh workflow run ci-main.yml --repo ChainArgos/java-monorepo --ref main -f providers=velnor -f scope=full
  sleep 10
  JVM_M2=$(gh run list --repo ChainArgos/java-monorepo --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$JVM_M2" --repo ChainArgos/java-monorepo --interval 15

  # 4. Dispatch and monitor Run 3 on main
  gh workflow run ci-main.yml --repo ChainArgos/java-monorepo --ref main -f providers=velnor -f scope=full
  sleep 10
  JVM_M3=$(gh run list --repo ChainArgos/java-monorepo --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
  gh run watch "$JVM_M3" --repo ChainArgos/java-monorepo --interval 15
  ```
- **Dependencies**: T-006.
- **Pitfalls & Invariants**:
  - Strict sign-off mandate on squash commit.
  - All 3 runs on `main` must complete successfully, marking the completion of Phase 1 (macOS Rollout).
- **Validation Commands & Expected Output**:
  - All 3 runs report `conclusion: "success"`, `status: "completed"`, `headBranch: "main"`.
- **Blockers**: None.

---

### T-008: Gate G6 - Debian Bastion Deployment (`velnor-runner 0.1.274`, `max_jobs = 16`, NVMe Untouched)
- **Linked Requirements**: `G-002`, `G-008`, `G-009`, `G-042`, `G-046` (Debian Bastion Real Deployment)
- **Starting State**: T-007 complete (macOS rollout 100% finished). Bastion accessible via `root@37.27.110.241`. Secondary NVMe `/dev/nvme1n1` untouched.
- **Files & Paths**:
  - Script: `/Users/donbeave/Projects/github/velnor-bastion/deploy-bastion-g6.sh`
  - Target Host: `root@37.27.110.241`
- **Concrete Next Action**:
  ```bash
  # 1. Execute Gate G6 deployment script for package 0.1.274
  cd /Users/donbeave/Projects/github/velnor-bastion
  ./deploy-bastion-g6.sh 0.1.274

  # 2. Deploy host capacity configuration with max_jobs = 16
  ssh root@37.27.110.241 "bash -s" << 'REMOTE_CFG'
  set -euo pipefail
  mkdir -p /etc/velnor /var/lib/velnor
  cat << 'CFG' > /etc/velnor/velnor-runner.toml
  max_jobs = 16
  permit_ledger = "/var/lib/velnor/permit-ledger.db"
  docker_image = "velnor/job-ubuntu:26.04"
  labels = ["self-hosted", "velnor", "velnor-target-mvp", "ubuntu-26.04"]
  trust_scope = "trusted"
  CFG
  systemctl restart velnor-runner || systemctl status velnor-runner --no-pager
  REMOTE_CFG
  ```
- **Dependencies**: T-007.
- **Pitfalls & Invariants**:
  - `/dev/nvme1n1` (3.5 TB) MUST remain 100% untouched: 0 partitions, 0 filesystems, 0 mounts.
  - GPG signing key fingerprint MUST be verified: `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`.
  - Package transaction lock `/run/velnor/package-transaction.lock` must be acquired via flock.
  - Bastion capacity authority enforces `max_jobs = 16`.
- **Validation Commands & Expected Output**:
  - Validation:
    ```bash
    ssh root@37.27.110.241 "lsblk -f /dev/nvme1n1 && dpkg-query -W velnor-runner && command -v velnorctl"
    ```
  - Expected Output:
    - `/dev/nvme1n1` shows NO filesystems or mountpoints.
    - `velnor-runner 0.1.274` installed.
    - `velnorctl` binary present and functional.
- **Blockers**: None.

---

### T-009: Gate G7 - Bastion 5-Consumer Sequential Replay (Repos 1 $	o$ 5)
- **Linked Requirements**: `G-001`, `G-002`, `G-007`, `G-043` (Bastion Multi-Repo Sequential Replay)
- **Starting State**: T-008 complete. Bastion daemon online under `/velnor.slice` with `max_jobs = 16`.
- **Files & Paths**:
  - Bastion Host: `root@37.27.110.241`
  - Playbooks: `/Users/donbeave/Projects/github/velnor-bastion/playbooks/`
  - Consumers: Repos 1 $	o$ 5
- **Concrete Next Action**:
  ```bash
  # Execute sequential replay across repos 1 -> 5
  for repo in     "donbeave/essential-mac"     "ChainArgos/jackin-agent-brown"     "ChainArgos/cloudflare-tofu"     "ChainArgos/github-terraform"     "ChainArgos/java-monorepo"; do
    echo "=== Bastion Replay: $repo ==="
    gh workflow run ci-main.yml --repo "$repo" --ref main -f providers=velnor -f scope=full
    sleep 10
    RUN_ID=$(gh run list --repo "$repo" --workflow ci-main.yml --branch main --limit 1 --json databaseId -q '.[0].databaseId')
    gh run watch "$RUN_ID" --repo "$repo" --interval 15
    gh run view "$RUN_ID" --repo "$repo" --json conclusion -q '.conclusion' | grep "success"
  done
  ```
- **Dependencies**: T-008.
- **Pitfalls & Invariants**:
  - Consumer execution order is MANDATORY: 1 $	o$ 2 $	o$ 3 $	o$ 4 $	o$ 5.
  - Under no circumstances may `java-monorepo` execute before repos 1–4 complete on Bastion.
  - Host capacity limit `max_jobs = 16` strictly arbitrated by SQLite PermitLedger.
- **Validation Commands & Expected Output**:
  - All 5 consumer workflows complete with `conclusion: success`.
  - Active permits drain back to 0.
- **Blockers**: None.

---

### T-010: Gate G8 - Recovery Chaos Tests, Soak Stability & Final Documentation Delivery
- **Linked Requirements**: `G-044`, `G-047`, `G-048`, `G-050` (Resilience Qualification, Soak Testing & Documentation)
- **Starting State**: T-009 complete. All 5 consumers passed on Bastion.
- **Files & Paths**:
  - Docs: `/Users/donbeave/Projects/github/velnor-bastion/docs/`
  - Playbooks: `/Users/donbeave/Projects/github/velnor-bastion/playbooks/`
  - PR: `https://github.com/donbeave/velnor-bastion/pull/1`
- **Concrete Next Action**:
  ```bash
  # 1. Recovery test: SIGKILL daemon during job execution and verify state reconciliation
  ssh root@37.27.110.241 "pkill -9 -f velnor-runner && sleep 2 && systemctl start velnor-runner"
  ssh root@37.27.110.241 "sqlite3 /var/lib/velnor/permit-ledger.db 'SELECT max_jobs, generation, reconciled_generation FROM permit_meta;'"

  # 2. Re-verify hardware safeguard
  ssh root@37.27.110.241 "lsblk -f /dev/nvme1n1"

  # 3. Document verified completion across all 5 repos and 8 gates
  # 4. Final commit with strict sign-off:
  git -C /Users/donbeave/Projects/github/velnor-bastion commit -s -m "docs: certify completion of Gates G0-G8 on macOS and Bastion"
  ```
- **Dependencies**: T-009.
- **Pitfalls & Invariants**:
  - Secondary NVMe `/dev/nvme1n1` must remain 100% untouched throughout chaos testing.
  - DCO commit sign-off `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` mandatory.
- **Validation Commands & Expected Output**:
  - `/dev/nvme1n1` remains untouched.
  - Commit sign-off verified.
- **Blockers**: None.

---

## Section I: Environment and Operational Recovery

- **macOS Host**:
  - Darwin Apple Silicon (`arm64`), macOS 15.6.1, 12 cores, 36 GiB RAM.
  - Store directory: `/Users/donbeave/.velnor-store/`
  - Central SQLite Permit Ledger: `/Users/donbeave/.velnor-store/permit-ledger.db` (`max_jobs = 4`).
  - Docker Provider: OrbStack (Docker Engine 29.8.1).
- **Debian Bastion Host**:
  - Host: `root@37.27.110.241` (Debian 13 trixie x86_64, AMD EPYC 9454P 32-Core / 64 threads, 128 GiB RAM).
  - Target package: `velnor-runner 0.1.274`
  - Safeguarded device: `/dev/nvme1n1` (3.5 TB, strictly untouched).
  - Bastion capacity: `max_jobs = 16`.
- **Docker Runtimes**:
  - Official image invariant: `velnor/job-ubuntu:26.04` (zero `ubuntu-24.04`).

---

## Section J: Fresh-Agent Resume Runbook

### J.1 Retrieval Instructions
To inspect and resume from this checkpoint:
```bash
git -C /Users/donbeave/Projects/github/velnor-bastion fetch origin
git -C /Users/donbeave/Projects/github/velnor-bastion checkout handoff/velnor-rollout-930b8384
```

### J.2 Exact Resume Command
To resume execution of the original goal, invoke:
```markdown
/goal Read and resume docs/goal-handoffs/velnor-rollout-bastion--20260921T220213Z--antigravity--930b8384.md
```

### J.3 Answers to the 7 Critical Resumption Questions
1. **What exactly did the user originally ask for?**: Implement, deploy, and qualify Velnor control plane on local macOS host FIRST, then on Debian Bastion, executing CI in Linux Docker containers across 5 repos in exact order (`essential-mac` $	o$ `jackin-agent-brown` $	o$ `cloudflare-tofu` $	o$ `github-terraform` $	o$ `java-monorepo`).
2. **Which amendments and constraints apply?**: Strict DCO sign-off (`Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`), `ubuntu-26.04` container images only, `/dev/nvme1n1` untouched on Bastion, host SQLite permit ledger enforces `max_jobs = 4` on macOS / `16` on Bastion, `cargo nextest` for tests, subagents used aggressively.
3. **What has actually been completed and verified?**: Gates G0–G3 verified complete. `essential-mac` has 3 consecutive green runs certified on `main` (`35640576186`, `35642170004`, `35643273619`) with 0 overcommit across 14,176 audit samples. Gate G4 is in-flight at repo 2; background tasks killed and permits reset to 0.
4. **Where is every recoverable piece of work?**: All 7 repositories, 16 worktrees, and 8 branches are catalogued in Section E. Pushed remote branches: `integrate/apple-ci-s2` on `tailrocks/velnor`, `handoff/velnor-rollout-930b8384` on `donbeave/velnor-bastion`, `rollout/velnor-3-provider` on all 4 ChainArgos repos.
5. **What remains, in what order, and why?**: Gate G4 (qualify and merge PRs #241, #5, #13 with 3 green runs each) $	o$ Gate G5 (qualify and merge PR #2063 with 71 units) $	o$ Gate G6 (Bastion deployment) $	o$ Gate G7 (Bastion replay 1 $	o$ 5) $	o$ Gate G8 (recovery tests).
6. **What is the first action?**: Execute Task `T-001`: Verify Docker engine, checkout `integrate/apple-ci-s2` in `tailrocks/velnor`, compile `velnor-runner`, launch daemon, and dispatch PR #241 qualification run (`T-002`).
7. **How will completion, integration, and safe cleanup be proven?**: Completion proven by 3 consecutive green runs on `main` for each repo across all 3 providers; Bastion verified with package `0.1.274`, `/dev/nvme1n1` untouched; cleanup proven by executing Section E.6 table after verifying PR merge status.

### J.4 Resumption Policy & Interpretation Rule
1. The pause was requested by the user and is now in effect.
2. The resume command authorizes continuing the **ORIGINAL** engineering goal starting directly at Task `T-001`.
3. The future agent must **NOT** pause again, regenerate this handoff, or create another handoff PR unless explicitly commanded to pause by the user.

---

## Section K: Blockers, Omissions, and Independent Review Findings

- **Blockers**: `NONE`. All background tasks are stopped, runner processes terminated, permits reclaimed to 0, and all branches/changes pushed to authorized remotes.
- **Omissions**: `NONE`. All 7 repositories, 16 worktrees, 8 branches, and 6 PRs are fully accounted for.
- **Independent Review Findings**:
  - **Intent-Fidelity Audit (`c68e159a`)**: Repaired previous 91% truncation of prompt by embedding complete verbatim prompt (`SRC-008`), consolidated operative goal, source register (`SRC-001`–`SRC-011`), and atomic requirements matrix (`G-001`–`G-052`, `H-001`–`H-010`).
  - **State-and-Remaining-Work Audit (`5cc2fdf6`)**: Verified host permit ledger permits = 0, demands = 0 active, Docker runner containers pruned, G3 runs green, G4 run cancelled cleanly. Replaced vague remaining-work text with structured tasks `T-001` through `T-010`.
  - **Integration-and-Preservation Audit (`fc7b7e34`)**: Added 2 missing worktrees in `essential-mac` to Table E.2; corrected checkout status of `WT-BASTION-MAIN` and `WT-VELNOR-TMP3`; added required tabular schema and cleanliness safety gates to Section E.6.
  - **Fresh-Reader Review (`c6a2e0b9`)**: Resolved fatal binary build defect by adding preflight compilation sequence to `T-001`; resolved `CAMPAIGN_LEDGER.md` discrepancy by adding explicit source-of-truth warning in Section D and K; detailed multi-run dispatch commands and Bastion runner configuration.
- **Final Audit Outcome**: `VERIFIED`. Available authoritative source coverage is complete; all operative requirements are faithfully and actionably documented; preservation and publication are verified; zero unresolved handoff gaps remain.
