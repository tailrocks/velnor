# A2 implementation evidence: G2 (signer ref) + G4 (verify-then-create)

Branch: `feat/a2-signer-order` (from `docs/bastion-final-plan`, isolated worktree)
Commits (both `Signed-off-by`):
- `754c1cd6` feat(a2): constrain runtime producer to default branch, verify before release (G2-producer + G4)
- `f79ab248` feat(a2): pin consumer attestation verification to default-branch ref (G2-consumer)

## G2 design correction (measured, live): `--signer-ref` does not exist

The gap table prescribes "`--signer-ref`/cert-identity pinning". Verified against
gh 2.100.0 (2026-09-03, current): `gh attestation verify --help` lists no
`--signer-ref`, and passing it fails with `unknown flag: --signer-ref`.
The constraining flag is `--source-ref` ("Enforce that the git ref associated
with the source repository matches the provided value").

Proven live against real product attestations (no mocks):
- Baseline (today's consumer command, no ref pin) on release
  `velnor-workflow-runtime-v1-9f236b40635970a4` asset: exit 0 (verifies).
- Same + `--source-ref refs/heads/main`: FAILS with
  `expected SourceRepositoryRef to be refs/heads/main, got refs/heads/chore/proof-candidate-path-3`.
- Same + `--source-ref refs/heads/chore/proof-candidate-path-3`: exit 0.
- Manifest attestation behaves identically (branch ref, exit 0 with branch pin).
So `--source-ref` accepts matching refs and rejects others: it is the pin.

Product-repo default branch verified: `gh repo view tailrocks/velnor` →
defaultBranchRef `main`. Pin value everywhere: `refs/heads/main`.

## G2 blast radius (probed all 14 runtime releases, manifest attestations)

Main-built (pass `--source-ref refs/heads/main`): 2
- `velnor-workflow-runtime-v1-93c8a35808d1210b`, `velnor-workflow-runtime-v1-fd45efac853b6570`

Branch-built (verify today, correctly rejected after G2): 12
- `...-9f236b40635970a4` ← chore/proof-candidate-path-3 — **LIVE**: origin/main
  HEAD closure, pinned bootstrap rev `7341ef4` (on main) resolves to it
- `...-ff27a1c010cb4faf` ← chore/proof-candidate-path-2
- `...-1ff514a2852c82d3`, `...-78673ee16c42581e` ← chore/proof-candidate-path
- `...-c0ce56f09b27859c`, `...-6ae4ca31ef453567` ← fix/policy-action-pins-sibling-path
- `...-ad493401f003a130` ← fix/policy-setup-action-path
- `...-d39cbf21e351b410`, `...-e5ad6f4f923cd154`, `...-2bd48976b59bf084`,
  `...-f1f88c200e5b3b82`, `...-203eb9b79141e5a2` ← feat/ci-immutable-runtime-products

## LANDING SEQUENCE (read before merge — deadlock hazard)

The live closure `9f236b40…` (origin/main HEAD) has only a branch-built
product, and the producer's tag-exists skip + never-overwrite rule means it
can never be rebuilt once any crate change lands (producer builds HEAD only).
Land in this order, or CI bootstrap fails closed with no self-serve recovery:

1. Land `754c1cd6` (producer guard + verify-before-create) alone. Safe:
   main pushes unaffected; branch dispatches fail loudly; smoke verifies
   same-run attestations.
2. Maintainer manual step (do ASAP, while origin/main HEAD still has closure
   `9f236b40…`): `gh release delete velnor-workflow-runtime-v1-9f236b40635970a4
   --repo tailrocks/velnor --yes` (+ delete the tag), then dispatch the
   producer from `main` (or push a docs-only commit) to rebuild main-attested.
   Transient window: bootstrap reports the precise "no runtime product"
   producer defect until the rebuild lands. Safe under current (unpinned)
   consumers, so this step can also run BEFORE step 1.
3. Land `f79ab248` (consumer `--source-ref` pin). All consumers then reject
   branch-built attestations.
4. Leave the other 11 stale branch-built releases in place: their closures
   are dead, and post-G2 they correctly fail closed. Any consumer still
   pinning one must advance its pin to a main-built product (correct per
   spec §3.2 "trusted signer workflow/ref").

Out of scope / sibling coordination: release.rs `--signer-workflow`
(package-signer for `v*` daemon/source releases) is a separate release
identity (G10), untouched. Consumer negative execution suite is G6
(sibling bundle); transition sequencing with other A2 authors' crate
changes must respect step 2's closure precondition.

## Changes

Commit `754c1cd6` (G2-producer + G4):
- `runtime_products.rs`: closure job opens with "Prove the default-branch
  ref" guard step (fails closed unless `github.ref == refs/heads/<default>`,
  rendered from config; fail-loud step chosen over silent job-level `if`,
  which would also interact badly with the `exists != 'true'` gates).
- `runtime_products.rs` publish job reordered to verify-then-create:
  Assemble-and-verify → Attest manifest → Smoke-test (local `dist/` bytes,
  no `gh release download`) → Create the release (never-overwrite re-check
  immediately before `gh release create`). `skipped` output removed (no
  consumers remain). Smoke adds `--source-ref refs/heads/<default>`.
- Regenerated `.github/workflows/ci-runtime-products.yml` via generator
  only (no hand edits); reviewed diff.
- Tests: guard static + trunk-following; ordering test
  (assemble < attest < smoke < create, single create, no download in
  publish, no `skipped`); smoke ref pin (+trunk); executable
  `rendered_shell_parses` (bash -n over every rendered `run: |` body,
  keyed by step name); executable
  `default_branch_guard_accepts_only_the_default_ref` (runs the RENDERED
  guard under bash with refs main/trunk/tag/empty → accept/reject).
  Render digest pin deliberately refreshed.

Commit `f79ab248` (G2-consumer):
- `.github-gen/sources/actions/setup-velnor-workflow/action.yml`: both
  `gh attestation verify` commands gain `--source-ref refs/heads/main`
  (+ header comment).
- `lib.rs` Velnor provisioner: same two-line pin.
- Regenerated via generator only: `.github/actions/...` static copy +
  `ci-unit-rust.yml` + `release.yml` (only files embedding the
  provisioner; diffs are exactly the two flag additions each).
- Tests: provisioner exact-line pin, setup-action manifest flag list,
  new `all_consumers_pin_the_same_producer_ref` (setup action + Velnor
  provisioner + smoke each pin both subjects), Velnor-lane integration
  pin in `velnor_first_ci.rs`.

## Verification (final tree)

- `cargo test -p velnor-workflow`: 492 + 2 + 6 + 5 + 9 + 33 = 547 passed,
  0 failed (commit-1 tree also fully green: 546/0).
- `cargo clippy -p velnor-workflow --all-targets`: exit 0, zero warnings.
- `cargo fmt -p velnor-workflow --check`: clean.
- `actionlint` on ci-runtime-products.yml, ci-unit-rust.yml, release.yml:
  clean. (actionlint cannot parse composite action.yml — structural,
  pre-existing; shellcheck on action scripts: only pre-existing
  info-level SC2153 on env-provided vars.)
- `cargo run -p velnor-workflow -- . --plain --dry-run`: **0 files**.
- No YAML hand-edits: all `.github/workflows/*` and `.github/actions/*`
  changes produced by the generator (`--plain --force` after review).
