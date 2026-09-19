# G2 checker independent review

## Decision

**REJECT scoped G2 checker approval.** The candidate is deterministic and its package unit suite passes, but every hostile product/distribution mutation below exits zero with `status=pass`. The checker therefore permits false-green G2 evidence for the required product/channel/provenance/install/upgrade contract.

This is a checker review only. It is not distribution-publication approval.

## Exact scope and commands

- Repository: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`
- Detached worktree: `/tmp/g2-checker-review.5FXqui`
- Exact `HEAD`: `b3b6b2ef5239ff3354f504b8aeb638129fd0504b`
- Implementation tree remained clean (`## HEAD (no branch)`).
- Unit gate:

  ```text
  rtk cargo test -p velnor-tools
  cargo test: 216 passed (1 suite, 15.28s)
  ```

- Hostile-fixture gate:

  ```text
  rtk bash /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-g2-review/run-hostile-g2-fixtures.sh \
    /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-g2-review/fixtures
  ```

The harness uses a 32-repository G2 manifest/evidence set, with one required release/install record and a passing baseline. It mutates only that required record. Machine-readable inputs and checker reports are in [`fixtures/`](./fixtures/); aggregate output is [`fixtures/results.tsv`](./fixtures/results.tsv).

| Fixture | Expected | Actual | Result |
|---|---:|---:|---|
| baseline | pass | exit 0 / pass | control correct |
| manifest-component-missing | fail | exit 0 / pass | **false green** |
| digest-binding | fail | exit 0 / pass | **false green** |
| arch-mismatch | fail | exit 0 / pass | **false green** |
| service-unsupported | fail | exit 0 / pass | **false green** |
| upgrade-paths-absent | fail | exit 0 / pass | **false green** |
| same-version-replacement | fail | exit 0 / pass | **false green** |
| source-tag-identity | fail | exit 0 / pass | **false green** |
| producer-consumer-chain | fail | exit 0 / pass | **false green** |
| stale-schema | fail | exit 0 / pass | **false green** |

## Exact findings

1. **Evidence-declared inventory is treated as canonical.** `check_release_manifest` only requires a nonempty component list and checks local uniqueness/field shape (`evidence_check.rs:2657-2720`); install validation only compares installed names to that same submitted list (`evidence_check.rs:2876-2891`). Removing one component and its installed binary passes (`manifest-component-missing`). The checker needs an external/config-derived expected component and target inventory, then exact set and mapping equality.

2. **Manifest digest is not bound to manifest bytes or a named manifest asset.** The checker validates only digest syntax (`evidence_check.rs:2575-2583`), and publication strings merely need to contain the submitted version and digest (`evidence_check.rs:2420-2454`). Installed identity is compared to the same submitted digest (`evidence_check.rs:2810-2823`). Replacing the digest consistently in those fields passes (`digest-binding`). Require a named canonical manifest artifact/sidecar digest from producer evidence, bind it to the manifest bytes, and require APT/Homebrew records to carry that same immutable identity. Do not make the manifest self-hash itself.

3. **Platform/architecture is not cross-bound.** Artifact/component targets are independently syntax-checked (`evidence_check.rs:2594-2625`, `evidence_check.rs:2700-2719`); the install environment independently passes `valid_target(platform-architecture)` (`evidence_check.rs:2937-2948`). Changing a linux-amd64 product to an arm64 install environment passes (`arch-mismatch`). Require selected artifact, every component, installer environment, and installed binary/package architecture to agree with the supported product target.

4. **Unsupported service manager can be asserted as arbitrary N/A.** `valid_service_result` accepts any nonempty `not-applicable:<reason>` (`evidence_check.rs:2953-2960`) without checking manifest/platform/package applicability. `service-unsupported` passes on Ubuntu. Require typed service capability/applicability from the target and a real systemd success for the APT Linux lane; permit N/A only for a target where the manifest explicitly says no service is applicable.

5. **Upgrade and switch operations are optional and semantically unchecked.** Required install validation rejects only an explicitly empty string; `null` is accepted for both fields (`evidence_check.rs:2728-2748`). No operation record, predecessor identity, channel transition, version ordering, or replacement prohibition is checked. Both missing paths (`upgrade-paths-absent`) and same-version replacement (`same-version-replacement`) pass. Require structured clean-install, same-channel upgrade, and channel-switch operations with predecessor product/channel/version/manifest identity; reject missing paths, same-version replacement, and invalid channel/version transitions.

