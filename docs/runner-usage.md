# Velnor Runner Usage

Operational reference for running the Velnor runner. Direction and scope live
in [master-plan.md](master-plan.md), [mission.md](mission.md), and
[roadmap.md](roadmap.md); this file is how-to only.

## Production operation (Debian package)

The runner installs and upgrades via apt (repo: `velnor-apt.tailrocks.com`):

```sh
sudo apt-get update && sudo apt-get install velnor-runner   # or upgrade
```

For first-install repository/keyring setup and the maintainer's complete
tag-to-signed-repository publication procedure, follow
[Debian package + apt-native repository](debian-apt-repo.md). Production
servers never install a local release asset.

Configuration (one daemon per target scope):

Production upgrades on Sentry come only from that signed repository. Commit and
push the source, tag the new version, wait for `release-deb.yml` and
`tailrocks/velnor-apt`'s `publish.yml` to pass, verify the signed repository
offers the new version, then run the apt command above. Do not deploy with
`dpkg -i` or install a local `.deb` path with apt.

The package ships the canonical job-image Dockerfile. During configuration,
`postinst` compares the image's OCI version label with the Debian package
version and rebuilds `velnor/job-ubuntu:26.04` before restarting any daemon when
they differ. An apt upgrade therefore cannot leave native adapters paired with
a stale tool image; image-build failure fails the package transaction.

- Default instance: `/etc/velnor/velnor.env` (URL, name, labels, slots,
  work dir) + `/etc/velnor/secrets.env` (0600, `GITHUB_TOKEN=...` — never
  shipped or touched by the package; `postinst` migrates a token out of
  `velnor.env` automatically).
- Additional instances: `/etc/velnor/<name>.env` + `<name>.secrets.env`,
  then `systemctl enable --now velnor-daemon@<name>`.
- Job container resource caps: package units default to
  `VELNOR_JOB_CPUS=4` and `VELNOR_JOB_MEMORY=12g`, appended after workflow
  `container.options` so daemon policy wins on shared warm-runner hosts.
  Set either value empty in the daemon env to disable that cap for a trusted
  scope, or tune per instance.
- Rust compile-cache defaults: every job starts with
  `CARGO_INCREMENTAL=0`, `SCCACHE_CACHE_SIZE=20G`, and
  `SCCACHE_BASEDIRS=/__w:/github/home`. Workflow environment may explicitly
  override the incremental setting and cache size. The path-normalization
  roots are runner-owned and cannot be overridden. Set
  `VELNOR_SCCACHE_CACHE_SIZE` on the daemon to change the default store bound.
- Capacity admission reserves 30 GiB per advertised slot and preserves a 10
  GiB emergency floor. Tune them per host with `VELNOR_JOB_PEAK_BYTES` and
  `VELNOR_EMERGENCY_RESERVE_BYTES`; doctor reports free/reserved bytes, active
  leases, and cache accounting.
- Regenerable class ceilings default to targets 200 GiB, actions cache 50 GiB,
  and artifacts/Cargo/mise 20 GiB each. Override with the corresponding
  `VELNOR_BUDGET_{TARGETS,CACHES,ARTIFACTS,CARGO,MISE}_BYTES` variables.
  The mise class includes repo-scoped `/opt/mise/installs` and the matching
  `/root/.rustup` payload; they are one executable-tool lifetime and budget.
- Optional Rust target persistence: set `VELNOR_CARGO_TARGET_PERSIST=true` in
  the daemon env only for trusted target scopes. Velnor stores targets under
  `_velnor_targets/<trust-scope>/<generation>/<repo>/<workflow>/<job-bucket>`
  so warm state is shared only across matching trust scope, repository,
  workflow, and job classes. After checkout, Velnor reflink/copies a complete
  generation into the job-local workspace `target/`; after the job it publishes
  the completed tree atomically. It never adds a nested `/__w/target` bind
  mount: nested mounts make an ordinary rename between `target/` and another
  workspace directory fail with `EXDEV`. Velnor does not set
  `CARGO_TARGET_DIR`, so paths and same-filesystem semantics remain identical
  to GitHub-hosted execution. Set `VELNOR_TRUST_SCOPE` per
  daemon/pool (`trusted` by default;
  use a distinct value such as `public-forks` for untrusted lanes) before
  enabling target persistence.
