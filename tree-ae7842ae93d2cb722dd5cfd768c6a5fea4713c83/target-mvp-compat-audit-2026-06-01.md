# Target MVP Compatibility Audit

This is the Phase 0 contract. Velnor should first match the exact GitHub Actions behavior used by these two repositories, not the whole GitHub runner feature set.

Refreshed target commits on 2026-06-01:

- `jackin-project/jackin`: `52a457b689940e05ed65015d2a82ff0e22577d2e`
- `ChainArgos/java-monorepo`: `56491ec5b17702186506217452b58bcf57572079`

Refresh command:

```bash
python3 scripts/target_audit.py /tmp/velnor-targets/jackin /tmp/velnor-targets/java-monorepo
```

The helper reports workflow files, action metadata, exact `uses:` inventory,
explicit shells, triggers, workflow/job env, job `if`, `needs`, strategy,
outputs, permissions, concurrency, `runs-on`, defaults, environments, job
timeouts, job containers, services, local actions, reusable workflows, and
`continue-on-error` shapes. It accepts either repository roots or their `.github`
directories.

## Scope Rule

If a feature, action option, expression form, shell, service shape, or checkout mode is not used by these `.github` trees, it is not Phase 0 work unless live target execution proves GitHub sends it in the job payload anyway. This is a capability boundary, not runtime workflow hardcoding: Velnor should implement reusable feature classes and Rust-native action adapters that happen to cover these target workflows first.

The MVP is successful when the target workflows can run on a Velnor self-hosted runner with the same observable behavior as GitHub-hosted/self-hosted Actions for these repos: same step ordering, same conditions, same outputs/env propagation, same cache/artifact/runtime env, same Docker/buildx behavior, and same success/failure status in the GitHub UI.

## Workflow Files

`jackin-project/jackin`:

- `.github/workflows/ci.yml`
- `.github/workflows/construct.yml`
- `.github/workflows/docs.yml`
- `.github/workflows/preview.yml`
- `.github/workflows/release.yml`
- `.github/workflows/renovate.yml`
- `.github/workflows/renovate-validate.yml`
- `.github/actions/aggregate-needs/action.yml`
- `.github/actions/check-deployed-docs/action.yml`

`ChainArgos/java-monorepo`:

- `.github/workflows/ansible.yml`
- `.github/workflows/kestra-build-image.yml`
- `.github/workflows/kestra-build-publish.yml`
- `.github/workflows/renovate.yml`
- `.github/workflows/rust.yml`
- `.github/workflows/rust-docker.yml`
- `.github/workflows/rust-docker-build.yml`

## Runner Labels

Observed labels:

- `hetzner-sentry-ci`
- `ubuntu-latest`
- `ubuntu-24.04`
- `${{ matrix.runner }}` resolving to `ubuntu-24.04` and `ubuntu-24.04-arm`
- `${{ matrix.os }}` resolving to Ubuntu and macOS values in release matrices

Phase 0 target:

- `ChainArgos/java-monorepo` is the first live target because all jobs use `hetzner-sentry-ci`.
- Linux `jackin` jobs need Velnor registered with compatible Linux labels such as `ubuntu-latest`, `ubuntu-24.04`, and optionally `ubuntu-24.04-arm`.
- macOS `jackin` matrix legs are not Phase 0 Docker runner work unless they are retargeted or skipped.

## Action Inventory

Exact `uses:` references currently present:

- `actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd`
- `actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae`
- `actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a`
- `actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c`
- `actions/upload-pages-artifact@fc324d3547104276b827a68afc52ff2a11cc49c9`
- `actions/deploy-pages@cd2ce8fcbc39b97be8ca5fce6e763baed58fa128`
- `actions/setup-python@a309ff8b426b58ec0e2a45f0f869d46889d02405`
- `dorny/paths-filter@fbd0ab8f3e69293af611ebaee6363fc25e6d187d`
- `jdx/mise-action@1648a7812b9aeae629881980618f079932869151`
- `mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696`
- `rui314/setup-mold@9c9c13bf4c3f1adef0cc596abc155580bcb04444`
- `extractions/setup-just@53165ef7e734c5c07cb06b3c8e7b647c5aa16db3`
- `dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8`
- `baptiste0928/cargo-install@f204293d9709061b7bc1756fec3ec4e2cd57dec0`
- `Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32`
- `crazy-max/ghaction-github-runtime@04d248b84655b509d8c44dc1d6f990c879747487`
- `renovatebot/github-action@693b9ef15eec82123529a37c782242f091365961`
- `docker/setup-buildx-action@d7f5e7f509e45cec5c76c4d5afdd7de93d0b3df5`
- `docker/login-action@650006c6eb7dba73a995cc03b0b2d7f5ca915bee`
- `docker/metadata-action@80c7e94dd9b9319bd5eb7a0e0fe9291e23a2a2e9`
- `docker/build-push-action@f9f3042f7e2789586610d6e8b85c8f03e5195baf`
- `docker/bake-action@6614cfa25eff9a0b2b2697efb0b6159e7680d584`
- local `./.github/actions/aggregate-needs`
- local `./.github/actions/check-deployed-docs`
- reusable workflow reference `./.github/workflows/kestra-build-image.yml`

