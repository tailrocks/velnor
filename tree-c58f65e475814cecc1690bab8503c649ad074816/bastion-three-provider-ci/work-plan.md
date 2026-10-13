# Bastion work plan: ordered steps A0–G2

Status: **authoritative execution order** — branch `docs/bastion-final-plan` (PR `tailrocks/velnor#912`).
Date: 2026-09-17.
Authority: `plans/bastion-three-provider-ci/spec.md` (§1–§9) governs every step. Revalidation inputs live in `plans/bastion-three-provider-ci/evidence.md`. Acceptance is tracked in `plans/bastion-three-provider-ci/checklist.md` (1:1 with the step IDs below). The paste-ready prompt is `goal-bastion-three-provider-ci.md` at the repo root.

Independent source modules are implemented in parallel, but no deployment/consumer gate is crossed before its prerequisites. Every step requires evidence reviewed by a verifier other than its author.

## 0. Operating rules

### 0.1 Orchestrator-only main agent

- The main agent is orchestrator only: maintains the ledger, assigns work, resolves dependencies/decisions, reviews evidence, and unblocks.
- ALL substantive research, design, code edits, source integration, tests, infra, release publication, deployment, review, and verification are delegated to subagents, aggressively, keeping all useful slots busy.
- A designated integration subagent performs source integration/regeneration; a designated infrastructure subagent is the only live bastion writer in its window.
- An agent is never claimed to have run unless it actually did.

### 0.2 Author/verifier separation

- Every module has a named author and a different independent verifier.
- Verifiers attempt to disprove the target invariants and rerun critical checks; author assertions, unchecked boxes, screenshots, or a PR body saying "all green" are not certification.

### 0.3 Parallel workstreams

Current/live evidence; CI/policy; runtime products; provider schema/rendering; APT primitives/feed; package/systemd/unbounded mode; native/global admission; Rust Scale Set protocol; official runner/DinD; trust/credentials; caches/performance; hosted verification; later-repository read-only inventories. Blocked authors get bounded tests, source audits, and adversarial review. No idle agent waits for a shared generator file it does not own.

### 0.4 Worktrees, integration, breaking changes

Isolated worktrees and explicit file ownership. Small verified vertical changes integrate continuously; no prolonged design freeze or giant final merge. Unrelated work and repository-required merge/DCO policy are preserved (`git commit -s`). No backward-compatibility shims, aliases, dual models, or deprecation windows. Breaking code/config/API changes are preferred when they directly reach the target. Package lifecycle/state migration stays explicit and correct.

### 0.5 Ledger

Ledger fields: task ID, dependency, author, verifier, source/branch, finding, target invariant, evidence location, status, blocker, next action. Evidence is append-only/versioned enough to survive compaction. Known external blockers do not stop independent work or authorize fake completion. Correctness and target fit judge every change, never effort/ROI.

### 0.6 Bootstrap dependency resolution

A3 cannot demand an unimplemented third local provider or a package not yet deliverable. A clearly declared generated hosted recovery/bootstrap provider set (or already healthy existing execution capacity) is used, with full expected source checks. An expected local job is never silently skipped and called green; bootstrap qualification is reported separately. After D3/E, trusted supported Linux defaults are all three providers and loss of any required provider fails. Real required checks stay and branch protection is never bypassed to cross this boundary.

### 0.7 Source dependencies versus rollout

The unbounded/global-native foundation is developed in parallel with A/B and included before C2, although operational verification lands at C2. Rust Scale Set and three-provider features may develop concurrently but must not delay a correct first APT deployment. Every feature deployed after it requires another signed APT package. These are successive releases of one architecture, not competing plans.

### 0.8 Gate regression

A later generic fix returns to its affected author/verifier pair. Tooling is published before consumers repin; host code is packaged before upgrade; affected already-migrated repositories are requalified before advancing. No repeated full campaign restart for unrelated changes, and no unverified mixed-version state.

---

## Phase A — Repair bootstrap CI and runtime products

### STEP A0 — Refresh live evidence ledger

**Objective:** Re-resolve every retained audit fact against live state and record corrections without changing the target architecture.

**Actions:**

1. Read repository instructions first: `AGENTS.md` (and `CLAUDE.md` symlink target) in `tailrocks/velnor`.
2. `git fetch origin` in each working checkout; record current `main` SHAs:
   `gh api repos/tailrocks/velnor/git/ref/heads/main --jq .object.sha`,
   same for `jackin-project/jackin` and `ChainArgos/java-monorepo`.
3. Re-check recorded PRs: `gh pr view 904 --repo tailrocks/velnor --json state,headRefOid,title`,
   same for PR 901; note open/closed/merged and head movement.
4. Re-read recorded failure runs with attempts and logs (paginated):
   `gh api repos/tailrocks/velnor/actions/runs/35129353335 --jq '{conclusion,head_sha,run_attempt: .run_attempt}'`,
   `gh run view 35129353335 --repo tailrocks/velnor --log-failed` (same pattern for runs
   `35136207272`, `35114867283`, `35094895601`); record whether each failure signature still reproduces.
