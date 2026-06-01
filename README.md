# Velnor

Codename for a unified workflow engine for CI/CD and general-purpose pipelines.

## Idea

Velnor aims to combine CI/CD orchestration with broader workflow automation.

The user experience should feel close to GitHub Actions: workflows, triggers, jobs, steps, matrices, environments, secrets, artifacts, caches, reusable workflow modules, and runner labels should all feel familiar.

The first implementation target:

- existing `.github/workflows/*.yml` files keep working
- Velnor registers as a GitHub self-hosted runner replacement
- jobs run inside Docker-isolated environments
- the initial scope is the workflows used by `jackin-project/jackin` and `ChainArgos/java-monorepo`

Out of current scope:

- no Pkl workflow authoring
- no Velnor-native workflow language
- no replacement for GitHub's YAML parser or scheduler
- no broad GitHub Actions parity beyond the two target repositories

## Direction

See [docs/vision.md](docs/vision.md).

## Current Runner State

The first Rust crate is `velnor-runner`. It can register as a GitHub
self-hosted runner, requires the current V2 broker/run-service flow, and runs
supported target jobs in Docker with Rust-native adapters for the marketplace
actions used by the target repositories.

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --pat "$GITHUB_TOKEN" \
  --labels velnor,hetzner-sentry-ci \
  --replace

cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --token fake \
  --labels velnor \
  --dry-run

cargo run --bin velnor-runner -- status
cargo run --bin velnor-runner -- run
cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN"
```

`configure` validates runner scope URLs, can request a short-lived GitHub runner
token from `--pat`, exchanges that runner token for tenant credentials, and can
add/replace a runner agent in the selected pool. `run` exchanges stored OAuth
runner credentials, requires GitHub's current V2 broker settings, runs Docker
preflight before polling for executable jobs, creates a broker session, polls
broker messages, acquires jobs from run-service, renews locks, executes
supported jobs, and completes them through run-service.

Local target coverage is checked with:

```sh
scripts/target_verify.sh
cargo test -q
```

Live host readiness is checked with:

```sh
scripts/live_host_doctor.sh
```

The ChainArgos Rust target smoke wrapper is:

```sh
scripts/chainargos_target_smoke.sh
```

The staged ChainArgos Rust proof wrapper is:

```sh
scripts/chainargos_rust_target_sequence.sh
```

The first Jackin Linux smoke wrapper is:

```sh
scripts/jackin_target_smoke.sh
```

Smoke scripts write sanitized job payloads under `.velnor-job-dumps` by default.
Set `VELNOR_TARGET_WORKFLOW=<workflow.yml>` on target smoke scripts to dispatch
that workflow before Velnor waits for target jobs.
Set `VELNOR_TARGET_REF=<branch-or-sha>` to dispatch from a specific ref.
Set `VELNOR_TARGET_INPUTS=key=value,other=value` for workflow dispatch inputs.
Set `VELNOR_TARGET_JOB_COUNT=<n>` when the target workflow needs Velnor to
consume more than one queued job.
Set `VELNOR_TARGET_WATCH_RUN=true` to wait for the GitHub workflow run to finish
after the selected Velnor jobs are consumed.
Set `VELNOR_TARGET_MVP_ARM_LABEL=true` only on ARM Linux target smoke hosts.

The remaining Phase 0 proof is live GitHub UI validation on the two target
repositories from a Linux host whose Docker daemon can see Velnor's bind-mounted
work directory.

Phase 0 runner compatibility: [docs/phase-0-github-runner-compat.md](docs/phase-0-github-runner-compat.md).

Phase 0 target checklist: [docs/phase-0-target-checklist.md](docs/phase-0-target-checklist.md).

Implementation roadmap: [docs/implementation-roadmap.md](docs/implementation-roadmap.md).

Public fixture repository plan: [docs/public-fixture-repo-plan.md](docs/public-fixture-repo-plan.md).

## Proof-Of-Concept Test Repository

Public fixture repository: https://github.com/donbeave/velnor-actions-fixture

This is the small public repository used to prove Velnor against real GitHub
Actions scheduling before running the large target repositories. It has paired
workflow lanes:

- GitHub-hosted runner lane: `runs-on: ubuntu-latest`
- Velnor lane: `runs-on: [self-hosted, velnor-target-mvp]`

Current fixture run:
https://github.com/donbeave/velnor-actions-fixture/actions/runs/26762850861

The GitHub-hosted lane has passed. The Velnor lane is the next live proof: once
Velnor is registered with label `velnor-target-mvp`, it should consume the
queued fixture jobs and the compare job should verify matching outputs.
Check the current fixture proof state with:

```sh
scripts/fixture_status.sh
```

On a Linux host with a local Docker socket and `GITHUB_TOKEN` set, the fixture
proof can be run with:

```sh
scripts/fixture_smoke.sh
```

The script runs two Velnor `--once` jobs by default because the fixture compat
workflow has two Velnor matrix jobs. Override with `VELNOR_FIXTURE_JOB_COUNT`
for a different fixture shape. Set `VELNOR_FIXTURE_REF=<branch-or-sha>` and
`VELNOR_FIXTURE_INPUTS=key=value,other=value` when dispatching fixture
workflows from a non-default ref or with workflow inputs.

If the Docker daemon sees the work directory at a different path than the
runner process, set `VELNOR_DOCKER_HOST_WORK_DIR` to that daemon-visible path.
For a remote Docker daemon without a local `/var/run/docker.sock`, set
`VELNOR_REQUIRE_DOCKER_SOCKET=false` for the fixture smoke run.
For target jobs, Velnor runs the job in a Docker container and mounts
`/var/run/docker.sock` plus the host Docker/Buildx client into that container
when the socket is present. This is the Phase 0 Docker-outside-of-Docker model:
workflow steps inside the job container can run `docker`/`docker buildx`, and
service containers share the per-job Docker network with GitHub-style aliases.
To create a fresh fixture run instead of using the current queued run, set
`VELNOR_FIXTURE_DISPATCH=true`.
The smoke script removes the temporary fixture runner on exit by default; set
`VELNOR_FIXTURE_CLEANUP_RUNNER=false` to keep it registered for debugging.

Target workflow audit and language brainstorming history: [docs/research/config-language-comparison.md](docs/research/config-language-comparison.md).

GitHub runner protocol contract: [docs/research/github-runner-protocol-contract.md](docs/research/github-runner-protocol-contract.md).

GitHub runner job-message contract: [docs/research/github-runner-job-message-contract-2026-06-01.md](docs/research/github-runner-job-message-contract-2026-06-01.md).

Self-hosted runner implementation blueprint: [docs/research/self-hosted-runner-implementation-blueprint-2026-06-01.md](docs/research/self-hosted-runner-implementation-blueprint-2026-06-01.md).

Phase 0 current status and live proof gaps: [docs/research/phase-0-current-status-2026-06-01.md](docs/research/phase-0-current-status-2026-06-01.md).

Target live validation runbook: [docs/target-live-runbook.md](docs/target-live-runbook.md).
