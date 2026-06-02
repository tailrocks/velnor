# Velnor

Codename for a unified workflow engine for CI/CD and general-purpose pipelines.

## Idea

Velnor aims to combine CI/CD orchestration with broader workflow automation.

The user experience should feel close to GitHub Actions: workflows, triggers, jobs, steps, matrices, environments, secrets, artifacts, caches, reusable workflow modules, and runner labels should all feel familiar.

The first implementation target:

- existing `.github/workflows/*.yml` files keep working
- Velnor appears to GitHub as a self-hosted runner replacement through the
  current V2 just-in-time runner path
- jobs run inside Docker-isolated environments
- the initial scope is the workflows used by `jackin-project/jackin` and `ChainArgos/java-monorepo`

Out of current scope:

- no Pkl, PQL, KCL, or Velnor-native workflow authoring
- no Velnor-native workflow language
- no typed workflow-language implementation work of any kind
- no config-language parser, compiler, evaluator, binding, package, schema, or
  runtime integration work
- no replacement for GitHub's YAML parser or scheduler
- no macOS job execution or macOS runner-label support; Phase 0 runs Linux jobs
  inside Docker containers
- no broad GitHub Actions parity beyond the two target repositories

Pkl, PQL, KCL, CUE, Jsonnet, Starlark, Nickel, Dhall, and Nix workflow-language
research is not active Phase 0 work. It is not a requirement, not a current
design target, and not an implementation plan.

## Direction

Vision: [docs/vision.md](docs/vision.md).
Roadmap: [docs/roadmap.md](docs/roadmap.md).
Docs index: [docs/README.md](docs/README.md).

## Current Runner State

The first Rust crate is `velnor-runner`. Velnor targets GitHub's current V2
broker/run-service flow only. The supported setup path is GitHub's public
just-in-time runner configuration API, which returns an `encoded_jit_config`
with `UseV2Flow=true` and `ServerUrlV2`.

Classic runner registration-token setup is not a Velnor product path. Hosted
GitHub can return classic-only settings through that path, and Velnor should
not chase that route or implement classic distributed-task polling as a fallback.
If a setup path does not provide V2 settings, it is unsupported.

Velnor runs supported target jobs in Docker with Rust-native adapters for the
marketplace actions used by the target repositories.
The product target is a daemon that manages multiple internal GitHub runner
slots, so one Velnor process can acquire multiple GitHub jobs and spawn one
isolated Docker container per job concurrently. The daemon does not reuse one
GitHub runner identity across concurrent jobs; each slot owns a separate JIT
runner identity and broker session. The current `run --once` path remains the
single-slot compatibility/proof path.
Product target: Velnor daemon can run on macOS or Linux, but assigned jobs run
inside Linux Docker containers. Velnor refuses macOS/Darwin runner labels and
does not claim macOS job capability. Current code still rejects non-Linux
process hosts in `configure`, `run`, and `preflight`; that guard must be changed
as part of the macOS-host support work. Running the Velnor daemon inside a
Linux Docker container is also supported, including from Docker Desktop on
macOS, as long as the container can reach the Docker daemon and the daemon can
see Velnor's bind-mounted work directory. Any macOS legs in existing target
workflows are outside Velnor's execution surface.

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

cargo run --bin velnor-runner -- status --slots 2
cargo run --bin velnor-runner -- daemon \
  --url https://github.com/OWNER/REPO \
  --pat "$GITHUB_TOKEN" \
  --labels velnor,hetzner-sentry-ci \
  --replace \
  --slots 2