5. Re-resolve releases and tags: `gh release list --repo tailrocks/velnor --limit 20`;
   confirm the `0.1.275`-era crate-version vs release vs package identities are not conflated.
6. Re-resolve upstream Scale Set main:
   `gh api repos/actions/scaleset/git/ref/heads/main --jq .object.sha`
   (retained reference `fb563005…`; record drift, do not assume it).
7. Re-read the APT omission declaration blob in `tailrocks/velnor-apt`
   (`.github-gen/NO_WORKFLOWS_REQUIRED.md`); record whether APT primitives are still absent.
8. Verify accessible credentials/permissions WITHOUT printing values:
   `gh api repos/tailrocks/velnor/actions/permissions`,
   runner-group and environment protection rules, Pages deployment permissions.
9. Read-only bastion inventory over SSH (no writes, no package changes):
   `uname -a; lscpu; free -m; lsblk -o NAME,SIZE,TYPE,MOUNTPOINT; df -hT; docker info; docker version; systemctl list-units --type=service --state=running | head -50; ss -tlnp`.
10. Record every correction as `old observation → new evidence → consequence` in the ledger; consequences never alter the spec architecture.

**Dependencies:** None (first step; read-only consumer research for later repos may run alongside).

**Outputs:** Refreshed ledger with exact SHAs, paginated inventories, failing logs, permission facts (no secret values), read-only bastion inventory, and the correction list.

### STEP A1 — Repair Velnor CI, policy, generated ownership, and test failures

**Objective:** Make the bootstrap configuration clean, owned, and fast without losing coverage.

**Actions:**

1. For each live failure from A0: reproduce on the current tree → fix the root cause at its source
   (identify why the architecture allowed the bug class; prefer removing the enabling condition)
   → add/extend a regression test → prove green. A symptom-layer patch is allowed only when the
   root fix is infeasible or belongs in a separate change, and then the deferred root cause is named.
2. Regenerate with the trusted generator and prove exactness, e.g.:
   `./target/debug/velnor-workflow --plain --force && ./target/debug/velnor-workflow --plain --dry-run`
   must report 0 files; ownership inventory must list every `.github/workflows/*.yml`,
   referenced local actions, and generated manifests, flagging unexpected files.
3. Prove structured policy clean on the generated tree and `actionlint` clean on all workflows.
4. Preserve: full source coverage (no dropped units), useful output, caches, streaming progress,
   parallel main runs, and PR-supersession scope (same-PR older attempt only).
5. Capture a timing baseline per job/step from live runs and improve the critical path
   (measure Planning/Policy/unit bootstrap; no cargo/source-build fallback is added to do it).
6. Gates on the final tree: `cargo test -p velnor-workflow`, contract tests,
   `cargo clippy --all-targets -p velnor-workflow -- -D warnings`, `cargo fmt --check` — all green.

**Dependencies:** A0 (live failure list).

**Outputs:** Fixed sources + regression tests, clean generation/policy/lint evidence, timing baseline with critical-path improvement, all gates green.

### STEP A2 — Implement and qualify immutable runtime producer/consumer plus publish-before-pin

**Objective:** Enforce spec §3.2 end to end: trusted `R` stays the normal consumer while candidate `C` is built once, verified, published, then promoted atomically.

**Actions:**

1. Implement producer/consumer per spec §3.2 steps 1–6 (closure identity, product-only setup,
   head-anchored candidate, fail-closed guards, digest-first provisioning, attestation).
   Files: `crates/velnor-workflow/**` (generator + policy), setup action sources, producer workflows.
2. Prove the closure covers every build-affecting input: local deps, build scripts/inputs, lockfiles,
   compiler/target/features/profile, relevant config. Never assume one crate hash covers the workspace.
3. Negative tests (must reject): missing product, wrong digest, wrong manifest, untrusted
   signer/ref, consumer-repo artifact lookup confusion, PR-provided checksum treated as trust.
   A missing product surfaces as a precise producer defect; normal Planning/Policy/unit
   bootstrap performs zero cargo/source builds.
4. Prove deduplicated candidate builds: one build per full source closure/platform; reused only
   on exact source + provenance; changed merge closure forces exactly one new build.
5. Cold-consumer test: a consumer with empty caches resolves and verifies the published product
   (producer repo, closure, source identity, OS/arch, features/profile, manifest+binary digests,
   trusted signer workflow/ref, binary self-report) with zero integrity waivers on cache hit.
6. Prove the atomic promotion transition: one commit updates pin/product metadata plus the entire
   generated tree using the verified product; final pin/tree consistency is mechanically checked.
7. Never select the daemon package via unqualified `releases/latest`; runtime products and daemon
   releases stay separate release identities in code and tests.

**Dependencies:** A1 (clean tree to build on).

**Outputs:** Producer/consumer implementation, negative-test suite, cold-consumer proof, atomic-promotion proof, no-fallback proof (zero cargo invocations in consumer logs).

### STEP A3 — Three consecutive full green bootstrap main runs

**Objective:** Prove the repaired bootstrap configuration is stably green before building delivery on top of it.

