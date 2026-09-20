# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --plain
velnor-workflow plan --config .github/ci/project.toml
velnor-workflow run --config .github/ci/project.toml --scope affected
velnor-workflow test-crates --config .github/ci/project.toml
velnor-workflow policy --workflow-root . \
  --head-sha <pr-head-sha> --base-sha <base-sha> \
  --base-revision <sha of the validator the base branch pins> \
  --ruleset-contexts ci-required,DCO
velnor-workflow --plain --check .
velnor-workflow --plain --verify-pinned .
velnor-workflow release verify-tag
```

`policy` is the trust validator the base branch's `ci-policy.yml` runs under
`pull_request_target` against a pull request's tree. It never compares the
tree with its own rendering: it reads the generator commit the tree declares
(`[generator] revision` in `.github-gen/velnor-workflow.toml`, the D19 pin),
proves the pin is reachable from the head and does not regress the base
branch's validator, and evaluates its own semantic rules
(only the entrypoint on `pull_request_target`; every self-hosted job gated;
every action SHA-pinned; the entrypoint on `contents: read` with no secrets;
the ruleset's required contexts emitted). Every rule prints `PASS`/`FAIL`
with a one-line reason. It never executes the generator pin.

`--check` renders with the currently running binary and compares the generated
files and ownership inputs with the checked-in tree. It does not read or select
the declared generator pin. `--verify-pinned` is the explicit local proof that
the existing tree is byte-identical to the renderer at `[generator] revision`.
It uses the trusted renderer named by `VELNOR_WORKFLOW_PINNED_BINARY` together
with its independently verified `VELNOR_WORKFLOW_PINNED_CLOSURE`; `--pin-build`
allows local source builds when no trusted renderer is provisioned. After a
generator change, update the declared pin, regenerate with the matching source,
run `--check`, and use `--verify-pinned` when the pin proof is required.

Runtime commands are derived from scanned capabilities, not from config-supplied
shell arrays. Provider placement uses typed provider and platform selectors;
the old `github`/`velnor`/`both` runner aliases are not accepted. The binary
owns selection, dependency ordering, policy, release validation, and every
`.github/workflows/*.{yml,yaml}` file it emits. Foreign workflow bodies are
never imported. The ownership sidecar stays at
`.github/ci/.github-actions-generator-state`.

Every target must declare `.github-gen/velnor-workflow.toml` with
`schema = 2`. Missing, malformed, schema-1, and unknown schemas fail closed.
Generation is a function of the scanned repository shape, this config, and the
generator revision (`GENERATOR_REVISION`); all three are recorded in the
ownership sidecar (`schema = 2`) and `--check` fails when they no longer match
the current run, even if every generated file is unchanged.

## Task-release jobs

Schema-2 `kind = "tasks"` releases run on tag pushes and `workflow_dispatch`.
Each `[[release.job]]` may use `modes = ["validate"]` for dispatch-only work or
`modes = ["publish"]` for tag-only work. These jobs currently require
`provider = "github-hosted"` on a supported hosted platform (`linux-x64` or
`macos-arm64`). Config validation rejects `velnor` and `github-self-hosted`
because policy admits local jobs only on an exact same-repository
default-branch push, which task-release triggers do not provide. Local task
releases need a separate trusted-push contract before they can be declared.

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
```

`verify_tasks` is a list of plain mise task names, not shell commands or an
arbitrary command array. Omit it when a package has no repository-owned
semantic verification beyond Velnor's generic manifest, checksum, and
provenance checks.

The rolling release is current-contract-only: an existing public release must
match the declared manifest, identity, asset digests, tag, source, and version
contract. An interrupted draft is discarded only after that same typed contract
and release/tag ownership are re-read; missing or mixed state is rejected before
publication mutation. Migration of an older consumer release belongs in that
consumer's release automation.

## Debian/APT feed publisher

Schema-2 repositories that publish a Debian feed declare a typed release
contract. APT publication requires the `github-hosted` provider and writes only
from the default branch. The signing values below name GitHub Actions secrets;
they are never literal key or passphrase values:

```toml
[workflow]
providers = ["github-hosted"]

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[release]
enabled = true
reason = "Publish the verified Debian package feed."
kind = "apt"
package = "example"
binary = "example"
source_repository = "OWNER/PROJECT"
consumer_repository = "OWNER/APT-FEED"
manifest_schema = "urn:example:consumer-manifest:v1"
signer_fingerprint = "0123456789ABCDEF0123456789ABCDEF01234567"
passphrase_secret = "APT_PASSPHRASE"
signing_key_secret = "APT_SIGNING_KEY"
apt_feed_url = "https://example.github.io/apt"
# apt_arches = ["amd64", "arm64"] # optional; these are the only supported arches
# keyring_path = "keys/example.gpg" # optional; defaults to "example.gpg"
# apt_origin = "Example"            # optional; defaults to the package name
# apt_identity_dir = "project"      # optional; defaults to the source repo name
# retention = 1                     # the only implemented previous-version policy
# description = "Example packages"  # optional; defaults from the package name
```

The contract checks both repository slugs, package and binary names, manifest
schema, full signing fingerprint, secret references, HTTPS feed URL, and the
exact architecture set during config validation. The generated workflow fetches
stable or preview assets, verifies their attestations and package identity,
publishes signed indexes and channel state, and guards Pages deployment against
rollback. `velnor-workflow release` exposes the typed `apt-resolve-commit`,
`apt-fetch`, `apt-verify`, `apt-previous-pointer`, `apt-publish`,
`apt-channel-update`, and `apt-deploy-guard` commands; generic
`release update-feed` does not publish APT.

## `[renovate]` — self-hosted dependency updates

Scan evidence alone (`renovate.json`, `renovate.json5`, or `.github/renovate.json*`)
records `renovate-configuration` but does not emit workflows. A repository opts
in explicitly:

```toml
[workflow]
providers = ["velnor"]
automatic_providers = ["velnor"]

[workflow.selectors.velnor]
runs_on = ["self-hosted", "example-lane"]

[renovate]
enabled = true
reason = "Repository-local Renovate on admitted default-branch pushes."
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

Generated `renovate.yml` uses the canonical local-provider admission gate. It
runs only when `velnor` is admitted, on a push to the exact default branch in
the configured repository; schedule, tag, pull-request, and manual-dispatch
events cannot admit the local writer. It uses `secrets.GH_RENOVATE_TOKEN` by
default, pins `renovatebot/github-action` and Renovate OSS `44.93.6`, and keys
the repository cache under `velnor-renovate-${{ github.repository }}-`. The
`renovate-validate.yml` job remains hosted and validates the config with
`ghcr.io/renovatebot/renovate:44.93.6`. Renovate settings are generation-time
only and are not written into `.github/ci/project.toml`.
The scan reads the git index (tracked files only), so untracked CI runtime
artifacts, scratch files, and linked-worktree `.git` files never enter the
recorded scan input. An ownership sidecar from another schema is rejected; do
not run this generator against it until the owning generator has migrated it.

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
