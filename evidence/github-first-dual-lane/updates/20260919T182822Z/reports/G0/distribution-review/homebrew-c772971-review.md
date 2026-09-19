# Homebrew c772971 source review

Observed: 2026-09-20. Read-only review. This is a source-contract verdict,
not publication or native-install approval.

## Exact input

- Consumer: `tailrocks/homebrew-velnor`
- Reviewed commit: `c772971de3df714b33bffb55febfcfe478428175`
- Historical formula: `7af1249f3d69c9f2e548583cdc9f3e737da41b81`
- Producer baseline: `tailrocks/velnor@abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`
- APT corrected shared release-ID grammar: `8c19fab405b8c608c5db810d4f01d4f3c65b08cd`
- Worktree: `/tmp/g2-homebrew-review-c772971` (detached, clean)

## Decision

**Conditional source-contract pass; G2 delivery remains blocked.**

The candidate closes the prior source defects H1/H2 and most of H3. It is safe
to stage as an arm64-only, producer-handoff updater with an explicit historical
formula guard. It is not evidence of a valid published Homebrew package: no
native Mach-O proof, provider/API provenance execution, clean Homebrew
installation, lifecycle test, or producer-consumer end-to-end handoff exists
at this commit.

## What passed

1. Strict stable/preview decimal SemVer rejects leading-zero core and numeric
   prerelease identifiers (`scripts/package-update.sh:95-127`). The hostile
   `01.2.3`, `preview.01`, and component `01.0.0` fixtures fail closed.
2. Same-version immutability now binds source commit, release tag, release ID,
   canonical manifest SHA, and archive SHA (`scripts/package-update.sh:272-304`).
   The test mutates a non-Homebrew canonical artifact and refreshes the
   attestation; the rerun is rejected (`scripts/test-package-update.sh:269-278`).
3. Canonical parent identity and subordinate archive records are checked. The
   archive contains exactly five root members, all three sibling files must be
   regular owner-executable files, and each sibling digest is compared with its
   subordinate record (`scripts/package-update.sh:331-439`).
4. The producer attestation is required, has an exact top-level key set, binds
   source ref/commit, release tag/ID/URL, external parent manifest digest, and
   an exact canonical asset census (`scripts/package-update.sh:136-175`).
5. The shared release-ID grammar is consistent with corrected APT:
   `^[A-Za-z0-9][A-Za-z0-9._:/-]*$` (APT `scripts/release-discovery.sh:19`,
   Homebrew `scripts/package-update.sh:107`). The fixture's namespaced
   `fixture/stable/...` ID passes.
6. Historical Intel/source-build replacement is not silent. An existing
   formula without `# target-support: aarch64-apple-darwin` is rejected unless
   `VELNOR_ALLOW_ARM64_FORMULA_REPLACEMENT=1` is explicit
   (`scripts/package-update.sh:446-459`; fixture `scripts/test-package-update.sh:298-310`).
   The generated formula advertises only arm64 and README/docs explicitly keep
   Intel unsupported until native artifact and clean-client proof.

Independent commands, all exit 0:

```text
rtk bash scripts/test-package-update.sh
  Homebrew contract fixture validation passed
rtk shellcheck scripts/package-update.sh scripts/test-package-update.sh
rtk bash -n scripts/package-update.sh scripts/test-package-update.sh
rtk ruby -c Formula/velnorctl.rb.template
  Syntax OK
rtk ruby -c Formula/velnorctl-preview.rb.template
  Syntax OK
rtk git diff --check HEAD^ HEAD
```

The fixture exercises stable 9.8.7 -> 9.8.8 -> 10.0.0, preview sequence
1 -> 2, rollback/same-version rejection, missing manifest/attestation,
component identity, canonical asset mutation, release-ID mutation, archive
tampering, and historical replacement guard. It is still only a temporary
archive smoke.

## Remaining false-green paths / required gates

### H3: lifecycle coverage is overstated (must fix test or narrow docs)

`docs/homebrew-release-contract.md:186-191` claims temporary archive
install/upgrade/channel/uninstall smoke. `scripts/test-package-update.sh:198-229`
only untars into a temp directory, moves files into `bin`, invokes fake sibling
shell scripts, runs Ruby syntax, then removes the directory. There is no
`brew install`, `brew upgrade`, formula test, stable/preview conflict switch,
rollback, or real uninstall. The detached commit has no `.github` CI workflow.
This is a documentation/test false-green, not a proof of Homebrew behavior.
Add a clean hosted macOS arm64 job and machine evidence for stable fresh,
stable upgrade, preview fresh, preview upgrade, cross-channel switch, and
uninstall before delivery approval.

### Architecture is metadata-only in the updater

The updater checks owner execute mode but never inspects Mach-O headers or an
architecture attestation (`scripts/package-update.sh:331-340`). The fixture
itself creates `#!/bin/sh` files (`scripts/test-package-update.sh:44-73`) and
accepts them under the claimed `aarch64-apple-darwin` archive. Thus a wrong
architecture or non-native executable can be green at source-test level.
The producer must build on native macOS arm64 (or provide equivalent trusted
native proof), attest the target, and run the clean-client binary/sibling
smoke. Do not call the fixture a native package test.

### Attestation is a trusted handoff, not independently authenticated here

`package-update.sh` validates a local JSON file. It does not call the provider,
verify a GitHub artifact attestation/signature, or prove that the file was
created from the immutable release API; it trusts `release-attestation.json`
as an input after shape/digest equality checks (`scripts/package-update.sh:138-175`).
The docs correctly describe producer creation and leave clean publication as a
later gate (`docs/homebrew-release-contract.md:79-107`), so this is an explicit
integration dependency, not a reason to claim the updater itself proves
provenance. The staged CI must source this file from an authenticated producer
job and retain provider verification evidence; a hand-authored local fixture is
not acceptable.

### Current producer baseline is not yet the Homebrew producer contract

At Velnor `abe9ad82`, the release config declares
`manifest_schema = "velnor.package-release.v1"` (`.github-gen/velnor-workflow.toml:65-77`).
The current native publisher emits a minimal `release-manifest.json` with only
schema/source ref/source commit/version/assets
(`crates/velnor-workflow/src/primitives/release.rs:2029-2036`), not the
Homebrew-required `velnor.product-manifest/v1` identity, component inventory,
release ID, artifact target/kind/size rows, and provider attestation. The
pending generic native-product producer must emit one canonical manifest for
APT and Homebrew; no package-specific fallback or stale schema acceptance is
allowed.

## Acceptance before G2 approval

- Producer emits the exact canonical product manifest and external digest with
  stable/preview source/tag/commit/release-ID binding; Homebrew and APT consume
  the same manifest and shared release-ID grammar.
- Producer resolves the immutable GitHub release/source through its API and
  supplies authenticated artifact attestations/census. Consumer staging records
  release ID, manifest/artifact digests, source ref/commit, and attestation
  result.
- Native macOS arm64 archives contain real executable binaries; clean hosted
  macOS arm64 runs formula install, all three `--version`/source checks,
  `host start` sibling discovery, same-channel upgrade, channel switch,
  rollback, and uninstall.
- Intel remains explicitly unsupported and the historical source formula is
  preserved unless a separately reviewed native Intel artifact and clean test
  land. Never use an override to turn an unproved arm64 archive into Intel
  support.
- Generated tap CI runs the updater hostile fixtures and real clean-client
  lifecycle matrix. Only then may publication/update automation be reviewed.
