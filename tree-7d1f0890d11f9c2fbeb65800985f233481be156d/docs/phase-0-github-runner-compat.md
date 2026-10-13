# Phase 0: GitHub Runner Compatibility

Status: active implementation

## Decision

Velnor's first phase is not Pkl workflow authoring.

Phase 0 is a Rust implementation of a GitHub self-hosted runner-compatible agent. Existing repositories keep their current `.github/workflows/*.yml` files and local `.github/actions/*` actions. The user installs/registers Velnor as a self-hosted runner, gives it the same labels used by current workflows, and GitHub schedules existing jobs to it.

Typed authoring remains only brainstorming. It is not part of Phase 0.

The GitHub runner wire protocol is private and can drift. That is acceptable for Phase 0. Velnor treats this as an implementation and maintenance cost, not as a product blocker, because the drop-in migration path is more important than having a public protocol contract at this stage.

Detailed implementation notes: `docs/research/phase-0-implementation-plan.md`.

## Goal

Make this work without changing workflow files:

```text
GitHub repository
  .github/workflows/*.yml
  .github/actions/*
        |
        | GitHub parses YAML, expands matrix, evaluates workflow-level scheduling
        v
GitHub Actions service
        |
        | runner protocol / job messages
        v
Velnor self-hosted runner
        |
        | Docker-isolated job execution
        v
job container + actions + scripts
```

## What GitHub Still Owns In Phase 0

Because Velnor registers as a self-hosted runner, GitHub still owns:

- event triggers
- YAML parsing
- workflow DAG scheduling
- matrix expansion
- reusable workflow expansion
- branch/path trigger filtering
- job assignment to runner labels
- secrets and tokens
- job messages
- artifact/cache/runtime endpoints
- UI, logs, checks, annotations, and status reporting

This is good. It means Phase 0 does not need to parse GitHub workflow YAML or implement GitHub's full expression engine.

## What Velnor Must Own In Phase 0

Velnor must implement enough of the runner side:

- register/unregister runner with GitHub
- maintain runner session
- long-poll for job messages
- acknowledge/delete messages
- renew running job locks
- support the hosted GitHub broker/run-service V2 job flow
- execute assigned jobs
- report logs/timeline/results
- run shell steps
- route supported marketplace action references to Rust-native adapters
- run composite actions
- run Docker/container behavior through Velnor-owned Rust adapters for supported target actions
- support local actions from `.github/actions/*`
- support action command files such as `GITHUB_OUTPUT`
- support environment/context variables expected by actions
- support artifacts/cache by passing GitHub-provided runtime endpoints and tokens
- isolate every job in Docker

Velnor must not hardcode target workflow files, job ids, or step ids in the
runtime. The two target repositories define the first capability set, but each
capability should be implemented as reusable runner behavior or a reusable
native action adapter. The native adapter boundary is documented in
`docs/native-action-adapter-contract.md`.

## Target Repositories

Phase 0 is scoped to these repositories first:

- https://github.com/jackin-project/jackin/tree/main/.github
- https://github.com/ChainArgos/java-monorepo/tree/main/.github

Compatibility is proven only when these workflows can run successfully on Velnor with their current YAML.

## Runner Labels

The current `ChainArgos/java-monorepo` workflows use:

```yaml
runs-on: hetzner-sentry-ci
```

Velnor must register with that label to receive those jobs.

The `jackin` workflows mostly use GitHub-hosted labels such as:

```yaml
runs-on: ubuntu-latest
runs-on: ubuntu-24.04
runs-on: ubuntu-24.04-arm
runs-on: macos-latest
```

For Phase 0, we can either:

1. Change only runner labels in `jackin` workflows to a Velnor self-hosted label, or
2. Provide Velnor runner labels matching selected hosted labels.

Option 2 is more "magic" but risky because labels like `macos-latest` imply OS capabilities that a Linux Docker runner cannot provide. Phase 0 should target Linux jobs first and explicitly defer macOS hosted-runner replacement.

## Docker Isolation Model

Velnor should execute each assigned GitHub job inside a fresh Docker job container.

Recommended initial model:

```text
velnor daemon on host
  -> create job workspace
  -> create per-job Docker network
  -> start job container
  -> bind mount workspace/temp/actions/tools
  -> bind mount Docker socket for workflows that build images
  -> run each step with docker exec
  -> collect command files and outputs
  -> remove container and network
```

