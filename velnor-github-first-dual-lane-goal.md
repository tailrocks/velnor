/goal Implement and verify the complete Velnor GitHub-first CI/CD recovery, preview and stable Debian/Homebrew delivery, migration of the 32 repositories listed below, and subsequent Velnor-on-macOS/OrbStack rollout. Continue through implementation, review, releases, merges, installation tests, and final verification. The specification and acceptance gates below define completion.

This document was prepared on 2026-09-19 using GPT-6 Pro with parallel research and independent review agents. Execute this goal using `gpt-5.6-luna` with reasoning effort `max`, including the execution orchestrator, implementers, investigators, and reviewers. Select that model in the execution session before launching; prompt text alone does not change a running session's model. Verify the installed Codex model/configuration capabilities and record the effective settings. Do not silently substitute another model or claim an unsupported setting.

## 1. Intended outcome and scope

Produce a reproducible, fully exercised CI/CD system in this order:

1. Repair Velnor and regenerate its workflows so all automatic CI and release prerequisites run on GitHub-hosted runners.
2. Publish and verify new Velnor preview and stable versions through both Debian/APT and Homebrew. Repair the two Velnor distribution repositories as part of this step.
3. Migrate every repository in the fixed 32-repository scope to the proven Velnor generator, initially using GitHub-hosted runners. Prove PR and main-branch CI across the complete fleet.
4. Only after that fleet baseline passes, install and operate Velnor on my actual current macOS host using OrbStack. Run every Velnor workload inside Docker containers. Diagnose and fix the Velnor execution lane through rapid local reproduction and real GitHub runs.
5. Release any resulting Velnor fixes, update installed packages and generator pins, then generate and merge both providers across the fleet. Prove both providers on every eligible workload and retain native-platform verification where required.

GitHub-hosted remains the default provider and the recovery path. The final configuration automatically runs both GitHub-hosted and Velnor for eligible trusted PR/main workloads; selecting both must not require a person to dispatch every normal build. Keep explicit provider selection for diagnosis and recovery.

The scope includes code fixes needed for these outcomes, generator capabilities, workflow configuration, runtime behavior, packaging, distribution, release automation, meaningful tests, documentation, and compatible required-check configuration. It authorizes creating and updating migration/fix PRs, merging reviewed changes after their gates pass, and publishing the Velnor versions needed to complete the work. Preserve unrelated work and history. Do not merge unrelated feature PRs merely to clear the PR list. Preserve other projects' existing release capabilities without imposing Velnor's Debian/Homebrew packaging requirement on every repository.

Do not turn this into a third-provider/Scale Set project or a deployment to bastion, selene, or another server. Extend the existing Velnor generator and macOS host implementation. Do not introduce a parallel workflow generator or competing runner controller.

## 2. Fixed repository manifest

Use these exact repositories; validate that the manifest has 32 unique entries. Resolve their actual default branches at execution time. They all reported `main` during preparation, but live state is authoritative.

```text
tailrocks/velnor
tailrocks/velnor-apt
tailrocks/parallax
tailrocks/tracing-request-level
tailrocks/termrock
tailrocks/termpane
tailrocks/tablerock
tailrocks/schemalane
tailrocks/ruxel
tailrocks/pg-bigdecimal
tailrocks/parallax-telemetry-playground
tailrocks/homebrew-tablerock
tailrocks/homebrew-ruxel
tailrocks/homebrew-parallax
tailrocks/homebrew-holla
tailrocks/holla-apt
tailrocks/holla
tailrocks/homebrew-velnor
tailrocks/tailrocks-typescript-skills
tailrocks/tailrocks-skill-authoring-skills
tailrocks/tailrocks-rust-skills
tailrocks/tailrocks-roadmap-skills
tailrocks/tailrocks-pull-request-skills
tailrocks/tailrocks-open-source-skills
tailrocks/tailrocks-macos-skills
tailrocks/tailrocks-code-quality-skills
jackin-project/jackin
jackin-project/jackin-agent-smith
jackin-project/homebrew-tap
jackin-project/jackin-the-architect
jackin-project/jackin-sentinel
jackin-project/jackin-role-action
```

Read the repositories' applicable AGENTS.md instructions, current implementation, generator inputs, workflow outputs, build manifests, release configuration, open PRs, and recent failed run logs. Use current source and logs to verify documentation claims. Evaluate existing fixes before duplicating them.

## 3. Parallel execution and durable state

Use subagents aggressively throughout discovery, implementation, diagnosis, review, and final verification. Keep the useful concurrency permitted by the actual platform occupied. Work in bounded waves; do not assume unlimited threads or recursively spawn without ownership.

Start independent workstreams for generator/policy/bootstrap, Velnor hosted failures, preview/stable packaging, fleet inventory and repository adapters, macOS runtime capability analysis, and independent verification. During fleet migrations, use multiple repository owners and a separate reviewer pool. Read-only investigation of later phases may happen early; their operational rollout and completion gates must respect the order below.

Every delegated task must carry an ID, repository/component, exact input revision, objective, dependencies, owned files/worktree, acceptance commands, evidence outputs, and reviewer. Give each mutable worktree and shared file one owner. Serialize generator integration, shared ledger merges, release/tag mutation, and package-index updates; parallelize independent repositories and checks. Use isolated worktrees and merge changes without discarding another worker's edits. Preserve visible merge history where repository policy permits.

If a spawn fails because of capacity, reuse idle agents, retire completed agents when supported, queue ready tasks, and continue independent work in the orchestrator. Do not repeatedly issue the same failed spawn or omit independent review. Verify that custom agent configuration has not overridden the requested model/effort.

Maintain one canonical execution record under an appropriate Velnor documentation directory, preferably `docs/ci/github-first-dual-lane/` unless existing conventions require another location:

- `SPEC.md`: accepted target, workload/platform contract, channel contract, and decisions.
- `PLAN.md`: task graph, phase gates, owners, dependencies, and acceptance commands.
- `STATUS.md`: current gate, blockers, completed work, and exact next actions.
- `fleet.json`: the 32 repositories, pins, workload coverage, branch/PR revisions, and evidence references.
- `evidence/`: compact run/package records and durable links; no credentials or massive raw logs committed.
- `RUNBOOK.md`: regenerate, select providers, release, install, start/drain/stop the host, diagnose, recover, and roll back.

Use existing compatible documents rather than making duplicate planning trees. Keep machine-readable records in a documented schema, with a deterministic validation command. Prefer Rust for new durable orchestration/verification tooling and reuse existing helpers; do not build a new framework merely to maintain the checklist.

Avoid self-referential evidence commits. Commit the specification, schema, checker, and implementation before their final verification. Keep live operational records outside the source checkout where they would make release inputs dirty. Publish the final ledger as an immutable CI artifact or on a separate evidence ref, linked from the report; do not keep committing successful-main evidence onto main and thereby changing the SHA being attested. Documentation changes that affect the source tree must land before the final snapshot and receive their applicable verification.

Checkpoint after every gate, before and after publication/merges, and whenever a central change invalidates downstream evidence. After compaction or restart, reload the records, inspect current Git state and remote revisions, reconcile running jobs, and continue the ready queue. Re-run proven work only when inputs changed or a discovered defect invalidates it. Keep concise progress updates while working. Missing access or an external dependency blocks only dependent work; finish all independent authorized work and record the precise remaining blocker.

## 4. Stage gates and sequencing

| Gate | Work and dependency | Required exit evidence |
| --- | --- | --- |
| G0 | Inventory and execution setup | Exactly 32 repositories; current SHAs, PRs, checks, workflows, workload/platform matrix, dependency graph, effective model configuration, and access gaps recorded. |
| G1 | Velnor GitHub-hosted recovery | Generated policy, real required PR checks, and post-merge main CI succeed on GitHub-hosted runners without needing Velnor capacity. |
| G2 | Velnor release and distribution | New preview and stable releases are delivered through APT and Homebrew and pass clean installation/upgrade tests; Velnor and both distribution repositories pass their hosted checks. |
| G3 | Entire fleet GitHub-hosted migration | All 32 use the approved generator; migration PRs and current main revisions pass real hosted CI; open PR coverage is reconciled. |
| G4 | Actual macOS/OrbStack pilot established | Actual host access, installation, routing, representative jobs, and hosted comparisons are exercised; concrete runtime defects are recorded for G5. |
| G5 | Velnor fixes delivered and pilot passed | Pilot defects are fixed, reviewed, released through both channels/distributions as needed, installed, and reverified against the complete pilot checklist; no final dependency on an unpublished checkout. |
| G6 | Entire fleet dual-provider migration | Both lanes are generated and merged for all repositories; eligible workload coverage passes on PRs and resulting main revisions. |
| G7 | Independent final audit | Current revisions, PRs, required jobs, package channels, pins, runtime identities, and run evidence reconcile; deterministic gate and separate reviewer both pass. |

G2 necessarily includes changes to `tailrocks/velnor-apt` and `tailrocks/homebrew-velnor`; waiting until G3 to repair them would make G2 impossible. These repositories still remain entries in the full fleet gates.

G4 and G5 form a repair loop: failing pilot runs may immediately produce reviewed hosted-built candidate fixes and updated packages. A fully passing installed pilot is the exit from that loop, not a prerequisite for building the fix. G6 may reopen the same loop for new workload-specific defects.

Do not start operational macOS lane rollout until G3 passes across the full fleet. Do not merge the broad dual-provider rollout before G4/G5 pass. Later generator/runtime defects reopen the relevant earlier validation and delivery work; the first successful Velnor release is not automatically the final one.

Native macOS compilation/packaging fixes, generated Homebrew CI, and functional package smoke tests on GitHub-hosted macOS are G2 prerequisites and may be implemented before G3. The G3 barrier concerns operational deployment and live Velnor job execution on my Mac. G2 must not require OrbStack or nested virtualization on hosted macOS runners.

## 5. G0–G1: generator integrity and GitHub-hosted recovery

Snapshot every workflow entry point, reusable workflow/action, scanner unit, generated state file, status-check contract, trigger, and release dependency. Capture existing failure logs before changing things. Identify whether each failure belongs to product code, generated output, bootstrap, external policy, or unavailable execution capacity.

Regenerate through Velnor's actual current configuration/schema and commands. At preparation time the input is `.github-gen/velnor-workflow.toml` with schema 2; verify against source and CLI help. Do not rely on obsolete README examples or invent flags. Change generator logic and typed declarative configuration, then regenerate all outputs and sidecars together. Do not hand-edit generated `.github` files, add repository-name conditionals to generic logic, or bypass the generator with copied workflows/raw YAML templates. Audit existing static-file escape hatches so they cannot hide handwritten CI behavior that belongs in generic primitives.

For the hosted baseline, make automatic providers and default dispatch select GitHub-hosted only. Preserve the Velnor implementation and future provider support, but remove operational dependence on Velnor runners, org pools, and queued self-hosted jobs. Velnor integration tests executed by a GitHub-hosted machine are allowed; they still need to run and pass.

Repair the bootstrap loop: a fresh checkout and empty cache must obtain a verified, pinned generator/runtime using a supported distribution path. Do not require artifacts from an unreachable PR run, a successful release that this same broken workflow must first produce, mutable `latest`, or a developer checkout. A bounded source-build bootstrap is acceptable while recovering the first version; ship a verified normal installation path and remove temporary recovery assumptions before completion.

