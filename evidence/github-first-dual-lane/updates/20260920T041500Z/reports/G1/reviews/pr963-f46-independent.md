# PR963 exact-f46 independent source review

## Verdict

**REJECT source admission for `f46fe7c2c3ca7635c44eab21cc6bf616709c7a50`.** Two exact-head P1 defects remain in the shared Rust include scanner. This is a source verdict only; it is not a hosted-gate or merge decision.

## Revision identity

- Reviewed detached checkout: `/tmp/velnor-pr963-f46-review`
- Exact reviewed head: `f46fe7c2c3ca7635c44eab21cc6bf616709c7a50`
- Exact parent: `b8c7abe821f016bfa1291daf24476a5df7be8305`
- PR base: `325719f1e05d3d46322c9fd3eeb9ad545e175638`
- Current main supplied for applicability: `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`
- Merge base with current main: `325719f1e05d3d46322c9fd3eeb9ad545e175638`
- `git merge-tree --write-tree main f46`: clean, candidate tree `da897c1731d5b29313e1f121cfe686d3ed80f83f`.
- During review, hosted PR head advanced to `cf0628b9aa5759c634fbee4ae784129737961cef`; no later-head result is transferred to f46.

## Blocking findings on exact f46

1. **Macro metavariable substitutions are rejected.** `crates/velnor-workflow/src/rust_include.rs:135-153` recursively scans every token group and treats any `include_str!`/`include_bytes!` token sequence as a direct built-in invocation. `parse_static_string_expression` and `evaluate_static_expression` at `:296-320` accept only a literal or supported macro, so a normal wrapper

   ```rust
   macro_rules! embed { ($p:literal) => { include_str!($p) } }
   embed!("asset.txt");
   ```

   reaches the unexpanded `$p` in the macro definition and fails as a non-static expression. The scanner is shared by both Rust adapters; valid wrappers make generation abort. Macro definitions need to be distinguished from expanded invocations, with supported literal substitution handled safely.

2. **Built-in include aliases are scanner-global, not lexical.** `IncludeScanner` stores one `BTreeSet` at `:128-132`; `scan_stream` discovers aliases through all nested groups at `:135-140`, and `discover_include_aliases`/`collect_use_branch` recursively insert names at `:194-212` and `:228-261`. An import such as `use std::include_str as asset` inside one module/block therefore marks `asset!` as built-in in unrelated sibling scopes. A user `asset!` macro can be parsed as a built-in, causing a false missing/dynamic-include error or a spurious watched path. Alias discovery must follow Rust lexical scopes and shadowing.

These are the exact-head review comments `4055847744` and `4055847745`; both remain unresolved in the f46 source.

## Prior findings independently checked

The four prior 6cc findings are fixed in f46:

- `rust_include.rs:322-355` evaluates concat string/bool/char/int/float literals and signed numeric literals; regression tests at `:663-677` cover accepted concat forms and direct non-string rejection.
- `s2/scan/skills.rs:245-254` emits helper syntax only for non-template TypeScript sources (case-insensitive); tests `:1881-1910` cover no-helper and uppercase `.TS` cases.
- `lane_compare.rs:971-1042` obtains `GH_TOKEN`/`GITHUB_TOKEN` or `gh auth token`, sends the token through curl stdin as a Bearer header, and does not put it in argv; the local-server test is `:2353-2393`.
- `lane_compare.rs:562-613` overfetches successful runs and retains only complete both-lane job censuses before selecting the requested sample; tests `:2333-2350` cover missing and skipped counterparts.

Later review fixes present in this exact tree were also checked: archived docs generation changes directory before execution (`skills.rs:48`), docs checks are gated by distinctive files (`:239-243`) and watch `docs/**` (`:207-221`), standard Agent Skills fields are typed/accepted (`:1112-1167`, test `:1570-1579`), and Velnor-only comparison rows are informational (`lane_compare.rs:2190-2200`). The standard-field fix still does not justify ignoring the two exact f46 Rust findings.

## Local verification

All commands ran against the detached exact f46 checkout. Targeted source checks:

- `rtk cargo test --locked -p velnor-workflow --lib`: **1837 passed**.
- `rtk cargo test --locked -p velnor-workflow --test package_release`: **3 passed**.
- `rtk cargo test --locked -p velnor-tools --bin velnor-tools`: **248 passed**.
- `rtk cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings`: clean.
- Same clippy command for `velnor-tools`: clean.
- `rtk cargo fmt --all -- --check`: clean.
- `rtk proxy actionlint`: clean.
- `git diff --check 325719f1..f46`: clean.

`rtk cargo test --locked --workspace` ran **2337 passed, 1 failed, 5 ignored**. The sole failure was the existing timing guard `workflow_command::tests::workflow_command_parse_benchmark` (10.792s over its threshold under concurrent workspace load); the exact test rerun alone passed (1 test, 4.54s). This is not evidence to waive the two source defects.

Generator check from f46:

- `velnor-workflow --revision`: `f46fe7c2c3ca7635c44eab21cc6bf616709c7a50`.
- `velnor-workflow --closure`: `25cb1d8b96515eb209235fdfb825837e3e9c91c364e7bb41d612e39118bff087`.
- `--plain --default-branch main --check .`: exit 0; tree matches candidate render closure `25cb1d8…`, not declared `.github-gen/velnor-workflow.toml` pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`. This is the known generated-output pin bump follow-up, separate from the source rejection.

## Hosted feedback and CI applicability

Fresh read-only pagination found 9 REST reviews, 11 issue comments, and 27 GraphQL review threads with `hasNextPage=false`. Current API head is cf, not the frozen f46; only the two comments named above are exact f46 findings. Later cf-only comments are not used to alter this revision-bound verdict.

For exact f46 commit checks, workflow run `35485190541` (`pull_request`, head f46) ended **cancelled** after the PR advanced; policy run `35485189230` also ended **cancelled**. Final check-run census: **70 total = 21 success, 48 skipped, 1 cancelled**. All Velnor/self-hosted lane jobs and the hosted `…/velnor` variants were skipped; only the GitHub-hosted lane largely ran. DCO was success, but this is not complete dual-lane evidence and does not approve merge or hosted admission.
