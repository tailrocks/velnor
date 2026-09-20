# Independent Homebrew consumer review

Reviewed exact consumer commit `6520aad7bd66d53349e040508146956e9c4f0c1e`
(`codex/github-first-homebrew`) in the separate repository
`dual-lane-homebrew`. Parent: `8feb46c19b6aca1a7079e45acad7cdc99cde23e8`.
Producer comparison is exact Velnor commit
`2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`.

Review is read-only and independently authored. No Homebrew install, formula
execution, macOS runtime, provider publication, Docker/OrbStack operation, or
source edit was performed.

## Verdict

**CHANGES REQUIRED; reject the producer/consumer handoff at this boundary.**

The Homebrew consumer implementation is structurally strong and its synthetic
fixture passes, but the exact producer currently emits a checksum sidecar that
cannot pass either the producer verifier or the consumer verifier. The fixture
writes a manually corrected basename row and therefore masks this cross-repo
incompatibility. This is release-blocking, not a test-environment limitation.

## Capability matrix

| Area | Exact evidence | Finding |
|---|---|---|
| Canonical schema and component contract | Consumer config requires `product-manifest/v1`, top-level identity, four target rows, and component `feature`/`identity` fields (`config/homebrew-release-contract.json:2-36`, `78-123`). Updater rejects other keys and cross-checks component rows (`scripts/package-update.sh:242-285`, `633-666`). Producer renderer carries `feature`/`identity` into the typed contract (`native_product.rs:145-228`) and build rows (`native_product.rs:392-399`). | **Source-contract pass.** Exact producer field model matches this consumer contract. Real producer payload still required. |
| Archive identity and cross-checks | Consumer requires exactly five archive members and executable binaries (`scripts/package-update.sh:522-573`), subordinate identity, parent release ID, component feature/identity, and binary digest checks (`:575-666`). | **Source pass.** Fixture exercises archive metadata and tamper paths; no real producer archive was consumed. |
| Positive provider release IDs | Consumer grammar is `^[1-9][0-9]*$` (`config/homebrew-release-contract.json:6-8`; `scripts/package-update.sh:151-162`). Fixture rejects leading zero and slash IDs (`scripts/test-package-update.sh:372-382`). Producer resolves numeric provider ID and checks the same grammar in its generated publisher (`release.rs:1955` rendered block; release binding at `:2337-2341`). | **Pass, conditional on actual provider response.** |
| Manifest sidecar | Consumer requires one lower-hex digest plus exactly the basename `product-manifest.json` (`scripts/package-update.sh:188-200`). | **P0 mismatch.** Producer exact source renders `sha256sum product-assets/product-manifest.json > product-assets/product-manifest.json.sha256` (`release.rs:2038-2042`), producing a path-bearing second field. Producer `parse_artifact_checksum` rejects any second field (`crates/velnor-runner/src/release.rs:1754-1760`). |
| Fixture fidelity | Fixture hand-writes `printf '%s  product-manifest.json\\n'` (`scripts/test-package-update.sh:198-219`) and separately tests a path prefix as a negative (`:350-358`). | **Fails integration fidelity.** It proves the consumer rule, not the exact producer payload; current passing fixture cannot certify the handoff. |
| Provider assets and attestations | Consumer expects manifest, sidecar, and release-attestation assets (`config/homebrew-release-contract.json:147-178`), checks API asset name/size/digest/URL and required entries (`scripts/package-update.sh:344-403`), then verifies provider attestations for all three plus every artifact (`:406-423`). | **Consumer source pass; real-provider proof absent.** `scripts/test-gh-provider.sh:27-130` is a local fake: OpenSSL signs fixture files and emits synthetic GitHub-shaped JSON. |
| Release-attestation binding | Consumer requires exact schema, source/ref/commit, release tag/ID/URL, manifest digest, and canonical asset census (`scripts/package-update.sh:203-240`). | **Source pass; producer handoff unproven.** No exact published producer release was read. |
| Stable/preview identity | Consumer enforces stable tag/ref and preview main/ref plus `preview-<full-commit>` tag (`scripts/package-update.sh:172-186`). Fixture covers both lanes and rerun/rollback (`scripts/test-package-update.sh:327-348`, `:410-462`). | **Consumer source pass.** Producer preview delivery remains incomplete: exact `release.rs` preview renderer assembles `SHA256SUMS`, `release-manifest.json`, and Debian assets, while its native-product preview block only verifies a handoff; it does not publish the Homebrew product manifest/sidecar/archive contract. |
| Generation/wiring | Consumer exact commit changes six files only (no `.github` workflow; tree contains templates and scripts only). Producer exact tree has the native-product renderer but no `.github` native-product contract/workflow reference (`git grep native-product 2f7... -- .github` empty). | **Open integration blocker.** Generated workflow/config must be rendered and committed at the producer pin before handoff. |
| Intel preservation | Consumer requires one Intel archive and verifies an x86_64 Mach-O (`scripts/package-update.sh:287-292`, `:669-680`; `scripts/verify-macos-binary.sh`). Fixture exercises native arm, native Intel, wrong-arm-x86, and shell-header failures (`scripts/test-package-update.sh:69-98`, `:389-393`). Producer keeps Intel in the typed four-target matrix but explicitly blocks it until macOS 27 Intel exists (`native_product.rs:18-22`, `:286-318`). | **Structural guard pass; capability not proven.** No real producer Intel artifact or clean Intel client test was run; Homebrew remains arm-only. |
| Lifecycle | Fixture only extracts archives, checks synthetic Mach-O headers, renders formula text, and runs Ruby syntax (`scripts/test-package-update.sh:261-299`). | **Open G2/runtime gate.** No `brew install`, upgrade, channel switch, rollback, uninstall, or clean hosted macOS arm64 client proof was performed or authorized. |