**Actions:**

1. Declare the bootstrap provider scope explicitly in the ledger (per §0.6: hosted recovery/bootstrap
   set or existing healthy capacity — labeled bootstrap, never triple-qualified).
2. Obtain three consecutive FULL green `main` runs with zero manual reruns and zero hidden source
   failures: `gh run list --repo tailrocks/velnor --branch main --workflow <ci-main> --limit 5`,
   then `gh run view <id> --repo tailrocks/velnor` per run including attempts.
3. For each run record: run URL, source SHA, plan digest, complete expected-result set per spec §2
   identity, per-unit outcomes, and per-job timings. Every expected result is present and green;
   skipped/cancelled/missing entries fail the gate.
4. Keep real required checks (including DCO) enforced; no protection bypass.

**Dependencies:** A1, A2.

**Outputs:** Three main run URLs with source/plan identities, complete expected results and timings, explicit bootstrap-scope record.

---

## Phase B — APT primitives, velnor-apt, bootstrap release and feed

### STEP B1 — Implement generic typed APT primitives

**Objective:** Implement the spec §7 APT capability model in the generator with no shell/YAML escape hatch.

**Actions:**

1. Implement typed contracts for: source repository + exact release identity, package,
   architecture set, stable/preview suites, signer fingerprint + secret references,
   release-coherence verification, repository assembly, publication records,
   previous-version retention, GitHub Pages artifact/deployment, channel-update tasks
   (only where actually required). One final suite implementation; no compat wrappers;
   `apt-repository` must produce a real feed, never docs-only output.
2. Audit `scripts/verify-release.sh` and reuse its logic through a narrowly defined typed
   verifier contract; generalize reusable logic. Configuration must not execute arbitrary shell/YAML.
3. Generator/IR unit tests for every capability, including malformed-input rejection.
4. Renamed-fixture test: a renamed package/repository proves generic behavior (no repo-name special cases).
5. Negative tests: bad source identity, bad digest, bad key, wrong architecture, incoherent
   release — all rejected before any mutation, with the previous trusted state intact.
6. Generate the full APT CI/publication/required-result workflow coverage from the new
   primitives; prove ownership inventory, local-reference resolution, structured policy,
   actionlint, and fail-closed aggregation on that coverage.

**Dependencies:** A2 (publish-before-pin machinery the APT publisher also uses).

**Outputs:** Typed APT implementation + tests, renamed-fixture proof, negative-test proof, generated APT workflow coverage.

### STEP B2 — Publish generator product, then pin and regenerate velnor-apt

**Objective:** Deliver `tailrocks/velnor-apt` as the first publish-then-pin consumer and remove the false omission.

**Actions:**

1. Publish and attest the approved generator product through the generated trusted hosted
   producer (spec §3.2 steps 4–5); confirm all platform assets + attestations verify.
2. In `tailrocks/velnor-apt`, land one atomic promotion commit: pin/product metadata plus the
   entire regenerated tree using the verified product.
3. Remove the `.github-gen/NO_WORKFLOWS_REQUIRED.md` omission notice and stale direct-install
   docs ONLY after generated coverage genuinely replaces them.
4. Prove sole ownership + reference checks on the velnor-apt tree, structured policy clean,
   actionlint clean.
5. Prove `verify-release.sh` (or its reviewed replacement) covered by tests on the new tree.

**Dependencies:** B1 (APT primitives), A2 (product flow).

**Outputs:** Atomic pin/tree diff in velnor-apt, omission removed on real coverage, ownership/policy/lint proofs.

### STEP B3 — Produce coherent bootstrap Velnor release

**Objective:** Cut a coherent, fully verified bootstrap release containing the unbounded/native-global-admission prerequisites for first bastion execution.

**Actions:**

1. From reviewed source/tag, produce the exact release identity (tag/commit/package version):
   coherent amd64 + arm64 products with manifest/record/checksums, required OCI/payload
   identity (including native arm64 payloads), and trusted attestations — all through
   generated hosted CI.
2. Include the unbounded-policy/global-native-admission prerequisites (developed in parallel
   per §0.7) so the bootstrap package can activate them before the first bastion job.
3. Negative coherence tests: mismatched arch payloads, missing manifest entries, bad digests,
   missing attestations — all fail the release.
4. Keep complete existing release contracts (kernel/rootfs staging is not a VM); genuine
   KVM-runtime tests stay a declared separately qualified capability, never run as VMs on bastion.

**Dependencies:** B2 (proven publisher path), A1–A2 (clean tree + products). Unbounded/global-native source work runs in parallel (§0.7).

**Outputs:** Exact tag/commit/version, amd64+arm64 digests, manifests/record, OCI/payload identity, attestations, coherence-test proof.

### STEP B4 — Publish signed APT through generated single-writer flow

**Objective:** Publish the bootstrap release to the live signed APT feed with verification before mutation.

**Actions:**

1. Run independent digest/source/manifest/OCI/provenance verification BEFORE any feed mutation;
   a failure blocks publication with no partial state.
