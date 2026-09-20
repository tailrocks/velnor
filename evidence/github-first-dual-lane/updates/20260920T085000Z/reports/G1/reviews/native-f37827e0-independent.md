# Independent native f37827e0 review

## Verdict

REJECT for publication. No publication/install/dispatch approval.

The duplicate Debian subject and stable rerun string-ID fixes work in the
tested rendered paths. The new rendered source-ref gate is broken and aborts
the nominal stable native product publisher under `set -euo pipefail`.
The shared signer/native-to-APT provenance boundary remains unresolved.

## Exact scope

- HEAD: `f37827e09415ba5733b37e97a16aef716e553322`
- Parent: `d402d57afedd3898467502337d8dbbef8f3a6ade`
- Tree: `a924837af55572dc21fc6997d615b24c2be68a4d`
- Remote branch `origin/codex/github-first-native-product-v3` resolves to the
  exact HEAD.
- Review tree: `/private/tmp/velnor-native-f378-review` (detached, clean).
- Diff from d402: one source file, `release.rs`, `+111/-7`.
- Valid render: `/private/tmp/native-f378-generated.azfWaJ`; durable generated
  copies are in this directory. Rendered release SHA-256:
  `1ab2ac9886b153c4a8eee615bb47ec75f4079a089ea053340acadcad10a9ac39`.

## Blocking findings

1. **Stable native publication cannot pass its new source-ref check.**

   Source template `crates/velnor-workflow/src/s2/primitives/release.rs:2427-2430`
   injects `[ "$source_ref" = "$GITHUB_REF" ]` into the native census step.
   The exact rendered shell has the check at generated `release.yml:4328`, but
   the preceding lines `4321-4324` assign only `product_version`,
   `source_commit`, `release_tag`, and `source_repository`. The publish job
   exports uppercase `SOURCE_REF` at `release.yml:4049`; shell variables are
   case-sensitive and each `run` step has `set -u`.

   Direct execution with the generated job environment gives `rc=1`:
   `bash: source_ref: unbound variable`. Evidence:
   `native-f37827e0-source-ref-repro.txt`. This is a real nominal-flow blocker,
   not a string-only concern; the full native product census cannot reach
   provider admission.

2. **Native attestation still does not use the shared package signer contract.**

   Stable native product assets are directly attested by
   `actions/attest-build-provenance` in generated `release.yml:4537-4540` and
   verified only with `gh attestation verify ... --repo` at `4541-4548`.
   The Debian path uses the shared
   `ci-release-package-signer.yml` at `4029-4034` and verifies its signer at
   `4093`. Preview native assets instead name `preview.yml` as signer at
   generated `preview.yml:1188-1199`. Thus no common signer/source-digest
   handoff is proven for native product assets; the emitted
   `release-attestation.json` remains a producer self-report, not that missing
   shared cryptographic handoff. This remains the acknowledged blocker.

3. **Checked-in generated tree is not at this renderer output.**

   `velnor-workflow --plain --check ../..` fails with the five known stale
   files: `native-product-preview.yml`, `native-product.yml`, `preview.yml`,
   `release.yml`, and `.github/ci/.github-actions-generator-state`. The valid
   external render itself succeeds. This is a generated-drift gate failure,
   not approval evidence.

## Fixes independently validated

- Generated stable assembly line `release.yml:4476` records Debian package
  rows but no longer copies the package into `product-assets`. The focused
  rendered assembly test passed and asserts both `amd64`/`arm64` Debian files
  are absent from `product-assets` (`release.rs:7793-7802`). This removes the
  duplicate basename collision while leaving root release inventory intact.
- Generated existing-release reconciliation at `release.yml:4709` admits
  only a positive decimal **string** release ID. Executing that exact line
  accepted `{"release_id":"12345"}` -> `12345`, rejected numeric `12345`,
  and rejected `12x`. Evidence:
  `native-f37827e0-rerun-results.txt`.
- Extracted actual generated stable boundary shell passed nominal draft and
  post-flip publication with a realistic native product inventory. It rejected
  wrong provider ID/source/tag/ref, missing/extra/duplicate assets, changed
  bytes/size, concurrent draft flip, post-read identity drift, and post-read
  byte replacement. Evidence:
  `native-f37827e0-boundary-synthetic-results.txt` and script hash
  `ae93cb16e7d5a47948125c0f1f7b74b04a2dfdf1bd644e3059ecfb747f8af144`.
- The current Mac policy did not regress: generated native stable/preview
  workflows explicitly block `x86_64-apple-darwin` with “macOS 27 Intel is
  unavailable; no fallback runner is permitted” and retain the arm64
  `xcode-27` lane.

## Checks

- Focused native tests: **65 passed**, 1679 filtered; rendered assembly test:
  **1 passed**.
- `cargo check --locked -p velnor-workflow`: pass.
- `cargo fmt --manifest-path crates/velnor-workflow/Cargo.toml -- --check`:
  pass.
- `cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings`:
  pass.
- Configured actionlint over all generated workflow YAML, using the generated
  `.github/actionlint.yaml` unchanged: **pass, rc=0, no output**.
- Bare actionlint, with no config and no fake labels: **rc=1**, 25 diagnostics
  (23 unknown `velnor-target-mvp`, 2 unknown `xcode-27`). Per-file configured
  / bare: `native-product.yml` 0/1,
  `native-product-preview.yml` 0/1, `preview.yml` 0/0, `release.yml` 0/1
  (17 Velnor-label diagnostics). This is the known custom-label distinction;
  the configured check was not weakened.
- No live provider, release, install, dispatch, or host-payload execution.

Durable logs, generated workflow bytes, hashes, and fixture scripts are under
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/reviews/`
with `native-f37827e0-` names.