Record separately the generator source revision, artifact digest, consumer source revision, workflow output digest, and scanner-input state. Avoid self-referential pinning that changes its own inputs indefinitely. Use a staged generator commit/release followed by regeneration/pin adoption; prove the stable generation result from clean clones, including shallow checkout behavior. Stale sidecars must be fixed without weakening drift checks.

Audit PR, push/main, dispatch, schedule, preview, stable-tag, and merge-queue triggers where applicable. Include the correct required status contexts and supplying GitHub Apps. Update check names/rulesets compatibly with workflow deployment: preserve meaningful checks, install working replacements, then retire obsolete contexts. Do not disable protection, DCO, tests, security checks, or required jobs to manufacture green.

Write the transition sequence for each check contract before applying it. First prove equivalent or stronger hosted verification on the exact migration candidate, including the correct check-producing App. Preserve neutral existing context names where their substantive work remains equivalent; never report hosted work as a successful Velnor job. Where obsolete provider-specific contexts would deadlock the transition, replace their requirement with the verified hosted contract in a coordinated ruleset/workflow cutover using the existing authorized permissions. Preserve unrelated required checks and avoid a period with no meaningful merge gate. Add dual requirements only after the corresponding jobs can report them reliably, then remove obsolete contexts. Record old/new rules and evidence; unavailable policy permissions remain a concrete blocker.

Follow dispatches through child runs and reusable workflows. A green scheduler, maintenance workflow, or dispatch wrapper does not establish successful CI. Ensure the aggregate check fails for missing, failed, canceled, or timed-out expected work. Validate affected-unit selection and include a full-unit baseline so a documentation-only diff or empty matrix cannot hide broken builds.

Fix cold-cache and warm-cache behavior, source-pin fetching, permissions, checkout depth, job dependencies, Docker/Buildx setup, version metadata, and packaging prerequisites. Keep independent main runs parallel; use narrowly scoped locks only for mutable publishing destinations or other real shared resources. Bound retries and timeouts, emit live progress, and diagnose repeated identical failures instead of retrying indefinitely.

Merge reviewed recovery changes only after the relevant PR checks pass, then verify the resulting main commit. G1 is incomplete until that post-merge execution succeeds.

## 6. G2: preview, stable, Debian/APT, and Homebrew

Write and implement one release contract covering product identity, channel, source commit, version, supported platform/architecture, binary inventory, package names, asset names, checksums, signing, consumer update, and installation commands. Preserve supported platforms; implement at least Linux amd64/arm64 Debian distribution and native Homebrew support for the actual macOS host. Inventory Intel macOS and other advertised targets and either validate their existing support or record the unresolved requirement explicitly. Do not advertise targets merely because a cross-compiler produced an archive.

The installed product must include the compatible binaries required to generate workflows and start/manage the intended Velnor runtime, directly or through explicit packaged dependencies. Audit `velnorctl`, `velnor-runner`, and `velnor-workflow` roles; a CLI-only formula that cannot start its sibling runner does not satisfy this goal. Every installed component must expose an unambiguous release version and source identity. Distinguish component crate versions from the product version without misleading the operator.

Keep generator-runtime artifacts and application releases in distinct, typed discovery namespaces. A newer `velnor-workflow-runtime-*` release must never be selected as the latest stable Velnor application package. Select by validated product identity, channel/version rules, manifest schema, and complete expected assets, then verify provenance. The runtime producer must remain acyclic with the application release that consumes it.

Test discovery across paginated mixed release listings, previews, runtime products, incomplete or invalid assets, no eligible release, and API failure. Feed updates must preserve the other channel and retained recovery versions.

### Channel contract

| Property | Preview | Stable |
| --- | --- | --- |
| Source | Tested, trusted main revision, with unique recorded build identity | Reviewed release revision and immutable version tag |
| Version | Unique, orderable prerelease identity; no accidental stable selection | Valid monotonically advancing release version |
| GitHub release | Explicit prerelease semantics and complete manifest/assets | Explicit stable semantics and complete manifest/assets |
| APT | Deliberate preview channel selection | Stable installation remains stable by default |
| Homebrew | Explicit preview formula/package channel; `--HEAD` alone is insufficient | Explicit stable formula/package channel |
| Mutation | Immutable build artifacts; a rolling pointer may advance after verification | Published version assets and tag identity remain immutable |

Derive concrete names and version syntax from the current product contract. Test ordering with actual Debian/Homebrew behavior, including a preview preceding its corresponding stable version. Preserve old install identities through compatible aliases or documented migration where names change. Document coexistence/conflicts and explicit channel switching.

Use real events and permissions. Exercise PR packaging checks, main preview publication, stable release triggering, consumer updates, and package repository publication. Verify automation-token trigger behavior against current GitHub documentation and the actual run chain; do not assume that a generated push/tag/update causes downstream CI. Use the existing supported App/dispatch mechanism or repair it. Ensure updates do not loop between repositories. Preview and stable consumer refresh paths must both work without a hidden manual repair.

For preview, publish immutable versioned assets first, validate them, then advance the channel reference/index. Do not delete the currently installable preview before its replacement exists. Retain enough history for recovery. For stable, reruns must confirm matching existing assets or finish missing work safely; they must not rewrite published tags or replace same-version bytes. Test partial publication and retry behavior in a bounded fixture/staging context before applying the real release flow.

Build and verify independently as appropriate, but give external publication one owner. After dual-provider rollout, require both eligible verification lanes and native-platform checks before the single publication job changes releases, images, feeds, or taps. Repository-scoped destination locks and monotonic updates must prevent slower old preview runs from overwriting newer state. Do not serialize unrelated CI to achieve this.