2. Assemble the staged APT index, retain the previous coherent version + recovery artifacts,
   sign InRelease/Release metadata with the trusted signer, and write the publication record.
3. Deploy via the generated hosted single-writer Pages flow; serialize same-feed/tree mutation
   (never all repository CI); prove an older publication cannot roll back a newer candidate.
4. Independently verify the LIVE APT candidate: signed metadata chain, exact candidate version,
   both architectures, package hashes, source/record, repository origin
   (`apt-get update`, `apt-cache policy velnor-runner`, hash comparisons against the record).
5. Record the trusted signer fingerprint and the exact candidate identity in the ledger.

**Dependencies:** B3.

**Outputs:** Live signed feed + metadata/package chain proof, exact candidate identity, previous-version recovery artifacts, single-writer proof.

---

## Phase C — Bastion install, quota-free native, provisional N

### STEP C1 — Install exact candidate on bastion via repository APT under package lock

**Objective:** Deploy the verified candidate to bastion through the locked APT transaction only, preserving access, config, and running work.

**Actions:**

1. Authenticate the public signing-key fingerprint against a separately trusted project
   reference FIRST; never trust the key from the download URL. Then configure repository-scoped
   `Signed-By` for `https://velnor-apt.tailrocks.com/`.
2. Verify before install: signed InRelease/Release metadata, candidate version, architecture,
   package hash, source/record, repository origin. The APT metadata signature/checksum chain
   and the artifact attestation are checked as distinct proofs.
3. Complete idempotent non-destructive host setup per spec §6 (read-first inventory, preserve
   SSH/recovery, second NVMe untouched, pinned Docker + Buildx/Compose, cgroup v2/systemd
   driver verified, managed paths `/etc/velnor`, `/var/lib/velnor`, `/run/velnor`,
   `/var/cache/velnor`), starting from the §6.1 ansible-configs reference paths re-resolved
   against live `ChainArgos/java-monorepo` `main`. No libvirt/KVM/QEMU.
4. Drain affected jobs per the implemented package procedure; preserve config/secrets.
5. Run the root installation transaction with `VERSION` set to the B4-verified candidate:
   `install -d -m 0750 /run/velnor`, `apt-get update`, `apt-cache policy velnor-runner`,
   `/usr/bin/flock --exclusive --nonblock --no-fork /run/velnor/package-transaction.lock
   apt-get install "velnor-runner=${VERSION}"`, `dpkg-query -W velnor-runner`.
   Never `dpkg -i`, `apt install ./file.deb`, copied executables, disabled signature
   verification, or altered packaged files.
6. Verify installed identity: compatible package/manifest/binary record, then
   `release verify-installed` BEFORE starting services. Never assume install starts the fleet.
7. Derive activation, drain, and health commands from the implemented package/help and verify
   each; never invent `velnorctl` mutation verbs. Prove post-install health + idempotent re-run.

**Dependencies:** B4 (live verified candidate). The infrastructure subagent owns bastion writes in this window.

**Outputs:** Transaction log, `dpkg-query` identity, binary/record/manifest proof, verify-installed proof, activation + health proof, idempotent-setup proof.

### STEP C2 — Enable quota-free execution + native-backed global N; smoke + provisional benchmark

**Objective:** Prove unbounded quota-free native execution under the shared global N BEFORE admitting bastion jobs, then measure provisional N.

**Actions:**

1. Enable unbounded policy + native-backed global N from the bootstrap package BEFORE the first
   bastion job is admitted. Bounded execution is never run "until later".
2. Prove quota-free state on the host: Docker HostConfig of real job containers
   (`docker inspect --format '{{.HostConfig}}'` — no `NanoCpus`/quotas/cpuset/memory ceilings),
   effective cgroup ancestry (`cpu.max`, `memory.max`, `memory.high`, swap/cpuset — all
   effectively `max`/unset on the workload ancestry), package units/drop-ins
   (`systemctl cat <velnor units>` — no `CPUQuota`/`MemoryMax`/`MemoryHigh`, no quota drop-ins),
   effective build env (no injected `CARGO_BUILD_JOBS`/MBX/BuildKit/Gradle/heap partitions).
   Inspect inherited limits, not only emitted flags.
3. Prove no raw socket mount: `docker inspect` mount lists of job containers contain no
   bastion `/var/run/docker.sock`, alternate host socket, or unrestricted host Docker proxy;
   native jobs use only the mediated lease API.
4. Prove ready health (authenticated outbound records per spec §8 bound to repo/source/run/
   attempt/provider/sequence/freshness/permits/progress), then run a real native smoke job and
   record its URL, engine identity, occupancy ledger entries, and cleanup receipt.
5. Benchmark provisional native-only N with a representative workload at 16, 24, 32, 48, 64
   (higher only while stable useful throughput improves): jobs/minute, time-to-green, queue +
   provisioning delay, phase timings, CPU, memory pressure/OOM, disk/inodes/IO latency,
   Docker/BuildKit/cache-lock contention, failures. Source/plan/image/cold-warm conditions
   comparable; only host-level N changes; unrelated jobs never intentionally destabilized.
