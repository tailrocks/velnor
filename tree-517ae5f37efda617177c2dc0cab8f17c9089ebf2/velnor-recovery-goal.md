Restore tailrocks/velnor end to end: unblock and merge PRs #809 and #812, deliver simple on-demand Docker-backed execution from Linux and macOS hosts with working native macOS velnorctl, validate main and the release pipeline, deploy the verified fixes to Sentry, and prove that both permanent Sentry capacity and independent temporary host capacity execute real GitHub Actions jobs correctly.

## Objective and scope

Repository: https://github.com/tailrocks/velnor
PRs:
- https://github.com/tailrocks/velnor/pull/809
- https://github.com/tailrocks/velnor/pull/812
Permanent deployment: `ssh sentry`.

The essential outcome is to eliminate the circular dependency where broken Velnor infrastructure prevents testing, merging, releasing, and deploying Velnor's own repairs. Sentry remains the always-on host. An operator can connect a Linux or macOS machine on demand to recover the repository or add execution capacity, without needing Sentry to work first.

Implement, test, independently verify, integrate, release, deploy, and validate the result. Do not stop after an audit, plan, local prototype, or instructions for someone else to finish.

Keep this mission limited to the runner, host onboarding, Docker portability, velnorctl, and repository CI/release/deployment defects necessary for this outcome. Do not launch an unrelated architectural rewrite, merge unrelated branches, migrate other repositories, add Kubernetes/cloud autoscaling, or expand MicroVM support. Preserve existing functionality and security boundaries.

## Execution discipline

Read applicable AGENTS.md files, existing execution plans, coordination files, deployment runbooks, and generator ownership rules before editing. Inspect local branches/worktrees and preserve unrelated or uncommitted work. Use the repository's actual commands and conventions rather than inventing replacements.

Use subagents aggressively for independent investigations, implementation, and verification. Parallelize independent work; serialize shared-file integration, credentialed production changes, and dependent merges. Give each implementation subagent a bounded task, owned files/worktree, acceptance criteria, and required evidence. Assign a different subagent to verify its exact resulting commit. An implementer's report is not verification.

Initially parallelize:
- PR ancestry, review feedback, required checks, and workflow failures.
- Sentry diagnosis and an independent recovery/bootstrap path.
- Linux/macOS Docker host support and native macOS velnorctl.
- Regression tests, release contracts, and final independent acceptance.

Maintain one concise, durable execution record using existing repository conventions: dependency-ordered tasks, owners, decisions, current SHAs, reproduction commands, evidence locations, blockers, and next steps. Update it at meaningful milestones so execution survives context compaction. Keep the user informed without narrating every command.

Proceed with the requested repository changes, workflow runs, merges, releases, and Sentry deployment through existing authorization and approval mechanisms. Do not bypass branch protection, release approvals, or credential boundaries. If a genuinely unavailable credential, permission, host, or tool blocks a requirement, document the exact missing prerequisite and continue all independent work. Never mark an unverified requirement complete.

## 1. Establish the live failure and dependency graph

Refresh all live information; these are investigation seeds, not immutable assumptions:

- At inspection, #809's head was `fb9a31a6af458e8ac56aed29a3d1b434b408bdc7`.
- #812's head was `21822888ef67a8b0a28e06291138a435e96750d7`, one commit ahead of #809. Its additional commit makes setup-runtime shell operations portable.
- Both targeted main at `d3e441fb36dc0fee7502cc08fdc7bebf8c8bf754`.
- #812 run `34882110261` contained queued jobs requesting `self-hosted` and `velnor-target-mvp`, without an assigned runner.
- That run also contained a failed GitHub-hosted job, `104104400653`, named `Rust / GitHub / rust-velnor-workflow`.
- Its log reported generation-state drift: generated files matched, but the scan input changed from `a2f70c15fd2473b4` to `861def8a99c861c2` in `.github/ci/.github-actions-generator-state`.
- #809's observed run `34862656273` was cancelled, not successfully verified.

Inspect both actual diffs, their shared ancestry and unique changes, review discussions, current head/base/merge-ref SHAs, required checks, workflow dependencies, and latest run attempts. Paginate job/run listings. Distinguish unavailable runners from code failures, generator drift, admission failures, invalid routing, cancelled runs, and release defects. Do not accept the PR description's validation claims as proof.

Diagnose Sentry before changing it. Record the deployed artifact/version, service topology, selected backend, redacted configuration, routing, available slots, resource pressure, and relevant logs. Trace registration/JIT credentials, broker/session connectivity, job delivery, admission, execution, reporting, and cleanup. An active service PID is not sufficient health evidence.

