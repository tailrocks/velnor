# Integration batch 7: feat/d1-scaleset-protocol merge (bastion campaign)

- Role: DESIGNATED INTEGRATION subagent. Source branch untouched; no campaign-branch
  source edits — only merge + generator regen of owned state.
- Start branch: `docs/bastion-final-plan` @ `1d3eb22e` (clean tree, verified).
- Source: `origin/feat/d1-scaleset-protocol` @ `966f3623` — SHA verified after
  `git fetch origin`; matches `/tmp/v-d1a.md` CERTIFIED record (26 files +4127/−7,
  base `411b8a99`, signed off).

## Pre-merge analysis

- Divergence `411b8a99..HEAD`: 8 commits (a2-consumer-negatives, signer-order,
  publish-403, docker-gha-cache merges) touching 13 files, all in
  `crates/velnor-workflow` + generated CI YAML.
- Overlap with source file set: **zero files** — clean merge expected.

## Merge

- `git merge origin/feat/d1-scaleset-protocol` → clean, ort strategy, no conflicts.
- Post-merge `--plain --dry-run`: **1 file would change** — only
  `.github/ci/.github-actions-generator-state` (`scan` hash, new scaleset/ sources).
- Regen (rebuilt generator, no hand edits):
  `cargo build --locked -p velnor-workflow`, then
  `./target/debug/velnor-workflow --plain --force .` → "Generated 21 files",
  exactly 1 line changed (`scan 6adfeeef0a7983b0` → `643e707f2d4ddfca`).
- Final `--plain --dry-run .` → **0 files would change**, exit 0.
- Amended merge: staged regen'd state + `git commit --amend -s --no-edit`
  (Signed-off-by present, matching prior integration merges).

## Gates at pushed bytes (`34c44fa7`)

- `--plain --dry-run`: `0 files would change`, exit 0
- `cargo test --locked -p velnor-workflow`: **525 lib** + 2/6/5/9/33
  integration, 0 failed (matches batch-5 baseline)
- `cargo test --locked -p velnor-runner`: **2212 lib** + 9/1/2/2/4/2/2
  integration targets, 0 failed (docker-API warning line is an expected
  negative-path test log, not a failure)
- `cargo test --locked -p velnor-runner --features test-support --test scaleset_protocol`: **8/8** pass
- `cargo test --locked -p velnor-model -p velnor-control`: 138 model lib +
  269 control lib (incl. v21 tests) + all integration, 0 failed
- `cargo clippy --locked -p velnor-workflow -p velnor-runner -p velnor-model -p velnor-control --all-targets -- -D warnings`: exit 0
- `cargo clippy --locked -p velnor-runner --all-targets --features test-support -- -D warnings`: exit 0
- `cargo fmt --all -- --check`: clean
- `actionlint` (repo-wide): exit 0, no findings
- `git diff --check`: clean

## Push

- `git push origin docs/bastion-final-plan`: `1d3eb22e..34c44fa7`, exit 0.
- Merge commit: `34c44fa74b40304cfb2c176f139b579e19b3c838`
  "Merge feat/d1-scaleset-protocol into docs/bastion-final-plan" (+ signoff).