6. **Source/tag identity is shape-only.** Source repository is only passed through repository-name syntax validation, `source_ref` only needs `refs/heads/` or `refs/tags/`, and `release_tag` only needs to be nonempty when present (`evidence_check.rs:2517-2559`). A different repository, evil branch, and unrelated tag pass (`source-tag-identity`). Require the configured canonical source repository; stable must use the exact version tag and tag target; preview must use the configured preview ref/grammar and commit; require tag/version/channel consistency.

7. **Schema identity is not pinned.** The checker only rejects an empty schema string (`evidence_check.rs:2474-2488`). `velnor.package-release.v1` passes (`stale-schema`). Require the exact current canonical manifest schema/version and reject stale/unknown schemas; do not allow a fallback that bypasses the typed inventory.

8. **Producer/consumer provenance is free text.** APT and Homebrew fields are `Option<String>` (`evidence_check.rs:437-444`) and are checked only for nonempty/version/digest substrings (`evidence_check.rs:2420-2454`). Replacing both with `"0.1.1 <manifest digest>"` passes (`producer-consumer-chain`). Require structured producer workflow/release ID/run URL, immutable source/tag, canonical manifest asset digest, and consumer-specific APT feed revision/suite/candidate plus Homebrew tap revision/formula/version. Validate each against the same product/channel/version/manifest identity.

9. **Legacy free-text environment remains accepted.** `InstallEnvironmentEvidence` is an untagged `Description(String)` or structured object (`evidence_check.rs:514-528`), and the string branch only looks for the words `clean` and `path` (`evidence_check.rs:2894-2924`). Required G2 install evidence must use the typed environment fields; otherwise a prose claim can bypass OS/platform/architecture/runner/workspace/PATH checks.

10. **Per-binary provenance is incomplete.** Installed binaries carry only name/path/digest (`evidence_check.rs:545-552`); checker validates absolute path and digest syntax but does not bind each binary digest to its manifest artifact, target, package, or source. Add per-binary artifact/component identity and exact digest/target/path provenance, and reject checkout/PATH fallback paths.

## Required acceptance before re-review

- Exact canonical schema and externally derived product/component/target inventory are mandatory; no evidence-only inventory authority and no stale typed/legacy fallback.
- Canonical manifest bytes have one producer-recorded digest/asset identity; package/release records and both consumers bind to it without self-hash recursion.
- Source repository, channel, version, ref/tag, source commit, release ID, artifact/component set, target, and package/binary identity are all exact and mutually consistent.
- Install environment, selected package/artifact, and installed binary/package architecture are cross-bound.
- APT service semantics are platform-aware; unsupported-service N/A cannot green an applicable Linux package.
- Structured clean install, upgrade, and channel-switch evidence is required; predecessor identity and valid version/channel transitions are checked.
- Producer and APT/Homebrew consumer provenance are structured and immutable, not free-form strings.
- Re-run this exact hostile suite and add negative fixtures for binary-to-artifact digest mismatch, checkout/PATH fallback, malformed producer/consumer records, and missing required target inventory. Every hostile mutation must be nonzero/`fail`; baseline must remain `pass`.

## Repository-derived staged installer CI design

The current source contracts give a concrete target matrix; the checker must not infer a broader one from a generic `valid_target` predicate.

- Producer `velnor3@abe9ad82` `.github/ci/project.toml:[release]` declares the Debian product as package/binary `velnor-runner` for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, from `tailrocks/velnor`, consumed by `tailrocks/velnor-apt`. Its `crates/velnor-runner/Cargo.toml` package assets include `velnor-runner`, `velnorctl`, `velnor-workflow`, `velnor-tools`, `velnor-guest-agent`, and systemd units. Those extra helper binaries/units belong in a typed subordinate Debian package inventory; they must not silently create a second product manifest authority.
- The same producer's `ci-runtime-products.yml` publishes a distinct generator-runtime namespace with only `Linux-X64`, `Linux-ARM64`, and `macOS-ARM64` products. A runtime tag/manifest is never an eligible Velnor application release.
- APT `dual-lane-apt@b24d7d4` `conf/distributions` and `scripts/verify-release.sh` require signed `amd64` + `arm64` suites, schema `velnor.package-release.v1`, stable `refs/tags/vX.Y.Z`, and preview `refs/heads/main` with `X.Y.Z~preview.N+<sha7>` ordering. Its package fixtures intentionally test exact two-architecture pairs, package identity, and systemd/package metadata.
- Homebrew's current contract implementation `dual-lane-homebrew@b1c9424` (descended from baseline `7af1249`) defines the single producer-owned schema `velnor.product-manifest/v1`, exactly three product components (`velnorctl`, `velnor-runner`, `velnor-workflow`), one supported target `aarch64-apple-darwin`, and no Intel artifact until native build plus clean-client proof. Stable uses `refs/tags/vX.Y.Z`; preview uses `refs/heads/main` plus `preview-<full-source-sha>` and `X.Y.Z-preview.N+<sha7>`. The archive must contain exactly `identity.json`, `manifest.json`, and those three executable binaries.