Produce concrete reproductions and regression tests for the causes found. Avoid unrelated host changes and do not expose secret values in output or evidence.

## 2. Break the circular dependency first

Establish the smallest working recovery path outside Sentry before undertaking broad hardening. Obtain a verified existing artifact or build an independently reviewed candidate from pinned source with the locked toolchain/dependencies. Record its source SHA and binary/image digests.

Recovery must not require a successful run on Sentry, a newly published Velnor release, or an artifact that can only be built by the broken deployment. Keep a reproducible source-bootstrap route when published artifacts are unavailable. Separate the trusted runner/bootstrap code from the untrusted code it will execute.

Register temporary, repository-scoped capacity through the existing Velnor GitHub integration and complete a real GitHub Actions job through Velnor's Docker backend. Preserve existing legitimate GitHub-hosted capacity as an independent bootstrap/verification option, but do not substitute GitHub-hosted success for proof that Velnor works.

A reviewed pre-merge candidate may be used for this recovery. Track it explicitly and replace it with the final merged/released build before completion. Do not deploy an untraceable dirty-tree binary.

## 3. Deliver the minimal on-demand host experience

Provide a simple binary-based entry point on Linux and macOS. Prefer extending existing Velnor and velnorctl commands over introducing another application or a collection of manual scripts.

After installation and authentication, the operator should be able to:
- Start foreground, Docker-backed capacity for `tailrocks/velnor` with sensible defaults and bounded concurrency.
- See the selected host, Docker endpoint, execution platform, GitHub scope, runner identities, readiness, and active jobs.
- Follow logs, diagnose failures, drain, stop, and cleanly disconnect.
- Select a specific PR/run for recovery, with precisely documented scheduling semantics.

Choose command names after inspecting the current CLI. Deliver actual implemented commands, help text, actionable errors, and copy-paste examples, not hypothetical flags. Normal startup must not require manually constructing runner registrations, editing system files, exposing an inbound webhook, or copying Sentry's private state. Use outbound GitHub connectivity and the existing authentication/registration mechanisms.

Reuse the existing Rust runner, control protocol, job admission, Docker execution, logging, and completion reporting. Introduce only necessary platform adapters. macOS must not require systemd, Debian packaging, Linux host paths, or an SSH connection to Sentry just to operate locally.

Keep operator credentials outside job containers and ordinary configuration. Reuse supported credential sources, document minimum permissions, prefer short-lived registration credentials, and redact secrets. Do not put tokens in command arguments, shell history, committed files, or logs. Retain repository/group/label/trust validation; repository-scoped recovery must not accidentally join an organization-wide privileged pool.

### Docker detection and portability

Resolve explicit configuration, Docker context, and environment overrides consistently with Docker's documented behavior. Display the effective endpoint. Do not silently select a remote daemon, especially Sentry, or modify the user's global Docker context.

Preflight must verify daemon connectivity and permissions, execution OS/architecture, required images, resource limits, workspace mounts, writable state/cache paths, and the ability to start and clean up a real test container. Detect missing or stopped Docker and explain the remedy; do not silently switch execution backends.

Verify Linux Docker and a real macOS Docker environment. Account for Docker's Linux VM on macOS, nonstandard sockets, filesystem sharing, UID/GID differences, networking, service containers, and Docker/buildx access needed by repository workflows. Keep trust restrictions intact rather than globally granting privileged containers or host socket access.

Distinguish host platform from job execution platform. Linux container jobs on a Mac are Linux jobs, not native macOS jobs. On Apple Silicon, validate native ARM64 or explicitly supported and tested AMD64 emulation. Match runner labels, images, downloaded tools, workflow-runtime artifacts, and cache keys to the actual execution platform. Never claim unsupported capabilities to make scheduling succeed.

Inventory every repository pipeline's requirements. Support all compatible Docker-backed pipelines from the temporary Mac host. Preserve correctly placed native macOS/Xcode/signing or Linux-specific hardware jobs where those capabilities are genuinely required; do not claim a Linux container substitutes for native platform verification.

### Native macOS velnorctl

Build and run velnorctl natively on macOS against the locally hosted Velnor instance. Verify status, readiness/preflight, runner/slot/job/run discovery, events, logs, and supported lifecycle operations. Exercise normal operation, unavailable Docker, failed authentication, disconnected control endpoints, and restart/reconnect behavior. Linux tests or a successful `--help` invocation do not satisfy this requirement.

### Lifecycle and coexistence