cargo run --bin velnor-runner -- daemon --slots 2 --once
cargo run --bin velnor-runner -- run
cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" --slots 2
```

Current commands still include legacy configuration flags while the code moves
to the JIT-only setup path. Product behavior is now defined as:

- Velnor uses `POST /repos/{owner}/{repo}/actions/runners/generate-jitconfig`
  for repository targets, or the matching organization/enterprise JIT endpoint.
- Velnor decodes `encoded_jit_config`, stores the ephemeral runner identity,
  OAuth credentials, `UseV2Flow`, and `ServerUrlV2`, then starts the broker
  session.
- Each daemon slot gets its own JIT runner config. JIT runners are ephemeral, so
  long-running daemon mode must recycle a slot by creating a new JIT runner
  config after a job completes or a slot becomes unusable.
- Classic distributed-task polling and classic registration-token fallback are
  intentionally unsupported.

`run` exchanges stored OAuth runner credentials, requires GitHub's current V2
broker settings, runs Docker preflight before polling for executable jobs,
creates a broker session, polls broker messages, acquires jobs from run-service,
renews locks, executes supported jobs, and completes them through run-service.
`daemon` runs the same V2 slot loop concurrently from one Velnor process. With
`--slots 1`, it uses the normal config
directory. With `--slots N` where `N > 1`, slot configs live under `<config-dir>/slots/slot-1`,
`<config-dir>/slots/slot-2`, and so on; each slot also receives its own `slot-N`
child under any explicit work, Docker-host work, or job-message dump directory.
GitHub sees multiple runner identities/sessions, while the operator runs one
daemon binary with one concurrency setting. Use `status --slots N` to inspect
the same internal slot configs and `remove --slots N` to unregister and delete
them. The daemon supervises slot tasks as a group and surfaces the first slot
panic or runner-loop error immediately instead of waiting for earlier slots to
exit. `daemon --once` is available for bounded proof runs; each slot exits after
one handled job, while normal daemon mode keeps polling.

Local target coverage is checked with:

```sh
scripts/target_verify.sh
cargo test -q
```

`scripts/target_verify.sh` expects the `jackin` and ChainArgos target checkouts
to have clean `.github` trees and to be current with their configured upstream
branches. Set `VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true` only when intentionally
auditing a local snapshot.

Live host readiness is checked with:

```sh
scripts/live_host_doctor.sh
```

Fixture proof readiness can be checked without registering a runner or
dispatching a workflow:

```sh
scripts/fixture_readiness.sh
```

To write a shareable readiness report without registering a runner or
dispatching a workflow:

```sh
scripts/fixture_report.sh
```

The fixture repository feature surface can be audited directly with:

```sh
cargo run -q -p velnor-tools -- fixture-audit
```

Repository automation policy: new committed automation should be Rust. Prefer
`velnor-tools` subcommands over adding shell or Python scripts. Existing shell
and Python automation is being migrated incrementally; Python runner-reference
and fixture-audit checks have already moved to `velnor-tools`.

The live proof scripts are Linux-only as well; they fail before runner
registration on non-Linux hosts, and reject `VELNOR_TARGET_MVP_ARM_LABEL=true`
unless the host is ARM Linux.

To run Velnor itself from Docker on a macOS workstation, build the project-owned
Ubuntu job image and the Velnor daemon image, then pass both the
container-visible work directory and the Docker-daemon-visible host work
directory:

```sh
docker build -f docker/job-ubuntu.Dockerfile -t velnor/job-ubuntu:24.04 .
docker build -t velnor-runner:local .
mkdir -p "$PWD/.velnor-work" "$PWD/.velnor-config" "$PWD/.velnor-job-dumps"

docker run --rm \
  --name velnor-local-preflight \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$PWD/.velnor-work:/work/.velnor-work" \
  -v "$PWD/.velnor-config:/config" \
  -v "$PWD/.velnor-job-dumps:/work/.velnor-job-dumps" \
  velnor-runner:local preflight \
    --work-dir /work/.velnor-work \
    --docker-host-work-dir "$PWD/.velnor-work" \
    --require-docker-socket
```

The same path mapping is required for `daemon` and `run`:

```sh
docker run --rm \
  --name velnor-fixture-daemon \
  -e GITHUB_TOKEN \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$PWD/.velnor-work:/work/.velnor-work" \
  -v "$PWD/.velnor-config:/config" \
  -v "$PWD/.velnor-job-dumps:/work/.velnor-job-dumps" \
  velnor-runner:local daemon \
    --url https://github.com/donbeave/velnor-actions-fixture \
    --pat "$GITHUB_TOKEN" \
    --name velnor-target-mvp \
    --labels velnor-target-mvp \
    --replace \
    --slots 2 \
    --once \
    --config-dir /config \
    --work-dir /work/.velnor-work \
    --docker-host-work-dir "$PWD/.velnor-work" \
    --dump-job-message /work/.velnor-job-dumps/fixture \
    --require-docker-socket