## Required bounded follow-up

1. **Fix the shared producer sidecar contract first.** Change producer
   generation at `release.rs:2038-2042` to write a two-field basename row (for
   example, hash from inside `product-assets`), or change the shared contract
   deliberately and update both the producer parser and consumer. Add a
   producer test that feeds the generated sidecar through
   `parse_artifact_checksum`; retain the consumer basename/path-negative test.
   Do not paper over this only in Homebrew.

2. **Add an exact-producer fixture.** At the fixed producer SHA, capture or
   deterministically generate the canonical `product-manifest.json`, sidecar,
   release attestation, four-target rows, and archives from the producer
   renderer. Run `scripts/package-update.sh` against those bytes. Acceptance:
   the unmodified producer output passes after task 1; a path-bearing sidecar,
   changed feature/identity, changed release ID, missing provider asset, and
   stale attestation each fail closed.

3. **Complete producer generation/wiring.** Render and commit the native
   product workflow/contracts for stable and preview; prove generated output
   at the same source SHA contains the exact four target/component census,
   provider release ID, external sidecar, release attestation, and archive
   rows. Keep Intel blocked from formula publication while preserving its
   producer row.

4. **Complete preview handoff.** The preview publisher must produce the same
   canonical product assets and provider-bound attestation fields consumed by
   this script, not only validate preview component JSONL.

5. **Run real provider/clean-client gates later.** Replace the fake `gh` fixture
   with read-only GitHub release API and `gh attestation verify` evidence at the
   producer pin, then run the separately authorized hosted macOS arm64
   Homebrew lifecycle. These are not proven by this review.

6. **Optional strictness check.** `verify_provider_release` requires every
   expected asset exactly once but does not reject additional release assets
   (`scripts/package-update.sh:395-403`). If “complete canonical census” means
   exact set equality, add that assertion and a positive-extra-asset negative
   fixture; if unrelated release assets are intentionally allowed, document
   that policy explicitly.

## Safe verification performed

All commands ran in the clean exact consumer tree and used temporary fixture
directories only:

- `rtk bash scripts/test-package-update.sh` — pass: `Homebrew contract fixture validation passed`.
- `rtk bash -n ...` for all four shell scripts — pass.
- `rtk ruby -c Formula/velnorctl.rb.template` and preview template — both `Syntax OK`.
- `rtk jq -e . config/homebrew-release-contract.json` — pass.
- `rtk git diff --check 8feb46c 6520aad7` — pass.
- `rtk shellcheck` on all four shell scripts — pass.
- `rtk git status --short --branch` — clean.

These are source/fixture checks only; they do not authorize or imply Homebrew,
macOS, Docker, provider publication, or G2/G4 completion.
