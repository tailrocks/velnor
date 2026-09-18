# VERDICT: CERTIFIED — a2-signer (G2 + G4)

Branch `feat/a2-signer-order` @ `f79ab248`, verified in scratch worktree
`/tmp/a2-verify-wt` (detached HEAD, no edits to any checkout; no merges/pushes).

## Identity
- `git fetch origin`: `origin/feat/a2-signer-order` == local `feat/a2-signer-order` ==
  `f79ab248` + `754c1cd6`, exact SHAs as claimed. Both `Signed-off-by`.
- Base: parent `754c1cd6~1` == `38ffbfd7`, an ancestor of
  `origin/docs/bastion-final-plan` (`96ccc0f1`). NOTE: docs tip moved two
  merges ahead (fix/pin-fetch-in-tool, fix/preview-guest-upload) after the
  fork; branch needs a rebase before landing. True branch diff
  (`754c1cd6~1..f79ab248`): 9 files, 371+/91- — matches the evidence doc's
  file list exactly. Tip-to-tip diff additionally shows reversal of the two
  fix merges (expected from stale base, not author content).

## G2-producer: default-branch guard (754c1cd6)
- Rendered `ci-runtime-products.yml:46`: `Prove the default-branch ref` is the
  FIRST closure step, reads `REF: ${{ github.ref }}`, fails closed unless
  `refs/heads/main`. Rendered from `config.default_branch` (trunk-following
  test passes).
- Fail-loud step, not a silent job-level `if` — confirmed in template.

## G4: verify-before-create (754c1cd6)
- Rendered publish job order: `Assemble and verify` (213) < `Attest release
  manifest` (248) < `Smoke-test the release` (252) < `Create the release`
  (280, `gh release create` @296). Attestation + smoke BEFORE create: YES.
- `gh release create`: exactly 1. `gh release download`: 0 in publish.
  `skipped` output: 0 repo-wide in rendered file. Never-overwrite re-check
  immediately precedes the create. Smoke verifies local `dist/` bytes.

## G2-consumer: --source-ref pin (f79ab248)
- Setup action (source + static copy, byte-identical): both `gh attestation
  verify` lines gain `--source-ref refs/heads/main` (2 occurrences each).
- Velnor provisioner (`lib.rs:4522`): both verify lines gain the pin
  (exactly 2 occurrences on the line).
- Regenerated embeds: `ci-unit-rust.yml` + `release.yml` diffs are exactly
  the two flag-addition lines each; no other file embeds the provisioner
  (preview.yml has 0 `attestation verify` on branch AND docs tip).
- Smoke test pins the same ref on both subjects (2 occurrences, trunk-following).

## --signer-ref absent / --source-ref correct (independent live probes, gh 2.100.0)
- `gh attestation verify --signer-ref ...` → `unknown flag: --signer-ref`.
  `--help` lists `--source-ref` ("Enforce that the git ref ... matches").
  Only `--signer-ref` string on branch: one explanatory comment.
- Live, real product attestation (`...-9f236b40635970a4` manifest):
  baseline (no pin) exit 0 → hole exists; `+ --source-ref refs/heads/main`
  exit 1 (`expected SourceRepositoryRef to be refs/heads/main, got
  refs/heads/chore/proof-candidate-path-3`) → pin rejects branch-built;
  `+ --source-ref refs/heads/chore/proof-candidate-path-3` exit 0 → pin
  accepts matching ref. Mechanism confirmed, matching the evidence doc.
- Product default branch: `main` (`gh repo view tailrocks/velnor`).

## Proof suite (scratch worktree @ f79ab248)
- `cargo build -p velnor-workflow`: exit 0.
- `cargo run -p velnor-workflow -- . --plain --dry-run`: **0 files** (no hand edits).
- 7 new/updated tests named explicitly: all pass (incl. executable guard
  run over main/trunk/tag/empty refs and `bash -n` over all 8 shell bodies).
- `cargo test -p velnor-workflow`: 492+2+6+5+9+33 = 547 passed, 0 failed
  (matches evidence doc).
- `cargo clippy -p velnor-workflow --all-targets`: exit 0, zero warnings.
- `cargo fmt -p velnor-workflow --check`: clean.
- `actionlint` on ci-runtime-products.yml, ci-unit-rust.yml, release.yml: clean.

## Landing notes (not certification blockers)
1. Rebase onto current `docs/bastion-final-plan` tip before landing; re-run
   generator (`--dry-run` must stay 0) after rebase.
2. Evidence-doc landing sequence (producer commit → delete+rebuild live
   branch-built `9f236b40…` release from main → consumer commit) stands:
   re-verified that release is branch-built and that the main pin rejects it.
