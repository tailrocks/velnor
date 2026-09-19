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
every action SHA-pinned; the entrypoint on `contents: read` with no secrets;
the ruleset's required contexts emitted). Every rule prints `PASS`/`FAIL`
with a one-line reason. Bump the pin with `velnor-workflow promote --rev HEAD`
after the last generator change: it verifies the running binary renders with
the pin's own source closure, then stamps the pin and regenerates the whole
tree in a single commit; `--check` verifies the pinned generator renders the
tree.

Runtime commands are derived from scanned capabilities, not from config-supplied
shell arrays. GitHub-hosted execution is the automatic and omitted-dispatch
default; Velnor runs only when dispatch selects `velnor` or `both`. The binary
owns selection, dependency ordering, policy, release validation, and every
`.github/workflows/*.{yml,yaml}` file it emits. Foreign workflow bodies are
never imported. The ownership sidecar stays at
`.github/ci/.github-actions-generator-state`.

Repositories may pin their generation inputs in an optional
`.github-gen/velnor-workflow.toml` (`schema = 1`): the repository slug, runner
and branch overrides, scan excludes, policy switches, and `[[declare]]` render
primitives. Generation is a function of the scanned repository shape, this
config, and the generator revision (`GENERATOR_REVISION`); all three are
recorded in the ownership sidecar (`schema = 2`) and `--check` fails when they
no longer match the current run, even if every generated file is unchanged.

## `package-release` — configured release lanes

`[[declare]] primitive = "package-release"` keeps build commands and asset
names in the target repository configuration. `version_format` is required and
typed: `"channel"` is for GitHub prereleases, and `"semver"` is for GitHub
releases. A `preview` channel must be a prerelease; `stable` must be a release.
Other channel names are allowed with either matching type and format. Unsupported
type/format pairs fail generation.

```toml
[[declare]]
primitive = "package-release"
file = "package-release.yml"

[declare.args]
channel = "stable"
version_format = "semver"
manifest_format = "core-v1"
release_tag = "stable"
github_release_type = "release"
# release_title_prefix = "Stable"          # defaults to channel with its first letter uppercased
```

With `version_format = "semver"`, the verifier accepts only plain SemVer
(`MAJOR.MINOR.PATCH`), with no leading zeroes, prerelease suffix, or build
metadata. With `version_format = "channel"`, it accepts only
`MAJOR.MINOR.PATCH-<channel>.<sequence>+<seven-lowercase-commit-hex>`; its
channel and source-commit suffix must match the configured channel and checked
out source. Stable immutable releases are promoted to GitHub Latest only after
the rolling-release preflight and refresh succeed. A prerelease never becomes
Latest. The separate rolling release stays `make_latest=false`.

`manifest_format = "core-v1"` defines the six-key consumer manifest used by
the stable fixture:
`assets`, `schema`, `source_commit`, `source_ref`, `source_repository`, and
`version`. `manifest_format = "supporting-assets-v1"` requires at least one
`supporting_assets` entry and adds the exact seventh `supporting_assets` key
with the same `{name, sha256}` entry shape. Prerelease lanes require this
format. Stable lanes may select it when their consumer contract supports the
additional key; `manifest_format` selects the exact verified shape.
`identity.json` always has exactly
`manifest`, `source_digest`, `source_ref`, and `source_repository`; `manifest`
must equal `release-manifest.json`, and the other identity values must match its
source commit, ref, and repository. These shared checks are covered by stable
six-key and preview seven-key consumer fixtures in
`tests/fixtures/package-release/`.

Before creating the source-bound immutable release, the publisher re-verifies
the downloaded handoff and build attestations, then checks the current Latest
floor and live rolling-release contract. Stale versions and incompatible
rolling manifests or asset sets fail before a candidate release is created.

Declared dot-prefixed asset names are supported. The generated upload includes
hidden files, while verification compares the exact configured file set; its
temporary inventory lives outside the downloaded asset directory. If a live
rolling release has a different configured asset set or manifest shape, the
workflow fails with a migration error before changing the release. Migrate its
live assets or generate a compatible contract before retrying. An incomplete
or incompatible rolling draft also fails before mutation; repair or remove it
before retrying.

The generated workflow uses a workflow-level concurrency group derived
unconditionally from the mutable `release_tag`, so channels sharing a rolling
tag cannot publish concurrently. A distinct global publish-job lock serializes
release mutations across tags and queues up to 100 pending jobs. GitHub cancels
additional jobs when that queue is full; rerun any canceled release lane after
the queue drains. Stable immutable releases are promoted to GitHub Latest only
after the rolling-release preflight and refresh succeed. The current Latest
floor is accepted only when its manifest and identity envelope match the
configured schema, source repository, and source ref, its tag matches the
manifest commit, and both files verify against the configured workflow
attestations. Rollback snapshots the release body without trimming bytes,
including a trailing newline. If it cannot verify restored asset bytes, it
restores the old tag object and safe release metadata, keeps the release as a
draft, and reports rollback incomplete.
Both jobs use Bash on a standard GitHub-hosted Ubuntu runner:
`ubuntu-latest`, `ubuntu-22.04`, `ubuntu-24.04`, `ubuntu-26.04`, or their
`-arm` variants for 22.04, 24.04, and 26.04. macOS, Windows, slim Ubuntu, and
multi-label runner selectors fail validation because publication uses Bash and
GNU package-verification tools.

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

Generated jobs install the runtime through the versioned composite action
(mise-action model: declare a revision, get the binary on PATH, cached)
instead of an inline `cargo install`, so toolchain setup stays centralized:

```yaml
- name: Set up Velnor workflow runtime
  if: ${{ runner.environment == 'github-hosted' }}
  uses: tailrocks/velnor/.github/actions/setup-velnor-workflow@<full-SHA>
  with:
    rev: <full-SHA>
```

The action isolates the install from job-level toolchain wrappers (for
example an `RUSTC_WRAPPER` pointing at an `sccache` that is set up later in
the job) and caches the cargo install keyed by revision and runner OS.
