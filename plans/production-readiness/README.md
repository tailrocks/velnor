# Velnor production-readiness execution prompt

Use this document as the single execution prompt for the production-readiness
campaign. The campaign is complete only when every checkbox is checked and
each check has attached evidence. Never claim completion from partial green
runs.

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
- [ ] Use current `actions/runner` behavior and latest protocol paths as the
  source of truth. Remove deprecated protocols, aliases, shims, and silent
  fallbacks completely.
- [ ] Never weaken `velnor-actions-fixture`; fixture failures are Velnor bugs.
- [ ] Preserve unrelated work and record the initial HEAD, branch, remotes,
  and dirty-worktree state before editing.
- [ ] Use fresh investigator, implementer, verifier, and reviewer subagents
  when available; independently verify their output.
- [ ] Use `git commit -s` and add
  `Co-authored-by: Codex <codex@openai.com>` to every commit.
- [ ] Never commit secrets, credentials, rendered GitHub HTML, or unsanitized
  logs. Keep only sanitized evidence in approved `.velnor-compare/` artifacts.

## P0 — Establish truth before changing production

- [ ] Inventory all open Velnor PRs and all remote branches; record unique
  commits relative to `main`.
- [ ] Inventory recent failed, cancelled, stuck, and flaky runs for all three
  repositories, including repository, SHA, lane, job, step, run ID, symptom,
  frequency, and first known failure.
- [ ] Inventory Sentry runners, stale registrations, active jobs, Docker
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
