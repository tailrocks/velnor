# Independent source rereview: PR #963 at `6ccf3748`

Verdict: **REJECT source admission at this exact head.** Two unresolved P1
source defects remain. Two P2 lane-compare defects also remain. This is a
revision-bound source verdict only: it does not approve merge, generated-output
admission, hosted checks, Policy, or rollout.

## Exact revision

- PR: `963`
- exact detached HEAD: `6ccf37486d255bbe3656f0066b0f1e5c84753903`
- PR base ref OID: `e94b48406c4ed206fce2bbf39b788264e72cf39c`
- merge parents: `8d04ecf308845fd811b75753f1db8d751082ce99`,
  `e94b48406c4ed206fce2bbf39b788264e72cf39c`
- current remote `main` queried separately: `325719f1e05d3d46322c9fd3eeb9ad545e175638`
- detached worktree: `/tmp/velnor-pr963-6ccf-review`
- worktree remained clean throughout; `git diff --check e94b484..HEAD`: pass
- read-only `git merge-tree --write-tree 325719f1e05d3d46322c9fd3eeb9ad545e175638 HEAD`:
  conflict in
  `.github/ci/.github-actions-generator-state`; current-main reconciliation is
  therefore not a clean merge candidate
- PR API at capture: open, `mergeable=false`, `review_decision=null`; this is
  not treated as a hosted gate result.

The PR base is historical relative to current `main`; no approval from an
earlier `0c1`/`c440`/`15c`/`0c1ec` revision transfers to this merge head.

## Feedback census

I read the paginated PR reviews, inline comments, GraphQL review threads, and
issue comments. The latest exact-head Codex review is `5258837130`, reviewed
commit `6ccf37486d`. Its four current unresolved comments are:

- `4055600360`, P1, `crates/velnor-workflow/src/rust_include.rs:210`
- `4055600372`, P1, `crates/velnor-workflow/src/s2/scan/skills.rs:62`
- `4055600378`, P2, `crates/velnor-tools/src/lane_compare.rs:854`
- `4055600385`, P2, `crates/velnor-tools/src/lane_compare.rs:457`

Older unresolved thread metadata includes `4055051090`, `4055327518`, and
`4055327523`, but the exact source now contains their fixes (see below); they
are not counted as remaining source defects after independent inspection. No
later user issue comment beyond the read `@codex review` trigger was present at
capture.

## Current source findings

1. **P1 — valid `concat!` literals still abort scanning.**

   `evaluate_static_expression` accepts only `syn::Lit::Str`
   (`crates/velnor-workflow/src/rust_include.rs:205-213`). Rust's
   `concat!` accepts static non-string literals and stringifies them, so a
   valid include such as
   `include_str!(concat!("asset-", 1, ".txt"))` reaches this branch and
   returns `must use a static string expression`; both scanners turn that
   parser error into a repository scan failure. The recursive `concat!`
   evaluator at `:234-240` does not change this rejection. Add the complete
   static literal subset Rust's `concat!` accepts and regression coverage.

2. **P1 — Skills helper verification is emitted without helper inputs.**

   `verification_commands` unconditionally pushes `SkillsCheck::HelperSyntax`
   (`crates/velnor-workflow/src/s2/scan/skills.rs:57-64`). The command at
   `:50-52` runs `find scripts ... | xargs -0 bun build` under `pipefail`.
   A valid provider/catalog/`skills/*/SKILL.md` repository with no `scripts/`
   directory therefore generates a command that fails before any helper can
   be checked (and an empty directory can invoke Bun with no entrypoints).
   Emit this check only when scan evidence contains a non-template TypeScript
   helper, and add a no-helper fixture.

3. **P2 — private repository HTML evidence is fetched without auth.**

   `fetch_job_html_steps` calls `curl -fsSL` directly on `job.html_url`
   (`crates/velnor-tools/src/lane_compare.rs:918-940`). For a private
   `--repo`, this has no credential/header even though the adjacent `gh api`
   paths reuse the authenticated CLI. The response can be a login page, then
   `parse_check_steps` returns no evidence and every comparison fails. Use an
   authenticated API/header path without leaking the token.

4. **P2 — `--watch` limits before proving complete both-lane pairs.**

   `lane_compare_watch` requests exactly `args.since` rows at
   `:457-492`; `recent_run_args` applies `--limit limit.max(2)` at
   `:529-543`. Only afterward does `lane_stats_for_run` inspect the pair
   census. Successful GitHub-only/Velnor-only runs can consume the limit and
   abort the watch even when older successful both-lane runs exist. Overfetch,
   validate complete both-lane eligibility, then retain the requested sample.

## Earlier feedback independently checked

- Root/workspace `CARGO_MANIFEST_DIR` ownership is implemented in the schema-1
  path: production passes all package roots at
  `crates/velnor-workflow/src/scan/rust.rs:382-386,559-565`, and
  `source_belongs_to_package` selects the longest matching root at `:806-817`.
  The old one-argument `include_str_paths` wrapper is test-only (`:749-756`).
  Nested ownership coverage is present at `:1200-1270`.
- `OUT_DIR` is represented as `BuildOutput` and filtered before emitting an
  include (`crates/velnor-workflow/src/rust_include.rs:30-37,242-265`), with
  `ignores_build_script_out_dir_includes` at `:573-581`; this old thread's
  failure description does not match the exact tree.
- The requested scope fixes are present: optional `env!` diagnostics are
  accepted and validated (`rust_include.rs:242-254`); docs checks are gated by
  `has_generated_docs_surface` (`s2/scan/skills.rs:191-203,236-241`); and
  Markdown fence parsing now tracks both backticks and tilde fences and the
  opening length (`s2/scan/skills.rs:695-779`).

## Independent exact tests

Commands ran in the detached `6ccf37486d` worktree:

```text
rtk cargo test --locked -p velnor-workflow --lib
  1829 passed (1 suite)
rtk cargo test --locked -p velnor-workflow --test package_release
  3 passed (1 suite)
rtk cargo test --locked --workspace
  5435 passed, 5 ignored, 2342 filtered out (79 suites)
rtk cargo test --locked -p velnor-tools --bin velnor-tools
  246 passed (1 suite)
rtk cargo clippy --locked --workspace --all-targets -- -D warnings
  No issues found
rtk cargo fmt --all -- --check
  pass
rtk proxy actionlint
  pass
rtk git diff --check e94b48406c4ed206fce2bbf39b788264e72cf39c..HEAD
  pass
```

The attempted `-p velnor-tools --lib` command was not applicable: that crate
declares only a binary target; the binary test command above is the applicable
test run.

The generator check also completed without changing the worktree:

```text
rtk cargo run --locked --manifest-path crates/velnor-workflow/Cargo.toml -- --plain --check
  generated files current in the detached tree
  candidate render cff9beb3f3b32c14f01fc42a59426e0e5ad8bad97f44118d91344bbe2f66923c
  declared generator pin 0dc79895ff1c5e88be7c3822c437e1c5b5282e12
```

The check printed the expected source-vs-declared-pin notice: generated files
match the candidate render, but `.github-gen` still declares the older pin.
That generated-output/pin follow-up is separate from this source verdict.

## Boundary

No source, branch, PR, generated file, or GitHub state was modified. Hosted
Policy run `35481211642` was reported missing an exact-head CI PR run/artifact;
hosted CI was not used to override the source rejection. A future approval must
review a new exact head after all four findings are fixed and re-read all
feedback; it cannot transfer this verdict or an earlier approval.