This mirrors the official runner's container model:

- official runner starts a long-running job container
- it mounts work/temp/actions/tools directories
- it starts the container with `tail -f /dev/null`
- it executes steps with `docker exec`
- service containers share a per-job Docker network

Velnor follows that shape and now performs a bind-mount visibility preflight before user scripts run. If the Docker daemon cannot see the host path used for Velnor's temp/work directories, the job fails early with an operator-facing mount error instead of failing later as a missing `/__t/<step>.sh` script.

Phase 0 difference:

- Velnor should use this Docker isolation even when the GitHub workflow does not specify `container:`.

## Docker And Buildx Caveat

The target workflows use Docker heavily:

- `docker/setup-buildx-action`
- `docker/login-action`
- `docker/bake-action`
- `docker/build-push-action`
- direct `docker buildx` commands
- Docker Hub auth
- registry and GitHub Actions cache backends

If the whole job runs inside a container, Docker support needs one of these:

### Option A: Mount Host Docker Socket

```text
-v /var/run/docker.sock:/var/run/docker.sock
```

Pros:

- simplest
- fastest
- compatible with Buildx/Bake
- likely enough for current target workflows

Cons:

- weak isolation; container can control host Docker daemon

### Option B: Docker-In-Docker Sidecar

Run a per-job DinD service container and point `DOCKER_HOST` at it.

Pros:

- stronger job isolation

Cons:

- slower
- more complex cache behavior
- Buildx and privileged mode complexity

Phase 0 should start with Option A and document the security tradeoff.

Initial code now models this Docker command shape without executing it yet. The next execution step is to connect assigned job steps to:

- command-file setup per step: implemented as step plan
- script writing under runner temp: implemented as step plan
- `docker exec` using `bash`/`sh`: implemented behind command runner abstraction
- command-file parsing after each step: implemented as state collector

## Required Workflow Features

From the target repositories, Phase 0 must support:

- triggers: handled by GitHub
- matrix expansion: handled by GitHub
- reusable workflow expansion: handled by GitHub
- `workflow_call`: handled by GitHub before runner receives jobs
- `workflow_run`, `schedule`, `merge_group`: handled by GitHub
- `runs-on` labels
- `permissions`
- `env`
- `defaults.run.shell`
- `defaults.run.working-directory`
- job and step `if`
- job `needs`
- job outputs
- step outputs
- contexts: `github`, `env`, `runner`, `secrets`, `inputs`, `steps`, `needs`, `matrix`
- expression functions observed: `always`, `contains`, `hashFiles`, `toJSON`
- command files: `GITHUB_OUTPUT`, `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_STATE`, `GITHUB_STEP_SUMMARY`
- legacy workflow commands enough for annotations/groups/masking
- artifacts upload/download
- cache restore/save
- GitHub Pages deploy actions
- local composite actions
- marketplace action behavior through Rust-native adapters for the observed action families
- Docker Buildx/Bake action behavior through Rust-native adapters
- Docker Buildx/Bake

Some of these are mostly implemented by actions themselves. Velnor's job is to provide the environment those actions expect.

For Velnor's intended architecture, "actions themselves" means Rust-native
compatibility adapters for the observed action references and input shapes, not
executing the original marketplace JavaScript or TypeScript bundles.

## Target Action Inventory

Observed in `jackin-project/jackin`:

- local `.github/actions/aggregate-needs`
- local `.github/actions/check-deployed-docs`
- `actions/checkout`
- `actions/cache`
- `actions/upload-artifact`
- `actions/download-artifact`
- `actions/upload-pages-artifact`
- `actions/deploy-pages`
- `docker/setup-buildx-action`
- `docker/login-action`
- `dorny/paths-filter`
- `jdx/mise-action`
- `mozilla-actions/sccache-action`
- `renovatebot/github-action`
- `rui314/setup-mold`
- `extractions/setup-just`
- `crazy-max/ghaction-github-runtime`

Observed in `ChainArgos/java-monorepo`:

- `actions/checkout`
- `actions/cache`
- `actions/setup-python`
- `docker/setup-buildx-action`
- `docker/login-action`
- `docker/metadata-action`
- `docker/build-push-action`
- `docker/bake-action`
- `dorny/paths-filter`
- `jdx/mise-action`
- `renovatebot/github-action`
- `dtolnay/rust-toolchain`
- `extractions/setup-just`
- `baptiste0928/cargo-install`
- `Swatinem/rust-cache`

## Minimum Execution Engine

Phase 0 Velnor runner should have these modules:

```text
velnor-runner
  config
    register
    unregister
    credential store
  protocol
    session create/delete
    message poll/ack/delete
    job lock renew
    job complete
  worker
    job lifecycle
    step lifecycle
    command files
    contexts/env
  actions
    download/resolve action
    action.yml parser
    native action adapter registry
    native action adapters
    composite action handler
  container
    job container
    service containers
    docker exec
    workspace mounts
    docker socket mode
  reporting
    logs
    annotations
    timeline
    results
```

## GitHub Runner Protocol Reality

The official `actions/runner` source is the best reference. The protocol is not designed as a stable third-party API, but the runner is open source and the wire behavior can be implemented.

For Velnor, this is an acceptable constraint. The project intentionally targets compatibility with the private GitHub runner protocol, starting with the behavior needed by the target repositories.

The official runner flow:

1. Configure runner using a registration token.
2. Register or replace an agent with labels and public key.
3. Store runner settings and OAuth credential data.
4. Create an agent session.
5. Long-poll for `TaskAgentMessage`.
6. Decrypt message if session encryption is enabled.
7. Dispatch `AgentJobRequestMessage` to a worker process.
8. Renew job lock every 60 seconds while running.
9. Run job steps.
10. Report logs/timeline through job server.
11. Finish job request with result.
12. Delete/ack message and continue polling.

Velnor can simplify process boundaries, but should preserve protocol behavior.

## Protocol Compatibility Stance

- Accept that the protocol is private.
- Track the official `actions/runner` implementation closely.
- Pin the official runner version used as the compatibility reference.
- Prefer pragmatic compatibility over clean abstraction.
- Build real GitHub integration tests early.
- Keep unsupported protocol branches explicit rather than pretending to be complete.

Live hosted GitHub compatibility proof from 2026-06-01:

- registration and removal tokens work
- runner agent registration works
- broker session/message poll works
- run-service job acquisition works
- run-service completion works for `--complete-noop`
- real run-service script input payloads are normalized into Velnor script steps

Current local target proof covers the target workflow action inventory, native
adapter routing, expression subset, cache/artifact handoff, Docker/Buildx action
shapes, Pages, Renovate, setup/tool actions, local composites, and command/log
behavior through `scripts/target_verify.sh` plus the Rust test suite.

Remaining proof is live, not unit-level: run the two target repositories through
GitHub UI on a host where Docker can see Velnor's bind-mounted work directory.
The earlier local development Docker daemon accepted bind mounts but exposed
empty directories inside containers, so Docker execution could not be accepted
as a valid failure of the Velnor step runner.

## Non-Goals For Phase 0

- no Pkl workflow authoring
- no Velnor-native scheduler
- no replacement GitHub UI
- no full GitHub-hosted runner image compatibility
- no macOS runner replacement
- no full Windows support
- no support for every action in the marketplace
- no support for arbitrary enterprise/GHES edge cases until needed

## Success Criteria

Phase 0 is successful when:

1. Velnor registers as a self-hosted runner in GitHub.
2. GitHub assigns a real job from one target repository to Velnor.
3. Velnor runs the job in a Docker-isolated environment.
4. Logs appear in the GitHub Actions UI.
5. Step outputs and env files work.
6. `actions/checkout` works.
7. At least one local composite action works.
8. Artifact/cache actions work for target workflows.
9. Docker Buildx/Bake workflows work using the selected Docker isolation mode.
10. The job completes with correct success/failure status in GitHub.

## Deferred Typed Authoring Brainstorm

After Phase 0 is proven with live target repository runs, typed authoring can be reconsidered. Do not implement it now.

```text
Typed workflow source
  -> compile to GitHub Actions YAML, or
  -> compile to Velnor native execution plan
```

The first typed-authoring deliverable, if revisited later, should likely be a generator/validator for GitHub-compatible YAML, because Phase 0 already relies on GitHub's scheduler.
