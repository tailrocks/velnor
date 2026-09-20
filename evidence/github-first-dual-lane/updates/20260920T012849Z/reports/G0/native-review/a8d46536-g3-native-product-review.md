# Native product v3 exact source review — a8d46536e7e11db0bbd5e207be802970362b751f

Review date: 2026-09-20. Detached review tree:
 /private/tmp/native-product-v3-review-a8d46536. Base:
9910495ef393c9b141285ca74468de1ca962ba98. Read-only review; no source,
implementer worktree, branch, remote, publication, host runtime, install, or
release mutation.

## Disposition

CHANGES REQUIRED; no G2 approval. The rendered lane has useful fail-closed
guards, but the product contract still admits self-authored identity/typed-row
and archive-content false greens. Intel blocking is correctly fail-closed, so
this exact contract cannot publish a four-target product until that capability
exists or the product contract is deliberately narrowed by an independently
approved scope change.

## Exact source and rendered proof

- .github-gen/velnor-workflow.toml:224-261 declares stable and preview product
  lanes with three typed components (velnor-runner, velnor-workflow, velnorctl)
  and four target identities. The rendered stable builder is three components x
  four targets = 12 binaries, four archives, and two Linux APT subjects:
  18 product artifacts.
- crates/velnor-workflow/src/s2/primitives/native_product.rs:220-252 requires
  the exact four-target census and explicit x86_64-apple-darwin blocker. The
  rendered stable file uses xcode-27 only for aarch64-apple-darwin and has no
  macos-27 or Intel fallback (native-product.yml:17-27,29-57).
- The typed verifier is actually invoked in rendered stable release.yml:
  4383-4407 builds product-payload, passes all identity fields, all four
  targets, all three component names, and runs
  artifacts/velnor-runner-release-tool release verify-product before
  attestation/publication.
- Provider release lookup is strict: release.yml:4305-4322 treats only an
  HTTP 404 response as absence; all other lookup failures exit before draft
  creation. The draft must bind to the exact tag/commit and be draft,
  non-prerelease.
- Rust verification reads gzip/tar streams without extraction and rejects
  links, directories, duplicate members, paths, and undeclared members
  (crates/velnor-runner/src/product.rs:481-538). Shell reconciliation repeats
  path/type checks (release.yml:4548-4576).
- Generated permissions isolate the product builder at contents: read
  (native-product.yml:13-14,43-44). Only the release publisher receives
  contents: write, id-token: write, and attestations: write
  (release.yml:4420-4423 plus job permissions). No competing product
  publisher exists.

## Findings

### 1. Identity expectations are optional, so the public verifier can accept a self-authored source identity

crates/velnor-runner/src/service.rs:370-401 declares schema/product/channel/
version/source repository/ref/commit/tag/release ID as Option<String>.
crates/velnor-runner/src/release.rs:1661-1710 compares each only when the
option is present. publication_identity() only rejects development builds and
malformed embedded identity; it does not compare manifest identity to the
embedded source/tag. A caller that supplies manifest/artifact/profile paths but
omits identity flags can pass a canonical, digest-correct manifest whose
source/tag/release identity is wrong. The generated path supplies all flags,
but the verifier contract itself remains false-green.

Required correction: make every publisher identity expectation mandatory, or
derive it from independently trusted event/provider/embedded identity and fail
closed when unavailable. Add omission mutations for every identity field; do
not treat generated caller completeness as verifier enforcement.

### 2. The exact profile checks names/targets, not the typed component contract; expectations come from downloaded rows

crates/velnor-runner/src/product.rs:301-403 accepts only expected target and
component-name lists. It derives expected binary names from the manifest's own
component.binary values and never compares crate, binary, version, feature, or
the configured per-component identity. The release shell reads only component
names for --component (release.yml:4405-4407), while release.yml:4377-4380
groups crate/version/binary from downloaded components-*.jsonl rows and does
not compare them with the complete typed COMPONENTS_JSON contract
(native-product.yml:57). Four contracts are only cmp-equal to one another
(release.yml:4284-4303), not independently bound to generator configuration
or Cargo package facts.