Use this staged matrix. A stage is green only when every listed JSON assertion and command succeeds; unavailable native capability is `blocked`, never `not-applicable`/pass.

| Stage | Required environment and command | Machine-verifiable acceptance fields |
|---|---|---|
| Producer handoff | Clean producer checkout; emit one `product-manifest.json` before consumer updates | `schema=velnor.product-manifest/v1`; `product_id=velnor`; exact `source_repository`; channel/version/ref/tag grammar; 40-lowercase-hex `source_commit`; nonempty immutable `release_id`; exact product component set; artifact rows with `name,target,kind,sha256,size`; no runtime namespace; externally computed manifest digest (no self-hash field) |
| APT offline contract | In `dual-lane-apt@b24d7d4`: `bash scripts/test-verify-release.sh` and `bash scripts/test-package-update.sh` | Stable/preview negative fixtures fail; exactly two package assets/arch rows `{amd64,arm64}`; schema/source/ref/version/tag/commit/package/deb identity and checksums agree; signed metadata says `Origin=Velnor`, `Suite=stable`, `Architectures=amd64 arm64`; preview state cannot overwrite stable state |
| APT fresh client | Disposable Debian/Ubuntu **systemd-capable** amd64 and arm64 clients; add scoped `signed-by=/etc/apt/keyrings/velnor.gpg`; `apt-get update`; `apt-cache policy velnor-runner`; `apt-get install velnor-runner` | Candidate equals staged channel/version/arch; `dpkg-query` reports expected package; exact required binaries and service units exist; `systemctl daemon-reload` and the applicable Velnor unit report `ActiveState=active`/successful health; `/etc/velnor/velnor.env` and execution config are present; no sideloaded `.deb`, checkout binary, or ambient `PATH` binary is used |
| APT upgrade/switch | On each client: stable N→N+1; preview N→N+1; documented preview→stable switch; `apt-get upgrade`/`apt-get install` through the live signed endpoint | `dpkg-query` old/new versions and arch are recorded; candidate strictly advances within channel; same-version different-source replacement rejects; config/state hashes are preserved where contract says so; services are drained/restarted and active after transaction; stable default never selects preview |
| Homebrew offline contract | In `dual-lane-homebrew@b1c9424`: `bash scripts/test-package-update.sh` | Stable and preview formulas render only from verified producer handoff; exact target `aarch64-apple-darwin`; exactly one Homebrew artifact; archive member set and executable bits exact; each binary digest equals subordinate manifest; parent release ID/source/version/channel/ref/tag all agree; same-version/different-source, rollback, tamper, stale-manifest, and missing-handoff fixtures reject |
| Homebrew clean client | Native macOS arm64 runner (`uname -m=arm64`); `brew install` generated stable formula, `brew test`; repeat preview in a clean prefix | `brew --prefix` package files contain all three binaries plus subordinate records; `velnorctl`, `velnor-runner`, `velnor-workflow --version` identify expected product/source; `velnorctl host start` resolves the sibling runner beside it; no source checkout/symlink/ambient PATH fallback; Intel (`x86_64`) is an explicit unsupported result, not a green test |
| Homebrew upgrade/switch | Stable N→N+1 via `brew upgrade`; uninstall stable then install preview (and reverse) using generated formulas | Formula version/channel/ref/tag/source commit and archive SHA remain immutable; stable/preview conflict is enforced; explicit switch leaves only selected command set; uninstall removes package-owned files; no same-version replacement passes |
| Cross-lane evidence | Producer, APT, Homebrew, and installer records refer to one parent `release_id`, source commit, product version/channel, canonical-manifest digest, and target | Checker input contains structured `producer_run_url/release_id`, `apt_feed_revision/suite/candidate`, `homebrew_tap_revision/formula`, `package_name/version/arch`, `installed_component/artifact/target/sha256/path`, and structured operation predecessor/successor identities; free-form publication strings are rejected |

The canonical parent component set should remain the three product-facing binaries above. If Debian ships `velnor-tools` or `velnor-guest-agent` as implementation helpers, record them under the Debian subordinate package/service inventory with explicit roles and checksums; do not add them to Homebrew's three-component product authority or create a second manifest authority. This is the acyclic split: one parent product manifest, package-specific subordinate records that reference its immutable `release_id`.