Use distinct host/session/slot identities. Handle credential renewal, transient GitHub/network failures, Docker interruption, cancellation, Ctrl-C, draining, and process restart without duplicate execution, leaked registrations, or orphaned job resources. Bound retries and provide visible failure reasons instead of polling forever while appearing healthy.

Scope state, workspaces, cleanup, and caches to the correct host/repository/trust boundary. Reuse safe existing persistent caches for speed; never require warm caches for correctness. Do not delete unrelated containers, volumes, networks, or caches, and do not expose the operator's home directory or credentials to jobs.

## 4. Implement PR-targeted recovery honestly

GitHub remains the scheduler and job source of truth. Do not implement a fake queued-job downloader or assume the REST API can assign an arbitrary job to a chosen runner.

Provide both ordinary repository capacity and a clearly defined targeted recovery operation. Resolve the requested PR to its current relevant workflow runs, attempts, and tested head/merge SHAs. Monitor the complete selected workflow, including dependent jobs that appear later; exit with its real outcome rather than after the first completed job.

For strict PR-only execution, use a supported, verified routing/admission design. Add only the minimal generic generator/runtime support required. Demonstrate that unrelated PR jobs cannot execute in the targeted session. Merely creating a runner after observing a PR in the queue is not proof of exclusivity, and claiming then discarding unrelated jobs is unacceptable.

Handle existing queued jobs versus newly scheduled jobs explicitly. Do not pretend changed labels or workflow YAML alter an already-created job. When fresh scheduling is necessary, use a legitimate trigger and verify the resulting run, checkout, and required-check association. A manually dispatched run against a PR branch must not be presented as satisfying the original PR checks unless GitHub actually associates it appropriately.

Provide precise errors when a requested scope is unsafe or unsupported; never silently broaden PR-only execution to the whole repository. Ensure temporary targeting does not leave permanent routing changes or disrupt Sentry's normal eligibility.

## 5. Repair the repository and prove the fixes

Fix every defect blocking this mission across runner startup, GitHub connectivity, admission, Docker execution, velnorctl, generated workflows, tests, and releases. #812's portable setup action is not evidence that the daemon and local macOS control path already work.

Investigate the observed generation-state drift in a clean checkout and the actual PR merge tree. Determine whether it is stale recorded inputs, unstable scanning, checkout contamination, or another cause; do not just overwrite a hash or suppress the check.

Respect generator ownership. Update the canonical generator/configuration/source actions, regenerate owned outputs, and verify a second generation is clean. Relevant starting points include `.github-gen/`, `.github/ci/`, `.github/actions/setup-velnor-workflow/`, and the `velnor-workflow` crate. Keep workflow capabilities generic and configuration declarative; do not add hand-maintained recovery YAML that bypasses the generator.

Preserve portable checksum/install behavior, trusted artifact provenance, digest verification, and isolation from caller-specific compiler/linker configuration. Test missing artifacts, controlled source bootstrap, incompatible platforms, and tampered artifacts as appropriate.

Run the repository's configured format, lint, test, generator, actionlint, Docker, documentation/frontend, and infrastructure validations. Use the applicable feature/target matrix and full-repository selection for final acceptance, not only affected-unit smoke tests. Add actual native macOS tests for portability and Docker integration tests for both host platforms. Record unsupported or unavailable cases honestly.

Do not make CI green by weakening tests, skipping required units, masking failures, fabricating statuses, removing Velnor coverage, changing trust defaults, or bypassing protection. Use legitimate conditional skips only where the workflow contract genuinely calls for them.

## 6. Independently verify and merge the PR stack

Recompute ancestry and choose the correct integration sequence, normally #809 before #812 given the observed relationship. Apply shared fixes at the appropriate point and retain #812's unique portability improvements.

After merging the first PR, refresh the dependent PR against the new main and review its actual remaining diff. Account for the repository's merge strategy so shared history is not reapplied or unique changes lost. Do not rewrite someone else's work or use an unsafe force push. Use focused additional PRs only when necessary for this mission; do not commit directly to main.

For each merge, require independent review of the exact final candidate, successful required checks on the correct tested revision, resolved blocking feedback, and an expected-head-SHA guard. A changed head or base invalidates relevant earlier evidence and must trigger appropriate revalidation. Do not use administrator overrides.

Verify that both requested PRs are actually merged in GitHub and record their merge SHAs. Do not silently replace the request with closing one as redundant.

Then run full main-branch verification on the final integrated SHA. Repair newly exposed failures through the same review/test/merge loop. Historical failed or cancelled runs need not be rewritten, but the latest applicable acceptance runs must pass and no current required job may remain abandoned in a queue.