A coherent self-authored/corrupted row set can rename a binary or crate/version,
produce the corresponding manifest and 18-row inventory, and still satisfy the
current profile. Required correction: carry the complete typed component map
(crate, binary, expected identity mode/version, and every target) as an
independently generated expected value into the publisher and verifier; reject
row/manifest mismatch before staging archives. Add three-component/four-target
fixtures plus single-field mutations.

### 3. Archive member bytes are not bound to sibling binary artifact digests

verify_archive_members() (crates/velnor-runner/src/product.rs:481-538) checks
archive member names/types/path shape only. It does not hash each component
member or compare it to the corresponding binary artifact row. The existing
release shell repeats name/type checks and checks only parent_manifest_id
(release.yml:4548-4576); it never compares archive payload bytes with manifest
binary digests. Fresh publishing creates archives from sibling files
(release.yml:4367-4373), but the product manifest then self-authors the archive
digest, so this is not an independent coherence proof. A different executable
can occupy an expected archive member while all current archive checks pass if
the self-authored archive digest is updated.

Required correction: stream/hash each archive binary member and compare it to
the exact manifest binary artifact including size, and validate embedded
identity/manifest documents against parent product identity and component map.
Keep the no-extraction rule.

### 4. Raw downloaded row names are used as filesystem paths before schema validation

release.yml:4351-4363 obtains name from downloaded JSONL rows, resolves it
through find -name, copies it into product-assets, and only later invokes the
Rust manifest verifier. release.yml:4367 similarly uses row binary values to
form archive paths. A hostile row can cause path/glob resolution or staging
outside the intended product namespace before the fail-closed verifier rejects
its unsafe basename. This is not a published false green in the current
sequence, but violates the producer path-safety boundary and can expose
unrelated workspace bytes to later steps.

Required correction: parse and validate the complete typed row schema and safe
basename/target binding before any find, cp, or archive operation; use
contract-derived exact paths rather than find -name over the merged artifact
tree.

### 5. Intel behavior is honest but delivery is intentionally unavailable

The blocked job exits 1 (native-product.yml:17-27), and the publisher rejects
any nonempty blocked_targets (release.yml:4288-4289). This correctly avoids
inventing macos-27, substituting Intel with arm64, or silently using an older
Intel image. It also means this exact four-target product cannot reach
publication/G2 while x86_64-apple-darwin is blocked. Treat this as an explicit
capability blocker, not a pass or a reason to mark the target N/A.

### 6. Preview product declaration is rendered but not connected to preview publication

native-product-preview.yml is generated and actionlint-valid, but rendered
preview.yml contains no native-product, product-manifest, or verify-product
job. render_native_preview() only emits identity/metadata/Debian/sign/publish
jobs (crates/velnor-workflow/src/s2/primitives/release.rs:2400-2433). If
preview product delivery is in scope, this is an unbound producer lane; if only
stable product delivery is in scope, record the preview declaration as out of
scope rather than implying dual-lane preview coverage.

## Verification commands

All commands ran against exact detached a8d46536; no source edits:

  cargo test --locked --all-features -p velnor-runner --lib product
    19 passed, 2403 filtered
  cargo test --locked --all-features -p velnor-runner --lib release
    117 passed, 2305 filtered
  cargo test --locked --all-features -p velnor-workflow --lib native_product
    4 passed, 1736 filtered
  cargo test --locked --all-features -p velnor-workflow --lib native_identity_release_wires_release_build_and_deb_publishing
    2 passed, 1738 filtered
  cargo fmt --all -- --check
    PASS
  actionlint -no-color -config-file
    /private/tmp/native-product-v3-render/.github/actionlint.yaml
    /private/tmp/native-product-v3-render/.github/workflows/*.yml
    PASS (all rendered workflows; generated config declares xcode-27 and
    velnor-target-mvp)
  git diff --check 9910495ef393c9b141285ca74468de1ca962ba98 a8d46536
    PASS

Focused tests cover one synthetic component/one target and string assertions;
they do not cover exact configured three-component/four-target/18-artifact
manifest, omitted identity flags, malformed typed rows before staging, or
archive member digest binding. No host build, install, release publication, or
runtime execution was performed.