6. Record provisional N with the full evidence set; quota-free proof is repeated for every
   later package upgrade.

**Dependencies:** C1.

**Outputs:** Quota-free inspection bundle, no-raw-socket proof, health + native smoke job URL, occupancy/cleanup ledger, provisional-N benchmark data.

---

## Phase D — Scale Set adapter, three-provider model, upgraded package

### STEP D1 — Finish Rust Scale Set integration, credentials, runner/DinD, shared allocator

**Objective:** Implement spec §5 inside Velnor in Rust: protocol, homogeneous official workers with private DinD, protected credentials, and the shared global allocator. No Go controller, no native rewrite.

**Actions:**

1. Pin the audited upstream revision; implement in Rust: scope/group/set registration +
   reconciliation, App credential/token refresh, session creation/renewal, long polling,
   `lastMessageID`, capacity reporting, initial statistics, empty poll responses,
   `JobAvailable`, explicit `AcquireJobs`, JIT config, start/completion events,
   acknowledgement, shutdown/recovery — following the 9-step processing order in spec §5.1
   (idempotent observations → queue submit + trust check → reserve + intent → record returned
   IDs → JIT intent + idempotent creation → durable ACK → `TotalAssignedJobs` convergence →
   shared-grant capacity advertisement → idle-poll reconciliation with backoff).
2. Add recorded/sanitized protocol fixtures AND real upstream conformance tests (live canary
   against the pinned API); mocks alone are not accepted. Cover deferred-offer validity/
   re-offer, initial/nil polls, token refresh, `lastMessageID`, durable ACK, partial/uncertain
   `AcquireJobs` IDs, redelivery/duplication/reordering, stale control generations.
3. Wire protected credentials (GitHub Apps, per-installation scope): App keys/admin tokens/
   signing keys/SSH/telemetry keys stay out of jobs; tokens refresh in the running provider;
   only ephemeral runner/session material reaches runners; publisher credentials stay hosted.
4. Build homogeneous official workers: pinned official runner image + private pinned DinD per
   worker, all pins by validated digest (never `latest`); runner version recorded separately;
   actual tool content verified; updates via the tested generated product path.
5. Prove private Docker semantics on real jobs: private Unix socket, no public TCP API, shared
   runner/DinD network namespace (localhost ports behave), inner bridge/DNS preserved,
   identical absolute bind paths (workspace, `_work`, `TMPDIR`, HOME/capsule, externals, tool
   cache, file-command) with matching ownership, recorded ownership identity per object,
   log export before cleanup, same inner names/ports collision-free across two jobs.
6. Prove the shared allocator: every native and Scale Set acquisition/readiness path gates at
   the one `max_jobs=N` ledger (reserved/acquiring/provisioning/assignable/running/cleaning/
   uncertain all counted; offered demand not occupied); two-scope mixed-mode race tests show
   total occupied ≤ N, no per-scope N, no permanent idle-slot starvation, oldest-observed
   order locally respected; generation-fenced grants; reconcile-before-advertise after restart.
7. Prove lifecycle reconciliation at every boundary: success, error, cancellation, lost ACK,
   runner death, daemon death, management restart. Normal restart preserves set identity
   (decommission is explicit); active work is adopted or explicitly failed with cleanup —
   never lost, never false-success. No duplicate local execution from replays is promised;
   no exactly-once external claim is made.
8. Native Velnor path keeps its existing execution/mediation/ownership/caches; only
   control-plane integration, admission, credential renewal, unbounded policy, and concrete
   conformance fixes touch it.

**Dependencies:** C2 (quota-free + global N foundation). Source work may parallel A/B/C but must not delay the first APT deployment (§0.7).

**Outputs:** Rust adapter + fixtures + live-canary proof, digest-pinned images, credential/trust wiring, private-Docker proof, allocator race-test proof, restart/recovery proof.

### STEP D2 — Finish three-provider model, visible routing, hosted watchdog, strict result set

**Objective:** Implement spec §2 end to end plus the spec §8 hosted verifier, with generic capability tests.

**Actions:**

1. Implement the one provider-set schema (`github-hosted`, `github-self-hosted`, `velnor`)
   across config, scanning, IR, plans, validation, `runs-on`, bootstrap, caching, artifacts,
   policy, dispatch, UI names, aggregation. Mechanically prove: no legacy mode, no aliases,
   no deprecation branches, no dual parsers, no inference from `runner.environment`/labels.
2. Implement platform/trust/capabilities as separate typed fields (Docker, nested privileged
   Docker, Buildx/Compose, Testcontainers, services+readiness, browsers, native macOS/arm64;
   no resource classes). Unknown selectors/unsupported capabilities fail explicitly — negative
   tests prove no silent hosted default.
3. Implement affected/full planner → fanout to all three providers with identical source/
   command/profile/features/fixtures/expectations; provider-exposing display names
   (`Rust · velnor-runner / github-self-hosted / bastion`); disjoint dedicated local
   selectors for official vs native. Provider-specific bootstrap per lane.
