# Homebrew source-review remediation

Source review: `b1c94246216d0006e9b7639b1a4a294212ce0bf7`.
Corrected source commit: `c772971de3df714b33bffb55febfcfe478428175`.

## H1 — SemVer

The updater now rejects leading-zero core identifiers and numeric prerelease
identifiers in product and component versions. Stable and preview checks use
strict canonical forms. Monotonic comparison compares arbitrary-length decimal
components by length and lexicographic order; fixed-width numeric padding is
gone. The fixture executes stable `01.2.3`, preview
`preview.01`, and component `01.0.0` hostile inputs.

## H2 — same-version manifest binding

The generated formula's `PRODUCT_MANIFEST_SHA256` is now part of the
same-version identity check. A fixture mutates only a non-Homebrew canonical
artifact row, refreshes the producer attestation, and proves the updater
rejects the same version despite unchanged source, tag, release ID, and
Homebrew archive.

## M1 — executable fixture proof

Fixtures now contain executable shell payloads that expose version/source
identity, `velnorctl --help`, and `velnorctl host start` sibling discovery.
The test extracts all three binaries, installs subordinate records into a
temporary Homebrew-shaped tree, runs sibling/version/manifest checks, parses
the generated formula with Ruby, and removes the temporary tree. This is local
payload and generated-formula proof only; clean published macOS Homebrew
install/upgrade/switch/rollback/uninstall remains required.

## M2 — provider/source/provenance binding

The producer handoff now requires an exact-key
`release-attestation.json` with GitHub provider, immutable release URL/id/tag,
requested and resolved source ref/commit, canonical manifest digest, and exact
canonical asset census. The updater rejects missing, stale, unresolved, or
asset-mismatched attestations. `target_commitish` is retained as diagnostic
metadata and is never accepted as source resolution. A producer/API-generated
attestation and later real GitHub clean-client validation remain external
dependencies.

## M3 — Intel migration

Generated formulas carry a target marker. The updater refuses replacement of a
historical source-build formula without that marker unless the operator
explicitly supplies `VELNOR_ALLOW_ARM64_FORMULA_REPLACEMENT=1`. No Intel
artifact or support claim was added; native Intel artifact and clean-client
proof remain required.

## Executed checks

From `tailrocks/homebrew-velnor`:

```text
rtk proxy bash -n scripts/package-update.sh
rtk proxy bash -n scripts/test-package-update.sh
rtk proxy shellcheck scripts/package-update.sh scripts/test-package-update.sh
rtk proxy ./scripts/test-package-update.sh
rtk proxy ruby -c Formula/velnorctl.rb.template
rtk proxy ruby -c Formula/velnorctl-preview.rb.template
rtk proxy jq empty config/homebrew-release-contract.json
rtk proxy git diff --check
```

All pass at the corrected source commit. No remote push, publication, or
published install was performed.