Keep an explicit GitHub-hosted recovery release mode for repairing an unavailable/broken Velnor runner or introducing a new runtime capability. It must be deliberately selected, record its recovery reason and exact candidate, pass the full applicable hosted/native/package checks, and use the same verified singular publication path. It must never silently activate after Velnor failure or claim dual-provider completion. An isolated source-identified development runner may also validate candidate behavior before publication. After recovery delivery, install the released fix, restore normal dual verification, and pass both lanes before G7. Final success requires this restored normal path, not merely a recovery publication.

Use an acyclic release sequence: validate candidate binaries/packages and staged feed/formula content; admit the exact source revision using the required prepublication checks; publish immutable product artifacts; update APT indexes and Homebrew formulas; then run clean-client installation and upgrade checks against the real endpoints. Postpublication checks are mandatory release acceptance evidence, but cannot be prerequisites for creating the first available version. Do not make a producer wait for a consumer that requires that unpublished release, or require the candidate to be installed from a public channel before publishing it. On postpublication failure, repair forward or restore an allowed channel reference while preserving immutable version assets.

### Required package verification

- [ ] A new preview and a new stable version are actually published through working workflows; successful dry runs alone do not pass G2.
- [ ] Tags, release IDs, source commits, manifests, package versions, binary identities, architecture declarations, and asset digests agree.
- [ ] Debian packages contain the intended runtime/CLI dependencies and correct metadata, permissions, configuration paths, and service definitions where applicable.
- [ ] The published APT repository has valid signed metadata and correct package indexes for both channels/architectures. Fresh clients use a scoped signing key configuration and verify signatures; no insecure trust bypass.
- [ ] A clean Debian environment can install via the real published APT endpoint. A local `dpkg -i` is supplementary evidence, not proof of APT delivery.
- [ ] Stable → new stable, preview → newer preview, and documented preview → stable/channel-switch paths work. Verify intended configuration/state preservation and service restart behavior where relevant.
- [ ] Service start/stop/restart tests run in an environment that actually supports the advertised service manager; a container without it is not proof.
- [ ] The published Homebrew tap has meaningful generated PR/main CI and both channels. Audit formulas, artifact URLs/checksums, supported architectures, install/test logic, and bottles where provided.
- [ ] A clean GitHub-hosted macOS environment installs from the published tap and verifies every required native binary. Default installation must not depend on an unpublished source checkout, local symlinks, or a fallback executable already on PATH.
- [ ] Preview and stable Homebrew installation, upgrade, switching, and uninstall behavior match the documented contract. Prefer verified prebuilt artifacts for ordinary installation; document/test any intentional source-build mode separately.
- [ ] Installed commands perform a useful smoke test, report the expected release/source identity, and can find their required sibling executables. Actual OrbStack host execution is verified later on my Mac in G4/G5.
- [ ] Producer, consumer, publication, and installation logs are linked for each channel. A feed state file or a released archive alone cannot substitute for this chain.

If no supported previous preview exists, publish an initial verified preview and a newer uniquely identified preview through the intended workflow, then test the real upgrade. Reuse these builds for APT and Homebrew evidence; do not invent a historical package only inside the verifier or mutate same-version assets.

A release build may legitimately change packaging metadata, but it must not conceal an unexpectedly dirty source tree or embed an unrelated SHA. Audit inherited Firecracker/guest-payload dependencies and separate feature/package boundaries so the requested Docker/macOS product does not require unsupported host virtualization. Preserve unrelated established release features deliberately rather than deleting them for a green build.

## 7. G3: migrate every repository to hosted workflows

Choose one reviewed, published, immutable generator baseline for each migration epoch. Its source revision, runtime product, digest, and generated output must agree. Start from the latest relevant Velnor work verified at that epoch; do not chase a moving `main` tip independently in each agent. When central fixes are needed, review/publish them, advance the epoch, regenerate every affected consumer, and invalidate affected evidence.

Build a behavior inventory before replacing old workflows. Map each former responsibility to its generated replacement, including release, preview, update feeds/taps, Renovate, schedules, signing/notarization, artifacts, service topology, and tests where present. Remove superseded handwritten/legacy Velnor workflows only after their required behavior has a verified replacement. Do not silently preserve duplicate competing pipelines.

Use scanner-derived units and typed configuration for non-detectable choices. Extend generic detection/primitives centrally for missing repository categories. Add regression fixtures for real scanner, routing, generation, release, and execution failures; do not merely assert newly written YAML snapshots or update expected cache keys without validating their semantics.

| Repository category | Meaningful generated verification |
| --- | --- |
| Rust libraries/workspaces | Applicable format/lint/test/doc/example/feature checks, integration dependencies, actual publishing contracts, and correct dependency closure. |
| Polyglot products/playground | Every discovered Rust, TypeScript/Bun, Gradle, Docker, database, telemetry, desktop, and end-to-end responsibility; no Rust-only reduction. |
| APT repositories | Feed generation, signatures/indexes, updater/channel behavior, package provenance, and fresh installation; docs lint alone is insufficient. |
| Homebrew repositories | Formula/cask metadata and update behavior, artifact checksums, platform-correct installation/version tests, and existing preview/stable contracts. |
| Skills/plugin repositories | Manifest/frontmatter/catalog consistency, references, bundled helper validation, and template contracts. Distinguish real executable units from embedded example/template manifests. |
| Jackin role images | Role validation, Dockerfile checks, image build, architecture availability, runtime smoke tests, and producer/image dependency compatibility. |
| GitHub action repository | Action metadata and shell checks plus real consumer fixtures exercising success, failure propagation, download/version selection, build/no-build behavior, and Docker/Buildx usage. |

