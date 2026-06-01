# Target Live Runbook

This is the remaining Phase 0 proof path. Unit-level target verification is covered by `scripts/target_verify.sh`; completion requires real GitHub UI runs from the two target repositories.

## Prerequisites

- Linux host where the Docker daemon can see Velnor's bind-mounted work directory.
- A local Docker socket is preferred. Remote `DOCKER_HOST=tcp://...` daemons
  usually fail unless `--work-dir` points to a path mounted into that daemon,
  because Velnor mounts job scripts, workspace, temp files, artifacts, and cache
  state into job containers.
- If the runner process and Docker daemon see the same mounted work directory at
  different paths, pass `--docker-host-work-dir <daemon-visible-path>` or set
  `VELNOR_DOCKER_HOST_WORK_DIR` when using `scripts/fixture_smoke.sh`.
- For the fixture smoke workflow only, `VELNOR_REQUIRE_DOCKER_SOCKET=false`
  skips the local socket preflight when using a remote daemon. Docker-heavy
  target workflows still need Docker access inside the job container.
- Git, Docker CLI, and Buildx plugin installed on the host.
- GitHub PAT in `GITHUB_TOKEN` with permission to register a repository self-hosted runner for the target repo.
- A persistent Velnor `--work-dir` shared by all jobs in the same validation run; native cache restore/save and artifact upload/download use this shared work directory for single-host handoff until GitHub cache/artifact service transport is implemented.
- Current target verifier passes locally:

```sh
scripts/target_verify.sh
cargo test -q
```

Run the Docker host preflight on the same host and with the same work directory that the live runner will use:

```sh
cargo run --bin velnor-runner -- preflight \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket
```

This verifies host Git, Docker daemon access, Buildx, `/var/run/docker.sock`, Docker CLI/Buildx usability inside the job image, required job-image tools, execution of a temp script from `/__t` with `/__w` workdir, and whether the daemon can see Velnor's bind-mounted work directory.

The same host readiness checks can be run with:

```sh
scripts/live_host_doctor.sh
```

Set `VELNOR_RUN_TARGET_VERIFY=true` to include the target workflow verifier, and
set `VELNOR_CHECK_TARGET_MVP_CONFIG=true` after runner registration to validate
the stored target labels, V2 settings, ids, and credentials.

`velnor-runner run` also performs Docker preflight by default before it polls
GitHub for executable jobs. For target repository runs, pass
`--require-docker-socket` too, because the current target workflows include
Docker/Buildx steps that need the host socket inside the job container. This is
deliberate: a runner should not acquire a queued GitHub job until the local
Docker environment can run the mounted job workspace. Use `--skip-preflight`
only for `--complete-noop`, `--dry-run-jobs`, or a deliberately controlled
diagnostic run.

## Docker From Job Containers

Velnor runs each Linux job inside a Docker job container. For the target
workflows, that job container must also be able to run Docker commands itself.
The Phase 0 model is Docker-outside-of-Docker:

- Velnor mounts the host Docker socket at `/var/run/docker.sock` inside the job
  container.
- Velnor mounts the host Docker CLI at `/usr/local/bin/docker` when it can find
  it.
- Velnor mounts the host Docker CLI plugin directory at
  `/usr/local/lib/docker/cli-plugins` when it can find Buildx.
- Native Docker adapters and any shell steps that run `docker ...` or
  `docker buildx ...` talk to the host daemon through that socket.

This intentionally prioritizes compatibility with GitHub Actions Docker/Buildx
workflows over strict container isolation. A later phase can add a DinD or
rootless/containerized daemon mode, but Phase 0 uses the host socket because the
two target repositories need Buildx/Bake and direct Docker commands to work from
inside the job container.

## Run Public Fixture First

Before the real target repositories, use the public fixture:

```sh
scripts/fixture_smoke.sh
```

