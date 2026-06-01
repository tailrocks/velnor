# Phase 0 Implementation Gap Audit

Date: 2026-06-01

This is the current engineering audit for the active Velnor goal: make a
Rust/Linux self-hosted runner that GitHub can schedule like a normal
self-hosted Actions runner, while executing the selected target workflows in
Docker with Rust-native action adapters.

The scope is intentionally narrow:

- input remains existing GitHub Actions YAML
- GitHub owns workflow parsing, triggers, matrices, reusable workflows, secrets,
  permissions, queueing, and UI
- Velnor starts from the expanded `AgentJobRequestMessage`
- Velnor supports only Linux target jobs
- macOS runner replacement is not a Velnor target
- PQL/Pkl/KCL authoring is archived brainstorming only, not Phase 0
  implementation and not a current requirement
- target proof is limited to the `.github` trees in `jackin-project/jackin` and
  `ChainArgos/java-monorepo`, with `java-monorepo` treated as a Rust/Docker
  workflow target despite its repository name

## Latest Upstream Runner Facts

Upstream source refreshed during this audit:

- repository: <https://github.com/actions/runner>
- latest release observed: `v2.334.0`
- release commit: `f1995ede5d885c997d13d8eca5467c4ce97fe69c`
- main branch rechecked during implementation planning:
  `c6a124e18496a6e5d2357415052d1799afc64b63`
- latest release page: <https://github.com/actions/runner/releases/tag/v2.334.0>

The latest release still uses the broker/run-service V2 path for current hosted
GitHub self-hosted runners:

- `Runner.cs` selects `BrokerMessageListener` when `UseV2Flow` is true.
- `BrokerMessageListener.cs` requires `ServerUrlV2` and creates a broker
  session with runner id, name, version, and OS description.
- broker polling sends session id, runner status, runner version, OS,
  architecture, and disable-update flag.
- V2 job messages arrive as run-service job references, not full workflow YAML.
- the runner best-effort acknowledges the broker request when requested, then
  acquires the full `AgentJobRequestMessage` from run-service.
- `acquirejob` treats `404`, `409`, and `422` as stale/unusable message cases
  to skip/back off.
- V2 completion uses run-service `completejob` with conclusion, job outputs,
  step results, annotations, environment URL, telemetry, billing owner id, and
  infrastructure failure category.
- run-service jobs are renewed while executing.

Important implementation consequence: the GitHub runner protocol still assigns
one active job to one runner session, but Velnor should not expose that as the
product model. Production Velnor should run as one daemon with a managed pool of
internal GitHub runner slots. Each slot owns its own broker session/runner
identity, and the daemon spawns one isolated Docker job container per assigned
job. That lets one Velnor daemon consume and execute multiple GitHub jobs at the
same time while keeping GitHub protocol state separated per slot.

## Current Velnor Evidence

Current local gates:

```sh
scripts/target_verify.sh
cargo test -q
```

These gates currently cover:

- latest runner version drift through `scripts/check_runner_reference.py`
- V2 broker/run-service registration, polling, acquire, renew, completion, and
  key control messages
- Linux host enforcement, macOS/Darwin label rejection, and ARM label validation
- Docker preflight before job acquisition
- fresh Docker job container, per-job network, shared workspace/temp/home/actions
  and tools mounts, Docker socket, Docker CLI, and Buildx plugin handling
- command files, workflow commands, step outputs, job outputs, step conditions,
  protected env behavior, and basic target expression support
- native adapters for the target action families
- target workflow/action inventory drift for the current checked-out target
  repositories
- live smoke script input validation and runner-label exclusivity checks

The local gates do not prove GitHub UI parity. They only prove that current code
and the current target workflow files agree on the expected feature subset.

## Implemented Enough For First Live Proof

These areas are no longer the first blockers. They may still need live fixes,
but they are implemented and locally guarded enough to attempt the public
fixture and target repository runs.

| Area | Current state |
| --- | --- |
| Runner protocol | V2 broker/run-service path, session lifecycle, acquire, renew, complete, cancellation, token refresh, runner refresh/update handling. |
| Job execution model | One GitHub job per runner process; target jobs execute in a fresh Linux Docker job container. |
| Docker access | Docker-outside-of-Docker via host socket plus mounted Docker CLI/Buildx plugin; preflight checks daemon-visible workdir. |
| Runtime env | Target-shaped `GITHUB_*`, `RUNNER_*`, `ACTIONS_*`, event file, tool cache, cache/results endpoints, and secret masking. |
| Script steps | `bash`/`sh`, default shell/working directory, env rendering, command files, logs, annotations, and summaries. |
| Expressions | Target subset for `github`, `runner`, `env`, `steps`, `needs`, `matrix`, `inputs`, `vars`, `secrets`, `contains`, `toJSON`, `hashFiles`, status functions, equality, unary `!`, `&&`, and `||`. |
| Checkout | Native `actions/checkout` for self/external checkout, path, ref, token, fetch depth/tags, credential cleanup, and safe directory. |
| Target native actions | Current target action inventory routes to Rust-native adapters instead of marketplace JavaScript/TypeScript bundles. |
| Composite actions | Target local composites and target repository composite metadata expansion are covered. |
| Results reporting | Best-effort timeline records, feed logs, step results, annotations, telemetry, job outputs, environment URL, and final run-service completion. |

