# Phase 0 Target Checklist

This checklist is the current implementation plan for making Velnor a Rust
self-hosted runner replacement for the selected GitHub Actions workflows in:

- `jackin-project/jackin`
- `ChainArgos/java-monorepo`, specifically the Rust, Docker, Buildx, and related
  workflow paths

Typed workflow authoring is deferred. The Phase 0 input format remains the
existing GitHub Actions YAML, with GitHub handling triggers, matrix expansion,
reusable workflow expansion, secrets, permissions, scheduling, and UI.

## Current Gates

Local gates:

```sh
scripts/target_verify.sh
cargo test -q
```

Live proof gates:

```sh
scripts/fixture_smoke.sh
scripts/chainargos_rust_target_sequence.sh
scripts/jackin_rust_linux_sequence.sh
scripts/jackin_target_smoke.sh
```

The local gates prove implementation coverage against current target workflow
files and unit-level runner behavior. They do not prove Phase 0 completion.
Completion requires successful GitHub UI runs on a Linux host whose Docker
daemon can see Velnor's bind-mounted work directory.

The agent-owned live proof surface is the public fixture repository first. The
fixture should emulate the feature classes used by the two target repositories
and compare GitHub-hosted runner behavior with Velnor behavior. The agent must
not automatically migrate, edit, retarget, or dispatch the real ChainArgos or
Jackin repositories without explicit user direction. After fixture evidence is
green, the agent should report readiness; the user/operator performs the manual
target-repository validation and reports findings back. The target smoke scripts
enforce this boundary for the two real repositories with
`VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.

## Implemented

Runner protocol:

- latest hosted GitHub V2 broker/run-service path
- runner configure, status, remove, and run commands
- Linux host enforcement for configure, run, and preflight
- macOS label rejection and ARM label validation against host architecture
- broker session creation, polling, control-message handling, and migration
- run-service acquire, renew, complete
- job-scoped token use when GitHub provides it
- cancellation handling while a Docker job is active

Docker execution:

- one fresh Docker job container per acquired job
- per-job Docker network
- service container model in lower-level executor support
- host Docker socket mount
- host Docker CLI and Buildx plugin mounts when discoverable
- Docker bind-mount visibility preflight before polling GitHub
- shared workspace, temp, home, actions, tools, and sccache directories
- stale container/network cleanup and one retry on startup collision

GitHub job execution:

- bash/sh script steps
- `defaults.run.shell` and `defaults.run.working-directory`
- step env, job env, workflow env, and command files
- `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`, `GITHUB_STEP_SUMMARY`
- protected env handling for `GITHUB_*`, `RUNNER_*`, and `NODE_OPTIONS`
- step logs, annotations, telemetry, job outputs, environment URL, and final
  run-service completion payload

Expression and condition subset:

- target-shaped `github`, `runner`, `env`, `steps`, `needs`, `matrix`,
  `inputs`, `vars`, and `secrets`
- status functions: `always()`, `success()`, `failure()`, `cancelled()`
- `contains(...)`, `toJSON(...)`, `hashFiles(...)`
- equality, unary `!`, and simple `&&` / `||` value expressions
- runtime step-output resolution for later steps and job outputs

Rust-native action families:

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

Composite action support:

- target local `.github/actions/...` actions
- repository composite action metadata discovery
- nested repository action use from composites
- composite run steps, inputs, outputs, and `GITHUB_ACTION_PATH`

Drift guards:

- target workflow/action inventory
- target labels and `runs-on`
- triggers, permissions, concurrency, defaults, env, outputs, needs, matrices,
  and conditions used by current target workflows
- live proof script controls fail fast on invalid boolean flags and job counts
- unsupported job containers, service containers, job timeouts, job-level
  concurrency, direct `docker://` uses, and unsupported checkout inputs

## ChainArgos Rust Target

Target workflows:

- `ansible.yml`: small smoke to validate runner protocol, checkout,
  setup-python, defaults, and bash execution in the target repository
