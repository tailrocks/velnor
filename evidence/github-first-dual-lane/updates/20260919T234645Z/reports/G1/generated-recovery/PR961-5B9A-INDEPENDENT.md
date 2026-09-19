# Independent PR961 review — `5b9a16a`

Date: 2026-09-20  
PR: [tailrocks/velnor#961](https://github.com/tailrocks/velnor/pull/961)  
Base: `b5a4b4afaa6ca807927cacc03659b570a895dd5c`  
Head reviewed: `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`  
Checkout: `/tmp/velnor-pr961-review` (detached, clean)

## Revision-bound verdict

**REJECT admission of the exact PR961 head.** The source and generated
metadata are internally correct, but hosted DCO is `ACTION_REQUIRED`; this is
a merge/admission blocker and is not overridden here. The unsigned commit is
the imported generated-tree commit `857646a09dc16506be4a1fe94fa73b2c0ab650a5`.

The owner’s legitimate non-force replacement must preserve the reviewed tree,
omit/recreate that imported commit with a valid `Signed-off-by:` trailer, and
receive fresh checks plus a new exact-SHA review. This review does not transfer
to a replacement SHA.

## Exact graph and diff

PR961 history is:

- `5913fa6ff07ad08fb42dc78ae1530e9f0e11b308` merges approved integration
  `857646a` with `b5a4` and has valid DCO/co-author trailers.
- `5b9a16a` is a single generator-only child of `5913fa6`.
- `git diff 5913fa6..5b9a16a` is exactly two generated files:
  `.github/ci/project.toml` and
  `.github/ci/.github-actions-generator-state`.
- `git diff --check 5913fa6..5b9a16a` passes.

The generator-only behavior is precise:

- `.github/ci/project.toml` adds the detected capability
  `rust-test-targets:velnor-tools:1`, matching the integrated
  `crates/velnor-tools/tests/strict_json_cli.rs` target.
- The ownership sidecar changes only `scan`
  `8d96315cc857133b` → `8fc426291af404f3` and the generated project digest
  `8025f19b38e1cad3` → `7734e0a104f1df1a`; `config` remains
  `ff06cd48adfa74db` and `generator` remains `52`.
- `.github-gen/velnor-workflow.toml` and all workflow/action outputs remain
  unchanged; the generator pin remains
  `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`.

## Provenance / reproducibility

On the exact head:

```text
rtk cargo run -p velnor-workflow --locked -- --plain --check
```

passed. The generator reported every generated output, including
`project.toml` and the ownership sidecar, `unchanged`; it recomputed the new
detected capability and reported `Result Generated files are current`. The
detached checkout remained clean afterward. This proves the two-file delta is
the exact output of the head generator, not a hand edit.

## Local / hosted validation

Local exact-head results:

- `rtk cargo fmt --all -- --check`: pass.
- `rtk cargo test -p velnor-tools --all-targets`: 246 passed.
- `rtk cargo test -p velnor-workflow --no-default-features --lib`: 1,744
  passed, 2 known closure failures. Both are
  `closure::tests::stamped_features_match_dev_features` and
  `s2::closure::tests::stamped_features_match_dev_features` at
  `closure.rs:291` (`left ""`, `right "tui"`); the prior exact integrated
  source review established the same two baseline failures at `b5a4`.

Paginated hosted checks for PR961: 21 pass, 48 skipped, 1 fail. `gh pr checks
961 --required` reports Policy pass, `ci-required` pass, and DCO fail. The
source/CI checks are green; DCO is the sole failing gate.

## DCO audit

Paginated commit-message inspection over `b5a4..5b9` found exactly one commit
without `Signed-off-by:`:

```text
857646a09dc16506be4a1fe94fa73b2c0ab650a5
ci: pin generated tree to published runtime
```

`5913fa6` and `5b9a16a` are signed, but signing descendants does not repair an
unsigned historical commit. Every other PR commit in the range has the normal
Alexey signoff and Codex co-author trailer. The DCO check’s
`ACTION_REQUIRED` result is therefore legitimate.

## All PR feedback

Paginated GitHub API review:

- issue comments: exactly one `chatgpt-codex-connector[bot]` review-summary
  comment, reporting Code Review completed on `5b9a16a`; no findings;
- pull-request reviews: zero;
- inline review comments: zero;
- timeline: the commit events above and that one bot summary only.

No unresolved human or bot review feedback exists beyond the DCO gate.

## Replacement requirement

Do not merge PR961 as-is. A non-force replacement branch can preserve the
same source/generated tree while dropping the redundant imported main commit
or recreating its generated diff with a valid DCO trailer. The resulting head
must be reviewed independently for exact tree equivalence and full diff; this
report is intentionally bound to `5b9a16a` and does not approve that future
revision.
