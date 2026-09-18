# INTEGRATION-1 — merge fix/preview-guest-upload + fix/pin-fetch-in-tool into docs/bastion-final-plan

## Inputs (both independently certified, read first)
- `/tmp/v-fix-preview.md`: MATCH / CERTIFIED — `fix/preview-guest-upload @ 6be652b5` on base `38ffbfd7`
- `/tmp/v-fix-stalerev.md`: CERTIFIED — `fix/pin-fetch-in-tool @ 48b49d92` on base `38ffbfd7`
- Verified pre-merge: `git fetch origin`; local `docs/bastion-final-plan == origin == 38ffbfd7`,
  both fix refs matched the certified hashes exactly. Working tree was clean.

## Merges (shared workspace /Users/donbeave/Projects/tailrocks/velnor-project/velnor3)
1. `c5a67ed4` — Merge fix/preview-guest-upload into docs/bastion-final-plan (clean, no conflicts)
2. `96ccc0f1` — Merge fix/pin-fetch-in-tool into docs/bastion-final-plan
   - Only conflict: `.github/ci/.github-actions-generator-state` (hash lines, both sides).
     `release.rs` and `release.yml` auto-merged; verified the auto-merge combined both
     generator logics (agent_bin upload block + live-pin provisioner both present).
   - Resolution per protocol: NO hand-edit of generated files. `git checkout HEAD` the state
     file (regen input only — `--force` refuses to run on conflict markers), rebuilt the
     generator (`cargo build --locked -p velnor-workflow`), ran
     `./target/debug/velnor-workflow --plain --force` ("Generated 21 files"), staged result.
   - Regen was byte-identical to git's auto-merge of release.yml (no unstaged diff after regen).
- Both merge commits carry `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` via `git commit -s`
  (merge 1 amended once to add the signoff before the redo; nothing had been pushed).
- Pushed: `38ffbfd7..96ccc0f1 docs/bastion-final-plan -> docs/bastion-final-plan`;
  `HEAD == origin/docs/bastion-final-plan == 96ccc0f1`, tree clean.

## Gates (all on final tree @ 96ccc0f1, observed)
- `./target/debug/velnor-workflow --plain --dry-run` → `0 files would change`, exit 0
- `cargo test --locked -p velnor-workflow` → 489 lib + 2/6/5/9/33 integration = 544 passed, 0 failed
- `cargo test --locked -p velnor-runner --bin velnor-guest-image` → 9 passed, 0 failed (covers merged builder hardening)
- `cargo clippy --locked -p velnor-workflow -p velnor-runner --all-targets -- -D warnings` → exit 0, 0 warnings
- `cargo fmt --check` → clean
- `actionlint preview.yml release.yml ci-unit-rust.yml` (all 3 touched workflows) → exit 0

## Content sanity (final tree)
- Explicit `dist/microvm/velnor-guest-agent` upload entries present in preview.yml (3) and release.yml (3); no `dist/microvm/*` wildcard reintroduced (full suite's rejection test passes).
- Live-pin `velnor-workflow.toml` parse present in release.yml (2) and ci-unit-rust.yml (3); `Fetch D19 pin history` step absent (negative tests pass).
