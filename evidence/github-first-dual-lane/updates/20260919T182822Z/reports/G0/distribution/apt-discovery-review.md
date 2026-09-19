# Independent APT application-release discovery review

## Decision

**REJECT source approval for G2 integration.** Commit `0081b5069134e4110e8bbbafb7ba2e61017e70b1` improves discovery materially: it paginates, rejects runtime/mutable-preview candidates, validates the producer-owned manifest, checks external manifest bytes, binds subordinate parent digests, and fails closed on API fetch errors. Its happy-path/unit fixtures pass. However, the helper still emits `pass` for malformed canonical/subordinate provenance cases that a complete G2 product discovery contract must reject.

This is source approval only. No workflow integration, publication, remote write, or package delivery was performed.

## Exact scope and verification

- Repository: `tailrocks/velnor-apt`
- Base: `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`
- Reviewed commit: `0081b5069134e4110e8bbbafb7ba2e61017e70b1`
- Detached worktree: `/tmp/g2-apt-discovery-review`
- Worktree remained clean.

Commands:

```text
rtk bash scripts/test-release-discovery.sh
release discovery checks passed

rtk mise run check
actionlint: pass
shellcheck: pass
release-discovery-test: pass
package-update-test: pass
verify-release-test: pass
```

The independent adversarial harness is [`apt-discovery-adversarial.sh`](./apt-discovery-adversarial.sh), with fixtures and stderr under [`apt-discovery-adversarial/`](./apt-discovery-adversarial/). Results:

| Fixture | Expected | Actual | Result |
|---|---:|---:|---|
| baseline | pass | exit 0 | control correct |
| binary-name-drift | fail | exit 0 | **false green** |
| sha256-sums-tamper | fail | exit 0 | **false green** |
| release-manifest-assets-tamper | fail | exit 0 | **false green** |
| homebrew-digest-tamper | fail | exit 0 | **false green** |
| asset-url-missing | fail | exit 0 | **false green** |

The hostile fixtures use a paginated synthetic GitHub Releases API and keep all canonical parent digests/sidecars internally coherent. Only the named field/asset is adversarially changed.

## Findings

1. **Canonical component binary identity is not exact.** `validate_product_manifest` requires component names to equal crate names, but only checks `binary` for a permissive name regex (`scripts/release-discovery.sh:331-349`). It never requires `component.binary == component.name` or checks the binary set against the configured product inventory. The `binary-name-drift` fixture changes `velnorctl` to `evilctl`; discovery exits 0. Require the exact producer/config-derived `{velnorctl, velnor-runner, velnor-workflow}` mapping, including binary, crate, component version, and target set.

2. **Required `SHA256SUMS` is presence-only.** The candidate requires an uploaded `SHA256SUMS` asset (`scripts/release-discovery.sh:443-454`) but never fetches or parses it. The `sha256-sums-tamper` fixture replaces its content with `tampered`; discovery exits 0. Fetch it, require exact expected package rows, and bind every listed digest/name to the canonical artifact and downloaded sidecar. This must remain separate from the external `product-manifest.json.sha256` proof.

3. **Subordinate package asset inventory is not validated.** `validate_subordinate_records` checks release-manifest parent digest and only six identity fields (`scripts/release-discovery.sh:261-300`); it does not validate the subordinate `.assets` array against the canonical APT projection, checksums, or package names. The `release-manifest-assets-tamper` fixture gives it an unrelated asset row while preserving parent/source/version fields; discovery exits 0. Require exact schema keys and the two APT package rows, matching canonical names/targets/digests/version/source identity. Keep this subordinate record acyclic: it references the parent manifest digest but does not become a second authority.

4. **Release-record package inventory is not validated at discovery.** The release record check covers schema, parent digest, build repository/tag/commit/version, and a syntactically valid compiled-manifest digest (`scripts/release-discovery.sh:275-286`), but not its architecture rows, package/binary checksums, or service/package identity. A record with the entire architecture inventory absent is accepted by this helper and deferred to a later verifier. Make discovery reject incomplete subordinate provenance, or make the downstream verifier a mandatory same-command gate whose failure cannot be represented as an eligible discovery result.