Reusable workflow references are expanded by GitHub before a runner receives a job. Velnor Phase 0 does not need to parse workflow-level `jobs.<id>.uses`; it needs to execute the expanded job payload.

## Exact Configuration Shapes To Support

### Workflow Routing And Server-Side Features

The target workflows use these triggers:

- `pull_request`, `push`, and `workflow_dispatch`
- `schedule`
- `merge_group`
- `workflow_run` in `jackin` preview publishing
- `workflow_call` for `java-monorepo` reusable Docker image build workflows

These are GitHub orchestration concerns before the runner receives an expanded
job. Velnor Phase 0 does not need to parse event triggers or expand reusable
workflow jobs locally. It needs to execute the `AgentJobRequestMessage` payloads
GitHub sends after trigger evaluation, matrix expansion, and reusable workflow
expansion.

Target workflow-level `concurrency` groups:

- PR concurrency with `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`
  for `ci`, `construct`, `docs`, `renovate-validate`, `rust-docker`, and `rust`
- singleton groups for `renovate`, `release`, and Homebrew preview publishing

Concurrency is also server-side. Velnor only needs to react correctly if GitHub
sends a broker cancellation message for a job that loses the concurrency race.

Target permissions are limited to:

- workflow-level `contents`
- workflow-level `pull-requests` for `java-monorepo` Rust path filters
- job-level `contents`
- job-level `id-token` and `pages` for the `jackin` docs deploy job

Permissions are enforced by the `GITHUB_TOKEN` and OIDC/runtime endpoints GitHub
provides in the acquired job payload. Velnor must pass those tokens and endpoint
environment variables through to actions; it should not implement an independent
permission engine in Phase 0.

Target job `environment` usage is limited to the `jackin` docs deploy job:

- `name: github-pages`
- `url: ${{ steps.deployment.outputs.page_url }}`

The runner-side requirement is already concrete: evaluate `environment.url` from
final step outputs and include the value in the run-service completion payload.

### Shell And Run Defaults

- Only `bash` is explicitly configured.
- `java-monorepo` uses `defaults.run.shell: bash` and `defaults.run.working-directory` for `ansible.yml` and `kestra-build-image.yml`.
- Step-level `working-directory` appears in `jackin` docs/release/preview flows.
- Command files used: `GITHUB_OUTPUT`, `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_STATE`, and `GITHUB_STEP_SUMMARY`.

### Checkout

Required `actions/checkout` inputs:

- default self checkout
- `fetch-depth: 0` for path-filter workflows
- external repository checkout in `jackin` Homebrew flows: `repository: jackin-project/homebrew-tap`, `path: homebrew-tap`, explicit token/ref shapes

Not required by current target workflows:

- `submodules`
- `sparse-checkout`
- `lfs`

### Expressions And Conditions

Target expressions include:

- status functions: `always()`, `success()`, `failure()`, `cancelled()`
- boolean operators: `&&`, `||`, unary `!`
- equality/inequality with GitHub's case-insensitive string comparisons
- `contains(...)`
- `toJSON(needs)`
- `hashFiles(...)`, including multiple patterns
- contexts: `github`, `github.event.*`, `runner`, `env`, `secrets`, `inputs`, `steps`, `needs`, `matrix`, `vars`
- output fallbacks such as `steps.dispatch.outputs.rust || steps.filter.outputs.rust`
- dynamic values inside multiline Docker action inputs

Expression timing is part of the target contract:

- job-start contexts such as `github.*`, `github.event.*`, `runner.*`,
  `matrix.*`, `needs.*`, `inputs.*`, `vars.*`, and `secrets.*` may be resolved
  during planning when the value is needed to download actions or run eager
  checkout
- `steps.<id>.outputs.<name>` must not be resolved during planning, because the
  producing step may not have run yet
- target examples requiring runtime step-output resolution include `jackin`
  preview checkout `ref: ${{ steps.source.outputs.sha }}`, docs deploy
  `environment.url: ${{ steps.deployment.outputs.page_url }}`, docs sitemap
  `PAGE_URL: ${{ steps.deployment.outputs.page_url }}`, local
  `check-deployed-docs` input `sitemap-url: ${{ steps.sitemap.outputs.url }}`,
  and Docker action inputs such as `steps.meta.outputs.tags` and
  `steps.cache.outputs.from`
- repository actions and Docker actions should carry unresolved step-output
  expressions into execution, where Velnor resolves them from accumulated step
  state before setting `INPUT_*` env

### Path Filters

`dorny/paths-filter` is used with multiline `filters:` blocks for:

- Rust source/package paths
- Docker image package paths
- docs paths
- construct image paths

Velnor must provide checkout history/event context/token/runtime env so the action can compute outputs that gate downstream jobs.

