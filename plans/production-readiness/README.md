# Velnor production-readiness acceptance plan

This is the acceptance ledger for `PRD-001`. The active `/goal` authority is
`docs/prompt.md`, the tracked goal graph, `plans/TASKS.md`, and the normative
direction documents. `/goal` must execute this plan, but this file must not
silently override those authorities.

The campaign is complete only when every atomic item is `DONE` with current
evidence. `TODO`, `RUNNING`, `BLOCKED`, `WAITING`, `UNKNOWN`, `UNPROVEN`, and
historical evidence never count as complete. Never infer completion from a
green local test, a shared commit, or partial workflow success.

## Execution order

1. Reconcile authority, scope, status, approvals, branch state, and external
   state. Regenerate the stale progress ledger before mutation.
2. Capture baseline evidence. Cancel or delete only explicitly authorized,
   owned, inactive validation resources; unresolved ownership is `BLOCKED`.
3. Resolve lifecycle and correctness blockers in Velnor. Do not work around
   Velnor gaps in repositories or in `velnor-actions-fixture`.
4. Execute Track A read-only fleet reconciliation and Track B migration in
   dependency order. Execute each C leaf only after its named dependencies are
   `DONE`; never batch-complete siblings.
5. Run protocol, capability, storage, executor, lifecycle, fixture, workflow,
   parity, and security gates at the current source SHA.
6. Run defined cold/warm/rerun performance campaigns, fault injection, and
   multi-repository soak. Missing telemetry fails the gate.
7. Finish Plan 079 and remove the legacy `velnor-runner` product surface.
8. Integrate unique branch behavior, then merge to `main` and prove green
   `main` before any release.
9. Publish, install, verify, roll back, and forward-recover only through the
   signed APT path. Release and deployment are terminal gates.
10. Run the independent final audit and generate the evidence index. Only then
    check the final items and declare completion.

## Atomic checklist and evidence contract

- Assign each checkbox a stable ID: `PRD-<section>-NN`; split bullets that
  contain multiple claims into separate rows.
- Each row records status, dependencies, owner, authorization, baseline SHA,
  final SHA, exact command, exit code, UTC capture time, external run IDs,
  artifact paths and SHA-256 hashes, verifier, reviewer, and next action.
- Only an independent verifier may change a row to `DONE`; the implementer
  and primary orchestrator may not self-approve.
- Every mutation requires a pre-state and post-state snapshot. Explicit
  authorization is required for cancellation, runner deletion, drain, restart,
  merge, tag, publish, install, rollback, policy changes, and re-admission.
- Evidence must be sanitized. Never commit secrets, credentials, unsanitized
  logs, or rendered GitHub HTML.
- Run a final machine check: every row is `DONE`, every dependency is `DONE`,
  every `DONE` row has valid evidence, and every mirror/index agrees.

## Mission and operating law

- [ ] Act as a senior Rust engineer building Velnor into a stable, modern,
  faster GitHub Actions alternative.
- [ ] Scope the campaign to Velnor, ChainArgos `java-monorepo`, Jackin, and
  Velnor deployment on Sentry.
- [ ] Preserve the marked unified-CI contract, current direction documents,
  strict capability contract, and fixture contract.
- [ ] Fix root causes. For every failure record trigger, enabling architecture,
  why tests missed it, structural fix, regression proof, and live proof.
- [ ] Implement missing capability in Velnor, never with repository-local
  workflow workarounds.
- [ ] Do not leave legacy code, legacy protocols, compatibility aliases,
  shims, dual implementations, fallback paths, or a transition period. When a
  modern replacement exists, remove the old path completely in the same
  campaign.
- [ ] Refactor continuously when the current structure enables bugs,
  instability, performance loss, or migration debt. Breaking changes are
  allowed and preferred when they produce the correct final architecture.
- [ ] Deliberately blocking changes may be deployed when they are required to
  remove legacy behavior or establish the final architecture. After each such
  deployment, immediately repair every resulting failure, re-run all affected
  gates, and continue until production is green. Never preserve a bad design
  merely to maintain a transition period.
- [ ] Use current `actions/runner` behavior and latest protocol paths as the
  source of truth. Remove deprecated protocols, aliases, shims, and silent
  fallbacks completely.
- [ ] Never weaken `velnor-actions-fixture`; fixture failures are Velnor bugs.
- [ ] Preserve unrelated work and record the initial HEAD, branch, remotes,
  and dirty-worktree state before editing.
- [ ] At the beginning of every iteration and immediately before every
  investigation, edit, refactor, deployment, verification, merge, rollback,
  or recovery action, re-read this file at
  `plans/production-readiness/README.md`. Identify the highest-priority
  unchecked item, confirm that the planned action advances it, and record any
  newly discovered evidence or changed priority before continuing.