Cover repositories with missing workflow history explicitly. A `NO_WORKFLOWS_REQUIRED.md` marker, unsupported scan, generated empty matrix, or absence of CI is not a completed migration when meaningful behavior exists. In particular, investigate the documented `termrock` scanner omission, omitted `holla-apt` delivery workflows, unconfigured skills repositories, and narrow `jackin-role-action` coverage.

Determine native platform capabilities from actual code/dependencies, not only language names. A Swift compiler on Linux cannot establish correctness for AppKit, SwiftUI, Xcode, or XCFramework code. Preserve the native Apple checks/cadence in Jackin, Tablerock, and the playground, and Homebrew cask obligations. Fix incorrect scanner/routing declarations centrally.

For each Apple workload, validate its required macOS version, Xcode/Swift toolchain, SDK, deployment target, and architecture against the actual hosted runner image. A macOS label alone is insufficient. If a required image/toolchain is unavailable, record the exact capability blocker rather than weakening the application requirements.

Migrate in dependency-aware waves: Velnor distribution/supporting primitives, representative category canaries, then remaining independent repositories in parallel. Preserve image/product → consumers → taps/feeds dependencies discovered from source. Do not extend the fixed fleet implicitly when reading an upstream dependency; record any essential out-of-scope change as a dependency.

For each repository, require byte-stable regeneration, meaningful local/static checks, successful migration PR checks, a deliberate full-workload hosted run, a reviewed merge, and successful CI on the resulting main SHA. Verify publishing repositories' actual update/publish behavior where relevant without issuing arbitrary new releases of every unrelated product.

Inventory all open PRs, including drafts, bots, and forks, at G0 and refresh at G3/G7. Keep them in the ledger with their applicability/trust requirements. Repair CI migration/infrastructure failures and relevant blocking regressions; merge updated main into branches where authorized and appropriate, then re-run current required checks. An unrelated feature failure or required outside action stays a named blocker. Do not close PRs, remove them from the report, or relax checks to claim that all PRs are green.

G3 is a full-fleet barrier. An inaccessible repository, unresolved required PR check, or missing meaningful workflow prevents that gate from passing; continue resolving it while other ready hosted work proceeds.

## 8. G4–G5: Velnor on the actual macOS host

Run this phase on my actual authorized current Mac. Record macOS version, host identity/architecture, OrbStack version, Docker context/socket, Docker server OS/architecture, engine resources, Velnor package version/source, and job image digest/platform. A Linux development container, GitHub-hosted macOS machine, or another Velnor server does not prove this phase. If the current execution environment cannot reach my Mac, finish independent work and identify that exact access dependency without claiming local verification.

Use native macOS Velnor control/runner processes with Linux Docker workloads through OrbStack. Do not provision Velnor-managed Firecracker, libvirt, or separate runner VMs. OrbStack's internal Linux VM is part of Docker on macOS and is permitted. Do not impose per-container CPU/RAM limits; enforce one explicit host-wide concurrency budget and bounded disk/cache retention. Do not alter or prune unrelated user containers, volumes, services, or projects.

Begin from the existing `velnorctl host` implementation and current source. Its preparation-time documentation describes repository-scoped recovery hosts, not automatic membership in an org runner pool. Verify registration scope, labels/groups, shared capacity, trust policy, Docker resolution, and daemon defaults in code and live operation. Implement the smallest coherent extension needed for the full fleet. Do not assume one repository-scoped process can claim all 32 repositories or that separately starting 32 instances gives a global limit.

Install the released Homebrew product first. Permit development builds in explicitly identified isolated instances for rapid reproduction; record their SHA and differences. Final pilot and fleet evidence must use released, installed binaries and published/verified images. Provide reproducible start/status/drain/stop commands and persistent startup through the appropriate macOS mechanism if needed for continuous PR/main execution. Do not depend on Linux systemd on the macOS host. Verify daemon restart and installed startup configuration through controlled Velnor service unload/reload or an equivalent isolated operation. A full machine reboot or user logout is not required for this goal.

### Platform and provider contract

| Workload | GitHub-hosted requirement | Velnor on macOS/OrbStack requirement |
| --- | --- | --- |
| Container-compatible Linux verification | Required | Same substantive tests/commands and inputs on the matching declared target |
| Linux package verification | Required for supported targets | Required for eligible targets, with native/emulated execution identified |
| Darwin-native builds/tests, Apple signing, macOS installation/casks | Required on native macOS | Explicitly outside Linux-container execution; portable metadata/updater checks still run in both lanes |
| Native Windows obligations, if discovered | Preserve matching hosted coverage | Explicitly outside Linux-container execution |
| Release/feed/tap mutation | One authoritative publisher after its applicable verification gates | Verify equivalent inputs/artifacts without a competing publisher |

Every repository must have meaningful eligible checks in both lanes. Platform-bound jobs stay visible in the coverage matrix and run on matching hosted systems. These are platform exclusions, not successful Velnor executions. Do not classify an ordinary Linux job as ineligible merely because Velnor currently fails it. Fix missing legitimate generator/runtime support. Trust exclusions must also be established before execution and reported separately.

On Apple Silicon, distinguish native Linux arm64 from emulated amd64. Prefer an available matching hosted architecture for direct parity comparisons and retain every required amd64 target. Verify artifact execution as appropriate for the advertised architecture. A different architecture's success is not proof of x64 coverage; an emulated run is not native performance evidence. Never make the hosted baseline depend on nested OrbStack virtualization on GitHub's macOS runners.

### Pilot acceptance tests

