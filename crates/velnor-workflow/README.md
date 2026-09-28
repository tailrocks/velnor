# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --plain
velnor-workflow REPOSITORY --runners both --plain
velnor-workflow promote --rev HEAD
velnor-workflow plan --config .github/ci/project.toml
velnor-workflow run --config .github/ci/project.toml --scope affected
velnor-workflow test-crates --config .github/ci/project.toml
velnor-workflow policy --workflow-root . \
  --head-sha <pr-head-sha> --base-sha <base-sha> \
  --base-revision <sha of the validator the base branch pins> \
  --ruleset-contexts ci-required,DCO
velnor-workflow release verify-tag
```

`policy` is the trust validator the base branch's `ci-policy.yml` runs under
`pull_request_target` against a pull request's tree. It never compares the
tree with its own rendering: it reads the generator commit the tree declares
(`[generator] revision` in `.github-gen/velnor-workflow.toml`, the D19 pin),
proves the pin is reachable from the head and does not regress the base
branch's validator, regenerates the tree with the generator built at that pin
and requires a byte-identical result, and evaluates its own semantic rules
(only the entrypoint on `pull_request_target`; every self-hosted job gated;
every action SHA-pinned; the entrypoint workflow on `contents: read` and its
policy job on `contents: read` or the exact read-only set `actions: read`,
`contents: read`, and `pull-requests: read`, with no secrets;
the ruleset's required contexts emitted). Every rule prints `PASS`/`FAIL`
with a one-line reason. Bump the pin with `velnor-workflow promote --rev HEAD`
after the last generator change: it verifies the running binary renders with
the pin's own source closure, then stamps the pin and regenerates the whole
tree in a single commit; `--check` verifies the pinned generator renders the
tree.

During candidate-renderer bootstrap, the exact legacy D19 pin
`e988d793937c044275e4086b00856444c60f6ab8` keeps policy workflow output at
the established byte shape. Promoting `[generator] revision` to the newer
renderer is the typed opt-in: it enables the hardened candidate-artifact
handoff and read-only API permissions without adding a config key that the
legacy validator would reject. The newer pin keeps that mode enabled.

Runtime commands are derived from scanned capabilities, not from config-supplied
shell arrays. GitHub-hosted execution is the automatic and omitted-dispatch
default; Velnor runs only when dispatch selects `velnor` or `both`. The binary
owns selection, dependency ordering, policy, release validation, and every
`.github/workflows/*.{yml,yaml}` file it emits. Foreign workflow bodies are
never imported. The ownership sidecar stays at
`.github/ci/.github-actions-generator-state`.

Runner capabilities come from concrete scan evidence. Generated Dockerfile
builds use Docker Buildx, so Docker units require Docker plus the runner's
bundled Buildx/Compose capability; this does not infer Docker Compose services
or Testcontainers use. A detected service adds Docker and service readiness.
Rust units require Docker/Testcontainers when they directly depend on the
`testcontainers` or `testcontainers-modules` crate. Rust, Gradle, Node, and Bun
unit kinds alone do not imply a Docker requirement.

Repositories may pin their generation inputs in an optional
`.github-gen/velnor-workflow.toml` (`schema = 1`): the repository slug, runner
and branch overrides, scan excludes, policy switches, and `[[declare]]` render
primitives. Generation is a function of the scanned repository shape, this
config, and the generator revision (`GENERATOR_REVISION`); all three are
recorded in the ownership sidecar (`schema = 2`) and `--check` fails when they
no longer match the current run, even if every generated file is unchanged.
When `[workflow].files` replaces the default list, it must still include
`ci-policy.yml`, the base-owned policy entrypoint. `[[static_files]]` cannot
claim `ci-policy.yml`, `ci-pr.yml`, or the owner-only `ci-runtime-products.yml`.
It also cannot alias any output rendered by the generator, including paths
that differ only by case or a supported filesystem alias. Non-colliding static
files remain available. Policy audits every checked-in workflow; unknown
workflow files fail generated-tree ownership checks and must be migrated into
generator inputs before they can pass.

The schema-1 Swift scanner fails closed for executable Swift products and
recognized XcodeGen specs because it cannot emit their complete phase and
selection contracts; use schema 2 for those Apple surfaces.

## Typed package-release verification hooks

Schema-2 `package-release` declarations may name repository-owned mise tasks in
`verify_tasks`. Velnor validates each name against the scanned `mise.toml` and
renders the task after producer creation, after the downloaded handoff is
re-verified, and after the immutable release is downloaded. The task runs from
the exact source checkout with `VELNOR_VERIFIED_PACKAGE_DIR` pointing at the
bytes under verification; Velnor does not interpret the task's package
semantics.

```toml
[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["build-package"]
verify_tasks = ["verify-package"]
publication_lock_branch = "package-release-lock"
```

`verify_tasks` is a list of plain mise task names, not shell commands or an
arbitrary command array. Omit it when a package has no repository-owned
semantic verification beyond Velnor's generic manifest, checksum, and
provenance checks.

`pre_publish_tasks` is an optional list of distinct plain mise task names for
one-time migration work that runs exactly once after the verified source
checkout and publication lock, but before immutable release mutation. These
tasks receive the publisher's GitHub token and source-checkout context; they
must write any remote-mutation marker to `GITHUB_ENV` before mutation and must
not overlap `verify_tasks`. `publication_lock_branch` is a required,
repository-specific lock namespace; do not reuse it across independent
publication lanes.

The rolling release is current-contract-only: an existing public release must
match the declared manifest, identity, asset digests, tag, source, and version
contract. An interrupted draft is discarded only after that same typed contract
and release/tag ownership are re-read; missing or mixed state is rejected before
publication mutation. Migration of an older consumer release belongs in that
consumer's release automation and must use `pre_publish_tasks` with a durable
recovery contract.

## `[renovate]` — self-hosted dependency updates

Scan evidence alone (`renovate.json`, `renovate.json5`, or `.github/renovate.json*`)
records `renovate-configuration` but does not emit workflows. A repository opts
in explicitly:

```toml
[workflow]
runners = "velnor"
velnor_labels = ["self-hosted", "example-lane"]
velnor_trusted_label = "example-trusted"
velnor_trusted_runner_available = true
files = [..., "renovate.yml", "renovate-validate.yml"]

[renovate]
enabled = true
reason = "Repository-local Renovate on trusted Velnor runners."
# schedule = "0 6 * * *"   # optional; default daily at 06:00 UTC
# token = "GH_RENOVATE_TOKEN" # optional; default GH_RENOVATE_TOKEN
# validate = true           # optional; default true — emits renovate-validate.yml
# cache = true              # optional; default true — repository-scoped actions cache

[[declare]]
primitive = "renovate"
file = "renovate.yml"

[[declare]]
primitive = "renovate-validate"
file = "renovate-validate.yml"
```

Generated `renovate.yml` runs on trusted Velnor runners only (`schedule` and
default-branch `workflow_dispatch`), uses `secrets.GH_RENOVATE_TOKEN` by default,
pins `renovatebot/github-action` and Renovate OSS `44.93.6`, and keys the
repository cache under `velnor-renovate-${{ github.repository }}-`. The
`renovate-validate.yml` job validates the config with
`ghcr.io/renovatebot/renovate:44.93.6` on the hosted runner. Renovate settings
are generation-time only and are not written into `.github/ci/project.toml`.
The scan reads the git index (tracked files only), so untracked CI runtime
artifacts, scratch files, and linked-worktree `.git` files never enter the
recorded scan input. A sidecar written by an older schema is never parsed:
rerun generate on a byte-matching tree to move it to schema 2.

## Hosted workflow runtime product

Generated GitHub-hosted jobs acquire a prebuilt runtime through the composite
action. For a consuming repository, pin `uses` to the full commit SHA of the
action and set `rev` to the full commit SHA of the generator source whose
closure identifies the runtime product. The values can differ: `uses` selects
the action code, while `rev` selects the generator revision. The action never
compiles Velnor or falls back to `cargo install`:

```yaml
- name: Set up Velnor workflow runtime
  if: ${{ runner.environment == 'github-hosted' }}
  uses: tailrocks/velnor/.github/actions/setup-velnor-workflow@<action-commit-SHA>
  with:
    rev: <generator-commit-SHA>
```

For workflows generated inside the Velnor owner repository, `uses` points to the
local action path `./.github/actions/setup-velnor-workflow`; `rev` still
resolves to the full generator commit SHA. The trusted `ci-policy.yml` job
checks out the setup action from the base commit into `policy-setup-action/`
and uses `./policy-setup-action/.github/actions/setup-velnor-workflow`. Its
trusted workflow validates and resolves the full `rev` before setup, so a pull
request cannot supply the action code used to validate itself.

The action resolves the source closure from the checkout when available, or
from the GitHub trees API. On a cache miss, it downloads the platform binary
and manifest from the immutable
`velnor-workflow-runtime-v1-<closure-prefix>` release in the Velnor owner
repository and verifies attestations for the downloaded binary and
manifest against the mainline runtime-product publisher. The tag is only a
locator. It checks the manifest's closure and platform, recomputes the binary
SHA-256, then checks the binary's reported closure and revision. Missing
products, unverifiable attestations, and mismatches fail closed; consumers
never build the runtime.

The Actions cache is only an acceleration, keyed by runner OS, architecture,
and source closure. On a cache hit, the action revalidates the manifest,
binary digest, and binary-reported closure and revision. Attestations for the
downloaded binary and manifest are verified on download. A cache miss downloads
the published release product; it does not compile it.

For a `pull_request_target` run, GitHub executes the base branch's trusted
`ci-policy.yml` and its referenced action code. A candidate PR cannot change
the validator used to judge itself. If an action or closure-protocol change
needs a newer resolver, the trusted base workflow must first advance through
the repository maintainers' approved bootstrap; consumer pin adoption must
wait until that base update and the immutable product for the target closure
are available.

## Scheduled-check token capabilities

A scheduled check that reads GitHub Actions history may request the narrow
job-level capability below:

```toml
[[check_profile]]
id = "ci-evidence"
tasks = ["ci-evidence"]

[check_profile.permissions]
actions = "read"
```

The generated job retains `contents: read`; no other scheduled-check token
scope or write level is accepted. A profile requesting this capability cannot
share a file with a `pull_request` trigger, because its task code would run
with repository Actions-history access on contributor-controlled input. The
maintenance workflow keeps cache mutation permissions job-scoped as well:
only its cache-deletion jobs receive `actions: write`.
