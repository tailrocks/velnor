# Phase 0 Current Status

Date: 2026-06-01

This is the working status for the real Phase 0 goal: run the current
`.github` trees from `jackin-project/jackin` and `ChainArgos/java-monorepo` on
Velnor as a GitHub-scheduled self-hosted runner.

The authoritative green local gate is:

```sh
scripts/target_verify.sh
cargo test -q
```

This is not yet completion proof. Completion still requires live GitHub Actions
UI runs on the two target repositories.

The current requirement checklist is maintained in
`docs/phase-0-target-checklist.md`.

## Upstream Runner Facts To Preserve

Reference upstream:

- `actions/runner` latest release: `v2.334.0`
- `actions/runner` release commit: `f1995ede5d885c997d13d8eca5467c4ce97fe69c`
- `actions/runner` `main` rechecked on 2026-06-01:
  `c6a124e18496a6e5d2357415052d1799afc64b63`
- Velnor drift check: `scripts/check_runner_reference.py`
- source audit: `docs/research/actions-runner-source-audit-2026-06-01.md`
- latest V2 refresh: `docs/research/latest-runner-v2-refresh-2026-06-01.md`
- implementation gap audit:
  `docs/research/phase-0-implementation-gap-audit-2026-06-01.md`

The 2026-06-01 source refresh found no broker/run-service protocol file changes
between `v2.334.0` and `origin/main`. The relevant diff in `src/Runner.Listener`,
`src/Sdk/RSWebApi`, and `src/Runner.Worker` is concentrated in worker/debugger
code (`ActionManager`, DAP files, `ExecutionContext`, `JobExtension`,
`NodeScriptActionHandler`, and `JobRunner`). That does not change Phase 0's
V2 broker/run-service implementation contract.

The latest release was rechecked again on 2026-06-01 during scope cleanup.
`scripts/check_runner_reference.py` still reports `actions/runner` `v2.334.0`
as current, and `crates/velnor-runner/src/protocol.rs` still advertises
`actions-runner/2.334.0 (velnor)`.

Implementation facts:

- Hosted GitHub target path is V2 broker/run-service, not classic polling.
- GitHub owns YAML parsing, trigger matching, reusable workflow expansion,
  matrix expansion, job graph scheduling, secrets, permissions, and UI.
- Velnor receives `AgentJobRequestMessage` payloads after GitHub expansion.
- Velnor's daemon path owns multiple internal runner slots and executes one
  isolated Docker container per assigned job concurrently. Bounded proof scripts
  now use `daemon --once --slots N` instead of repeated single-slot `run --once`
  loops.
- Each target Linux job runs in a fresh Docker job container.
- Supported marketplace action families are selected by normalized action name;
  YAML refs, tags, and SHAs are ignored for Velnor's Rust-native implementation.

## Implemented And Locally Guarded

| Area | Current evidence |
| --- | --- |
| Latest runner pin | `RUNNER_VERSION = 2.334.0`; `scripts/check_runner_reference.py` checks GitHub latest release and user-agent drift. |
| V2-only hosted path | `velnor-runner run` requires `UseV2Flow` and `ServerUrlV2`; normal path uses broker session/message plus run-service acquire/renew/complete, with bounded broker session startup retry and best-effort session deletion on runner-loop errors. |
| Run safety preflight | `velnor-runner run` performs Docker preflight before polling GitHub for executable jobs, preserving target workdir, daemon-visible workdir, Buildx, and Docker socket requirements before acquiring a queued job. |
| Broker controls | `BrokerMigration`, `ForceTokenRefresh`, runner update/refresh, hosted shutdown, busy-job cancellation, transient broker poll retry, and empty-message backoff are recognized. |
| V2 job tokens | Run-service renew/complete use the job-scoped `SystemVssConnection` token when available. |
| Job acquisition | Run-service acquire handles non-retriable `404`, `409`, and `422` as stale/unusable messages. |
| Docker job isolation | Fresh workspace/temp/home/actions/tools layout, Docker job container, network, host Docker socket plus Docker CLI/Buildx mounting into the job container and action sidecars, and bind-mount preflight exist. |
| Script steps | Bash/sh steps, defaults, working directory, env/context rendering, command files, outputs, summaries, and protected env handling are covered. |
| Expression subset | Target-shaped `github`, `runner`, `env`, `steps`, `needs`, `matrix`, `inputs`, `vars`, `secrets`, `contains`, `toJSON`, `hashFiles`, status functions, and boolean/value `&&`/`||` are guarded. |
| Checkout | Native `actions/checkout` covers self/external repository, path, ref, token, fetch-depth, fetch-tags, persist-credentials, clean, safe-directory, cleanup, step-output refs, and needs/github refs. |
| Native action routing | Target action inventory maps to Rust-native adapters by normalized family name, independent of pinned refs/SHAs. |
| Setup/tool adapters | `jdx/mise-action`, `actions/setup-python`, `dtolnay/rust-toolchain`, `extractions/setup-just`, `baptiste0928/cargo-install`, `mozilla-actions/sccache-action`, `Swatinem/rust-cache`, and `rui314/setup-mold` have target-shaped coverage. |
| Cache/artifacts | Native cache plus upload/download artifact adapters work through Velnor shared workdir storage for same-host same-run target handoff. |
| Docker adapters | Native Docker login, Buildx setup, metadata, build-push, and bake adapters invoke Docker CLI without Node sidecars for target shapes. |
| Pages/renovate/runtime | Target-shaped Pages, Renovate, and GitHub runtime-export behavior has native/adapter coverage. |
| Composite actions | Local and repository composite expansion covers target local actions and target nested repository action usage. |
| Timeline/results | Velnor sends best-effort in-progress job/step records, step feed lines, annotations, telemetry, step results, job outputs, environment URL, and final run-service completion. |
| Target drift audit | `scripts/target_audit.py --check-target-mvp` gates current workflow files, action inventory, triggers, matrices, env, outputs, permissions, needs, conditions, defaults, labels, and unsupported feature drift. |