5. **Non-APT canonical artifacts are not byte/digest verified by this consumer.** The canonical manifest may list Homebrew artifacts and the helper checks only release metadata size for every artifact (`scripts/release-discovery.sh:384-388`); it downloads/checks sidecars only for APT assets (`scripts/release-discovery.sh:359-373`). The `homebrew-digest-tamper` fixture advertises a Homebrew artifact with a canonical SHA but serves different bytes; discovery exits 0. Either verify every canonical artifact's bytes/sidecar here or explicitly scope the output to an APT projection and make the cross-consumer canonical artifact verification mandatory in the producer/Homebrew gate. Do not report an unverified full parent manifest as complete.

6. **Release asset URL provenance is not checked.** `release_assets_are_well_formed` checks names, state, size, and numeric ID but not `browser_download_url` (`scripts/release-discovery.sh:179-191`). The `asset-url-missing` fixture clears the canonical manifest URL; discovery exits 0 while emitting the empty URL in `release_assets`. Require a nonempty HTTPS URL under the expected source repository/release asset endpoint, or omit URLs from the trust-bearing output and derive them only from validated IDs/names.

7. **Source tag/branch is API-claim bound, not independently resolved.** Stable and preview identity are derived from `tag_name` and `.target_commitish` (`scripts/release-discovery.sh:411-429`), then the same value is copied into the manifest expectations (`scripts/release-discovery.sh:308-322`). No independent `git ls-remote`/GitHub ref lookup proves `refs/tags/vX.Y.Z` or `refs/heads/main` resolves to that commit. The downstream APT verifier has a stable tag-resolution primitive, but discovery output must not be trusted as source provenance before that independent check. Add an explicit immutable ref-resolution result (or require the next verifier to consume and expose it) and make preview main-head identity explicit.

8. **Candidate selection has no ambiguity guard.** Stable selection sorts by numeric version and preview by base version/sequence (`scripts/release-discovery.sh:515-523`) but does not reject two eligible candidates with the same selection key and divergent source/release identity. Reject ties instead of depending on GitHub API ordering; retain channel state independently and never silently replace a retained candidate with a same-version different-source asset.

9. **Canonical contract is product-specific but hardcoded in the helper.** The implementation hardcodes source `tailrocks/velnor`, package `velnor-runner`, product `velnor`, component names, and target triples (`scripts/release-discovery.sh:8-16`, `331-349`). That is acceptable only for this typed APT consumer; it must not be reused as a generic product generator. Keep product names/config authority in the producer handoff and ensure the Homebrew consumer consumes the same parent manifest rather than another hardcoded inventory.

## Confirmed good behavior

- `gh api --paginate` output is normalized as multiple JSON pages and validated as arrays (`scripts/release-discovery.sh:143-149`, `496-501`). Existing fixtures prove mixed pages and runtime releases do not hide a valid older application release.
- Stable accepts only non-prerelease `vX.Y.Z`; preview accepts only immutable `preview-<full-source-sha>` releases, requires prerelease metadata, and checks the seven-hex product version suffix (`scripts/release-discovery.sh:411-429`, `validate_product_manifest` at `325-329`). Mutable `preview` is rejected.
- Canonical `product-manifest.json` bytes are externally hashed through a sidecar, and all three subordinate records must carry the same `parent_manifest_sha256` (`scripts/release-discovery.sh:214-224`, `261-300`). No self-referential `manifest_sha256` field is introduced in the canonical parent.
- Stable and preview selection are independent and the helper does not mutate package-state files. Existing retained-state fixture hashes remain unchanged.
- Listing, asset fetch, malformed page, no eligible release, missing/invalid assets, and sidecar failures fail closed in the supplied tests.

## Cross-consumer contract decision for root

The acyclic shape is sound and should remain:

```text
producer product-manifest.json
        ├── APT subordinate records/package projection
        └── Homebrew subordinate archive records/formula projection
```

The parent manifest owns product/channel/version/source/component/artifact identity. APT and Homebrew subordinate records may carry `parent_manifest_sha256` and immutable `release_id`; neither may redefine the parent inventory. Do not put a digest of canonical manifest bytes inside those same bytes. Resolve the current syntax mismatch before integration: this helper accepts `release_id` characters including `/` (`:317`), while Homebrew `b1c9424`'s updater currently permits `[A-Za-z0-9._:-]` only. Pick one producer-owned grammar and use it in both consumers.

## Source approval gate

Do not approve integration/publication until the author adds negative fixtures for the five false-green cases above, plus independent source-ref resolution and ambiguous same-version selection. Required baseline remains pass; every hostile mutation must produce nonzero/failure. Then rerun the exact `mise run check` and review the regenerated workflow separately.