### Setup And Tool Actions

Target configurations:

- `jdx/mise-action` with `install_args`, `github_token`, and `working_directory`
- `actions/setup-python` with `python-version`
- `dtolnay/rust-toolchain` stable action
- `extractions/setup-just`
- `baptiste0928/cargo-install` with `crate` and `locked`
- `rui314/setup-mold`
- `mozilla-actions/sccache-action` with `continue-on-error: true`, `SCCACHE_GHA_ENABLED=true`, post hook, and `steps.sccache.outcome == 'success'` gating

The shared `/github/home`, `GITHUB_PATH`, `RUNNER_TOOL_CACHE`, and `AGENT_TOOLSDIRECTORY` behavior is part of the target contract because setup actions install tools for later script steps.

### Cache And Artifact Actions

Target cache shapes:

- `actions/cache` with multiline `path`, `key`, and `restore-keys`
- `hashFiles(...)` in cache keys
- `Swatinem/rust-cache` with `cache-directories`, `shared-key`, and `cache-on-failure: true`
- BuildKit GitHub Actions cache env for direct `docker buildx` commands and Docker actions

Target artifact shapes:

- `actions/upload-artifact` with `name`, `path`, `if-no-files-found`, and `retention-days`
- `actions/download-artifact` with `pattern`, `path`, and `merge-multiple: true`
- `actions/upload-pages-artifact` wrapping upload-artifact behavior
- `actions/deploy-pages` output `page_url` and OIDC/runtime values

### Docker And Buildx

Target Docker configurations:

- Docker socket available from inside job container and action sidecars
- host Docker CLI and Buildx plugin available in job/action containers
- `docker/setup-buildx-action` with `name`, `driver: docker-container`, and `cleanup: false`
- `docker/login-action` with Docker Hub username/password secrets
- `docker/metadata-action` with multiline `images` and `tags`, including `enable=${{ ... }}`
- `docker/build-push-action` with `context`, `file`, `platforms`, `push`, `tags`, `labels`, `cache-from`, and `cache-to`
- `docker/bake-action` with `files`, newline-separated `targets`, multiline `set`, and env `PUSH`, `SHA`, `PR_NUMBER`
- direct shell `docker buildx build`, `docker buildx ls`, and `docker buildx imagetools --help`

### Local Composite Actions

`aggregate-needs`:

- receives `needs-json: ${{ toJSON(needs) }}`
- receives `workflow-label`
- runs bash and fails when any needed job result is `failure` or `cancelled`

`check-deployed-docs`:

- validates URL inputs in bash
- nests `jdx/mise-action`
- uses composite inputs in scripts/env
- reads `github.workspace`
- runs lychee with remap arguments

### Renovate

`renovatebot/github-action` is used with:

- token input
- repository context
- Renovate env values
- Docker socket/CLI, because Renovate action shells out to Docker

## Explicit Non-Targets

Current target scan found no usage of:

- workflow `services:`
- job `container:`
- job-level `concurrency:`
- `timeout-minutes`
- non-bash explicit shells
- `docker://` direct action references in workflows
- checkout `submodules`
- checkout `sparse-checkout`
- checkout `lfs`

These should stay out of Phase 0 unless a target workflow starts using them or GitHub sends them in an expanded job payload.

## Current Evidence

After refreshing both target repositories, these Velnor checks pass against the current `.github` trees and cached action metadata:

- `scripts/target_verify.sh`
- `cargo test -q target_workflow_repository_actions_plan_from_cached_metadata`
- `cargo test -q fetched_target_workflow_actions_have_metadata`
- `cargo test -q target_workflow_expressions_use_supported_subset`
- `cargo test -q cached_target_action_metadata_expressions_use_supported_subset`
- `cargo test -q target_docker_action_inputs_match_current_workflows`

These prove the current target action inventory, target-local composites, expression subset, and exact Docker/buildx action input shapes still fit the runner's planned execution model. They do not prove live completion in GitHub UI.

## Remaining Target MVP Proof

The remaining proof must be live, not just unit-level:

Live runbook: `docs/target-live-runbook.md`.

1. Register Velnor with labels needed by `java-monorepo` first: `hetzner-sentry-ci`. The CLI `--target-mvp-labels` preset now adds this plus the x64 Linux Jackin labels.
2. Run one small `java-monorepo` workflow through GitHub UI on Velnor.
3. Run Docker-heavy `rust-docker.yml` or `rust-docker-build.yml` on a host where Docker bind mounts are visible to the daemon.
4. Add labels or temporary retargeting for Linux `jackin` jobs. `--target-mvp-labels` covers `ubuntu-latest` and `ubuntu-24.04`; use `--target-mvp-arm-label` only on an ARM Linux runner; macOS labels remain excluded.
5. Run `jackin` Linux CI/construct/docs paths.
6. Verify GitHub UI logs, annotations, cache/artifact behavior, job outputs, required aggregator jobs, and final success/failure statuses match the existing GitHub Actions experience.