## Public Fixture Proof

Public repo:

- `https://github.com/donbeave/velnor-actions-fixture`

Purpose:

- give Velnor a small public repository that mimics the target workflow feature
  classes before using the large target repositories
- run equivalent lanes on `ubuntu-latest` and `[self-hosted, velnor-target-mvp]`
- compare normalized artifacts from both lanes after Velnor runs

Fixture coverage:

- Rust workspace with multiple packages
- matrix jobs
- `actions/checkout@v6`
- `dorny/paths-filter@v4`
- `dtolnay/rust-toolchain@stable`
- `extractions/setup-just@v4`
- `actions/cache@v5`
- `actions/upload-artifact@v7`
- `actions/download-artifact@v8`
- local composite actions
- `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`, and `GITHUB_STEP_SUMMARY`
- Docker Buildx setup/build workflow through `docker/setup-buildx-action@v4`
  and `docker/build-push-action@v7`

Evidence captured on 2026-06-01:

- GitHub accepted and activated both fixture workflows: `compat` and `docker`.
- Push run `26762850861` parsed correctly:
  `https://github.com/donbeave/velnor-actions-fixture/actions/runs/26762850861`
- `changes` passed on GitHub-hosted runner, including checkout and
  `dorny/paths-filter`.
- `compat-github (app-a)` and `compat-github (app-b)` passed on
  GitHub-hosted runners, including rust-toolchain, setup-just, cache, command
  files, local composite action, Rust tests, and upload-artifact.
- `scripts/fixture_status.sh` still shows `compat-velnor (app-a)` and
  `compat-velnor (app-b)` queued until a Velnor self-hosted runner with label
  `velnor-target-mvp` is registered.
- `scripts/fixture_audit.py` passes against `donbeave/velnor-actions-fixture@main`,
  checking the current `compat.yml`, `docker.yml`, and local composite action
  metadata expected by the fixture proof.

This fixture is not a replacement for target-repo proof. It is the next live
bridge: first make the queued Velnor fixture jobs pass, collect evidence, then
report readiness for manual target-repository validation. The user/operator,
not the agent, owns the real ChainArgos and Jackin validation run.

Current local environment finding:

- The active agent environment has `DOCKER_HOST=tcp://jk-php3ngrs-thearchitect-dind:2376`.
- `scripts/live_host_doctor.sh` with the default socket requirement fails before
  registration because `/var/run/docker.sock` does not exist on this host.
- The preflight error now calls out the remote `DOCKER_HOST` case explicitly:
  target Docker/Buildx jobs need a local socket mounted into Velnor job
  containers, while `VELNOR_REQUIRE_DOCKER_SOCKET=false` is only appropriate
  for fixture checks that do not need Docker from inside the job container.
- `scripts/fixture_readiness.sh` confirms the same safe stopping point: fixture
  GitHub-hosted jobs are complete, Velnor fixture jobs are queued, fixture audit
  passes, and host readiness fails before any runner registration or workflow
  dispatch.
