# Phase 0 Implementation Contract

This is the active engineering contract for making Velnor work as a Rust
self-hosted runner replacement for the selected target workflows.

Phase 0 does not introduce Pkl, KCL, PQL, or a Velnor-native workflow language.
GitHub Actions YAML remains the source format. GitHub remains responsible for
parsing workflow files, evaluating triggers, expanding matrices, expanding
`workflow_call`, scheduling jobs, applying repository secrets, and rendering the
Actions UI. Velnor starts after GitHub has queued an already-expanded job for a
self-hosted runner.

## Compatibility Boundary

Velnor should be drop-in compatible for the workflows currently used by:

- `jackin-project/jackin`
- `ChainArgos/java-monorepo`, limited to its Rust, Docker, Buildx, cache,
  artifact, Renovate, and related workflow paths

Velnor runner execution is Linux-only. macOS runner replacement is not a target
for this project. If a target workflow contains macOS matrix legs, those legs
remain on GitHub-hosted infrastructure or are outside Velnor's supported
execution surface.

The supported unit is not a hardcoded workflow file. The supported unit is the
runner-side behavior those workflows require: labels, job-message fields,
runtime contexts, script execution, command files, native action adapters,
Docker access, cache/artifact handoff, logs, annotations, outputs, and
completion payloads.

Anything outside that inventory should fail clearly rather than falling back to
arbitrary marketplace JavaScript execution.

## Runner Protocol

Velnor follows the latest hosted GitHub self-hosted runner path from
`actions/runner`, currently verified against:

- latest release: `v2.334.0`
- tag commit: `f1995ede5d885c997d13d8eca5467c4ce97fe69c`
- main at refresh: `c6a124e18496a6e5d2357415052d1799afc64b63`

Required protocol behavior:

- register with GitHub using a repository runner token or a PAT that can request
  one
- exchange the runner token through `actions/runner-registration`
- add or replace a `TaskAgent` with the configured labels
- store OAuth runner credentials, pool id, agent id, `UseV2Flow`, and
  `ServerUrlV2`
- mint short-lived OAuth access tokens from the stored runner credentials
- require broker/run-service V2 for hosted GitHub targets
- create a broker session and poll broker messages
- acknowledge run-service job references best-effort
- acquire full job payloads from run-service
- renew acquired jobs while they run
- complete acquired jobs through run-service with the job-scoped
  `SystemVssConnection` token when present
- handle cancellation, broker migration, token refresh, refresh/update, and
  hosted-shutdown messages as control-plane events

Classic distributed-task message polling is reference material only. It is not a
normal Phase 0 execution path.

## Execution Model

Every target Linux job runs inside a fresh Docker job container, even when the
workflow does not declare `container:`.

Required container behavior:

- per-job Docker network
- shared host work directory mounted into the job container
- shared workspace, temp, home, actions, tools, and sccache directories
- GitHub-style container paths for workspace/temp/actions/tools where target
  commands expect them
- bind-mount visibility preflight before polling GitHub for work
- host Docker socket mounted into job containers for Docker-heavy target jobs
- host Docker CLI and Buildx plugin mounted when discoverable
- stale container/network cleanup and bounded retry for startup collisions

The Phase 0 Docker model is Docker-outside-of-Docker through the host socket.
DinD and rootless Docker are future isolation options, not required for the
current proof.

## Job Runtime

The job executor must support the runner-side behavior present in the acquired
GitHub job messages:

- bash and sh script steps
- `defaults.run.shell`
- `defaults.run.working-directory`
- workflow, job, and step environment overlays
- protected `GITHUB_*`, `RUNNER_*`, and `NODE_OPTIONS` handling
- `GITHUB_ENV`
- `GITHUB_OUTPUT`
- `GITHUB_PATH`
- `GITHUB_STEP_SUMMARY`
- command-file `NAME=value` and heredoc syntax
- workflow commands for annotations, grouping, masking, and logs used by target
  workflows
- runtime step outputs and final job outputs
- environment URL evaluation
- step result, outcome, and `continue-on-error` semantics for target shapes
- cancellation mapped to Docker container termination and canceled completion

## Expressions

Velnor should implement the runtime expression subset that target job messages
and target action metadata actually use:

- contexts: `github`, `runner`, `env`, `steps`, `needs`, `matrix`, `inputs`,
  `vars`, `secrets`
- status functions: `always()`, `success()`, `failure()`, `cancelled()`
- functions: `contains(...)`, `toJSON(...)`, `hashFiles(...)`
- equality and inequality
- unary `!`
- simple `&&` and `||`
- runtime fallback expressions such as
  `steps.dispatch.outputs.docs || steps.filter.outputs.docs`

GitHub-side workflow parsing and matrix expansion are intentionally out of
scope because GitHub performs them before Velnor receives a job.

## Native Action Adapters

Velnor does not execute marketplace JavaScript or TypeScript bundles in Phase 0.
Known marketplace action families are routed by repository name to Rust-native
adapters. Pinned refs and commit SHAs in YAML are ignored for those known
families.

The current target adapter inventory:

- `actions/checkout`
- `actions/cache`
- `actions/upload-artifact`
- `actions/download-artifact`
- `actions/upload-pages-artifact`
- `actions/deploy-pages`
- `actions/setup-python`
- `dorny/paths-filter`
- `jdx/mise-action`
- `dtolnay/rust-toolchain`
- `extractions/setup-just`
- `baptiste0928/cargo-install`
- `Swatinem/rust-cache`
- `mozilla-actions/sccache-action`
- `rui314/setup-mold`
- `crazy-max/ghaction-github-runtime`
- `renovatebot/github-action`
- `docker/login-action`
- `docker/setup-buildx-action`
- `docker/metadata-action`
- `docker/build-push-action`
- `docker/bake-action`

Local composite actions are supported for the current target shapes:

- `./.github/actions/aggregate-needs`
- `./.github/actions/check-deployed-docs`

## Verification Gates

Local gates prove static target coverage and unit-level behavior:

```sh
scripts/target_verify.sh
cargo test -q
```

Live gates prove actual GitHub compatibility:

```sh
scripts/fixture_smoke.sh
scripts/chainargos_rust_target_sequence.sh
scripts/jackin_rust_linux_sequence.sh
```

Live proof must run on a Linux host whose Docker daemon can see Velnor's
bind-mounted work directory. Current local verification is not sufficient to
declare Phase 0 complete.

## Remaining Phase 0 Work

The remaining work is primarily live proof and any fixes discovered from real
sanitized job payloads:

- public fixture Velnor lane must pass and compare against GitHub-hosted lane
- ChainArgos `ansible.yml`, `rust.yml`, `rust-docker.yml`, and reusable Docker
  image jobs must pass through Velnor in GitHub UI
- Jackin `ci.yml`, `construct.yml`, and `docs.yml` Linux paths must pass through
  Velnor in GitHub UI
- GitHub UI logs, annotations, summaries, job outputs, artifacts, cache
  behavior, environment URLs, and final workflow conclusions must be checked
- real Docker login and Buildx/Bake paths must be exercised with target secrets
- cache/artifact service transport remains open for multi-host parity; current
  proof uses a shared local Velnor work directory
