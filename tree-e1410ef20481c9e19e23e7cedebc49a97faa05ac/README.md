# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --runners both --plain
velnor-workflow plan --config .github/ci/project.toml
velnor-workflow run --config .github/ci/project.toml --scope affected
velnor-workflow test-crates --config .github/ci/project.toml
velnor-workflow policy --workflow-root . \
  --approved-policy-revision 12da6232672f039e42c21fe9dff00085856ef92d
velnor-workflow release verify-tag
```

Project commands remain explicit shell command strings in the checked-in TOML;
the binary owns selection, dependency ordering, policy, release validation,
and workflow file ownership. The ownership sidecar keeps the historical path
`.github/ci/.github-actions-generator-state` for safe adoption of older trees.

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