- [ ] **Routing:** actual accepted GitHub jobs reach this Mac, with stable provider, host, instance, slot, and repository identity. Test multiple repository scopes and trust-specific Docker labels/groups. Newly generated routing needs new eligible runs; editing YAML does not retarget already queued jobs.
- [ ] **Container execution:** inspect each job/container relationship, guest OS/architecture, mounts, image digest, and process execution. All repository build/test/action payloads assigned to Velnor execute inside containers; host orchestration alone may run natively. No silent host-shell or hosted-provider fallback counts as Velnor success.
- [ ] **Docker endpoint/preflight:** resolve the intended OrbStack endpoint consistently across all commands. Missing socket, incompatible platform, or missing image fails before admission with useful diagnostics. Do not silently use another Docker engine.
- [ ] **Real workload coverage:** pilot Velnor plus representative library, polyglot/service/Docker, skills, packaging, and action workloads. Compare the actual generated action/command graph with runtime capabilities. Exercise checkout, expressions, outputs, environment/path files, reusable/composite/action support, post steps, and artifact/cache operations that the fleet uses.
- [ ] **Nested Docker/services:** prove service health checks, networking, testcontainers, Docker actions, and Buildx/BuildKit behavior. The outer OrbStack socket belongs to Velnor orchestration; Docker-using jobs receive a private per-job daemon/endpoint or an equivalently enforced boundary. Test that job A's Docker client cannot list, control, or remove job B's resources or unrelated user containers. Include nested builders/services in cancellation and cleanup, and preserve useful caches within the verified boundary.
- [ ] **Shared capacity:** queue more than N jobs from multiple repository instances against one configured host-wide `max_jobs=N` budget. Active jobs never exceed N, and all eligible finite jobs eventually complete. Capacity waiting must not lose GitHub leases; cancellation of a waiter must not leak a permit; a restarted instance must not reuse a stale permit concurrently. Verify FIFO at the Velnor admission boundary where Velnor controls order, without claiming control of GitHub's upstream scheduler.
- [ ] **Resources:** container inspection confirms that Velnor has not imposed per-container CPU/RAM limits. Record the engine's actual capacity, bounded retention, backpressure, and admission behavior.
- [ ] **Cold/warm correctness:** run representative identical workloads at the same source/target with empty and populated caches. Verify substantive tests and outcomes, cache compatibility, repository/trust/architecture boundaries, corruption recovery, and timing. Do not change cache-key expectations without proving the intended invalidation/preservation contract.
- [ ] **Cancellation:** cancel long-running work with child processes, service containers, Docker actions, BuildKit activity, and post steps. Verify bounded termination, `always()`/`cancelled()` semantics, logs, remote terminal status, released permits, and removal of owned resources. Address existing documented live Docker cancellation gaps.
- [ ] **Recovery:** interrupt an isolated runner instance, restart it, and exercise a controlled Docker connection interruption. Prove reconciliation, bounded retries, no duplicate completion/publication, cleanup of owned orphan resources, and successful subsequent work. Do not interrupt unrelated user workloads.
- [ ] **Connectivity:** verify recovery after a controlled transport disconnect/reconnect. Validate handling of host wake/reconnection through supported isolated tests or naturally observed events; do not force the user's Mac to sleep or reboot. Record which real host lifecycle conditions were exercised.
- [ ] **Observability:** logs are readable locally and on the actual GitHub job; exit status and job completion agree. Publish useful identity/version/image metadata and capture queue time, duration, cache outcomes, retries, and failure classes. Missing remote logs or permanently unresolved completion is an operational failure.
- [ ] **Trust:** fork/unknown PR code must not gain host credentials, publishing secrets, or privileged host Docker access. Keep a concrete eligibility/review policy. Hosted checks must remain available. Required Velnor evidence needing trusted approval stays pending until valid execution; exclusions cannot be reported as passes.
- [ ] **Trust identity:** derive eligibility from the event, repository identity, and configured policy. Any required external-PR authorization binds to an immutable head/integration SHA and expires when it changes. A label, title, actor string, or workflow edited by the PR cannot independently grant host access. Verify that ordinary checks do not inherit runner-registration or publishing credentials.
- [ ] **Parity:** both providers test the same source object and logical workload with equivalent locked dependencies, fixtures, targets, and success criteria. Record environmental differences that affect interpretation. A manual dispatch is diagnostic evidence unless its required PR-check association is proven.
- [ ] **Packaged operation:** repeat the successful pilot using the released Homebrew package, including runtime start, useful job execution, diagnostics, drain, stop, restart, and cleanup. A successful development checkout alone does not pass G5.

For each defect, preserve a minimal reproduction, fix its owning generator/runtime component, add a meaningful regression test, independently review it, re-run the failed scenario, and then the affected hosted/Velnor canary. Do not fix 32 output files separately when the cause is shared.

If G4–G6 changes Velnor runtime, generator, packaging, or release behavior, publish new preview/stable versions as required, verify APT and Homebrew again, install the corrected runtime on this Mac, advance the approved generator epoch, regenerate affected repositories, and refresh their evidence. Track runtime and generator versions separately when they have different release identities. Repeat until the final fleet and distributed product contain all fixes.

## 9. G6: merge and prove both providers throughout the fleet

Generate both automatic lanes through the same typed provider model and logical verification plan. Keep GitHub-hosted default/recovery selection. Make names and summaries identify provider and actual host; the word `Velnor` in a job title is insufficient provenance.

For each repository: prepare the dual-provider change, run all applicable hosted and Velnor checks on the same PR integration candidate, independently review, merge, then run both providers on the resulting main SHA. Preserve native-only checks and singular publishing effects. If the host is offline or a required Velnor job cannot be admitted, show a pending/failing lane; do not silently fall back or mark it green.

Update required-check contracts deliberately so each applicable lane contributes to the final merge gate. Preserve correct handling of external checks, affected-unit selection, trusted events, forks, and merge queues. Diagnose legitimate exclusions explicitly; no empty provider matrix, `continue-on-error`, removed test, skipped required job, or permissive aggregate may substitute for passing work.