- Trust scopes are enforced at runtime. The `trusted` scope keeps the current
  full-capability lane, including the host Docker socket for Docker/buildx
  jobs. Any other `VELNOR_TRUST_SCOPE` refuses jobs when GitHub sends
  user/repository secrets such as `secrets.*`, and job/action containers do not
  receive the host Docker socket. Use separate runner labels and runner groups
  for trusted and untrusted daemons so fork PRs cannot land on a trusted warm
  pool by accident.
- Organization fleets: use `--url https://github.com/<org>` with
  `--pool-name <runner-group>` to resolve the current group id through GitHub.
  Follow the drain, trust-lane, label-continuity, and rollback procedure in
  [org-fleet-migration.md](org-fleet-migration.md).

Units (all shipped by the package):

- `velnor-daemon.service` / `velnor-daemon@<name>.service` — `Type=notify`
  with `WatchdogSec=180`; the daemon reports status via sd_notify and
  **never exits** on registration/credential/network failures (it retries
  forever with backoff and shows the precise problem in `systemctl status`,
  e.g. an unexpanded `${...}` token placeholder).
- `velnor-doctor.timer` / `velnor-doctor@<name>.timer` — every 10 minutes,
  lists the daemon's registered runners on GitHub and **fails loudly when
  none are online** (`velnor-runner doctor --url ... --name ... --slots N`).
  The matching `velnor-doctor.service` / `velnor-doctor@<name>.service` units
  are `Type=oneshot`; seeing them as `loaded inactive dead` in
  `systemctl list-units --type=service --all` is the normal completed state
  after a timer run. Inspect `systemctl list-timers 'velnor-doctor*'` and
  failed units instead of treating inactive one-shot services as stale daemons.

Persistent stores use `/var/cache/velnor/v1/<trust-scope>/...`; durable state,
runtime leases, and logs use `/var/lib/velnor`, `/run/velnor`, and
`/var/log/velnor`. Package units set `VELNOR_STORAGE_ROOT=/var`. Before an
upgrade that adopts the canonical tree, drain every daemon and move each
legacy `/var/lib/velnor*/work/_velnor_*` class into its matching canonical
trust/class path. Velnor reads an existing legacy class only while its
canonical destination is absent, so migration is explicit and reversible.
Use `velnor-runner storage paths` and `storage status` to inspect resolution.
Primary-repository bare mirrors live in the regenerable `git-mirrors` cache
class (`/var/cache/velnor/v1/<trust-scope>/git-mirrors`). Each mirror is keyed
by owner/repository, locked across slots during delta fetch, and never stores a
remote URL or credential. Checkout fetches locally from the refreshed mirror
while preserving the normal checkout trace and falls back to its direct origin
when refresh fails. Cache and artifact tree copies try the filesystem's native
reflink operation first (XFS `FICLONE` on production Linux, `clonefile` on
macOS) and transparently fall back to a byte copy when unsupported.

## Current runner state

The first Rust crate is `velnor-runner`. Velnor targets GitHub's current V2
broker/run-service flow only. The supported setup path is GitHub's public
just-in-time runner configuration API, which returns an `encoded_jit_config`
with `UseV2Flow=true` and `ServerUrlV2`.

Classic runner registration-token setup is not a Velnor product path. Hosted
GitHub can return classic-only settings through that path, and Velnor should not
chase that route or implement classic distributed-task polling as a fallback.
If a setup path does not provide V2 settings, it is unsupported.

Velnor runs supported target jobs in Docker with Rust-native adapters for the
marketplace actions used by the target repositories. The product target is a
daemon that manages multiple internal GitHub runner slots, so one Velnor process
can acquire multiple GitHub jobs and spawn one isolated Docker container per job
concurrently. The daemon does not reuse one GitHub runner identity across
concurrent jobs; each slot owns a separate JIT runner identity and broker
session. The current `run --once` path remains the single-slot
compatibility/proof path.

Product target: the Velnor daemon can run on macOS or Linux, but assigned jobs
run inside Linux Docker containers. Velnor refuses macOS/Darwin runner labels and
does not claim macOS job capability. Running the Velnor daemon inside a Linux
Docker container is also supported, including from Docker Desktop on macOS, as
long as the container can reach the Docker daemon and the daemon can see Velnor's
bind-mounted work directory. Any macOS legs in existing target workflows are
outside Velnor's execution surface.

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --pat "$GITHUB_TOKEN" \
  --labels velnor,hetzner-sentry-ci \
  --replace

cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --pat "$GITHUB_TOKEN" \
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