That script runs the local verifier, Docker preflight, fixture runner
registration, two Velnor `--once` jobs for the fixture matrix by default, and a
GitHub run status summary. Override the count with `VELNOR_FIXTURE_JOB_COUNT`
when the fixture shape changes. Set `VELNOR_FIXTURE_DISPATCH=true` to start a
fresh `compat.yml` run instead of using the existing queued run id. Set
`VELNOR_FIXTURE_REF=<branch-or-sha>` and
`VELNOR_FIXTURE_INPUTS=key=value,other=value` when dispatching fixture workflows
from a non-default ref or with workflow inputs. The script removes the temporary
fixture runner on exit by default; set
`VELNOR_FIXTURE_CLEANUP_RUNNER=false` to keep it registered for debugging. The
manual equivalent is:

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/donbeave/velnor-actions-fixture \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --labels velnor-target-mvp \
  --replace
```

Start Velnor:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket \
  --once \
  --idle-timeout-seconds 900
```

The fixture run to complete is:

```text
https://github.com/donbeave/velnor-actions-fixture/actions/runs/26762850861
```

The GitHub-hosted `compat-github` matrix jobs already passed. The Velnor proof
is that `compat-velnor` consumes the queued jobs and `compare-results` passes.
Check current fixture status with:

```sh
scripts/fixture_status.sh
```

After the existing queued jobs are gone, new fixture runs can be started with:

```sh
gh workflow run compat.yml --repo donbeave/velnor-actions-fixture
gh workflow run docker.yml --repo donbeave/velnor-actions-fixture
```

## Register Target Repositories

For `ChainArgos/java-monorepo`, register with the target preset. This includes `hetzner-sentry-ci`, `ubuntu-latest`, and `ubuntu-24.04`.

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/ChainArgos/java-monorepo \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --target-mvp-labels \
  --replace
```

Check GitHub repository settings and verify `velnor-target-mvp` is online before dispatching workflows.
Also validate the local config before running jobs:

```sh
cargo run --bin velnor-runner -- status --check-target-mvp
```

## Run ChainArgos Rust Target

After the public fixture passes, the first real target smoke can be run with:

```sh
scripts/chainargos_target_smoke.sh
```

To run the staged ChainArgos Rust proof path in one command, use:

```sh
scripts/chainargos_rust_target_sequence.sh
```

The sequence runs `ansible.yml`, a narrow `rust.yml` dispatch, a narrow
`rust-docker.yml` dispatch, and `kestra-build-publish.yml` with `gh run watch`
enabled by default. Tune it with `VELNOR_CHAINARGOS_RUST_PACKAGES`,
`VELNOR_CHAINARGOS_DOCKER_TARGETS`, `VELNOR_CHAINARGOS_DOCKER_PUSH`,
`VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_DOCKER`,
`VELNOR_CHAINARGOS_SEQUENCE_INCLUDE_KESTRA`, and per-workflow job counts such as
`VELNOR_CHAINARGOS_RUST_JOB_COUNT`.

It runs the live host doctor, registers `ChainArgos/java-monorepo` with the
target label preset, validates stored V2/label config, and consumes queued jobs
with repeated `--once` runs. Set `VELNOR_TARGET_CLEANUP_RUNNER=true` to remove
the registered runner on exit. Sanitized job payloads are written to
`.velnor-job-dumps/chainargos-target` by default; set
`VELNOR_DUMP_JOB_MESSAGES=` to disable dumps or point it at another directory. Set
`VELNOR_TARGET_WORKFLOW=ansible.yml` to have the script dispatch the first
recommended ChainArgos workflow before waiting for one Velnor job, or leave it
unset to consume already queued work. Set `VELNOR_TARGET_REF=<branch-or-sha>` when the
workflow should be dispatched from a non-default ref. Set
`VELNOR_TARGET_INPUTS=packages=bitcoin-processor-app,push=false` for
`workflow_dispatch` inputs; each comma-separated `key=value` is passed to
`gh workflow run -f`. Set `VELNOR_TARGET_JOB_COUNT=<n>` when validating a
workflow that queues multiple Velnor jobs and should be consumed by one smoke
script invocation. Set `VELNOR_TARGET_WATCH_RUN=true` when the selected job
count should be followed by `gh run watch --exit-status` to prove the full
GitHub workflow conclusion.

Start Velnor:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket \
  --once \
  --idle-timeout-seconds 900
```

