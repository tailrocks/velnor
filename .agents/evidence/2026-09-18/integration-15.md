# INTEGRATION-15 evidence: MAIN-MERGE (campaign + dead5ecb)
Date: 2026-09-17. Branch docs/bastion-final-plan 04607e6b -> 8b7b4ac1 (pushed).
Target verified: origin/main = dead5ecb7baff1a5d28975bb81ee2832690b7e4a (fetched first).

## Merge commit
8b7b4ac1450051dda5d39ca124dc8b011a0d102d (signed, -s). 79 files staged.

## Conflicts (7 files, 29 markers — matched drift doc exactly)
Source (hand-resolved, both features kept):
- config/mod.rs (2): ReleaseSection keeps campaign apt_* + retention fields AND
  #917 dockerfile/platforms/modes/archive/credential/registry fields; new
  CredentialSection/DocsSection/CheckProfileSection structs+impls kept; getters
  concatenated in both impl blocks.
- lib.rs (5): `mod promote` + `mod reuse`; ReleaseSpec struct concatenated;
  apply_release keeps campaign `-> Result<(), GeneratorError>` signature (caller
  uses `?`, body uses `?` for retention + AptContract::resolve) with #917's
  `#[allow(clippy::too_many_lines)]`; `declared` predicate and assignment block
  concatenated (added missing `}` closing campaign's apt-validation `if`).
- runtime.rs (3): `promote` + `prepared-tool-install` dispatch arms; release
  usage string + match carry campaign apt-* verbs + #917 verbs; campaign
  apt-publish ReleaseSpec literal adapted with image_package + 15 docker fields.
- release.rs (12): schema keys interleaved alphabetically; declared_spec runs
  #917 declared_registry_auth prelude then `let spec` for campaign apt
  validation + Ok(spec); preview spec concatenated; incomplete_contract gains
  apt + docker arms; release_contract_complete keeps campaign's FULL apt gate
  (8-field) + #917 docker gate; fixtures: docker_spec (new, main) completed
  with apt defaults.
Generated (regen ONLY, never hand-edited):
- Staged all, scaffolding main's bytes for the 3 marker files (generator refuses
  marker input), `--plain --force` regen from merged-tree binary, 21 files.
- Regen reproduced the merge byte-identical for ALL non-conflicted files; only
  the 3 conflicted files changed (MM).

## Semantic reconciliations (beyond textual)
1. apply_release signature: Result wins (campaign validation + `?` caller).
2. release_contract_complete apt arm: campaign's strict 8-field gate wins over
   base/main's 2-field gate (main did not touch it); docker arm added.
3. "Fetch D19 pin history" step absent from regen: campaign already removed it
   (HEAD=0, base/main=1) — regen correctly follows campaign, not a loss.
4. Campaign ReleaseSpec literals (runtime apt-publish, apt.rs fixture, 2
   release.rs tests) adapted to #917 fields; main literals adapted to apt fields.
5. Regen-vs-main release.yml = campaign closure/policy render (pin-from-toml,
   API closure proof, --source-ref, check_slot); regen-vs-HEAD = #917 additions
   (apple_executor, unit_dependencies, image-admission, dispatch gating).

## Gates (proven in working tree AND clean scratch worktree @ 8b7b4ac1)
- cargo build --workspace: 0 errors (both)
- cargo test -p velnor-workflow: 970 passed, 0 failed (both)
- cargo test -p velnor-runner: 2371 passed, 0 failed (both)
- cargo test -p velnor-runner --features test-support: 2489 passed, 0 failed
- clippy --workspace --all-targets -- -D warnings: 0 findings (both)
- cargo fmt --check: clean (both)
- actionlint (.github/actionlint.yaml): exit 0 (both)
- generator --plain --dry-run: 0 files would change (both)
- generator --plain --check: current (both)
- scratch worktree removed after proof; pushed 04607e6b..8b7b4ac1.
