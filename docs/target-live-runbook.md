# Target Live Runbook

This is the remaining Phase 0 proof path. Unit-level target verification is covered by `scripts/target_verify.sh`; completion requires real GitHub UI runs from the two target repositories.

The implementation checklist is tracked in [roadmap.md](roadmap.md).

## Prerequisites

- macOS or Linux process environment where the Docker daemon can run Linux job
  containers and see Velnor's bind-mounted work directory. Running Velnor inside
  a Linux Docker container is valid, including from a macOS workstation, if
  `/var/run/docker.sock` is mounted and `--docker-host-work-dir` points at the
  host path visible to the Docker daemon.
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
- GitHub PAT in `GITHUB_TOKEN` with permission to create and delete JIT self-hosted runner configs for the target repo.
- Set `VELNOR_TARGET_MVP_ARM_LABEL=true` only on hosts where Docker can provide ARM64 Linux job containers; the live proof
  scripts reject that label on incompatible hosts before JIT config.
- A persistent Velnor `--work-dir` for bounded native caches and offline
  diagnostics. Artifact upload/download uses GitHub's Results Service v4 so
  fan-in jobs remain correct across slots and hosts.
- Current target verifier passes locally:

```sh
scripts/target_verify.sh
cargo test -q
```

The target verifier checks that the local `jackin` and ChainArgos checkouts have
clean `.github` trees and are current with their configured upstream branches.
Set `VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true` only for a deliberate local
snapshot.

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

Before attempting the fixture smoke, run the fixture readiness gate. It checks
the current fixture workflow status, fixture feature surface, and live host
readiness, but does not create JIT runner configs or dispatch workflows:

```sh
scripts/fixture_readiness.sh
```

To capture the same information as a Markdown artifact, use:

```sh
scripts/fixture_report.sh
```

Set `VELNOR_RUN_TARGET_VERIFY=true` to include the target workflow verifier, and
set `VELNOR_CHECK_TARGET_MVP_CONFIG=true` after JIT config creation to validate
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
- The default `velnor/job-ubuntu:26.04` job image is built from official
  `ubuntu:26.04` and includes Docker CLI plus Buildx.
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
scripts/fixture_readiness.sh
scripts/fixture_smoke.sh
```

The readiness script checks fixture status, fixture feature-surface drift, and
host Docker readiness without JIT runner config creation or workflow dispatch. The
smoke script runs `cargo test -q`, Docker preflight through the host doctor,
fixture JIT runner slot setup, one bounded Velnor daemon with `--once` and
one internal slot per requested fixture job, and a GitHub run status summary.
Set `VELNOR_RUN_TARGET_VERIFY=true` when you also want the host doctor to run
`scripts/target_verify.sh` before JIT runner config creation. Override the count with
`VELNOR_FIXTURE_JOB_COUNT` when the fixture shape changes. By default it
dispatches a fresh `compat.yml` run and waits for the new run id. Set
`VELNOR_FIXTURE_RUN_ID=<run-id>` to consume an existing run; set
`VELNOR_FIXTURE_DISPATCH=false` only when using an existing run id. Set
`VELNOR_FIXTURE_REF=<branch-or-sha>` and
`VELNOR_FIXTURE_INPUTS=key=value,other=value` when dispatching fixture workflows
from a non-default ref or with workflow inputs. Input validation uses the same
`key=value` rules as target smoke scripts. Fixture workflow values are required
and must be file names ending in `.yml` or `.yaml`. The script removes the temporary
fixture runner on exit by default; set
`VELNOR_FIXTURE_CLEANUP_RUNNER=false` to keep it visible for debugging. The
manual daemon equivalent is:

```sh
cargo run --bin velnor-runner -- daemon \
  --url https://github.com/tailrocks/velnor-actions-fixture \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --labels velnor-target-mvp \
  --replace \
  --slots 2 \
  --once \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket
```

The GitHub-hosted `compat-github` matrix jobs should pass on the fresh run. The
Velnor proof is that `compat-velnor` consumes the queued jobs and
`compare-results` passes.
Check the latest fixture run, or an explicit `VELNOR_FIXTURE_RUN_ID`, with:

```sh
scripts/fixture_status.sh
```

Manual fixture runs can still be started with:

```sh
gh workflow run compat.yml --repo tailrocks/velnor-actions-fixture
gh workflow run docker.yml --repo tailrocks/velnor-actions-fixture
```

## Configure Target Repositories

For `ChainArgos/java-monorepo`, create a JIT runner config with the target preset. This includes `hetzner-sentry-ci`, `ubuntu-latest`, and `ubuntu-24.04`.

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
cargo run --bin velnor-runner -- status --check-target-mvp --slots 1
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
Boolean controls must be exactly `true` or `false`, and job counts must be
positive integers; the sequence fails before JIT runner config creation on invalid
values.

The real target repositories are manual validation surfaces. Before running any
target smoke or target sequence script against `ChainArgos/java-monorepo` or
`jackin-project/jackin`, the operator must explicitly set
`VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`. Without that confirmation, the scripts
fail before JIT runner config creation or workflow dispatch.

It runs the live host doctor, creates JIT runner configs for `ChainArgos/java-monorepo` with the
target label preset and consumes queued jobs through bounded daemon mode
(`daemon --once --slots N`). Set `VELNOR_TARGET_CLEANUP_RUNNER=true` to delete
the stored JIT runner id on exit. Sanitized job payloads are written to
`.velnor-job-dumps/chainargos-target` by default; set
`VELNOR_DUMP_JOB_MESSAGES=` to disable dumps or point it at another directory. Set
`VELNOR_TARGET_WORKFLOW=ansible.yml` to have the script dispatch the first
recommended ChainArgos workflow before waiting for one Velnor job, or leave it
unset to consume already queued work. Workflow values must be file names ending
in `.yml` or `.yaml`. Set `VELNOR_TARGET_REF=<branch-or-sha>` when the
workflow should be dispatched from a non-default ref. Set
`VELNOR_TARGET_INPUTS=packages=bitcoin-processor-app,push=false` for
`workflow_dispatch` inputs; each comma-separated `key=value` is passed to
`gh workflow run -f`. Input keys must match `[A-Za-z_][A-Za-z0-9_-]*`; empty
entries and entries without `=` are rejected before JIT runner config creation. Set
`VELNOR_TARGET_JOB_COUNT=<n>` when validating a workflow that queues multiple
Velnor jobs and should be consumed by one smoke script invocation. Production
Velnor is one daemon with multiple internal GitHub runner slots: each slot owns
a broker session, and each assigned job gets its own isolated Docker container
so jobs can run concurrently. The smoke scripts use the same daemon shape in a
bounded proof mode. Set `VELNOR_TARGET_WATCH_RUN=true` when the
selected job count should be followed by `gh run watch --exit-status` to prove
the full GitHub workflow conclusion. Set `VELNOR_IDLE_TIMEOUT_SECONDS=<n>` to
tune per-job wait time; it must be a positive integer. Explicit run IDs such as
`VELNOR_TARGET_RUN_ID` and `VELNOR_FIXTURE_RUN_ID` must also be positive
integers.

For manual low-level debugging, one bounded daemon invocation looks like:

```sh
cargo run --bin velnor-runner -- daemon \
  --url https://github.com/ChainArgos/java-monorepo \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --target-mvp-labels \
  --replace \
  --slots 1 \
  --once \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket \
  --idle-timeout-seconds 900
```

`daemon --once` starts the requested internal runner slot count and each slot
waits until one GitHub job is actually acquired and handled. It does not exit
just because the broker has no message yet or sends a control message. The idle
timeout is optional, but it makes smoke runs fail clearly when labels, workflow
dispatch, or JIT runner config creation are wrong.

Before dispatch, fixture and target smoke scripts check repository self-hosted
runners and fail if another online runner can match the proof labels. This
keeps Phase 0 cache/artifact evidence tied to one Velnor host. Set
`VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true` only for a deliberate non-exclusive
run.

Initial recommended runs, in this order:

1. `ansible.yml`: smallest `hetzner-sentry-ci` target; validates checkout, setup-python, defaults.run working directory, and bash script execution. Use `VELNOR_TARGET_JOB_COUNT=1`.
2. `rust.yml`: validates path-filter outputs, required-job gates, setup/toolchain path propagation, and job status handling. Use workflow inputs to start narrow, for example `VELNOR_TARGET_INPUTS=packages=bitcoin-processor-app`, then raise `VELNOR_TARGET_JOB_COUNT` as GitHub queues additional package jobs.
3. `rust-docker.yml` or `rust-docker-build.yml`: validates Docker socket/CLI/Buildx, Docker Hub login, Buildx/Bake, cache env, and Docker action input/output flow. Use `VELNOR_TARGET_INPUTS=push=false` for rehearsal runs.
4. `kestra-build-publish.yml`: validates GitHub-expanded reusable workflow jobs using `kestra-build-image.yml`; set `VELNOR_TARGET_JOB_COUNT` high enough to drain the selected reusable build jobs.

Run without `--once` after the bounded smoke jobs are clean:

```sh
cargo run --bin velnor-runner -- daemon \
  --url https://github.com/ChainArgos/java-monorepo \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --target-mvp-labels \
  --slots 4 \
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
and bounded daemon execution shape as `scripts/chainargos_target_smoke.sh`.
Set `VELNOR_TARGET_MVP_ARM_LABEL=true` only on an ARM Linux host to add the
`ubuntu-24.04-arm` label. Velnor rejects that label on non-ARM hosts.

