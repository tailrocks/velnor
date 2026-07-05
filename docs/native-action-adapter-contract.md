# Native Action Adapter Contract

Status: Phase 0 direction

Velnor keeps GitHub Actions YAML as the compatibility input, but it must not treat
marketplace action source code as the implementation. The runner accepts the same
`uses:` syntax and the same `with:` inputs used by the target repositories, then
routes supported actions to Rust-native adapters.

This keeps the migration path close to GitHub Actions while preserving the long
term Velnor direction: deterministic, typed, Rust-owned execution instead of
running arbitrary JavaScript or TypeScript action bundles.

## Boundary

GitHub still owns these pieces when Velnor is registered as a self-hosted runner:

- event trigger matching
- workflow YAML parsing
- matrix expansion
- reusable workflow expansion
- job DAG scheduling
- permission/token creation
- job message delivery

Velnor owns runner-side execution:

- Docker job isolation
- script step execution
- expression resolution needed at runner execution time
- command files and workflow commands
- log/timeline/result upload
- native behavior for supported `uses:` actions

The runtime must never hardcode a workflow file, job id, or step id from
`jackin` or `java-monorepo`. Target-specific snapshots in
`velnor-tools target-audit` are only drift alarms: they tell us when the target
repos start using a new feature class that Velnor does not support yet.

## Adapter Shape

A native adapter is selected by action family, not by workflow location or the
literal `@ref` in YAML:

```text
uses: actions/cache@<sha>
with:
  path: ...
  key: ...
  restore-keys: ...

        |
        v

NativeAction::Cache {
  path,
  key,
  restore_keys,
  runtime_endpoint,
  runtime_token,
}
```

The `@ref` portion is accepted for GitHub YAML compatibility, but Velnor does
not execute that pinned marketplace implementation. For supported action
families, Velnor ignores the pinned SHA/tag and runs its internal Rust adapter.
Adapters should track the latest behavior Velnor intentionally supports for the
target repositories; old marketplace versions are not a compatibility goal.
Known native action families must not require downloaded marketplace metadata
before execution. GitHub still sends the ref in the job message, and Velnor may
keep it for diagnostics, but the implementation is selected from the repository
family such as `actions/cache` or `docker/login-action`. Downloaded `action.yml`
metadata is only required for non-native actions and for composite expansion that
has not been replaced by a native adapter.

The adapter receives already-resolved runner contexts where GitHub would resolve
them for that step:

- `github`, `runner`, `env`, `matrix`, `needs`, `inputs`, `vars`, `secrets`
- previous `steps.<id>.outputs.*`
- command-file state from earlier steps
- GitHub runtime endpoints and tokens from the job message

The adapter returns the same observable step result GitHub users expect:

- exit status
- stdout/stderr log lines
- outputs
- env/path/state/summary mutations
- annotations/masks/groups/debug lines
- post-step work, if the action has cleanup behavior

## Phase 0 Adapter Inventory

These adapters are required because the two target repositories use them now.
The implementation should cover only the input shapes observed in those repos
until a new target workflow needs more.

| Action family | Native adapter | Required behavior |
| --- | --- | --- |
| `actions/checkout` | `Checkout` | self checkout, external repo checkout, `path`, `ref`, `token`, `fetch-depth` |
| `actions/cache` | `Cache` | restore/save paths, key, restore keys with newest prefix match, `hashFiles(...)` keys, latest `cache-hit` output semantics (`true` exact hit, `false` prefix hit, empty miss), `fail-on-cache-miss`, `lookup-only` without restoring paths, shared workdir cache storage |
| `actions/upload-artifact` | `UploadArtifact` | `name`, `path` including target glob patterns, `if-no-files-found`, `include-hidden-files` defaulting to false like the latest action, `overwrite` defaulting to false with duplicate-name failure, `retention-days`, target outputs, deterministic per-run/per-name artifact id, run-scoped workdir storage, Results Service upload required for cross-host handoff |
| `actions/download-artifact` | `DownloadArtifact` | `name`, `pattern`, all-artifacts mode, `path`, container-visible `download-path` output, `merge-multiple`, latest directory layout semantics, downloaded directory/file permissions normalized to `755`/`644`, same-run cross-job handoff on one Velnor host |
| `actions/upload-pages-artifact` | `UploadPagesArtifact` | package pages directory and expose artifact handoff |
| `actions/deploy-pages` | `DeployPages` | pages artifact name and deployment output `page_url` |
| `dorny/paths-filter` | `PathsFilter` | evaluate target multiline filters for push, PR, workflow dispatch |
| `jdx/mise-action` | `Mise` | install mise when missing, install requested tools, use shared home, update `GITHUB_PATH` |
| `mozilla-actions/sccache-action` | `Sccache` | configure env/path/cache, fail honestly when `sccache` is unavailable so target `continue-on-error` gates work |
| `rui314/setup-mold` | `SetupMold` | install/link mold for later Rust builds |
| `extractions/setup-just` | `SetupJust` | install just or reuse image-provided just, update cargo bin path |
| `Swatinem/rust-cache` | `RustCache` | restore/save cache dirs, `shared-key`, `cache-on-failure`, shared workdir cache storage |
| `crazy-max/ghaction-github-runtime` | `GitHubRuntimeExport` | export runtime/cache/results env values |
| `renovatebot/github-action` | `Renovate` | run Renovate with target env/token/Docker access |
| `docker/setup-buildx-action` | `DockerSetupBuildx` | inspect/reuse or create/select builder and honor target inputs |
| `docker/login-action` | `DockerLogin` | registry login from resolved credentials through `--password-stdin` |
| `docker/metadata-action` | `DockerMetadata` | compute target tags/labels outputs |
| `docker/build-push-action` | `DockerBuildPush` | invoke Buildx for target context/tag/cache/push shapes, with `push` and `load` treated as separate latest-action inputs, and pass resolved job env to the Buildx process for GHA cache endpoints |
| `docker/bake-action` | `DockerBake` | invoke Buildx Bake for target file/target/push/cache shapes and pass resolved job/action env such as `PUSH`, `SHA`, and `PR_NUMBER` into the Bake process |

Artifact transport decision: Results Service upload is required, not
best-effort, for `actions/upload-artifact`. The fixture is the contract here:
`compat.yml`, `docker.yml`, and `multi-arch.yml` upload artifacts from the lane
matrix jobs, including the Velnor lane, then download them from Ubuntu-hosted
fan-in/compare jobs. A failed remote upload would make Velnor report success
while the downstream GitHub-hosted job cannot find the artifact, so the upload
step must fail after preserving the local `_velnor_artifacts` copy.

Local composite actions remain first-class workflow code. Velnor should parse
their metadata and expand their nested `run` and `uses` steps, but nested
marketplace `uses` steps still route through this same native adapter registry.

## Not Supported By Phase 0

These are intentionally outside the first target contract:

- arbitrary marketplace JavaScript execution
- arbitrary marketplace Docker action execution
- Node runtime installation for action bundles
- broad `actions/checkout` features such as submodules, sparse checkout, or LFS
- service containers unless a target workflow starts using them
- job containers from workflow YAML unless a target workflow starts using them
- macOS runner replacement

If a target repo adds one of these, the audit should fail first. Then Velnor
should add the feature as a reusable capability, not as a workflow-specific case.