Reconcile every open PR at its current head/base and trust state. A PR that remains blocked by unrelated failing feature work or an external required approval must be identified; the overall all-PR-green objective remains incomplete. Migration authorization does not authorize silently merging that unrelated feature.

After the last central fix and final pin adoption, verify all 32 again to the extent their inputs changed. Ensure the current Velnor preview/stable delivery chain itself exercises the final dual-provider verification contract, with one publisher. If relevant late changes require another version, release it; do not stop with a corrected local Mac and outdated package channels.

## 10. Evidence schema and deterministic verification

Maintain machine-readable evidence sufficient for a separate reviewer to reconstruct each claim. Store these fields, with explicit applicability where necessary:

```text
repository, repository_role, default_branch, default_branch_sha, observed_at_utc
generator_revision, runtime_product_id, generator_artifact_digest
configuration_digest, generated_tree_digest, scan_state_digest
runtime_release_version, runtime_source_sha, job_image_digest
expected_workload_ids, required_check_contexts_and_apps
workload_platform_architecture, provider_eligibility, justified_exclusions
PR_number, PR_head_sha, PR_base_sha, tested_merge_sha, merge_group_sha
workflow_path, workflow_revision, event, run_id, run_attempt, run_url
trigger_source_sha, actual_checkout_sha, provider, runner_name, host_id
expected_jobs, actual_job_ids, actual_job_conclusions, logs, child_run_links
release_channel, release_version, tag_target_sha, release_id, asset_digests
APT_feed_revision_suite_and_candidate, Homebrew_tap_revision_and_formula
install_upgrade_test_environment, installed_binary_identity, functional_result
owner, reviewer, gate_status, blocker, next_action
```

Do not trust a workflow's display name, badge, overall green conclusion, newest run, or `github.sha` without checking event semantics and what was actually checked out. For PRs, distinguish contributor head, base, synthetic merge candidate, merge-queue candidate, and final main commit. For `workflow_run` release chains, validate producer workflow/repository/event/branch/conclusion and consume its exact source SHA; do not publish a different moving main tip under inherited privileges.

The deterministic checker must verify manifest coverage, pins/digests, current revision correspondence, expected nonempty workload inventory, proper provider/platform, required job conclusions, child-run completion, check-context association, and release/install evidence. It must fail for stale, missing, queued, canceled, timed-out, unexpectedly skipped, or failed required work. It may accept an explicitly justified non-applicable case only without counting it as executed success. Test the checker with stale-SHA, skipped-job, missing-repository, wrong-provider, failed-child, and mismatched-artifact fixtures.

Require real success on every affected final revision, plus representative clean-cache and warm-cache executions for generator bootstrap, package delivery, and Velnor runtime behavior. Do not create an arbitrary soak-duration requirement or repeat unchanged tests indefinitely; use failures and changed inputs to determine further testing.

## 11. Final completion checklist

- [ ] **Scope:** all 32 exact repositories appear once; none is substituted, silently omitted, or accepted solely because it has no CI.
- [ ] **Sequence:** hosted Velnor and package delivery passed before broad hosted migration; the full hosted fleet passed before operational macOS rollout; pilot stabilization preceded broad dual-provider merges.
- [ ] **Generation:** every repository uses the approved Velnor generator; declared pins, runtime artifacts, scanner state, and generated output agree and reproduce cleanly.
- [ ] **Coverage:** every required behavior has a generated replacement; missing category support, template classification, native routing, feed automation, and action behavior are resolved.
- [ ] **Hosted CI:** actual required jobs pass for final migration PR candidates and resulting/current main revisions in every repository; no hidden Velnor prerequisite remains in the hosted recovery route.
- [ ] **Velnor CI:** all eligible workloads pass on this Mac in OrbStack Docker containers, on the same source/target contract as hosted; native-only work passes on appropriate hosted systems.
- [ ] **PRs:** all current open PRs are reconciled at the declared snapshot with their required checks; unresolved cases remain blockers rather than green rows.
- [ ] **Distribution:** new Velnor preview and stable versions containing all relevant fixes are published and installable through both APT and Homebrew, with tested upgrade/channel behavior and functional installed-product evidence.
- [ ] **Publication:** application/runtime discovery is correct; channel updates are monotonic, retry-safe, traceable, and preserve the other channel; final publication has one authority after applicable verification gates.
- [ ] **Mac runtime:** routing, actual identity, packaged binaries, shared admission, container execution, logs, cancellation, restart, orphan cleanup, and cache behavior are verified.
- [ ] **Merges:** approved changes are merged; their resulting main commits have completed the required executions. No green PR is substituted for a missing post-merge run.
- [ ] **Reconciliation:** final generator/runtime fixes have propagated through new packages, local installation, fleet regeneration, and affected verification. No consumer remains on a known broken epoch.
- [ ] **Independent review:** another agent checks implementation and raw evidence, attempts to disprove completion, and resolves material findings. The author does not approve their own work.
- [ ] **Deterministic gate:** the complete evidence validator succeeds after a fresh read of all default-branch tips and open PR heads.
- [ ] **Operations:** the runbook documents exact tested regeneration, provider selection, release, install, host lifecycle, recovery, and rollback procedures.

Before declaring success, take a final UTC snapshot of all default-branch tips and open PRs. If any relevant revision moved, including automatic release/version-bump commits, invalidate the affected evidence and verify the new revision. State the snapshot time and revisions; do not promise that future commits will remain green.

The final report must contain a compact 32-row fleet table with PR/main revisions, hosted results, Velnor results, native-only coverage, generator/runtime identity, and evidence links; a Velnor preview/stable × APT/Homebrew delivery table; merged PR/release links; the tested Mac identity/configuration; and explicit remaining blockers. Put verbose job-level details in the ledger. Say the goal is complete only when every applicable acceptance condition passes. Otherwise state the exact incomplete gate and required action, after completing all independent work.

