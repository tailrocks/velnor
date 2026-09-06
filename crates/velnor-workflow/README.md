# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --runners both --plain
velnor-workflow REPOSITORY --adopt --runners both --plain
velnor-workflow plan --config .github/ci/project.toml
velnor-workflow run --config .github/ci/project.toml --scope affected
velnor-workflow test-crates --config .github/ci/project.toml
velnor-workflow policy --workflow-root . \
  --approved-policy-revision 6e6653a54f3ed64f6188af10c8417e7df9c1b8d1
velnor-workflow release verify-tag
```

Project commands remain explicit shell command strings in the checked-in TOML;
each unit declares separate GitHub and Velnor PR/full command arrays. Runtime
defaults to GitHub and selects Velnor commands from the runner's standard
`RUNNER_ENVIRONMENT=self-hosted` marker. The binary owns selection, dependency
ordering, policy, release validation, and every `.github/workflows/*.{yml,yaml}`
file it emits. `--adopt` is the
reviewed migration switch for an existing workflow surface: it snapshots each
reviewed body under `.github/ci/workflow-templates/`, then renders the workflow
from that generator-owned template. The ownership sidecar keeps the historical path
`.github/ci/.github-actions-generator-state` for safe adoption of older trees.
Template loading and `--adopt` fail closed on active workflow references to any
retired `ChainArgos/velnor-actions`, `jackin-project/velnor-actions`, or
`tailrocks/velnor-actions` mirror; the distinct `tailrocks/velnor-actions-fixture`
repository name remains valid fixture data.

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
