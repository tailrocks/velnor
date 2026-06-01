# Target Live Runbook

This is the remaining Phase 0 proof path. Unit-level target verification is covered by `scripts/target_verify.sh`; completion requires real GitHub UI runs from the two target repositories.

## Prerequisites

- Linux host where the Docker daemon can see Velnor's bind-mounted work directory.
- A local Docker socket is preferred. Remote `DOCKER_HOST=tcp://...` daemons
  usually fail unless `--work-dir` points to a path mounted into that daemon,
  because Velnor mounts job scripts, workspace, temp files, artifacts, and cache
  state into job containers.
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

`velnor-runner run` also performs Docker preflight by default before it polls
GitHub for executable jobs. This is deliberate: a runner should not acquire a
queued GitHub job until the local Docker environment can run the mounted job
workspace. Use `--skip-preflight` only for `--complete-noop`, `--dry-run-jobs`,
or a deliberately controlled diagnostic run.

## Run Public Fixture First

Before the real target repositories, use the public fixture:

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
  --once \
  --idle-timeout-seconds 900
```

The fixture run to complete is:

```text
https://github.com/donbeave/velnor-actions-fixture/actions/runs/26762850861
```

The GitHub-hosted `compat-github` matrix jobs already passed. The Velnor proof
is that `compat-velnor` consumes the queued jobs and `compare-results` passes.

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

## Run Java Target

Start Velnor:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --once \
  --idle-timeout-seconds 900
```

`--once` waits until one GitHub job is actually acquired and handled. It does
not exit just because the broker has no message yet or sends a control message.
The idle timeout is optional, but it makes smoke runs fail clearly when labels,
workflow dispatch, or runner registration are wrong.

Initial recommended runs, in this order:

1. `ansible.yml`: smallest `hetzner-sentry-ci` target, validates checkout, setup-python, defaults.run working directory, and bash script execution.
2. `rust.yml`: validates path-filter outputs, required-job gates, setup/toolchain path propagation, and job status handling.
3. `rust-docker.yml` or `rust-docker-build.yml`: validates Docker socket/CLI/Buildx, Docker Hub login, Buildx/Bake, cache env, and Docker action input/output flow.
4. `kestra-build-publish.yml`: validates GitHub-expanded reusable workflow jobs using `kestra-build-image.yml`.

Run without `--once` after the first smoke job is clean:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work"
```

## Run Jackin Linux Paths

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

1. `ci.yml` Linux jobs: validates checkout, paths-filter, sccache soft-fail gate, mise, cache, artifact upload, aggregate-needs.
2. `construct.yml`: validates Docker Buildx direct shell commands, Docker login, artifact upload/download, and aggregate-needs.
3. `docs.yml`: validates Pages artifact/deploy runtime env, environment URL output, sitemap output, check-deployed-docs local action, and docs required aggregator.

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
  --dump-job-message "$PWD/.velnor-job-dumps" \
  --once
```

## Cleanup

Remove the runner after live validation:

```sh
cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN"
```

Delete `.velnor-work` only after logs, sanitized payloads, and GitHub run URLs are captured.