4. Implement the strict expected-result set (spec §2 identity): fixed before execution;
   missing/skipped/cancelled/timed-out/failed/duplicate-conflicting/identity-mismatched
   expected results fail — negative tests for each, plus stale-attempt and wrong-provider
   report cases. Planner-declared exclusions only; diagnostic subsets labeled reduced coverage.
   Matrix fail-fast disabled for qualification; each provider's execution proven independently.
5. Implement generic capability tests proving real routing/execution per capability.
6. Implement the hosted watchdog per spec §8: one generated hosted authority, excludes
   itself/telemetry, starts after planning without `needs` on local jobs; authenticated fresh
   outbound health (missing = unavailable); full correlation (runner/job metadata ×
   provisioning IDs × engine versions × digests × tests/JUnit × cache/timing × cleanup);
   artifacts outside disposable containers; all API pages; attempts distinguished; no
   failure-to-success overwrite; reruns revalidate identity/provenance. Prove hosted outage
   detection against the 180s/120s/5m/10m targets with measured deadlines.
7. Trust enforcement per spec §6: controller-side checks outside PR YAML, fork default
   hosted, `pull_request_target` never privileged-untrusted, label-spoofing/input-substitution
   negatives, explicit event coverage.

**Dependencies:** D1 (Scale Set lane exists to route to). Generator work may parallel D1.

**Outputs:** No-legacy mechanical proof, routing/fanout proof, strict-result negative-test proof, capability-test proof, watchdog + outage-detection proof, trust-denial proof.

### STEP D3 — Publish Scale Set package + signed feed; upgrade bastion on the locked path

**Objective:** Ship the D1/D2 code to bastion as a second signed APT release through the identical locked procedure.

**Actions:**

1. Cut the new coherent release (B3 procedure: exact tag/commit/version, amd64+arm64,
   manifests/record, OCI/payloads, attestations, coherence negatives) and publish the signed
   APT update (B4 procedure: verify-before-mutate, staged index, previous-version retention,
   signed metadata + record, single-writer Pages, live candidate verification).
2. Upgrade bastion via the C1 locked path: fingerprint/Signed-By/metadata/version/arch/hash/
   source/origin checks, `flock` transaction with the new exact `VERSION`, `dpkg-query`,
   binary/record/manifest identity, `release verify-installed`, activation from package/help,
   drain + config/secret preservation. No sideloaded binaries; no untracked feature binary.
3. Verify post-upgrade: package/config/state identity, image pins, restart/activation health,
   and REPEAT the full C2 quota-free inspection (HostConfig, cgroup ancestry, units/drop-ins,
   build env) after the upgrade.

**Dependencies:** D1, D2, B3/B4 procedures (repeated for the new release).

**Outputs:** New release/feed chain proof, upgrade transaction log, installed identity, repeated quota-free proof.

---

## Phase E — Velnor full three-provider qualification

### STEP E1 — Qualify Velnor workflows on all three providers

**Objective:** Dogfood Velnor's full corrected Linux inventory on `github-hosted` + `github-self-hosted` + `velnor` by default, with production-topology and full Docker conformance.

**Actions:**

1. Confirm the Velnor unit inventory: full 17-unit baseline from the evidence document
   (`bun-velnor`, `docker`, `docs`, `opentofu`, `rust-policy`, `rust-unit-collector`,
   `rust-velnor-bench`, `rust-velnor-client`, `rust-velnor-control`, `rust-velnor-model`,
   `rust-velnor-render`, `rust-velnor-runner`, `rust-velnor-tools`, `rust-velnor-workflow`,
   `rust-velnor-workflow-contract`, `rust-velnorctl`, `rust-production-topology`)
   or a reviewed coverage-equivalent correction recorded in the ledger.
2. Run the full affected/full planner and fan every eligible Linux unit out to all three
   providers with identical source/command/profile/features/fixtures/expectations;
   `production-topology` and every Docker-conformance unit included, on both local engines.
3. Record actual unit×provider results with native and official engine identities, image
   digests, selected tests + JUnit counts, cache/timing reports, cleanup receipts; real
   platform exceptions declared by the trusted planner before expansion (never after failure).
4. Fail-fast stays disabled; each provider's execution stands on its own evidence —
   a hosted green never certifies a local lane.
5. Prove `velnor-workflow` sole ownership of the final Velnor tree: exact regeneration,
   ownership inventory, local-reference resolution, structured policy, actionlint, provider
   expansion, native-platform routing, fail-closed aggregation.

**Dependencies:** D3 (upgraded bastion with Scale Set package), D2 (three-provider generation + watchdog).

**Outputs:** Full unit×provider result matrix with engine identities and test counts, ownership proof, declared-exception list.

### STEP E2 — Adversarial fault matrix, mixed-engine N, triple-green streak + PR

**Objective:** Break the system on purpose, measure mixed-engine N, and prove a stable triple-provider green streak.

**Actions:**