### Product behavior

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
`--slots 1` it uses the normal config directory. With `--slots N` where `N > 1`,
slot configs live under `<config-dir>/slots/slot-1`, `<config-dir>/slots/slot-2`,
and so on; each slot also receives its own `slot-N` child under any explicit
work, Docker-host work, or job-message dump directory. GitHub sees multiple
runner identities/sessions, while the operator runs one daemon binary with one
concurrency setting. Use `status --slots N` to inspect the same internal slot
configs and `remove --slots N` to unregister and delete them. The daemon
supervises slot tasks as a group and surfaces the first slot panic or
runner-loop error immediately instead of waiting for earlier slots to exit.
`daemon --once` is available for bounded proof runs; each slot exits after one
handled job, while normal daemon mode keeps polling.

## Local target coverage checks

```sh
scripts/target_verify.sh
cargo nextest run --workspace --locked
cargo run -q -p velnor-tools -- target-audit --check-target-mvp /tmp/velnor-jackin /tmp/velnor-chainargos
cargo run -q -p velnor-tools -- target-verify
```

`scripts/target_verify.sh` expects the `jackin` and ChainArgos target checkouts
to have clean `.github` trees and to be current with their configured upstream
branches. Set `VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true` only when intentionally
auditing a local snapshot.

## Live host readiness

```sh
cargo run -q -p velnor-tools -- live-host-doctor-plan
scripts/live_host_doctor.sh
```

## Fixture proof readiness (non-mutating)

```sh
cargo run -q -p velnor-tools -- fixture-readiness
cargo run -q -p velnor-tools -- fixture-report
cargo run -q -p velnor-tools -- fixture-smoke-plan
cargo run -q -p velnor-tools -- fixture-status
cargo run -q -p velnor-tools -- target-smoke-plan --repo owner/repo
scripts/fixture_readiness.sh
```

To write a shareable readiness report without registering a runner or
dispatching a workflow:

```sh
scripts/fixture_report.sh
```

Audit the fixture repository feature surface directly:

```sh
cargo run -q -p velnor-tools -- fixture-audit
```

Repository automation policy: new committed automation should be Rust. Prefer
`velnor-tools` subcommands over adding shell or Python scripts. Existing shell
and Python automation is being migrated incrementally; runner-reference,
fixture-audit, live host doctor planning, fixture smoke planning, and target
smoke planning checks have already moved to `velnor-tools`.

The live proof scripts are Linux-only; they fail before runner registration on
non-Linux hosts, and reject `VELNOR_TARGET_MVP_ARM_LABEL=true` unless the host
is ARM Linux.

## Running Velnor itself from Docker (macOS workstation)

Build the project-owned Ubuntu job image and the Velnor daemon image, then pass
both the container-visible work directory and the Docker-daemon-visible host work
directory:

