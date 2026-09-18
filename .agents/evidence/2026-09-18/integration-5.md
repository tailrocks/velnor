# Integration 5: merge fix/publish-403 into docs/bastion-final-plan — DONE

Integrator: batch 5 (bastion campaign). Shared workspace, no worktree.
Certification read first: `/tmp/v-fix-pub403.md` (CERTIFIED, c3a35c77).

## Merge
- Base: `docs/bastion-final-plan` @ `89773efc` (already contains signer tip
  f79ab248 via 8f030b7c; branch ancestry check ANCESTOR_OK).
- Source: `origin/fix/publish-403` @ `c3a35c77` (single commit).
- Result: merge commit `1d3eb22eea8ce16c14d195ad4d6f8e3afe173fda`
  (Signed-off-by), pushed `89773efc..1d3eb22e` to
  `origin/docs/bastion-final-plan`.

## Conflicts (2) — resolved in source only, no hand-edited YAML
1. `crates/velnor-workflow/src/primitives/runtime_products.rs`: only the
   `rendered_bytes_are_pinned` digest conflicted (HEAD e3224ae8 vs branch
   a335641e; both moved from base 379826fe). Template/test bodies
   auto-merged — HEAD side adds `--revision` stamping, branch side adds the
   F1–F4+F6 publish path; disjoint regions. Resolution: temp-kept HEAD pin,
   ran the pin test to learn the merged render digest
   `a76dfe51d0b5d28d3b0441a74b9fdb564d593b74535bc7e140ea5f183be32363`,
   verified merged source carries both sides (backoff loop, tag
   concurrency, freshness proof, `--revision` clauses), set the pin.
2. `.github/ci/.github-actions-generator-state`: restored `--ours` as a
   placeholder (generator rejects conflicted state), then regenerated.

## Regen
- `cargo run -p velnor-workflow -- . --plain --force` (rebuilt generator).
  First run left dry-run at 1 file (state `scan` hash not yet at fixed
  point); second `--force` converged — state-only self-reference, no
  source/YAML difference. Final: `--plain --dry-run` →
  `0 files would change`.
- Merge scope exactly 3 files: generator source (+651), regenerated
  `ci-runtime-products.yml` (+82), state hash bump.

## Gates (all on the merge commit tree, before push)
- `--plain --dry-run`: 0 files would change
- `cargo test -p velnor-workflow`: 525 lib + 2/6/5/9/33 integration, 0 failed
- `cargo clippy -p velnor-workflow --all-targets -- -D warnings`: exit 0
- `cargo fmt -p velnor-workflow -- --check`: clean
- `actionlint .github/workflows/ci-runtime-products.yml`: exit 0

## Regenerated workflow spot-check
Backoff loop, tag concurrency, freshness proof, converge/fail messages,
`--revision` checks, manifest revision binding all present; exactly one
`gh release create`; no `workflows:write`.