- [ ] Whenever working on any plan item and touching any file, verify that
  every touched file is modern, internally consistent, and aligned with the
  final implementation we intend to keep. Do not leave legacy, compatibility,
  or transitional state in touched files. Prefer breaking changes; repair their
  immediate fallout in the same iteration instead of adding long-lived legacy
  support or compatibility periods.
- [ ] Use fresh investigator, implementer, verifier, and reviewer subagents
  when available; independently verify their output.
- [ ] Use `git commit -s` and add
  `Co-authored-by: Codex <codex@openai.com>` to every commit.
- [ ] Never commit secrets, credentials, rendered GitHub HTML, or unsanitized
  logs. Keep only sanitized evidence in approved `.velnor-compare/` artifacts.

## P0 — Establish truth before changing production

- [x] Inventory all open Velnor PRs and all remote branches; record unique
  commits relative to `main`.
- [x] Inventory recent failed, cancelled, stuck, and flaky runs for all three
  repositories, including repository, SHA, lane, job, step, run ID, symptom,
  frequency, and first known failure.
- [x] Inventory Sentry runners, stale registrations, active jobs, Docker
  resources, Velnor-owned caches, disk pressure, systemd units, package
  version, backend, and health state without deleting anything.
- [ ] Cancel pending/in-progress runs from prior verification attempts and
  remove stale runner registrations before dispatching new verification.
- [ ] Classify every failure as protocol, workflow, expression, adapter,
  checkout, container, executor, artifact/results, cache, storage, lifecycle,
  watchdog, release, permissions, or repository configuration.
- [ ] Reconcile stale prompt, README, plan, release, and workflow text against
  `docs/mission.md`, `docs/vision.md`, `docs/roadmap.md`, and `AGENTS.md`.
- [ ] Preserve exactly the plural `lanes` selector with values `velnor`,
  `github`, `both`; reusable workflows retain singular `lane`.
- [ ] Preserve exact required checks `ci-required` and `DCO`, generated class
  equality, trust routing, and organization defaults.

## P0 — Make Velnor correct and fail closed

- [ ] Verify current V2 broker, JIT, Results Service, artifact, log, status,
  cancellation, annotation, output, credential, and failure-mapping behavior
  against `actions/runner`; remove legacy paths.
- [ ] Validate the complete job against the compiled capability manifest before
  checkout, cache mutation, service startup, container creation, or other side
  effects.
- [ ] Reject unsupported refs, inputs, values, expressions, actions, backends,
  and combinations with exact field/value, accepted alternatives, and manifest
  version.
- [ ] Ensure every approved `uses:` path has a native Rust adapter pinned to an
  approved upstream commit; unknown remote/JavaScript actions fail closed.
- [ ] Verify Docker execution, services, networks, mounts, cleanup, ownership,
  and failure recovery.
- [ ] Verify explicit `[execution] backend = "docker" | "microvm"` selection
  with no automatic fallback.
- [ ] Verify Firecracker production microVM behavior through HTTP API and
  jailer, Linux KVM, guest-local Docker, immutable block storage, job-local
  writable storage, and bounded vsock; reject unsupported legacy devices and
  host passthrough.
- [ ] Verify guardian, per-scope controller, per-slot process, transient job
  unit, health vector, systemd readiness/watchdog, drain, restart, cancellation,
  process reaping, and registration recovery.
- [ ] Prove broker outage, credential failure, Docker failure, cache failure,
  job failure, result-upload failure, and host restart leave the daemon alive,
  observable, and recoverable without silently abandoning accepted work.

## P0 — Storage, cache, and workflow correctness

- [ ] Verify canonical `/var/lib`, `/var/cache`, `/run`, `/var/log`, and
  job-work boundaries, ownership, quotas, leases, reservations, and pressure
  thresholds.
- [ ] Eliminate `unknown-repository` and `unknown-workflow` identity from
  persistent state and cache paths.
- [ ] Verify target generations include compiler, toolchain, target, profile,
  features, lockfile, and schema compatibility; materialize job-local targets
  and publish atomically after completion.
- [ ] Share only immutable Cargo archives/indexes and bare Git databases;
  extracted sources remain job-local.
- [ ] Keep compiler caches mutually exclusive: selected sccache, selected
  Kache, or off. Never stack wrappers.
- [ ] Prove active-job-safe GC, physical-byte accounting, ownership, bounded
  cleanup, reclaim-before-advertise, and reservations through result upload.
- [ ] Never run broad Docker prune or delete resources outside Velnor ownership.
- [ ] Inspect exact current workflows in Velnor, ChainArgos, and Jackin.
- [ ] Verify PR, push-to-main, merge-queue, tag, schedule, and manual dispatch
  behavior on `velnor`, `github`, and `both`.