- `scripts/fixture_report.sh` writes the same non-mutating status/audit/host
  readiness checks to `.velnor-live-evidence/fixture-readiness-report.md` for
  operator handoff.
- `VELNOR_REQUIRE_DOCKER_SOCKET=false scripts/live_host_doctor.sh` reaches the
  bind-mount visibility preflight, then fails because the remote Docker daemon
  cannot see `/Users/donbeave/Projects/velnor-project/velnor/.velnor-work`.
- Velnor preflight still cannot run jobs here because the remote Docker daemon
  cannot see bind-mounted paths from this agent container.
- `scripts/live_host_doctor.sh` reproduces the same expected blocker in this
  environment before any GitHub job can be acquired.
- Direct bind probes against the daemon showed the current workspace,
  `/Users/donbeave/Projects/velnor-project/velnor`, `/tmp`, and `/home/agent`
  are not visible inside containers created by that daemon.
- This blocks live fixture execution in this environment. The live fixture proof
  needs a local Docker socket or a `--work-dir` path mounted into the Docker
  daemon host.
- Velnor now supports `--docker-host-work-dir` for deployments where the runner
  process and Docker daemon see the same work directory at different paths.

## Still Not Proven

| Gap | Why it matters | Proof needed |
| --- | --- | --- |
| Live fixture Velnor run | GitHub-hosted lanes pass, but Velnor has not yet consumed the queued fixture jobs. | Register Velnor to `donbeave/velnor-actions-fixture` with label `velnor-target-mvp`; run queued `compat-velnor` jobs; verify `compare-results` passes. |
| Live GitHub UI run on ChainArgos Rust workflows | Local unit tests cannot prove GitHub accepts the full live protocol and renders logs/status correctly. | User/operator manually runs `ansible.yml`, then `rust.yml`, then Docker/Buildx-heavy workflows through a registered Velnor runner after fixture evidence is green. |
| Live GitHub UI run on `jackin` Linux paths | `jackin` has Linux hosted labels plus artifact/pages/homebrew flows that must work without YAML changes for selected Linux jobs. | User/operator manually registers with target x64 labels and runs `ci.yml`, `construct.yml`, and `docs.yml` Linux paths after fixture evidence is green. |
| GitHub artifact/cache service transport | Current artifact/cache handoff is shared-workdir local storage, good for one-host proof but not multi-runner parity. | Either prove target live jobs run on one host with shared workdir or implement service-backed transport before multi-host validation. |
| MacOS matrix legs | Docker Linux runner cannot truthfully replace `macos-latest`. | Not a Velnor target; keep those legs outside Velnor. |
| Hosted image parity | Velnor uses a Docker image with common tools; it is not a byte-for-byte GitHub-hosted runner image. | Target workflow live runs prove enough parity for current scripts. |
| Broad GitHub Actions compatibility | Not a Phase 0 requirement. | Add only when new target workflows need more features. |

## Next Best Proof Path

1. Run `scripts/target_verify.sh` and `cargo test -q`.
2. Run `scripts/fixture_readiness.sh` on the live Linux host and intended
   workdir. It checks fixture status, fixture feature surface, and live host
   readiness without registering a runner or dispatching workflows.
3. If readiness fails, run `scripts/fixture_report.sh` and share the generated
   Markdown report with the implementation loop.
4. On a host that passes readiness, run `scripts/fixture_smoke.sh`. This
   registers `donbeave/velnor-actions-fixture` with label `velnor-target-mvp`,
   runs the queued fixture jobs through `daemon --once --slots N`, and confirms
   `compare-results` passes.
5. Fix only failures backed by fixture live evidence, sanitized job payloads, or
   target workflow drift.
6. When fixture evidence is green, report that Velnor is ready for manual
   target-repository validation.
7. Stop agent-owned execution there. The agent must not set
   `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`, register Velnor against the real
   target repositories, dispatch their workflows, or migrate either repository.
8. The user/operator explicitly sets `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`
   and runs the ChainArgos target sequence.
9. The user/operator explicitly sets `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`
   and runs the Jackin Linux target sequence.

## Completion Rule

Do not call Phase 0 done because the local verifier passes. Phase 0 is done only
when current target workflows assigned to Velnor complete successfully in GitHub
Actions UI with readable logs, correct step/job conclusions, required job
outputs, cache/artifact behavior needed by the target jobs, and no unsupported
feature drift in the target audit.
