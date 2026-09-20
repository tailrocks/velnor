# Native product exact rereview — `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`

Scope: read-only review of exact branch `codex/github-first-native-product-v3`, compared with `a8d46536e7e11db0bbd5e207be802970362b751f`. No publication, host runtime, or source edits. This is not G2 approval.

## Verdict

**REJECT / CHANGES REQUIRED.** The producer-side typed census and fail-closed Intel policy improved materially, but the exact implementation cannot produce an artifact set accepted by the current APT/Homebrew consumers or by its own product verifier. Preview native-product delivery is also incomplete.

## What is correct in this exact commit

- `NativeProductContract` is typed and `deny_unknown_fields`; component names, crates, binaries, versions, feature, identity mode, and target coverage are checked before downloaded rows are used (`crates/velnor-runner/src/product.rs:152-234`, `536-588`). The release publisher independently reloads the source contract and Cargo metadata, then compares downloaded contracts to it (`crates/velnor-workflow/src/s2/primitives/release.rs:2084-2116`). This is the right authority direction; downloaded rows are not the component-name authority.
- The generated matrix is exactly 4 targets, 3 components, Intel blocked. Independent render census: 12 binary rows + 4 archive rows + 2 Linux APT rows = 18 rows. Generator requires the four targets and `x86_64-apple-darwin` in `blocked_targets` (`crates/velnor-workflow/src/s2/primitives/native_product.rs:278-323`). Stable and preview publication paths fail when any blocked target remains (`crates/velnor-workflow/src/s2/primitives/release.rs:1969-1975`, `2571-2600`). No Intel fallback is present.
- Raw component names are selected from the source contract before each build command; the command resolves package/binary/identity from that typed object, validates target and version, then runs the selected binary identity (`native_product.rs:392-413`). Identity command failures now fail closed.
- Archive member names are root-only regular files, duplicate archive members are rejected, binary bytes are hashed against manifest rows, and subordinate identity binds to the provider release ID (`crates/velnor-runner/src/product.rs:737-860`).
- Release lookup is provider-bound and numeric: only HTTP 404 enters creation; other lookup failures fail; the release ID must be positive decimal (`release.rs` generated source replacement at `1985-2012`; rendered release workflow `4350-4370`).

## Blocking findings

### P0 — generated product checksum is guaranteed to fail its own verifier

The publisher writes `sha256sum product-assets/product-manifest.json > product-assets/product-manifest.json.sha256` (`release.rs:2041`, rendered `release.yml:4473`). GNU `sha256sum` emits two fields (`digest  product-assets/product-manifest.json`). `read_artifact_checksum` accepts exactly one token (`crates/velnor-runner/src/release.rs:1754-1778`), and the verifier is invoked with that sidecar (`release.rs:2050-2066`). Independent CLI reproduction on this exact tree:

```text
rc=1
Error: artifact checksum file must contain only one checksum
```

The APT collector has the same basename-only sidecar rule (`dual-lane-apt/scripts/release-discovery.sh:68-83`), so the path-bearing producer sidecar is rejected there too. Emit one digest token (or digest plus the exact basename) and test the real verifier plus APT parser.

### P0 — canonical component schema disagrees with both consumers

The Velnor product manifest and archive subordinate component records include `feature` and `identity` (`crates/velnor-runner/src/product.rs:70-78`, `release.rs:2030-2032`, rendered `release.yml:4460-4468`). Current APT requires every canonical component key set to exactly `[binary, crate, name, targets, version]` (`dual-lane-apt/scripts/release-discovery.sh:331-349`). Current Homebrew requires the same exact five-key set (`homebrew-velnor/scripts/package-update.sh:187-201`) and requires subordinate archive component keys exactly `[binary_sha256, crate, crate_version, name, release_version, source_commit]` (`package-update.sh:362-392`).

Therefore a normally generated 3-component product is rejected by both consumers, despite passing the producer’s typed verifier. Choose one canonical schema and migrate all consumers/producers together; aliases or permissive extra fields are not acceptable.

### P0 — APT subordinate parent binding is impossible with the emitted records

APT requires `.parent_manifest_sha256 == sha256(product-manifest.json)` on `release-record.json`, `release-manifest.json`, and compiled `manifest.json` (`dual-lane-apt/scripts/release-discovery.sh:253-300`). Velnor `ReleaseRecord` has no such field and uses `deny_unknown_fields` (`crates/velnor-runner/src/release.rs:341-351`); the rendered stable release emits the existing acyclic release record and consumer manifest without that parent field (`release.yml:4094-4205`). The compiled manifest is also copied without a parent field. This is an actual cross-lane contract mismatch, not a missing test. Add a subordinate record schema that can carry the external product digest without creating a cycle, or change APT’s contract coherently and update its acceptance tests.