To run the staged Jackin Rust/Linux proof path in one command, use:

```sh
scripts/jackin_rust_linux_sequence.sh
```

The sequence runs `ci.yml`, `construct.yml`, and `docs.yml` with
`gh run watch` enabled by default. Tune it with
`VELNOR_JACKIN_CI_JOB_COUNT`, `VELNOR_JACKIN_CONSTRUCT_JOB_COUNT`,
`VELNOR_JACKIN_DOCS_JOB_COUNT`, `VELNOR_JACKIN_SEQUENCE_INCLUDE_CONSTRUCT`,
`VELNOR_JACKIN_SEQUENCE_INCLUDE_DOCS`, and
`VELNOR_JACKIN_SEQUENCE_WATCH_RUN`.
Boolean controls must be exactly `true` or `false`, and job counts must be
positive integers; the sequence fails before JIT runner config creation on invalid
values.

Create a JIT runner config for `jackin-project/jackin` with x64 Linux labels:

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/jackin-project/jackin \
  --pat "$GITHUB_TOKEN" \
  --name velnor-target-mvp \
  --target-mvp-labels \
  --replace
```

Only add `--target-mvp-arm-label` on an ARM Linux host. Velnor rejects that
label on non-ARM hosts. Do not add macOS labels; Velnor has no macOS execution
surface in Phase 0.

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
- artifact upload/download behavior for target artifact actions, including
  cross-job Results Service handoff when producer and consumer use different
  slots or hosts
- job outputs used by downstream jobs
- required aggregator job status and final workflow status

The smoke scripts write a Markdown evidence file under `.velnor-live-evidence`
by default after Velnor consumes jobs and after a watched run completes or
fails. Those files include a best-effort GitHub API snapshot of the JIT
runner labels, run job IDs/URLs, run artifacts, job steps, a bounded
first/last-line log excerpt, a bounded snapshot of Velnor's local
`_velnor_caches`, `_velnor_artifacts`, and `_velnor_sccache` directories under
the shared work directory, and the sanitized job-message dump files consumed by
Velnor. Set
`VELNOR_LIVE_EVIDENCE_DIR=<path>` to store those records elsewhere. Set
`VELNOR_LIVE_EVIDENCE_LOG_LINES=<n>` to change the log excerpt size. Set
`VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES=<n>` to change the per-store listing size.
Both evidence count values must be positive integers; smoke scripts validate
them before JIT runner config creation.

If a fixture or target smoke script fails after a GitHub run id is known, it
writes a best-effort evidence file before cleanup with phase
`failed-before-completion`. Use that file together with any sanitized job
payload dump before retrying or changing runner code.

Keep sanitized job payloads for failures:

```sh
cargo run --bin velnor-runner -- run \
  --work-dir "$PWD/.velnor-work" \
  --require-docker-socket \
  --dump-job-message "$PWD/.velnor-job-dumps" \
  --once
```

For manual target testing, prefer the smoke scripts or `daemon --once --slots N`;
the single-slot `run --once` command is only a low-level diagnostic path.

## Cleanup

Remove the runner after live validation:

```sh
cargo run --bin velnor-runner -- remove --pat "$GITHUB_TOKEN" --slots 1
```

Delete `.velnor-work` only after logs, sanitized payloads, and GitHub run URLs are captured.