1. Execute every row of the spec §8 fault matrix against identified canaries/test fixtures
   (kill runner/DinD/native worker/Velnor at each lifecycle point; redelivery + partial
   `AcquireJobs`; cancel queued/running incl. upstream reassignment; scope/engine permit races;
   Docker restart + network loss during poll/acquire/ACK/refresh; bad digest/manifest/signer/
   ref/key/package/arch/record; orphans + partial deletion + unknown events + stale generation;
   fork selector spoofing + protected-workflow input substitution; missing/skipped/wrong-
   provider/stale reports; hidden ancestor limits + injected budgets). Shared Docker restart/
   host reboot runs in an isolated coordinated window preserving unrelated work and SSH —
   no destructive disk ops, broad prune, deliberate whole-host OOM, or unrelated outage.
2. Prove per-row invariants: no false success, diagnostic export, owned-only cleanup, capacity
   released exactly once, durable reservations reconciled, deduplication + returned-ID handling
   correct, total occupied ≤ N monotonically, visible degraded states, pre-execution rejection
   with trusted state intact, no bastion execution for spoofed trust, aggregate fails on any
   bad/missing expected result, quota-free descendants.
3. Benchmark mixed-engine N (cold + warm) per spec §4.4 at 16, 24, 32, 48, 64 (higher only
   while stable useful throughput improves); record jobs/min, time-to-green, queue/setup/
   compile/test/cache/cleanup, CPU, memory pressure/OOM, IO/disk/inodes, contention.
   Containers stay unbounded; only host-level N changes. Serial test groups respected.
4. Obtain three consecutive full triple-provider green `main` runs (zero manual reruns, full
   expected-result sets per spec §2 identity) plus one representative PR run end to end.
   Record run URLs, source/plan identities, attempts, timings.
5. Complete the independent Velnor gate report (author/verifier pair per §0.2).

**Dependencies:** E1.

**Outputs:** Fault records + recovery/cleanup evidence per matrix row, fork-denial proof, monotonic ≤N ledger, mixed-engine N evidence, three triple-green run URLs + PR URL, gate report.

---

## Phase F — Jackin migration and qualification (ONLY after E)

### STEP F1 — Migrate Jackin to the final typed schema + repair routing/release/E2E

**Objective:** Migrate `jackin-project/jackin` directly to the final schema with access/scope config, repaired native routing, and explicit Docker E2E.

**Actions:**

1. Gate check: E1+E2 signed off. Read-only Jackin research may have run early; mutation starts now.
2. Add ONLY: repository authorization, generic scope entries where needed, typed generator
   configuration. Select an already published generator pin (never unpublished source).
   No per-Jackin host architecture, daemon, pool, or custom script.
3. Regenerate the whole Jackin tree; prove sole ownership, reference resolution, structured
   policy, actionlint, provider expansion.
4. Confirm the 40-unit coverage ledger (or justified correction): baseline 38 Linux units × 3
   providers + two actual Apple Silicon Swift units, plus added E2E/control jobs. Never count
   120 Linux executions.
5. Fix generic native routing to actual Apple Silicon macOS; preserve same-job local
   XCFramework production → Swift build/test. Re-resolve tool/image versions from the
   historic declarations (Rust 1.97.1, Bun 1.3.14, Node 24.18.0, Ubuntu 26.04, macOS 26):
   never silently force a different Linux userspace, never install Apple-only mise tools on Linux.
6. Reconcile the obsolete desktop `release.yml` assertion with generator-owned disabled/enabled
   release policy; do not restore legacy YAML or enable releases merely to pass the test.
   Application releases stay disabled.
7. Explicitly execute `docker-e2e` with `e2e` enabled: build the capsule Linux ELF from the
   tested source, set `JACKIN_CAPSULE_BIN` (no preview-release substitute), prove Docker,
   Buildx, Compose where used, `script(1)` PTY support, nested privileged DinD, Java
   Testcontainers, TLS/no-proxy behavior, temporary relay socket/file mounts; preserve serial
   E2E groups; exercise the 20-capsule fixture fanout (one top-level job). No fixture
   weakening to make a lane green.
8. If a generic bug surfaces: fix/publish the generator or Velnor package, requalify Velnor
   for that change (§0.8), then continue Jackin.

**Dependencies:** E1, E2 (Velnor qualified first — hard rollout gate).

**Outputs:** Migrated + regenerated Jackin tree, 40-unit coverage ledger, routing/release fixes, explicit E2E proof.

### STEP F2 — Qualify Jackin full parity + PR/main; remeasure N if mix changes

**Objective:** Prove full Jackin Linux parity plus actual native Apple checks with a representative PR and main.

**Actions:**

1. Run full Jackin qualification: complete unit×provider results, test/JUnit counts, engine
   identities, image digests, cache reports, no-quotas inspection on real Jackin job
   containers/cgroups, cleanup receipts.
2. Require a representative PR run plus main-run evidence; record URLs, source/plan identities,
   attempts, timings.
3. If the workload mix changes N materially, remeasure mixed-engine N per spec §4.4 and record.
4. Prove Velnor non-regression for any generic change made during F1 (affected author/verifier
   pair re-runs the Velnor proofs per §0.8).
5. Independent Jackin parity verifier signs the gate report.