Start with G0 and immediately schedule independent investigations. Turn findings into bounded implementation tasks, execute them, verify them, and continue through G7. Do not stop after producing another plan, opening PRs, generating YAML, uploading archives, or seeing one green run.

## 12. Preparation findings to revalidate at execution time

These are read-only audit observations from 2026-09-19, not fixed assumptions about the next checkout. They identify concrete starting points. The acceptance requirements above remain authoritative if these particular defects have already been repaired.

| Observed evidence | Investigation required |
| --- | --- |
| At Velnor main `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`, schema 2 selects both automatic/default-dispatch providers and pins generator `fdeed261bd2247a38db6922a7726cd45d3d6f31e`. [Configuration](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/.github-gen/velnor-workflow.toml) | Change automatic, dispatch, aggregate, and release prerequisites coherently for hosted-first. |
| Main policy reports scan-state drift and candidate-artifact acquisition failure; preview fails generated-tree policy before publication. [Main run](https://github.com/tailrocks/velnor/actions/runs/35430875046), [preview run](https://github.com/tailrocks/velnor/actions/runs/35338351916) | Repair exact source/pin/output/sidecar identity and the acyclic runtime-product bootstrap. |
| Stable-tag run `v0.1.277` failed on missing Docker seed context, unavailable generator pin in shallow checkout, dirty release input, and Velnor admission rejection. [Release run](https://github.com/tailrocks/velnor/actions/runs/35332416794) | Reproduce each relevant failure; separate build/policy defects from runtime admission. |
| A hosted PR test failed cache-key compatibility while Velnor jobs queued without capacity. [PR run](https://github.com/tailrocks/velnor/actions/runs/35452270126) | Determine whether cache semantics changed legitimately; do not only replace the expected hash. |
| Existing overlapping work includes seed/pin fixes in PR 952 and stacked generator/release work in PRs 953–954. [PR 952](https://github.com/tailrocks/velnor/pull/952), [PR 953](https://github.com/tailrocks/velnor/pull/953), [PR 954](https://github.com/tailrocks/velnor/pull/954) | Refresh their state, review dependencies, integrate useful work, and complete any required post-merge pin promotion. |
| The APT publisher selected `velnor-workflow-runtime-v1-63cea86d9b7bf2d5` as stable and rejected its version. [Failed feed run](https://github.com/tailrocks/velnor-apt/actions/runs/35433047049) | Separate product/runtime discovery and prove both live channel update paths. |
| Homebrew tap at `7af1249f3d69c9f2e548583cdc9f3e737da41b81` contains a source formula installing only `velnorctl`, with no preview formula or CI. [Tap](https://github.com/tailrocks/homebrew-velnor/tree/7af1249f3d69c9f2e548583cdc9f3e737da41b81) | Implement the complete packaged product/channel contract and generated tap verification. |
| Fleet tree audit found 23 generator configurations; the eight skills repositories and Homebrew Velnor had none. `termrock` had no workflow YAML; `holla-apt` retained only a docs unit. [Termrock omission](https://github.com/tailrocks/termrock/blob/main/.github-gen/NO_WORKFLOWS_REQUIRED.md), [Holla APT omission](https://github.com/tailrocks/holla-apt/blob/main/.github-gen/NO_WORKFLOWS_REQUIRED.md) | Reinventory all 32 and restore meaningful category-specific coverage instead of accepting no-op generation. |
| A successful nightly dispatch led to failing actual CI; sampled policy logs also showed generated-tree/ruleset mismatch. [Nightly wrapper](https://github.com/tailrocks/tracing-request-level/actions/runs/35431025704), [actual CI](https://github.com/tailrocks/tracing-request-level/actions/runs/35431028510) | Follow the complete child-run graph and preserve the actual required-check contract. |
| Tablerock's native package uses Apple frameworks while its inspected Swift workflow targeted Ubuntu. [Native package](https://github.com/tailrocks/tablerock/blob/main/native/Package.swift), [workflow](https://github.com/tailrocks/tablerock/blob/main/.github/workflows/ci-unit-swift.yml) | Correct capability detection/routing and preserve native Apple evidence. |
| `velnorctl host` already supports native macOS orchestration but documents repository-only recovery scope and Linux jobs; execution docs acknowledge action/cancellation gaps. [Host guide](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/content/docs/guides/macos-host.mdx), [execution guide](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/content/docs/guides/execution.mdx) | Extend existing code; verify real multi-repository routing, global capacity, required action support, and owned-resource cleanup. |

The long-execution structure uses durable task state, small verifiable milestones, and repair after observed failures, consistent with [OpenAI's long-horizon Codex guidance](https://developers.openai.com/blog/run-long-horizon-tasks-with-codex). Revalidate the execution session's model overrides and available concurrency against its installed configuration and [current subagent documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents).

Consult current primary documentation where behavior matters: [OrbStack architecture](https://docs.orbstack.dev/architecture) and [Docker support](https://docs.orbstack.dev/docker/) establish the Linux-container/native-emulation boundary; [GitHub runner documentation](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) defines available hosted platforms. [Debian version rules](https://www.debian.org/doc/debian-policy/ch-controlfields.html#version) and [APT authentication](https://manpages.debian.org/trixie/apt/apt-secure.8.en.html) govern version/channel and signed-feed checks. [Homebrew functional tests](https://docs.brew.sh/Formula-Cookbook#add-a-test-to-the-formula) and [bottles](https://docs.brew.sh/Bottles) inform package validation. [GitHub trigger behavior](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow) and [workflow_run semantics](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run) must be checked when connecting producers, consumers, and privileged release workflows.