## 7. Validate real release outputs

Inventory the current release workflows, supported artifacts/platforms, registries, signing requirements, installation channels, and version policy. Validate what the repository actually ships; do not assume an APT repository or another distribution channel exists merely because a package can be built.

Run the established release process for an appropriate new version containing the verified fixes, respecting approval and publishing policies. Verify build, packaging, signing/provenance where configured, upload, release metadata, and artifact availability. Do not overwrite published tags/assets or bypass signing to get a successful workflow.

Download and install the produced artifacts through documented consumer paths. Verify checksums, executable architecture, reported version/source identity, required runtime assets, and real operation on Linux and macOS. Include native macOS velnorctl and the on-demand host entry point. A dry run, compilation, or successful upload alone is insufficient release evidence.

Document a reproducible source-bootstrap fallback plus a normal released-artifact installation path so the next outage does not recreate this dependency cycle.

## 8. Deploy to Sentry and verify both operating modes

Prepare a rollback before changing Sentry: record the current artifact, configuration, state compatibility, and supported restoration procedure. Use the repository's existing deployment/package transaction mechanisms and preserve unrelated services and data. Drain relevant work before replacement; avoid duplicate old/new daemons.

Deploy the final verified release corresponding to merged source, not the temporary rescue build. Check the actual guardian/daemon/doctor topology, startup persistence, backend readiness, GitHub authentication, routing, available capacity, and reporting.

Complete real Velnor repository jobs on Sentry after deployment. Correlate GitHub runner/job identity with local Velnor events and execution logs so success cannot be accidentally attributed to the temporary Mac host. Verify consecutive jobs and a controlled restart/reconnection, not merely a health endpoint.

Prove the following end-to-end scenarios:

A. Sentry-independent recovery: from a fresh local configuration, bootstrap and connect a temporary host without using Sentry's control plane, Docker daemon, caches, or unpublished artifacts; execute and report real repository CI work.

B. macOS recovery: native macOS velnorctl controls local Velnor, and Docker-backed repository jobs complete with correct GitHub results and observable local evidence.

C. Burst capacity: Sentry and the temporary host participate simultaneously with correct identities and routing, execute distinct jobs, and do not duplicate work or corrupt shared state. Record queue/start/execution timings without unsupported performance claims.

D. Return to steady state: drain/disconnect the temporary host, verify cleanup, and prove Sentry still picks up a newly scheduled job. Reconnecting temporary capacity must remain straightforward.

Prefer isolated routing and test sessions for outage simulation rather than deliberately breaking production. Any necessary disruptive test must follow existing operational safeguards and have a rollback.

## Completion gate and final report

The goal is complete only when independent evidence establishes all of these,
in this order. Do not mark the goal finished after a local host, a plan, or a
partial merge.

1. The observed blockers have explained root causes, implemented fixes, and regression coverage.
2. A macOS-hosted Velnor runner executes real GitHub Actions jobs for `tailrocks/velnor` (Docker-backed Linux jobs driven by native macOS velnorctl). GitHub-hosted green is not this proof.
3. Every related Velnor recovery PR is merged using that macOS-hosted Velnor capacity as the Velnor-lane runner: at least #809, #812, #814, and #815. Intended changes are preserved. Required checks are green. No branch-protection, DCO, or signing bypass.
4. `main` is fully green after that merge stack (required checks and Velnor-lane jobs).
5. A new Velnor version is released through the real pipelines: Debian apt (`tailrocks/velnor-apt`) and Homebrew. Artifacts install and operate on Linux and macOS.
6. That verified release is deployed to Sentry (not a rescue or dirty-tree build). Sentry then executes real repository jobs; success is not attributed to the temporary Mac host.
7. Independent recovery, simultaneous extra capacity, and return to Sentry-only operation are demonstrated.
8. No missing evidence, failed required check, unfinished migration, or unavailable validation is disguised as success. The release process itself is proven, not only described.

Deliver a concise final report with root causes and fixes; changed/merged PRs and SHAs; independent reviewer findings; exact Linux/macOS installation, connection, PR-targeting, diagnostics, and shutdown commands; main/release run links; artifact versions and digests; Sentry deployment and rollback evidence; and the completed scenario matrix.

For each important test include the code/artifact revision, actual host and execution platform, GitHub run ID/attempt/job ID where applicable, result, and evidence location. Explicitly separate executed tests from compile-only checks and unperformed validation. Finish with verified outcomes, not promises of future work.