**Dependencies:** F1.

**Outputs:** Full parity evidence set, PR + main URLs, N re-measurement (if triggered), Velnor regression proof, gate report.

---

## Phase G — ChainArgos migration/qualification and campaign close (ONLY after F)

### STEP G1 — Migrate ChainArgos, repair ownership/actions/Docker/services, qualify parity

**Objective:** Migrate `ChainArgos/java-monorepo` directly to the final schema and prove full 71-unit parity with explicit external/platform exclusions.

**Actions:**

1. Gate check: F1+F2 signed off. Read-only ChainArgos research may have run early; mutation starts now.
2. Add ONLY: repository authorization, generic scope entries where needed, typed generator
   configuration. Select an already published generator pin. Same Velnor controller, same
   global N — new scope metadata, never new infra.
3. Regenerate the whole tree; repair policy ownership/trust failures and missing generated
   local actions FIRST (the recorded `generated-tree` + `trusted-runners` failure class);
   prove sole ownership, reference resolution, structured policy, actionlint.
4. Confirm the 71-unit coverage ledger (or reviewed equivalent): 213 baseline three-provider
   executions before additional controls.
5. Provision disposable job-local PostgreSQL for Flyway/jOOQ and tests: inspect the actual
   Gradle graph and remove duplicate root/module execution of the 16 historical
   `flywayMigrate jooqCodegen` preparations ONLY with equal task/test coverage; both tools
   use the same job database through actual build-supported variables (including
   `POSTGRESQL_DB_HOST/PORT` where declared); never production or shared default databases.
6. Exercise Rust Testcontainers PostgreSQL/RabbitMQ/Redis/RustFS suites: explicitly select
   the intended nextest CI profile, retain the one-at-a-time RustFS group, independently
   verify the effective configuration. Resolve and lock tested digests for
   `rustfs/rustfs:1.0.0-beta.8`, `postgres:18-alpine`, and the rest — never assume tags
   are immutable. Separate fixture acceptance from live Ethereum/Base/blockchain RPC suites;
   label actual external failures as such.
7. Fix the generic Rust Docker context to include root workspace inputs, `backend/`, and
   `scripts/`; build the full historic bake contract (root context, all 12 service targets) —
   final-stage-only builds are insufficient. Never run root Compose unchanged with fixed
   global names/ports/home mounts through a shared host namespace.
8. Preserve GraalVM Java 25/native-image (oracle-graalvm-25.0.3), Gradle wrapper 9.5.1,
   Rust 1.98.1, native/protobuf tools, Node 24.20.0, Bun 1.3.14, and declared browser
   requirements unless a separately tested source change updates them; audit Playwright/
   browser gates (unit success alone is not browser coverage); check the Micronaut Docker
   API `1.44` override against the selected daemon.
9. Keep mutation single-writer and application release-disabled policy unchanged.
10. Require a representative PR run plus main-run evidence; record URLs, identities, attempts,
    timings. Any generic bug follows §0.8 (fix/publish, requalify Velnor, then continue).

**Dependencies:** F1, F2 (Jackin qualified first — hard rollout gate).

**Outputs:** Migrated + regenerated ChainArgos tree, 71-unit/213-execution ledger, DB/Testcontainers/RustFS/Docker/browser proofs, PR + main URLs.

### STEP G2 — Close campaign: generic onboarding, pins, runbooks, acceptance report

**Objective:** Prove the campaign complete, repeatable, and operable — then write the final acceptance report.

**Actions:**

1. Run mechanical no-regression checks across all three migrated trees: no VM/libvirt/KVM/QEMU
   runner infra, no Go controller/sidecar, no quota ceilings (configs + package + live
   inspection), no per-repo/org/engine reservations, no legacy provider model/aliases/shims/
   dual parsers, no hand-edited generated YAML (regeneration exact everywhere).
2. Prove generic onboarding on a fresh fixture repository with a renamed package/repo identity:
   verify trust/access → authorize + declarative scope only if needed → typed config →
   already-published pin → regenerate/check → full qualification → health inventory.
   No new daemon/VM/pool/script/copied workflow. Prove a new GitHub organization adds only
   authorization/registration metadata.
3. Record current pins and deployed identities: generator pins per repo, exact installed
   `velnor-runner` version, image digests (runner/DinD/toolchains), upstream protocol
   reference, signer fingerprint, APT candidate record.
4. Verify reproducible infra: idempotent host setup re-run, package reinstall path, config/
   state recovery (APT downgrade ONLY with proven matching snapshot restore, else tested
   forward recovery — no compat shims), operations/recovery runbook reviewed against the
   implemented package/help (no invented verbs).
5. Record final N evidence (mixed-engine, post-ChainArgos) and the shared-pool/provenance/
   fault evidence bundle.
6. Write the complete acceptance report covering all three final default trees and every
   gate; the final independent verifier (not any step author) signs it.

**Dependencies:** G1 (and transitively every prior step).

**Outputs:** Mechanical no-regression proofs, fresh-fixture onboarding proof, pin/identity record, runbooks, final N evidence, signed acceptance report.