- [ ] Verify identical applicable job graph, permissions, inputs, cache policy,
  timeouts, concurrency, writer gates, outputs, and generated bytes.
- [ ] Verify current action majors are SHA-pinned and Renovate-managed; remove
  deprecated commands, inputs, runtimes, and privileged convenience steps.
- [ ] Run complete fixture proof before and after relevant fixes. Preserve and
  strengthen fixture coverage; never simplify it to hide a Velnor defect.

## P0 — Signed Sentry release

- [ ] Drain Sentry safely and record pre-deployment package, process, runner,
  cache, disk, Docker, systemd, and health evidence.
- [ ] Merge only a green source commit to Velnor `main` and tag the exact
  intended commit.
- [ ] Build immutable release records, Debian package, OCI/guest assets, and
  capability manifest from the exact source SHA.
- [ ] Publish through the signed `velnor-apt` repository and verify signature,
  publication, exact candidate, package contents, manifest hash, and OCI digest.
- [ ] Install on Sentry only with exact-version signed APT commands; never use
  local `.deb`, direct `dpkg -i`, copied binaries, local-path APT, or an
  uncommitted build.
- [ ] Verify installed package, binary, systemd units, configuration, explicit
  backend, manifest, `velnorctl` surface, and service health.
- [ ] Re-admit capacity only after registration and smoke checks pass.
- [ ] Prove rollback to the previous signed APT version and forward recovery to
  the new version, preserving active-job safety, caches, leases, logs, and
  evidence.

## P1 — Deliver all Velnor PR and branch work

- [ ] Freeze the Velnor PR/branch inventory and inspect every unique commit.
- [ ] Update each open PR onto current `main`, resolve conflicts using the
  modern contract, and run checks on the exact PR head.
- [ ] Merge each PR only after `ci-required` and `DCO` are green; verify a green
  `main` run after every merge.
- [ ] For every branch, integrate all non-obsolete unique behavior through a PR
  or document an explicit modern replacement; never merge deprecated code just
  to empty a branch.
- [ ] Confirm every original Velnor PR is delivered to `main`.
- [ ] Confirm no branch retains unique undelivered behavior; delete branches
  only after proving integration or documented supersession.

## P1 — Prove cache reuse and performance

For each representative workflow in each repository:

- [ ] Run cold, unchanged warm, and unchanged rerun on Velnor.
- [ ] Run equivalent cold and warm executions on GitHub-hosted infrastructure.
- [ ] Run `both` and compare checks, outputs, artifacts, annotations, logs, and
  timings; monitor only the new run ID.
- [ ] Stop and diagnose any job pending or stuck for more than two minutes.
- [ ] Repeat enough times to report median and tail latency.
- [ ] Capture dependency network requests and downloaded bytes.
- [ ] Capture compiler invocations, cache hits, misses, miss reasons, target
  restore/materialization/publication, tool downloads, and layer-cache hits.
- [ ] Capture logical/physical bytes, reflink/deduplication, GC, leases, and
  reservations.
- [ ] Prove unchanged warm/rerun workflows download zero unchanged dependencies,
  recompile zero unchanged dependencies, reinstall no unchanged tools, and
  still execute tests correctly.
- [ ] Prove keys exclude refs and include every compatibility input; prove cache
  isolation across repositories, workflows, trust boundaries, toolchains, and
  targets; prove exactly one writer.
- [ ] Prove Velnor warm execution is faster than equivalent GitHub execution
  with step-level and end-to-end measurements; fix Velnor architecture when it
  is not, never weaken workflows or hide measurements.

## P1 — Reliability, soak, and final verification

- [ ] Inject broker outage, registration timeout, credential failure, Docker
  failure, disk pressure, cache corruption, cancellation, process restart,
  reboot, result-upload failure, and interrupted APT deployment.
- [ ] Prove failures are bounded, explicit, recoverable, and observable; no
  altered-semantics silent retry, stale runner, stale job, leaked workspace,
  container, lease, cache, or secret appears.
- [ ] Run multi-repository concurrent Sentry soak and verify no slot death, lost
  job, watchdog violation, corruption, or unexplained health disagreement.
- [ ] Run `rtk mise run check`, focused `rtk cargo nextest run`, workspace
  `rtk cargo nextest run --workspace`, formatting, lint, capability, fixture,
  workflow, package, and security audits.
- [ ] Verify no `cargo test`, `sudo` convenience use, secret leakage, rendered
  GitHub HTML, deprecated path, compatibility alias, or silent fallback remains.
- [ ] Produce a sanitized evidence index mapping every checkbox to commits, run
  IDs, logs, metrics, package versions, host state, and reviewer sign-off.
- [ ] Obtain independent final review with zero unresolved correctness,
  security, capability, reliability, performance, or operational findings.
