# INTEGRATION-4: merge feat/a2-consumer-negatives into docs/bastion-final-plan

- Certification read first: `/tmp/v-a2-negtests.md` — CERTIFIED, branch
  `feat/a2-consumer-negatives` @ `4284f246f00f0e07d299f5536cc4a479f839ac23`
  (parent `38ffbfd7`, +1495/−0, test-only).
- Workspace: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`,
  branch `docs/bastion-final-plan`, pre-merge HEAD `40ff66ec`.
- `git fetch origin` done before merge and before push; origin did not move
  during the work (push fast-forwarded `40ff66ec..89773efc`).

## Merge

- Merge commit: `89773efcd8fc86e9161a0f0b00eefedbf93eba77`
- Parents: `40ff66ec` + `4284f246` (exact certified SHA).
- Message: `Merge remote-tracking branch
  'origin/feat/a2-consumer-negatives' into docs/bastion-final-plan` +
  `Signed-off-by` (`git commit -s`, non-interactive via `GIT_EDITOR=true`
  after killing a stuck editor spawn of the first attempt).
- The 2-line `lib.rs` mod decl merged CLEAN (no conflict); staged diff is
  exactly the certified `#[cfg(all(test, unix))] mod consumer_negatives;`.
- Pushed: `origin/docs/bastion-final-plan` = `89773efc` (push exit 0).

## Adaptation (required: HEAD moved the consumer contract past the base)

The certified module was written against base `38ffbfd7`; HEAD contains the
signer-order + source-identity merges, so the module did not compile or
agree with the scripts under test until adapted in source (all inside the
new test-only file; zero production lines touched):

1. `workflow_pinned_policy_runtime_velnor(revision, checkout)` → `(checkout)`:
   `velnor_provisioner_script` takes only checkout; asserts the env carries
   `CHECKOUT_PATH` and the pin parses from `.github-gen/velnor-workflow.toml`.
2. `run_velnor_provisioner` writes the fixture pin to
   `$CHECKOUT/.github-gen/velnor-workflow.toml` (outside the closure
   pathspec, uncommitted) and drops the dead `PINNED_REVISION` env.
3. `release_manifest` gains the `revision` field (11 call sites); the new
   accept filters require 40-hex `revision`.
4. `stub_binary_script` answers `--revision` too (4 call sites); the new
   `--revision` self-report gates in the setup action and provisioner need it.
5. Stub `gh` refuses `attestation verify` without
   `--source-ref refs/heads/main` (new pin dimension); N4 ×2 + cold-consumer
   assert the ref pin in the stub log. Candidate tail has no attestation
   calls, so no path breaks.
6. `#[expect(clippy::too_many_lines)]` on the cold-consumer test (103 lines
   after adaptation; same pattern HEAD's lib.rs already uses).
7. `cargo fmt` applied (release_manifest call reflow).

## Generator state (sanctioned writer only, no hand edits)

- Post-merge `--plain --dry-run` reported 1 file: only the ownership state.
  Attribution: current binary on a clean `40ff66ec` scratch worktree = 0,
  so the merge's new source file moved the `scan` digest.
- Regen: `cargo run -p velnor-workflow -- --plain --force .` changed exactly
  one line (`scan 5fc04182098c3e1d` → `6adfeeef0a7983b0`); all 20 generated
  outputs byte-identical. Scratch worktree removed.
- Final `--plain --dry-run`: **0 files would change** (re-verified after the
  last source edit; scan covers inventory, not `.rs` contents).

## Gates (all on the final tree, all green)

- `cargo test -p velnor-workflow`: lib **518 passed / 0 failed**
  (499 HEAD + 19 consumer_negatives), integration 2+6+5+9+33, 0 failed.
- `cargo test -p velnor-workflow --lib consumer_negatives`: **19/19**.
- `cargo clippy -p velnor-workflow --all-targets --all-features -- -D warnings`:
  exit 0, 0 errors.
- `cargo fmt -p velnor-workflow -- --check`: clean.
- `actionlint`: exit 0, no findings.

## Files in the merge commit (vs first parent)

- `A crates/velnor-workflow/src/consumer_negatives.rs` (1604 lines,
  adapted + certified suite)
- `M crates/velnor-workflow/src/lib.rs` (+2 mod decl)
- `M .github/ci/.github-actions-generator-state` (1-line scan re-pin)