## Remaining Gaps By Proof Level

### Level 1: Public Fixture Live Proof

This is the immediate next proof. It must run on a Linux host where the Docker
daemon can see Velnor's bind-mounted work directory.

Required evidence:

- `scripts/fixture_smoke.sh` registers `donbeave/velnor-actions-fixture` with
  label `velnor-target-mvp`
- the queued `compat-velnor` jobs are acquired by Velnor, not by another runner
- checkout, Rust tool setup, cache, command files, local composite action,
  artifact upload/download, and compare job pass
- Docker fixture path proves Buildx/Docker access inside the job container
- GitHub UI shows readable logs and correct job conclusions

Current blocker in this agent environment:

- the active Docker daemon is remote and cannot see this workspace path
- live proof needs a local Linux Docker socket or a `VELNOR_DOCKER_HOST_WORK_DIR`
  mapping to a path visible inside the Docker daemon host

### Level 2: ChainArgos Rust/Docker Target Proof

Run this before `jackin` because the target workflows already use
`hetzner-sentry-ci`.

Required evidence:

- `scripts/chainargos_rust_target_sequence.sh` passes on the live host
- `ansible.yml` proves checkout, setup-python, defaults, and bash execution
- `rust.yml` proves Rust setup, mise/just/cargo tools, sccache soft-fail gates,
  paths-filter, cache, required checks, workflow-dispatch inputs, and job
  outputs
- Docker workflows prove Docker login, Buildx/Bake, cache runtime env,
  `push=false` rehearsal, direct Docker CLI commands, and reusable workflow jobs
  as GitHub expands them
- final GitHub UI state matches expected pass/fail/skip behavior

### Level 3: Jackin Linux Target Proof

Run this after the ChainArgos Docker path is solid. Velnor must use only Linux
labels such as `ubuntu-latest`, `ubuntu-24.04`, and optionally
`ubuntu-24.04-arm` on an actual ARM Linux host.

Required evidence:

- `scripts/jackin_rust_linux_sequence.sh` passes on the live host
- `ci.yml` Linux jobs prove paths-filter, Rust setup, sccache, mise, cache,
  artifacts, and aggregate-needs behavior
- `construct.yml` proves Docker Buildx direct shell commands, Docker login,
  artifact handoff, and publish rehearsal
- `docs.yml` proves Pages artifact/deploy, environment URL, sitemap output,
  local `check-deployed-docs`, and required aggregator
- macOS matrix jobs remain outside Velnor scope and are not claimed by Velnor
  labels

## Design Rules That Should Not Change

- Do not implement Phase 0 workflow parsing or scheduling. GitHub already did
  that before Velnor receives a job.
- Do not execute marketplace JavaScript or TypeScript for supported target
  action families. Keep routing to Rust-native adapters by normalized action
  family name.
- Do not hardcode target workflow ids or step ids in the executor. Target scans
  may be drift alarms; runtime behavior must stay reusable.
- Do not add macOS support or macOS labels. A Linux Docker runner cannot
  truthfully replace `macos-latest`.
- Do not build PQL, Pkl, KCL, or any native workflow language now. They are not
  needed for the current goal.
- Do not treat local green tests as completion. Completion requires live GitHub
  UI runs on the two target repositories.

## Research-To-Build Contract

The current research points to this build contract:

- registration/configuration may be automated by Velnor, but concurrency is not
  one GitHub runner session handling many jobs. Concurrency is one daemon
  supervising multiple isolated runner slots, each with its own GitHub runner
  identity and broker session.
- Velnor should follow latest `actions/runner` V2 control-plane behavior
  closely, because GitHub owns scheduling and the wire protocol is private.
- Velnor should intentionally diverge from the official runner's execution
  model by always using Docker isolation for target Linux jobs, even when the
  workflow YAML does not request a job container.
- supported marketplace action families should be implemented as Rust adapters,
  not by executing the marketplace JavaScript/TypeScript bundles.
- configuration-language research is separate archived brainstorming, not a
  replacement for the Phase 0 GitHub runner protocol implementation.

## Next Engineering Sequence

1. Re-run `scripts/target_verify.sh` and `cargo test -q` before every live
   proof attempt.
2. On the live Linux host, run `scripts/fixture_readiness.sh` with the same
   work directory intended for the runner. It checks fixture status, fixture
   feature-surface drift, and Docker host readiness without GitHub mutation.
3. Run `scripts/fixture_smoke.sh` and capture the generated evidence markdown.
4. Fix only failures backed by fixture live payload dumps or target workflow
   drift.
5. After fixture evidence is green, ask the user/operator to manually run
   `scripts/chainargos_rust_target_sequence.sh` with
   `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
6. After ChainArgos is green, ask the user/operator to manually run
   `scripts/jackin_rust_linux_sequence.sh` with
   `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
7. After both target repositories pass in GitHub UI, decide separately whether
   any archived configuration-language brainstorming should be reopened.
