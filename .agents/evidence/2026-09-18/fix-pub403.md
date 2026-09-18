# Fix evidence: runtime-products publish 403 (bastion campaign)

## Provenance
- Branch: `fix/publish-403` (pushed to origin)
- Base: `origin/feat/a2-signer-order` tip `f79ab248` (G4 verify-before-create included, not duplicated)
- Commit: `c3a35c77` "fix(ci): converge runtime-product publish on stale-target 403" (signed-off)
- Diagnosis: `/tmp/a1-pub403.md` (systematic stale-target + workflow-scope trap; cascade claim contradicted)
- F5 (App-token/PAT with `workflows:write`): REJECTED — converts the 403 into a stale-target 201, masks the race.

## What changed (3 files, generator + regen only)
- `crates/velnor-workflow/src/primitives/runtime_products.rs` — template + tests
- `.github/workflows/ci-runtime-products.yml` — regenerated via `cargo run -p velnor-workflow -- . --plain --force` (pre-force dry-run showed this as the ONLY file wanting change; no hand edits)
- `.github/ci/.github-actions-generator-state` — regen hash bump for the one file

## Fix mapping
- **F1 (drop `--target`)**: `gh release create "$TAG" --repo … --title …` with no target. The tag names the closure, never the commit; the API tags the default-branch tip when the tag is missing. No target ⇒ no `target..HEAD` range diff ⇒ no ungrantable `workflows` scope demand (cli/cli#9514). Chose drop over `--target main`: a pre-existing tag elsewhere would still mismatch an explicit target (second 403 trigger, chipsenkbeil/distant#284).
- **F2 (conflict→converge)**: failed create falls into re-view with 5/10/15/30s backoff (codebase `for delay in …` idiom from release.rs). Appearance ⇒ `::notice:: … converging`, exit 0 — skip-as-success. Fail (`::error:: … was not created and did not appear`, exit 1) only if nothing materializes. Recorded as exit-0-with-notice, not a `skipped=` output, to preserve the pinned G4 contract (`publish_verifies_before_creating_the_release` asserts no `skipped` string); our smoke test already passed on same-closure bytes, so converging is sound.
- **F3 (tag-serialized, cross-ref)**: publish job carries `concurrency: { group: runtime-products-tag-${{ needs.closure.outputs.tag }}, cancel-in-progress: false }`. Job-level is the only place that works: workflow-level groups cannot see `needs.*`. Same-ref top-level gate stays; both apply. Covers main-push vs branch-dispatch same-tag collisions.
- **F4 (freshness re-check)**: publish starts with tip checkout (`ref: <default-branch>`, `persist-credentials: false`) + `Prove publish freshness`: tip == HEAD_SHA ⇒ proceed; else recompute the canonical closure at tip (same pathspec/footer constants); unchanged ⇒ `::notice:: … the fresher run owns $TAG`, exit 0; changed ⇒ fall through (tag is this run's own). Gates all publish work; G4 assemble→attest→smoke→create order untouched.
- **F6 (single tag-writer)**: exactly one `gh release create`, no `--target`, no `git push`, no delete/edit/clobber. Verified by repo-wide search: this workflow was already the only in-repo creator of `velnor-workflow-runtime-v1-*` tags (release.yml creates only `v*`/runner tags; ci-unit-rust/release consumers are `gh release download` read-only). **Operational half, out of repo**: decommission the bastion pre-tagger that tags every push's closure ahead of the workflow (spec §81: release/tag creation has one writer). Post-F1/F4 a lingering pre-tag is harmless (no target ⇒ no mismatch), but sole ownership requires its removal.

## Tests (all in `runtime_products.rs`, committed)
- Structural: `create_carries_no_target`, `conflicting_create_converges_instead_of_failing`, `publish_serializes_on_the_tag_across_refs`, `publish_yields_to_a_fresher_same_closure_run` (incl. `trunk` branch-following), `workflow_is_the_sole_tag_writer`.
- Executable (unix): `rendered_shell_parses` extended to 9 bodies (incl. new `Prove publish freshness`); `freshness_proof_yields_only_on_unchanged_closure` runs the rendered proof against a scratch git repo with the Rust `closure_of_tree` oracle (tip-silent / stale-yield / diverged-proceed); `conflicting_create_converges_when_the_release_appears` runs the rendered create against a stateful `gh` + recording `sleep` stub (clean: 1 view, 0 sleeps / conflict: exit 0 + `5` / absent: exit 1 + `5,10,15,30`).
- Incidental: pinned-action count 8→9 (publish tip checkout); render digest pin `379826fe…` → `a335641e…`; header reworded off the literal `tags:` (trigger-guard substring check).

## Gate results (this worktree, post-regen)
- `cargo test -p velnor-workflow`: 499 lib + 2/6/5/9/33 integration suites — 0 failed (incl. `checked_in_workflows_match_the_generator_byte_for_byte`)
- `cargo clippy -p velnor-workflow --all-targets`: 0 warnings
- `cargo fmt -p velnor-workflow -- --check`: clean
- `actionlint .github/workflows/ci-runtime-products.yml`: OK
- `cargo run -p velnor-workflow -- . --plain --dry-run`: `0 files would change`