### P0 — Preview native-product path does not publish the product

Preview build handoff validates four target contracts, component rows, binary rows, and Intel blocking (`release.rs:2558-2620`). The subsequent preview publisher assembles only `SHA256SUMS`, `release-manifest.json`, and preview Debian assets (`release.rs:2531-2555`, `2638-2650`). It never creates `product-manifest.json`, its external digest, Homebrew archives, native-product attestations, or canonical product asset rows. A preview native product therefore cannot satisfy the required product/channel/provenance/install contract.

### P1 — published-release idempotency path is unreachable after the first publication

The product-release lookup accepts only an existing draft (`rendered release.yml:4360-4370`). Later code contains an existing published-release reconciliation path (`release.yml:4598-4745`), but the earlier draft assertion aborts before reaching it when the tag already names a published release. A rerun cannot use the existing immutable provider release ID. Keep create-on-404, but permit and independently verify the published existing state before entering the no-upload reconciliation path.

### P1 — archive component arrays silently collapse duplicate names

`verify_archive_members` converts `ArchiveManifest.components` directly to a `BTreeMap` (`crates/velnor-runner/src/product.rs:893-910`). Duplicate component records overwrite one another; two identical records can therefore reduce to the expected map and pass. Reject duplicate component names and require exact array cardinality before map conversion. Add a hostile duplicate-component archive fixture.

### P1 — external canonical-manifest digest is optional in the reusable verifier

`ApplicationManifest::verify_bytes` accepts `Option<&str>` for the external digest (`product.rs:591-605`). Publication supplies one, but a caller can validate a product as complete without an independent manifest digest. The G2 product contract requires the digest binding; make it mandatory for the publication verifier and retain an explicitly separate structural-only API if needed.

### Integration blocker — exact commit is source-only; generated producer/publisher surface is absent

At exact `2f7d5f...`, `git ls-files '.github/workflows/native-product*.yml' '.github/ci/native-product*.json'` returns no files, and checked-in `release.yml`/`preview.yml` contain no native-product wiring. The 3×4/18 census and line references above come from an isolated generator render of this source commit; that render is not committed in the reviewed tree. Do not treat source tests as generated workflow delivery. Regenerate, inspect, and commit the exact workflows/contracts before any gate claim.

## Verification performed

Exact tree was clean and remote-equal at `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`; `git diff --check a8d46536e7e11db0bbd5e207be802970362b751f..2f7d5f...` passed.

Passed focused checks:

- `cargo test --locked --all-features -p velnor-runner --lib product` — 21 passed.
- `cargo test --locked --all-features -p velnor-runner --lib release` — 117 passed.
- `cargo test --locked --all-features -p velnor-workflow --lib native_product` — 4 passed.
- `cargo test --locked --all-features -p velnor-workflow --lib native_identity_release_wires_release_build_and_deb_publishing` — 2 passed.
- `cargo fmt --all -- --check`; targeted clippy for `velnor-runner`/`velnor-workflow`; isolated rendered workflow `actionlint` — passed.

These are source/render checks only. They do not close the cross-repository schema, APT parent binding, preview publication, clean-client Homebrew, install/upgrade, or hosted Intel-capability gates.

## Acceptance constraints before re-review

1. Make one canonical component schema accepted byte-for-byte by Velnor, APT, and Homebrew; include exactly three components and exactly the agreed archive subordinate schema.
2. Emit and consume a valid external product-manifest digest sidecar; require it in the publication verifier. Add the path-bearing sidecar negative fixture.
3. Define and implement acyclic parent binding for every APT subordinate record, including exact `release-record`, `release-manifest`, and compiled/package manifest schemas.
4. Generate/publish Preview product manifest, digest, four archives/target rows, attestation, and install metadata through the same authoritative manifest path; verify published assets.
5. Preserve numeric provider release identity, 404-only creation, exact tag/commit/source binding, and make the existing published-release reconciliation reachable.
6. Reject duplicate archive component names before map construction; add hostile duplicate/missing/wrong-digest archive fixtures.
7. Commit regenerated `.github` workflows/contracts and run full rendered `actionlint` plus an end-to-end producer→APT/Homebrew fixture with 3 components, 4 targets, 18 rows, Intel blocked, stable and preview.

No G2 approval. Re-review exact regenerated/pushed commit only.