```

Those commands create only explicitly named `velnor-*` proof containers and
job-specific `velnor-job-*` / `velnor-net-*` resources. They do not prune,
remove, or stop unrelated Docker containers.

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

The staged Jackin Rust/Linux proof wrapper is:

```sh
scripts/jackin_rust_linux_sequence.sh
```

Smoke scripts write sanitized job payloads under `.velnor-job-dumps` by default.
They also write live proof evidence under `.velnor-live-evidence` by default
after Velnor consumes jobs and after a watched run completes or fails, including
best-effort runner-label, GitHub job ID/URL, artifact, job-step, bounded log
snapshots from GitHub, bounded local cache/artifact/sccache store snapshots from
Velnor's shared workdir, and sanitized job-message dump file listings. Override
that path with `VELNOR_LIVE_EVIDENCE_DIR`; override log excerpt length with
`VELNOR_LIVE_EVIDENCE_LOG_LINES`; override local store entry count with
`VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES`. Both evidence count values must be
positive integers and are validated before runner registration.
If a fixture or target smoke script fails after a GitHub run id is known, it
also writes a best-effort evidence file with phase `failed-before-completion`
before cleanup.
Set `VELNOR_TARGET_WORKFLOW=<workflow.yml>` on target smoke scripts to dispatch
that workflow before Velnor waits for target jobs.
Workflow values must be file names ending in `.yml` or `.yaml`.
Set `VELNOR_TARGET_REF=<branch-or-sha>` to dispatch from a specific ref.
Set `VELNOR_TARGET_INPUTS=key=value,other=value` for workflow dispatch inputs.
Input keys must match `[A-Za-z_][A-Za-z0-9_-]*`; empty entries and entries
without `=` are rejected before runner registration.
Set `VELNOR_TARGET_JOB_COUNT=<n>` when the target workflow needs Velnor to
consume more than one queued job. Target smoke scripts consume those jobs
through one bounded daemon invocation with one internal runner slot per
requested job.
Set `VELNOR_TARGET_WATCH_RUN=true` to wait for the GitHub workflow run to finish
after the selected Velnor jobs are consumed.
Set `VELNOR_IDLE_TIMEOUT_SECONDS=<n>` to tune per-job wait time; it must be a
positive integer. Explicit run IDs must also be positive integers.
Set `VELNOR_TARGET_MVP_ARM_LABEL=true` only on ARM Linux target smoke hosts; the
live scripts and runner reject the ARM label on non-ARM hosts.
Smoke scripts fail before dispatch if another online self-hosted runner can
match the proof labels, because Phase 0 cache/artifact proof assumes one Velnor
host. Set `VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true` only for a deliberate
non-exclusive run.
The real target repositories are guarded from accidental agent execution. Set
`VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` only when you are intentionally running
manual validation against `ChainArgos/java-monorepo` or `jackin-project/jackin`.
Agents must not set this variable, migrate those repositories, dispatch their
workflows, or register Velnor against them automatically. Agent-owned proof
stops at the public fixture repository and a clear "ready for manual target
testing" report; the user/operator performs the real ChainArgos and Jackin
validation manually and reports findings back.

The remaining Phase 0 proof is live GitHub UI validation on the two target
repositories from a Linux host whose Docker daemon can see Velnor's bind-mounted
work directory.

Active docs:

- [docs/README.md](docs/README.md)
- [docs/roadmap.md](docs/roadmap.md)
- [docs/target-live-runbook.md](docs/target-live-runbook.md)
- [docs/native-action-adapter-contract.md](docs/native-action-adapter-contract.md)
- [docs/rust-automation-policy.md](docs/rust-automation-policy.md)

## Proof-Of-Concept Test Repository

Public fixture repository: https://github.com/donbeave/velnor-actions-fixture

This is the small public repository used to prove Velnor against real GitHub
Actions scheduling before running the large target repositories. It has paired
workflow lanes:

- GitHub-hosted runner lane: `runs-on: ubuntu-latest`
- Velnor lane: `runs-on: [self-hosted, velnor-target-mvp]`

The fixture proof should normally use a fresh run. `scripts/fixture_smoke.sh`
dispatches `compat.yml` by default, then Velnor consumes the queued fixture jobs
and the compare job verifies matching outputs. To inspect the latest fixture run
or an explicit `VELNOR_FIXTURE_RUN_ID`, use:

```sh
scripts/fixture_status.sh
```

On a Linux host with a local Docker socket and `GITHUB_TOKEN` set, the fixture
proof can be run with:

```sh
scripts/fixture_readiness.sh
scripts/fixture_smoke.sh
```

The script runs one bounded Velnor daemon with two internal slots by default
because the fixture compat workflow has two Velnor matrix jobs. Override with
`VELNOR_FIXTURE_JOB_COUNT` for a different fixture shape. Set
`VELNOR_FIXTURE_RUN_ID=<run-id>` to consume a specific existing run; otherwise
the script dispatches a fresh run. Set
`VELNOR_FIXTURE_REF=<branch-or-sha>` and
`VELNOR_FIXTURE_INPUTS=key=value,other=value` when dispatching fixture
workflows from a non-default ref or with workflow inputs. Input validation uses
the same `key=value` rules as target smoke scripts.
Fixture workflow values are required and must be file names ending in `.yml` or
`.yaml`.

If the Docker daemon sees the work directory at a different path than the
runner process, set `VELNOR_DOCKER_HOST_WORK_DIR` to that daemon-visible path.
For a remote Docker daemon without a local `/var/run/docker.sock`, set
`VELNOR_REQUIRE_DOCKER_SOCKET=false` for the fixture smoke run.
For target jobs, Velnor runs the job in a Docker container and mounts
`/var/run/docker.sock` into that container when the socket is present. The
default `velnor/job-ubuntu:24.04` image is built from official `ubuntu:24.04`
and contains the Docker CLI and Buildx plugin, so workflow steps inside the job
container can run `docker`/`docker buildx` without relying on host binary
mounts. Service containers share the per-job Docker network with GitHub-style
aliases.
To force reuse of an existing run, set `VELNOR_FIXTURE_DISPATCH=false` together
with `VELNOR_FIXTURE_RUN_ID=<run-id>`.
The smoke script removes the temporary fixture runner on exit by default; set
`VELNOR_FIXTURE_CLEANUP_RUNNER=false` to keep it registered for debugging.
