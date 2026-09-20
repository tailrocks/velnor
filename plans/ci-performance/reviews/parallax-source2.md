# Parallax schema-2 migration review

Independent review of the uncommitted Parallax migration against base
`0da45dafabc7a46cf1a5c1ff461e2d193d115cb1`. The review covers the actual
working diff, generated plans, required gates, provider admission, Docker
context handling, and nightly dispatch semantics. No Parallax files were
modified.

## Verdict

**HOLD for real CI and structural corrections.** The migration has a coherent
schema-2 declaration and pins generator revision
`325719f1e05d3d46322c9fd3eeb9ad545e175638`. It declares both providers while
selecting GitHub-hosted for automatic and default dispatch lanes. The generated
`ci-required` job checks the plan digest, selected unit/provider set, expected
success, unexpected success, and provider admission; selected jobs that are
skipped or missing fail the gate. The Sentry Docker command correctly supplies
the named `parallax-checkout` BuildKit context required by its Dockerfile.

The following blockers remain:

1. `nightly.yml` only runs `gh workflow run ci-main.yml` and exits. It does not
capture the child run ID, poll its terminal conclusion, validate the child
source/provider/scope, or propagate failure. `nightly-alert` depends only on
the deliberate `simulate_failure` job. A scheduled nightly can therefore be
green while its dispatched `ci-main` fails, which preserves the cited defect.

2. `ci-pr.yml` and `ci-main.yml` statically declare every unit/provider job and
use `if` expressions to skip irrelevant ones. This keeps skipped jobs visible
in the Actions UI; it is not dynamic matrix omission. The required gate
correctly rejects an expected skipped result, but the migration does not meet
the requirement that irrelevant expensive jobs be absent.

3. The generated `bun-ui` unit has root `ui` but its watch list is
`src/**`, `scripts/**`, `package.json`, and `bun.lock` without the `ui/` prefix.
The runtime watch matcher operates on repository-root paths. A change such as
`ui/src/router.tsx` matches no unit and therefore triggers conservative full
fallback, scheduling the entire graph instead of the Bun product. This is a
generator root-normalization defect, not a missing workflow condition.

4. The Maple Docker unit adds workspace Rust/toolchain files to its watch set
even though its Dockerfile only installs the external Maple bundle. An
unrelated `Cargo.lock`, `mise.lock`, or Rust toolchain edit therefore selects
the Maple image. The generated Docker closure must be derived per Dockerfile,
not from a generic Docker unit prelude.

5. No scenario fixture or executed plan evidence is present in the Parallax
working diff. Docs-only, UI-only, isolated/shared Rust, lockfile/toolchain,
Dockerfile/context, `.dockerignore`, shared-script, rename/delete, provider,
manual, and incomplete-diff cases remain unvalidated. Actionlint or a local
render check cannot establish those selection and gate semantics.

The earlier review raised a possible missing policy verdict. Reinspection of
the actual `ci-main.yml` source shows that `ci-required` already reads
`needs.policy.result` and exits unless it is `success`; no policy false-green
was reproduced. The separate `ci-pr.yml` path intentionally has no policy job.
This item is withdrawn as a defect; real manual and empty-plan runs still need
to exercise the existing contract.

## Evidence inspected

- Working Parallax paths: `.github-gen/velnor-workflow.toml`, generated
  `.github/ci/project.toml`, `ci-pr.yml`, `ci-main.yml`, `ci-policy.yml`, all
  three unit workflows, `nightly.yml`, and `maintenance.yml`.
- `crates/parallax-sentry-proxy/Dockerfile` uses
  `RUN --mount=type=bind,from=parallax-checkout,...`; generated commands supply
  `--build-context parallax-checkout='.'`.
- `nightly.yml` dispatches providers defaulting to `github-hosted`, but has no
  `gh run watch`, API polling, child-run identity check, or child-result gate.
- The Velnor runtime's `WatchGraph` and `reuse::select_affected` match watch
  globs against repository-root changed paths and fall back to full selection
  when any changed path is unmatched.

No 10x claim or real-CI acceptance is made from this structural review.