```sh
docker build -f docker/job-ubuntu.Dockerfile -t velnor/job-ubuntu:26.04 .
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

The canonical job image includes the current architecture-pinned Node.js
runtime (v26.5.0) as a base-runner tool. This matches GitHub-hosted semantics
for ordinary run steps and lets tools such as Oxlint's type-aware plugin invoke
`node` without a workflow-only installation. Runner preflight rejects an image
that lacks Node or any required estate Rust target.

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
    --url https://github.com/tailrocks/velnor-actions-fixture \
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

## Target smoke wrappers

```sh
scripts/chainargos_target_smoke.sh            # ChainArgos Rust target smoke
scripts/chainargos_rust_target_sequence.sh    # staged ChainArgos Rust proof
scripts/jackin_target_smoke.sh                # first Jackin Linux smoke
scripts/jackin_rust_linux_sequence.sh         # staged Jackin Rust/Linux proof
```

Smoke scripts write sanitized job payloads under `.velnor-job-dumps` by default.
They also write live proof evidence under `.velnor-live-evidence` by default
after Velnor consumes jobs and after a watched run completes or fails, including
best-effort runner-label, GitHub job ID/URL, artifact, job-step, bounded log
snapshots from GitHub, bounded local cache/artifact/sccache store snapshots from
Velnor's shared workdir, and sanitized job-message dump file listings.

Evidence/control environment variables:

- `VELNOR_LIVE_EVIDENCE_DIR` — override the evidence path.
- `VELNOR_LIVE_EVIDENCE_LOG_LINES` — log excerpt length (positive integer).
- `VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES` — local store entry count (positive
  integer). Both evidence counts are validated before runner registration.
- If a fixture or target smoke script fails after a GitHub run id is known, it
  writes a best-effort evidence file with phase `failed-before-completion`
  before cleanup.
- `VELNOR_TARGET_WORKFLOW=<workflow.yml>` — dispatch that workflow before Velnor
  waits for target jobs. Values must end in `.yml` or `.yaml`.
- `VELNOR_TARGET_REF=<branch-or-sha>` — dispatch from a specific ref.
- `VELNOR_TARGET_INPUTS=key=value,other=value` — workflow dispatch inputs. Keys
  must match `[A-Za-z_][A-Za-z0-9_-]*`; empty entries and entries without `=` are
  rejected before runner registration.
- `VELNOR_TARGET_JOB_COUNT=<n>` — consume more than one queued job (one internal
  runner slot per requested job).
- `VELNOR_TARGET_WATCH_RUN=true` — wait for the GitHub workflow run to finish
  after the selected Velnor jobs are consumed.
- `VELNOR_IDLE_TIMEOUT_SECONDS=<n>` — per-job wait time (positive integer);
  explicit run IDs must also be positive integers.
- `VELNOR_TARGET_MVP_ARM_LABEL=true` — only on ARM Linux target smoke hosts; the
  live scripts and runner reject the ARM label on non-ARM hosts.
- `VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true` — only for a deliberate
  non-exclusive run; smoke scripts otherwise fail before dispatch if another
  online self-hosted runner can match the proof labels (Phase 0 cache/artifact
  proof assumes one Velnor host).
- `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` — only when intentionally running
  manual validation against `ChainArgos/java-monorepo` or `jackin-project/jackin`.
  **Agents must not set this**, migrate those repositories, dispatch their
  workflows, or register Velnor against them automatically. Agent-owned proof
  stops at the public fixture repository and a clear "ready for manual target
  testing" report; the user/operator performs the real ChainArgos and Jackin
  validation manually and reports findings back.

The remaining Phase 0 proof is live GitHub UI validation on the two target
repositories from a Linux host whose Docker daemon can see Velnor's bind-mounted
work directory.

## Public fixture repository

Public fixture: <https://github.com/tailrocks/velnor-actions-fixture>

A small public repository used to prove Velnor against real GitHub Actions
scheduling before running the large target repositories. Paired workflow lanes:

- GitHub-hosted runner lane: `runs-on: ubuntu-latest`
- Velnor lane: `runs-on: [self-hosted, velnor-target-mvp]`

The fixture proof should normally use a fresh run. `scripts/fixture_smoke.sh`
dispatches `compat.yml` by default, then Velnor consumes the queued fixture jobs
and the compare job verifies matching outputs. Inspect the latest fixture run or
an explicit `VELNOR_FIXTURE_RUN_ID` with:

```sh
scripts/fixture_status.sh
```

On a Linux host with a local Docker socket and `GITHUB_TOKEN` set:

```sh
scripts/fixture_readiness.sh
scripts/fixture_smoke.sh
```

The script runs one bounded Velnor daemon with two internal slots by default
because the fixture compat workflow has two Velnor matrix jobs. Override with
`VELNOR_FIXTURE_JOB_COUNT` for a different fixture shape. Set
`VELNOR_FIXTURE_RUN_ID=<run-id>` to consume a specific existing run; otherwise
the script dispatches a fresh run. Set `VELNOR_FIXTURE_REF=<branch-or-sha>` and
`VELNOR_FIXTURE_INPUTS=key=value,other=value` when dispatching fixture workflows
from a non-default ref or with workflow inputs (same `key=value` rules as target
smoke scripts). Fixture workflow values are required and must end in `.yml` or
`.yaml`.

If the Docker daemon sees the work directory at a different path than the runner
process, set `VELNOR_DOCKER_HOST_WORK_DIR` to that daemon-visible path. For a
remote Docker daemon without a local `/var/run/docker.sock`, set
`VELNOR_REQUIRE_DOCKER_SOCKET=false` for the fixture smoke run.

For target jobs, Velnor runs the job in a Docker container and mounts
`/var/run/docker.sock` into that container when the socket is present. The
default `velnor/job-ubuntu:26.04` image is built from official `ubuntu:26.04` and
contains the Docker CLI and Buildx plugin, so workflow steps inside the job
container can run `docker`/`docker buildx` without relying on host binary mounts.
Service containers share the per-job Docker network with GitHub-style aliases.

To force reuse of an existing run, set `VELNOR_FIXTURE_DISPATCH=false` together
with `VELNOR_FIXTURE_RUN_ID=<run-id>`. The smoke script removes the temporary
fixture runner on exit by default; set `VELNOR_FIXTURE_CLEANUP_RUNNER=false` to
keep it registered for debugging.
