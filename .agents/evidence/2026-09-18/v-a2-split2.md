# V-A2 split2: independent re-review of restructured PR #916 @ 1bee4f23

Date: 2026-09-17. Inputs: /tmp/pr916-red.md, /tmp/pr916-fix.md, /tmp/v-a2-split.md.
Reviewer scratch worktree: /tmp/v-a2-split2-wt @ 1bee4f23 (detached, PR head).
No repo edits made (main checkout untouched; validation only, no merge).
Verdict: **MISMATCH (not green, not safe to merge as-is)**.

## Net-vs-base: PASS (matches the prescribed phase-1 shape)

- Fetched origin; `origin/feat/a2-producer-revision` = `1bee4f237277fe32194c313b6bdaffdfd43eb7dc` as claimed.
- Base: merge-base HEAD..origin/main = `33688938` (PR #914 merge).
- Net diff base..head = exactly 3 files (no more, no less):
  - `crates/velnor-workflow/src/primitives/runtime_products.rs` (generator change),
  - `.github/workflows/ci-runtime-products.yml` (93-line genuine new-generator render),
  - `.github/ci/.github-actions-generator-state` (1 hash line for the above).
- Pin REVERTED: `.github-gen/velnor-workflow.toml` diff vs base is empty; head = base = `7341ef4bdf750c1fbe419e94fb3848c5b8dde718`.
- No self-bump: `git grep 51e635af <head>` returns NONE; `ci-pr.yml` (4x) and `ci-policy.yml` (3x) embed only the old pin.
- No consumer changes: `git diff --quiet` IDENTICAL for both setup-action copies (`.github/actions` + `.github-gen/sources`), `lib.rs`, `primitives/release.rs`; only Rust file changed is `runtime_products.rs`; `MANIFEST_ACCEPT_FILTER` string identical (line 74 -> 76).
- Commit `1bee4f23` is a new signed commit (Signed-off-by, DCO PASS), not an amend; its own 13-file diff is the revert of `f4d46b3b` vs its parent, netting to the 3 files above vs base.

## Gates rerun in scratch worktree (all observed, all green)

- `cargo test -p velnor-workflow`: 546 passed (491 lib + 0 main + 2 + 6 + 5 + 9 + 33), 0 failed. Exit 0.
- `cargo clippy -p velnor-workflow --all-targets`: exit 0, no warnings.
- `cargo fmt -p velnor-workflow --check`: exit 0.
- `actionlint` (whole tree): exit 0.
- `cargo run -q -p velnor-workflow -- --plain --dry-run`: exit 0, "0 files would change".
- `--plain --check`: exit 0, "Generated files are current", PLUS the designed notice: tree matches the CANDIDATE render (`7dcabc83ea2c2a6b…`, same closure as the red-diagnosis candidate artifact), not the declared pin; bump revision after merge.

## Fresh PR CI status (recorded 2026-09-17, head 1bee4f23)

- State OPEN, mergeable MERGEABLE, mergeStateStatus BLOCKED.
- `Control / Planning` (run 35175925732): PASS (10s). Log: `INSTALL_REV=7341ef4b…` (12 hits, zero `51e635af`) — old-pin product, chicken-and-egg gone.
- `Rust · velnor-workflow / GitHub`: PASS (new generator compiles, tests pass, candidate publishes).
- `DCO`: PASS. All other GitHub-lane units: PASS.
- `Policy` (run 35175919250, `pull_request_target` @ 1bee4f23): FAILURE (11s):
  - `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` empty (same-closure early exit taken).
  - `FAIL generated-tree: the tree differs from the render of velnor-workflow at 7341ef4b…`, naming exactly the 2 kept regen files (state + `ci-runtime-products.yml`); other 10 rules PASS (`policy: 11 rules, 1 failed`).
- Velnor-lane `FAILURE`s (`Docker/Documentation/OpenTofu/Velnor`, `Prepare Cargo/prepare-cargo`): all `Velnor rejected job (operational_store)` before workflow execution ("no declared workflow command was executed") — admission-gate signature, outside this diff's blast radius (velnor-workflow source + runtime-products workflow only), not caused by this PR.

## Phase-1-green prediction: FALSIFIED (path TRUE, green FALSE)

- "Planning uses old pin product": TRUE (INSTALL_REV + PASS, verified above).
- "Policy same-closure path": PATH TRUE — pin == base pin so `ci-policy.yml` Acquire takes the `pin_closure == base_closure` early exit (empty manifest, no `PINNED_BINARY` pointer; confirmed in workflow lines 78-81 and the CI log env). But the path yields FAIL, not green.
- Root cause independently verified in code: `policy.rs render_with_candidate` (lines 1469-1545) tries only `[pinned_binary?, current_exe]`; in this path both slots are empty-or-base-release, while `wanted` is the audited tree's CANDIDATE closure (`closure.rs`: release vs candidate namespaces never collide) — `reported != wanted` always, exception dead, `Differences` returned. Locally the NEW binary is manifest-exempt and self-recognizes as Candidate (hence local `--check` 0 + notice); CI Policy runs the OLD base product where self-recognition can never fire. The red diagnosis assumed a render-neutral generator; the render is NOT neutral (93-line producer diff), so same-closure byte-identity is impossible for this shape.
- This confirms (not just repeats) the fix report's falsification: same 2 files, same empty-manifest mechanism, same closure `7dcabc83…` in the local notice and the old candidate artifact name.

## Verdict

MISMATCH. The restructure is faithful (net diff, pin revert, no consumers, all local gates green) but the phase-1-green prediction does not hold: Planning is green, Policy is red by construction for old-pin+new-render, so 1bee4f23 cannot land green and is not certified safe to merge as-is. The only Policy-passing phase-1 shape for a render-changing generator is source-only (revert the 2 regen files too, leaving ONLY `runtime_products.rs`; local `--check` then exits 1 by design until the phase-2 pin bump — a parent decision, NOT pushed here).