- [ ] Confirm Velnor, ChainArgos, and Jackin are green on PR and `main`; both
  lanes and `both` parity are green; Sentry runs the exact signed APT release;
  cache proof and rollback proof pass.
- [ ] Check every checkbox in this document. Only then declare the campaign
  complete.

## Progress ledger

The previous dated ledger is historical evidence, not current state. Before
each iteration, regenerate this section from the repository, GitHub, and
Sentry. Record the current branch, HEAD, remotes, worktree paths, plan/index
statuses, open PRs, run IDs, runner registrations, package/backend/health
state, and all unresolved blockers. Any state change invalidates dependent
evidence and requires a fresh baseline.

### Current baseline — 2026-08-27T23:29:53Z

- Repository: branch `fix/watchdog-registration-deadline`, HEAD
  `0a5d5b459c2b82f7141d75ba363576cfcc7fe8fb`, clean worktree, remote
  `origin=https://github.com/tailrocks/velnor.git`; branch is 269 commits ahead
  of `origin/main` and equal to its pushed branch tip.
- Open Velnor PR: #411 at this HEAD. Current-SHA CI run `33126029601` and
  Guest image run `33126029298` stalled in build/download work and were
  cancelled at `2026-08-27T23:29:53Z`; neither is green-main proof. Superseded
  runs `33125969739` and `33125969112` are also cancelled.
- Plan 066 authority is now synchronized as `IN PROGRESS` in its task file,
  migration README, and `plans/TASKS.md`; its six atomic criteria remain
  unchecked because current-SHA fixture proof and independent sign-off are
  absent. Root campaign progress remains 4/94 done; 039 remains in progress.
- Branch integration: all six current remote branch tips with unique commits
  are ancestors of this branch; immutable recovery refs for all local and
  remote tips remain under `refs/backup/branch-sync/`. The local-only
  Firecracker safety fork was not merged wholesale because its four commits
  conflict with and are superseded by the active guest/runtime architecture.
  The remaining local branch audit classified package/cache, PAT pacing,
  lease, heartbeat, worker-recovery, and performance commits as already
  present or safely superseded; obsolete release/docs-only commits were not
  cherry-picked. `dcf9bfe` waiter ownership is covered by the current
  controller path, with its stale-job/live-waiter edge case fixed at
  `ee7ecca`. Current remote #408 behavior `eaf772d` was ported as bounded
  kernel-download retry/timeout hardening at `8f660ad`; its forced HTTP/1.1
  downgrade was intentionally omitted under the modern-protocol rule.
- Current-SHA local evidence: `rtk mise run check` passed, exit 0, with
  actionlint, cargo deny, cargo fmt, fleet generation, clippy `-D warnings`,
  and workspace nextest `1508/1508` (nextest run ID
  `4efdab5b-65d8-46e4-8839-50a7fda8674f`). Focused retention tests passed
  `7/7`. Code commits `a7ca6bb` and `80d1b63` add persisted slot identity,
  bounded retention convergence, complete deletion accounting, resource policy,
  legal no-op lifecycle transitions, and raw-row sanitization.
- GitHub read-only group snapshot: `tailrocks/velnor-trusted` id 3,
  `ChainArgos/velnor-trusted` id 4, and `jackin-project/velnor-trusted` id 3
  are all `visibility=selected`, `allows_public_repositories=true`,
  `restricted_to_workflows=false`; Tailrocks currently selects 21 repositories
  including `cloudflare-tofu` and `github-terraform`.
- Current GitHub repository readback shows the 28 canonical repositories are
  present, but live default branches are not uniform: `master`, `develop`,
  `staging`, and `port/cross-agent-dry` occur alongside `main`. This conflicts
  with the 039 release-ref assumption that all 28 resolve to `refs/heads/main`;
  the ref-shape stop remains active and no policy was changed.
- Sentry read-only snapshot last captured at `2026-08-27T16:07:53Z` (not a
  current-state proof): Docker 29.7.2 active; `velnor-runner 0.1.242` and
  `/usr/bin/velnorctl` present; `/usr/bin/velnor-tools` absent; exact unit
  states were not regenerated in this pass; the health vector was degraded with
  `github_reachable=false`,
  `routing_valid=false`, `runner_group_valid=false`, and zero ready slots.
- Blockers: the Plan 039 digest/closure and workflow-ref admission ruling are
  not approved for public-code policy mutation; stale validation-run cleanup,
  runner deletion, drain, dispatch, package publication, Sentry install, and
  rollback require explicit authorization; Sentry lacks the packaged
  `velnor-tools` prerequisite; PID start-identity binding remains a separate
  lifecycle hardening leaf; Plan 079 and final signed-APT/independent-audit
  gates are not complete. No external mutation was performed.
