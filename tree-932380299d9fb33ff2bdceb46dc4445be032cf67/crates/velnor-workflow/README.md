# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --plain
velnor-workflow REPOSITORY --runners both --plain
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
with a one-line reason. Bump the pin in a single commit after the last
generator change; `--check` verifies the pinned generator renders the tree.

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