- `rust.yml`: Rust formatting, linting, tests, sccache, mise, path filters,
  package workflow-dispatch inputs, required job gates
- `rust-docker.yml`: Docker/Buildx/Bake, Docker login, GHA cache env export,
  Docker workflow-dispatch inputs, required job gates
- `rust-docker-build.yml`: reusable build workflow as expanded by GitHub
- `kestra-build-image.yml` and `kestra-build-publish.yml`: reusable Docker image
  build flow, Buildx, registry checks, and Docker cache behavior

Implemented locally:

- all used action families route to Rust-native adapters
- current workflow inventory passes `scripts/target_verify.sh`
- expression shapes for Rust and Docker required jobs are covered
- Docker adapter input shapes for current Rust Docker workflows are covered
- local shared workdir cache/artifact handoff is covered at unit level

Still needs live proof:

- `scripts/chainargos_rust_target_sequence.sh` on a Docker-visible Linux host
- `rust.yml` with narrow and broader `packages=` inputs
- `rust-docker.yml` with `targets=` and `push=false`
- Docker Hub login path with real target secrets
- Buildx/Bake access from inside the Velnor job container
- reusable Docker image workflow jobs expanded and scheduled by GitHub
- GitHub UI logs, conclusions, job outputs, and required-job status

## Jackin Rust/Linux Target

Target workflows:

- `ci.yml` Linux jobs
- `construct.yml`
- `docs.yml`
- later Linux paths from `preview.yml` and `release.yml`

Implemented locally:

- checkout, paths-filter, Rust setup, mise, cache, sccache, mold, and nextest
  action paths are covered for target shapes
- Docker/Buildx direct command path is covered for construct-shaped behavior
- artifact upload/download handoff is covered through shared Velnor workdir
- local `aggregate-needs` and `check-deployed-docs` composite actions are
  covered for target shapes
- Pages artifact/deploy environment URL behavior is covered for target shapes

Still needs live proof:

- `scripts/jackin_rust_linux_sequence.sh` on a Docker-visible Linux host
- `ci.yml` changes, check, msrv, validator, and required jobs
- `construct.yml` non-publish rehearsal path first, publish path later
- Docker Buildx direct shell commands in the job container
- Docker login with real target secrets
- construct digest artifact upload/download and manifest rehearsal/publish flow
- `docs.yml` build/link-check, Pages artifact/deploy, environment URL, sitemap
  output, and required aggregator
- GitHub UI rendering for annotations, logs, summaries, job outputs, and final
  conclusions

Out of current Docker-Linux scope:

- macOS runner replacement is not a Velnor target
- ARM Linux labels unless running on an actual ARM Linux host
- full hosted-runner image parity beyond target workflow needs
- broad marketplace action compatibility beyond the target action inventory

## Shared Missing Work

Live environment:

- run on a Linux host with local Docker socket or a daemon-visible workdir
- prove `velnor-runner preflight --require-docker-socket`
- prove the public fixture Velnor lanes and compare job

Transport parity:

- current cache/artifact transport is shared local workdir storage
- multi-host parity needs GitHub service-backed cache/artifact transport or an
  explicit one-host validation constraint

Docker isolation options:

- current MVP uses Docker-outside-of-Docker via host socket
- DinD/rootless Docker mode is not implemented

Future typed authoring:

- Pkl/PQL-style typed workflows remain future work
- when added, typed authoring should compile to the same normalized plan used by
  the GitHub job-message adapter

## Next Execution Order

1. Run `scripts/fixture_smoke.sh` on the live Docker-visible Linux host.
2. Run `scripts/chainargos_rust_target_sequence.sh`.
3. Fix only failures backed by sanitized live payloads or target workflow
   evidence.
4. Run `scripts/jackin_target_smoke.sh` for `ci.yml`, then `construct.yml`, then
   `docs.yml`.
5. Record GitHub run URLs, Velnor commit SHA, runner labels, logs, artifacts,
   cache behavior, outputs, and final workflow conclusions.
