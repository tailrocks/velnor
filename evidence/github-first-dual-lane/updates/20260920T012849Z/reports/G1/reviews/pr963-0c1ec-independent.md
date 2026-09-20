# Independent source review: PR #963 at `0c1ec757`

Verdict: **APPROVE source admission at this exact head.** This is a source-only
approval. It does not approve the pull request merge, generated-output
admission, hosted checks, Policy, latest-Mac reconciliation, or any rollout.

## Exact revision

- PR: `963`
- live head at review: `0c1ec75753cf9f8044a3a2ff01c2d144e9c59132`
- live PR base ref OID: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- current `main` queried separately: `d20d4d1d17590cca85b501d982cbaad70d42c641`
- PR/base merge-base: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- exact detached review worktree: `/tmp/velnor-pr963-0c1ec-review`
- worktree: clean; `git diff --check refs/remotes/origin/pr962-current-main...HEAD`: pass
- `git merge-tree --write-tree refs/remotes/origin/pr962-current-main HEAD`:
  conflict-free tree `f225446fd27cf30f95807e3070fe45bc4b30a0a6`

The PR body still names old base/head `b5a4b4a`/`fb78d85`; it is not identity
evidence. Earlier reviews at `6250b0f` and `2cdce1c` do not transfer to this
revision. The current chain includes the latest fixes `630c2c9` (generated
Rust includes), `ed0ebd8` (schema-1 package ownership), and `0c1ec75`
(test-only legacy wrapper).

## Feedback census

I paginated the PR reviews, inline review comments, and issue comments. Reviews
read: `5258131245` (`fb78`), `5258386143` (`6250`), and `5258451449`
(`2cd`). Inline comments read: `4055051090`, `4055051092`, `4055051097`,
`4055051100`, `4055051103`, `4055269914`, `4055269916`, `4055269918`,
`4055327518`, `4055327523`, and `4055327525`. Issue comments read:
`5745823264`, `5746180102`, `5746285181`, and `5746400028`. The three latest
comments are on `0c1ec757` and are addressed below; no later unresolved inline
feedback was present at capture.

## Source findings and exact behavior

1. **Rust include ownership is fixed in both scanners.**

   - Schema 2 builds all accepted package roots and calls
     `include_str_paths_for_package` with them (`crates/velnor-workflow/src/s2/scan/rust.rs:432-435,558-564`).
   - Schema 1 now does the same (`crates/velnor-workflow/src/scan/rust.rs:382-385,558-564`).
   - Both implementations assign each `.rs` source to the longest matching
     package root (`s2/scan/rust.rs:759-819`; `scan/rust.rs:758-817`). Thus root,
     outer-member, and nested-member `CARGO_MANIFEST_DIR` includes resolve to
     the owning manifest; the old test-only wrapper is explicitly `#[cfg(test)]`
     (`scan/rust.rs:748-756`).
   - Regression coverage includes nested outer/inner packages in both scanner
     test modules (`s2/scan/rust.rs:1266-1337`; `scan/rust.rs:1199-1270`).

2. **Qualified and generated Rust includes are handled fail-closed.**

   - `std::include_str!`, `core::include_bytes!`, and absolute `::std`/`::core`
     forms are recognized while arbitrary/user-qualified macros remain ignored
     (`rust_include.rs:171-187,293-306`), with a regression test at
     `rust_include.rs:590-605`.
   - `env!("OUT_DIR")` marks the expression as generated build output and is
     skipped rather than treated as a repository input or a fatal parse error
     (`rust_include.rs:242-264,350-360`). The test is at `:552-560`.
   - Include arguments are parsed as exactly one expression with an optional
     trailing comma (`rust_include.rs:190-203`); the current trailing-comma
     fixture is at `:538-550`.

3. **Skills Bun selection is target-aware and pinned.**

   `target_bun_version` scans non-template target `package.json` files,
   validates exact Bun `packageManager` versions, rejects conflicts, and uses
   the generator's checked central pin only when the target declares no Bun
   version (`s2/scan/skills.rs:67-163`). The central fallback cross-checks
   `mise.toml` and `mise.lock` (`:67-107`), currently both `1.4.0`; emitted
   `tool_version` and both verification commands use the selected value
   (`:194-227`). The exact target fixture proves `1.2.3` propagation and
   conflict rejection (`:1991-2013,2033-2043`). Template package manifests are
   excluded before selection (`:110-119`). No target-facing hardcoded `1.4.0`
   remains.

4. **Provider projection is target-specific.**

   `validate_plugin_manifests` reads only provider manifests present in the
   target and validates shared compatibility fields; unrelated Kimi/Claude
   files are not required (`s2/scan/skills.rs:482-558`). The Codex-only fixture
   passes (`:1991-2013`). Shared root `plugin.json` remains required by
   `read_catalog` (`:246-288`), matching the reviewed Skills contract.

5. **Template exclusion no longer depends on Skill prose.**

   Every catalogued `skills/<name>/templates/**` path is hidden structurally
   from generic detectors (`s2/scan/skills.rs:870-892`), and the regression
   removes the Markdown link while still asserting exclusion (`:2015-2031`).
   Helper templates remain a narrower source-declaration contract: a helper
   source must contain a string path component named `templates`
   (`:894-971`). This is distinct from the fixed catalogued-Skill prose bug.

6. **Lane watch filters before limiting.**

   `recent_run_args` supplies `--status success` before `--limit`
   (`crates/velnor-tools/src/lane_compare.rs:529-543`), with an exact argument
   regression at the lane tests. Current per-job Velnor log evidence remains
   keyed by job, and each pair receives only its own statistics
   (`lane_compare.rs:838-885,1081-1187`); the new isolation test is at
   `:2880-2897`.

## Independent exact tests

Commands ran in the detached `0c1ec757` worktree:

```text
rtk cargo test -p velnor-workflow --all-features --lib -- --test-threads=1
  1818 passed
rtk cargo test -p velnor-tools --all-targets -- --test-threads=1
  248 passed (2 suites)
rtk cargo clippy -p velnor-workflow --all-targets --all-features -- -D warnings
  No issues found
rtk cargo clippy -p velnor-tools --all-targets -- -D warnings
  No issues found
rtk cargo fmt --all -- --check
  pass
rtk git diff --check refs/remotes/origin/pr962-current-main...HEAD
  pass
```

These are independent source-checkout results. No claim is made here that the
stale PR-body counts (`1804`, `3019`) describe this head, and no generated
workflow check was run from this review.

## Hosted-gate boundary

At the final live query, GitHub reported 17 successful checks, 48 skipped
matrix checks, and three pending hosted checks: `velnorctl`, `velnor-runner`,
and `velnor-workflow` (Policy had completed since the earlier query). DCO was
successful. PR status was `BLOCKED`/`MERGEABLE` while hosted checks were
pending. These hosted states are not folded into the source verdict; merge
authority remains with the parent gate and latest-Mac/generated-output review.

No source, branch, PR comment, or GitHub state was modified by this review.
