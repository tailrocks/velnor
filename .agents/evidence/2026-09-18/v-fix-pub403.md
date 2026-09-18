# Verification: fix-pub403 (bastion campaign) — CERTIFIED

Verifier: independent, validation only. No edits, no merges, no pushes.
Worktree: `/tmp/v-pub403-wt` (detached HEAD, scratch; main checkout untouched).
Inputs read: `/tmp/a1-pub403.md`, `/tmp/fix-pub403.md`.

## Provenance — OK
- `git fetch origin` done; `origin/fix/publish-403` = `c3a35c77`,
  `origin/feat/a2-signer-order` = `f79ab248` (tip).
- `git merge-base --is-ancestor f79ab248 c3a35c77` → true; the branch range
  `f79ab248..c3a35c77` is exactly one commit, direct child.
- Commit signed off (`Signed-off-by: Alexey Zhokhov`).
- Diff touches 3 files only: generator
  `crates/velnor-workflow/src/primitives/runtime_products.rs`, regenerated
  `.github/workflows/ci-runtime-products.yml`, generator-state hash bump.

## Fix mapping — all confirmed in diff at c3a35c77
- **F1 (no `--target`)**: create is
  `gh release create "$TAG" --repo … --title …`, no target. `--target` occurs
  in the generator only in comments and in the negated test assertion; no
  other runtime-product create path exists.
- **F2 (re-view backoff converge)**: failed create falls into
  `for delay in 5 10 15 30` re-view loop; appearance → `::notice::…converging`,
  exit 0; absent → `::error::…was not created and did not appear`, exit 1.
  Recorded as exit-0-with-notice, no `skipped=` output — G4 pin test
  `publish_verifies_before_creating_the_release` passes.
- **F3 (tag-serialized, cross-ref)**: publish job carries
  `concurrency: { group: runtime-products-tag-${{ needs.closure.outputs.tag }},
  cancel-in-progress: false }`; top-level same-ref gate retained.
- **F4 (freshness re-check)**: publish starts with default-branch tip checkout
  (`persist-credentials: false`) + `Prove publish freshness` gating every
  later step (yield on unchanged closure at moved tip, proceed otherwise).
  Pathspec + footer byte-identical to the resolve step and to
  `canonical_digest` (`closure-version:1`, features `""`, profile `release`).
  G4 order assemble→attest→smoke→create intact (steps verified in order).
- **F6 (single tag-writer)**: exactly one `gh release create "$TAG"`; no
  `git push`, upload, edit, delete, or releases-API write in the template.
  Other in-repo creators mint only `v*`/runner and `preview` tags; runtime
  consumers (`ci-unit-rust.yml`, `release.yml`, setup action) are
  `gh release download` read-only. Out-of-repo half (decommission bastion
  pre-tagger) noted as operational, correctly out of scope.
- **F5 rejected**: no `workflows:write`, App-token, or PAT anywhere in the
  changed template/workflow — confirmed absent by grep.

## Live proof (scratch worktree @ c3a35c77)
- `cargo run -p velnor-workflow -- . --plain --dry-run` → `0 files would change`
- `cargo test -p velnor-workflow --lib primitives::runtime_products` →
  31 passed, 0 failed; all 8 claimed tests present by name
  (`create_carries_no_target`, `conflicting_create_converges_instead_of_failing`,
  `conflicting_create_converges_when_the_release_appears`,
  `publish_serializes_on_the_tag_across_refs`,
  `publish_yields_to_a_fresher_same_closure_run`,
  `workflow_is_the_sole_tag_writer`,
  `freshness_proof_yields_only_on_unchanged_closure`, `rendered_shell_parses`)
- Full `cargo test -p velnor-workflow` → 499 lib + 2/6/5/9/33 integration, 0 failed
- `checked_in_workflows_match_the_generator_byte_for_byte` → ok (regen-only, no hand edits)
- `cargo clippy -p velnor-workflow --all-targets` → exit 0, 0 warnings
- `cargo fmt -p velnor-workflow -- --check` → clean
- `actionlint .github/workflows/ci-runtime-products.yml` → OK

## Verdict: CERTIFIED
Branch, ancestry, commit, diff scope, F1–F4+F6 implementation, F5 rejection,
and every claimed gate reproduce exactly. No scope expansion detected.
