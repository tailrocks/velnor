# VERDICT: MISMATCH — a2-provmech (G3+G5+G8+G9)

Branch `feat/a2-provision-promotion`, commit `a05b0e2a12405e737868a2bbf8872ce72169d674`
(fetched from origin; SHA matches the evidence file). Base `96ccc0f1` clean for
comparison. All proof in scratch worktrees under `/tmp` (`/tmp/v-a2-wt` @ a05b0e2a,
`/tmp/v-a2-base` @ 96ccc0f1, `/tmp/v-a2-regen` @ a05b0e2a, `/tmp/v-a2-consumer`
synthetic consumer, binary `/tmp/v-a2-bin`). No repo edits, no merges, no pushes.

Two independent disproofs, each reproducible in a clean checkout:

## 1. STALE ownership state — `--dry-run` = 1 change, `--check` exits 1 (claimed 0)

- Committed `.github/ci/.github-actions-generator-state` carries
  `scan 5fc04182098c3e1d` — byte-identical to the BASE commit's value — yet the
  bundle adds two scan-covered paths (`src/promote.rs`,
  `tests/promote_atomic.rs`), and the scan digest covers every tracked path
  (`scan_shape` → `canonical_json` → `files`; `repository_files` prefers the
  git index, `file_walk.rs`).
- Clean-checkout regen deterministically yields `scan 5774aa62dffb375e`
  (two consecutive `--force` runs identical). `--plain --dry-run` reports
  "1 file would change"; `--plain --check` exits 1:
  "generated files match but generation inputs changed: scan input changed
  from 5fc0… to 5774…". Base commit dry-run = 0 in the same environment.
- Index experiment proves the cause: `git rm --cached` the two new files
  (index = pre-`git add` state) → dry-run = 0. The author regenerated BEFORE
  staging the new files and never re-ran after `git add`. The committed state
  is stale; mainline `--check` is red on this commit.

## 2. `promote` cannot promote any real tree — G5 mechanism fails its sole purpose

- `promote.rs:167-175` calls `write_generated_with_options(..., force=false)`.
  `plan_generated_write_with_options` marks EVERY changed on-disk file a
  conflict, and `apply_generated_write_plan` refuses conflicts without force.
  `promote` has no `--force` flag, so there is no workaround.
- The `[generator] revision` pin IS embedded in rendered output (old pin found
  in 10 owner workflows; likewise in a freshly rendered synthetic consumer),
  so every genuine pin advance changes existing files → always conflicts.
- Reproduced twice with the binary's OWN revision (binding passes, write fails):
  - `/tmp/v-a2-regen` (owner tree): "would overwrite generated files:
    ci-main.yml, ci-policy.yml, …; rerun with --force after review", no commit.
  - `/tmp/v-a2-consumer` (synthetic workspace, old pin, generated files
    COMMITTED, then promote to own rev): same failure, no commit, tree restored.
- The bundled `promote_commits_pin_metadata_and_tree_atomically` passes only
  because its fixture never commits a prior render — all outputs are Creates,
  never Updates, so the conflicts guard never fires. The test is blind to the
  Update path that every real promotion takes.

## What verified CLEAN (not in dispute)

- G3: consumer-repo `git fetch … $GITHUB_REPOSITORY … $PINNED_REVISION` deleted
  (base `ci-unit-rust.yml:689` gone); rendered provisioner uses
  `PRODUCT_REPOSITORY` + product trees API with truncation guard; no
  `git fetch`/`$GITHUB_REPOSITORY` in the provisioner step (remaining fetches
  are pre-existing PR-ref/base-pin fetches, identical in base). No shims.
  All 4 provisioner tests pass, incl. the executable stubbed-network test.
- G5 binding: foreign pin refused naming render≡stamp, tree untouched —
  reproduced independently (`promote --rev 96ccc0f1…` → "refusing to stamp…",
  exit≠0, `git status` clean) as well as via the bundled test.
- G8: `platforms()` takes no config; hostile-label test passes; mapping file
  proven under `CLOSURE_PATHS`; `ci-runtime-products.yml` byte-identical
  (absent from the diff). Remaining `config.github_runner` uses are the
  non-compiling closure/publish orchestration jobs.
- G9: `crate_inputs_stay_closure_complete` + extended stamp test pass.
- Gates: full `cargo test --locked --all-features -p velnor-workflow` =
  553 passed / 0 failed (495+2+6+3+5+9+33); clippy `-D warnings` clean;
  `cargo fmt --check` clean; `actionlint` clean (exit 0).
- `GENERATOR_REVISION` 49 → 50; sibling surfaces untouched (no manifest,
  signer-line, or ordering changes in the diff).

## Minimal fix direction (for the author, not applied)

1. `git add -A && velnor-workflow . --plain --force` (post-staging regen),
   verify `--plain --dry-run` = 0 and `--check` = 0, amend.
2. Pass `force=true` in promote's writer call (promote already guards with a
   clean-tree requirement, snapshot restore, determinism + integrity proofs —
   force is the intended semantic there), and extend `promote_atomic.rs` with
   a fixture that commits a prior render before promoting (Update path).

Scratch kept for re-verification: `/tmp/v-a2-wt`, `/tmp/v-a2-base`,
`/tmp/v-a2-regen`, `/tmp/v-a2-consumer`, `/tmp/v-a2-bin`.