`--once` waits until one GitHub job is actually acquired and handled. It does
not exit just because the broker has no message yet or sends a control message.
The idle timeout is optional, but it makes smoke runs fail clearly when labels,
workflow dispatch, or runner registration are wrong.

Initial recommended runs, in this order:

1. `ansible.yml`: smallest `hetzner-sentry-ci` target; validates checkout, setup-python, defaults.run working directory, and bash script execution. Use `VELNOR_TARGET_JOB_COUNT=1`.
2. `rust.yml`: validates path-filter outputs, required-job gates, setup/toolchain path propagation, and job status handling. Use workflow inputs to start narrow, for example `VELNOR_TARGET_INPUTS=packages=bitcoin-processor-app`, then raise `VELNOR_TARGET_JOB_COUNT` as GitHub queues additional package jobs.
3. `rust-docker.yml` or `rust-docker-build.yml`: validates Docker socket/CLI/Buildx, Docker Hub login, Buildx/Bake, cache env, and Docker action input/output flow. Use `VELNOR_TARGET_INPUTS=push=false` for rehearsal runs.
4. `kestra-build-publish.yml`: validates GitHub-expanded reusable workflow jobs using `kestra-build-image.yml`; set `VELNOR_TARGET_JOB_COUNT` high enough to drain the selected reusable build jobs.

Run without `--once` after the first smoke job is clean:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket
```

## Run Jackin Linux Paths

After the ChainArgos Rust target passes, the first Jackin Linux target smoke can
be run with:

```sh
scripts/jackin_target_smoke.sh
```

It uses the same host readiness, target label preset, V2 config validation,
sanitized job dumps, optional `VELNOR_TARGET_WORKFLOW=<workflow.yml>` dispatch,
and repeated `--once` execution shape as `scripts/chainargos_target_smoke.sh`.
Set `VELNOR_TARGET_MVP_ARM_LABEL=true` only on an ARM Linux host to add the
`ubuntu-24.04-arm` label.

Register the same runner to `jackin-project/jackin` with x64 Linux labels:

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/jackin-project/jackin \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --target-mvp-labels \
  --replace
```

Only add `--target-mvp-arm-label` on an ARM Linux host. macOS labels are intentionally out of Phase 0.

Recommended Jackin live sequence:

1. `ci.yml` Linux jobs: validates checkout, paths-filter, sccache soft-fail gate, mise, cache, artifact upload, aggregate-needs. Start with the `changes` job and then raise `VELNOR_TARGET_JOB_COUNT` to cover the queued Linux jobs.
2. `construct.yml`: validates Docker Buildx direct shell commands, Docker login, artifact upload/download, and aggregate-needs. Use a non-publishing ref first, then add publish paths after Docker Buildx is proven.
3. `docs.yml`: validates Pages artifact/deploy runtime env, environment URL output, sitemap output, check-deployed-docs local action, and docs required aggregator. Use workflow dispatch on a branch first, then main-branch deployment after dry paths pass.

## Evidence To Capture

For each run, record:

- workflow file, run id, run URL, branch/ref, and Velnor commit SHA
- runner labels shown in GitHub UI
- first and last step logs for each job assigned to Velnor
- timeline/annotation readability for `::error::`, `::notice::`, and grouped logs where present
- cache restore/save behavior for target cache actions through the shared Velnor work directory
- artifact upload/download behavior for target artifact actions, including cross-job handoff through the shared Velnor work directory
- job outputs used by downstream jobs
- required aggregator job status and final workflow status

Keep sanitized job payloads for failures:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket \
  --dump-job-message "$PWD/.velnor-job-dumps" \
  --once
```

## Cleanup

Remove the runner after live validation:

```sh
cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN"
```

Delete `.velnor-work` only after logs, sanitized payloads, and GitHub run URLs are captured.
