# G2 independent distribution review

Observed: 2026-09-19. This is an independent acceptance review, not an
implementation approval. Source repositories were inspected read-only.

## Baseline and effective model

- Velnor producer: `tailrocks/velnor` at `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- APT consumer: `tailrocks/velnor-apt` at `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`.
- Homebrew consumer: `tailrocks/homebrew-velnor` at
  `7af1249f3d69c9f2e548583cdc9f3e737da41b81`.
- Existing evidence: `../distribution/findings.md`.
- Effective child settings: `gpt-5.6-luna`, reasoning `max`; `rtk` 0.49.0.

## Observed blockers and false-green paths

1. APT stable discovery uses `gh release list ... --limit 1` and can select
   `velnor-workflow-runtime-*` instead of an application release. The current
   code is at `../dual-lane-apt/.github/workflows/release.yml:96-102`.
2. APT preview expects a GitHub `preview` release, but the pinned producer had
   only a `preview` tag and no release. The consumer path is
   `../dual-lane-apt/.github/workflows/release.yml:103-115`.
3. The pinned APT workflow drops hidden `.reprepro-ok` during the
   verify-to-publish artifact handoff (`:141-147`). The producer already has
   the intended fix and regression test at
   `../velnor/crates/velnor-workflow/src/primitives/release.rs:4283-4289`
   and `:8330-8353`, but the consumer pin is stale.
4. APT config schedules both GitHub and Velnor lanes by default, including
   Velnor capacity that may be queued or unavailable:
   `../dual-lane-apt/.github-gen/velnor-workflow.toml:13-20`.
5. Homebrew is a source-built `velnorctl`-only formula. It installs no
   `velnor-runner` or `velnor-workflow`, and its README explicitly disclaims
   the runner: `../dual-lane-homebrew/Formula/velnorctl.rb:1-19` and
   `README.md:3-20`.
6. Component versions are not coherent: `velnor-runner` is `0.1.277`, while
   `velnorctl` and `velnor-workflow` remain `0.1.0`. A formula/tag version or
   regex-only smoke test can therefore pass while product identity is wrong.
7. Preview publication verifies the current release, then deletes the rolling
   release before creating its replacement (`../velnor/.github/workflows/preview.yml:1001-1038`).
   A failed create leaves consumers without a current preview endpoint.
8. Runtime products are a separate `velnor-workflow-runtime-*` namespace and
   currently cover Linux x64/arm64 plus macOS arm64 only:
   `../velnor/crates/velnor-workflow/src/primitives/runtime_products.rs:70-148`.
   Intel Mac support must not be implied.

## Acceptance constraints: APT (`g0_distribution`)

- Regenerate from a reviewed generator pin containing the hidden-file fix, and
  prove byte-identical generation with the pin/check command. Do not hand-edit
  generated workflows.
- Stable discovery must paginate all releases and accept only typed
  application releases: exact stable version grammar, application manifest
  schema/product identity, matching source tag/commit, and complete expected
  amd64+arm64 assets. Reject runtime releases, previews, drafts, invalid or
  incomplete manifests, and API/pagination failures. Add fixtures for each.
- Preview must consume an explicit complete GitHub release/manifest bound to
  `refs/heads/main` and its exact commit. Tag-only, Pages-only, or stale
  pointer state is not a valid source.
- Make routine feed publication GitHub-hosted by default. Velnor can remain an
  explicit recovery/qualification choice only; publication must not depend on
  queued Velnor jobs.
- Preserve `.reprepro-ok` through the artifact handoff and retain a test that
  fails when hidden files are omitted.
- Feed mutation must be verify-before-publish, sign both suites and arches,
  preserve the other suite, retain a rollback, reject version rollback, and
  fail closed unless verify, publish, and deploy all succeed.
- Prove on clean Debian hosts: stable fresh install and upgrade, preview fresh
  install and upgrade, preview-to-stable switch, both arches, signature
  validation, every required binary, package metadata/permissions/config, and
  real service-manager behavior.

## Acceptance constraints: Homebrew (`g2_homebrew_contract`)

- Replace the CLI-only contract. Install the compatible workflow-generation
  and start/manage runtime surface; explicitly decide and test the native vs
  Docker-backed macOS boundary. A missing sibling binary or PATH fallback is a
  failure.
- Define explicit stable and preview formula/channel identities, commands,
  version ordering, upgrade/switch behavior, and uninstall behavior.
- Use immutable release inputs with checksums and source commit/product
  identity. No source checkout, local symlink, or ambient PATH can satisfy a
  clean install.
- Generated PR/main CI must cover formula contents, checksums, architecture,
  all installed binaries, `--version`, source identity, and sibling discovery.
- Test clean hosted macOS arm64 installs. Record Intel Mac unsupported unless
  an actual native artifact and test exist; do not advertise cross-compiled
  support.

## Acceptance constraints: producer (`g2_native_packages`)

- Stable and preview must publish real immutable artifacts with complete
  manifests/release records, exact source ref/commit/version, checksums,
  attestations, signatures, and every declared platform asset.
- Keep application release discovery typed and separate from
  `velnor-workflow-runtime-*`. Runtime tags must never satisfy the app
  consumer contract.
- Stable releases/tags are immutable and idempotent reruns byte-confirm
  existing assets. Preview versions are unique and orderable; publish
  immutable versioned assets, verify them, then advance the pointer. Never
  delete the current preview before replacement is proven.
- Make product/component identity coherent. The Debian package inventory
  currently includes `velnor-runner`, `velnorctl`, `velnor-workflow`, guest
  tools, services, and config (`../velnor/crates/velnor-runner/Cargo.toml:56-65`).
  The consumer contract must test that inventory rather than only one binary.
- Require Linux amd64/arm64 and explicitly declared native macOS targets;
  Intel remains unresolved until built and tested.
- Use one publisher after required hosted/native checks. Recovery publication
  must be explicit and remain acyclic.

## Machine-verifiable installer gate

Every channel/platform case must emit a structured record containing:

```text
product_id
channel
version
source_repository
source_ref
source_commit
platform
architecture
artifact_name
artifact_sha256
manifest_sha256
attestation_verified
signature_verified
installed_binary_inventory
installed_binary_versions
installed_binary_source_identity
upgrade_from
switch_from
service_manager_result
```

Required cases:

- Debian amd64 and arm64: stable fresh, stable upgrade, preview fresh,
  preview upgrade, preview-to-stable switch, signature and service checks.
- Homebrew macOS arm64: stable and preview fresh install, same-channel
  upgrade, cross-channel switch, uninstall; verify all required binaries and
  source identity. Intel is a required negative/unsupported result unless
  native assets exist.
- Every case must prove the downloaded artifact and manifest digests before
  install, and must fail if a sibling binary is absent, a component reports a
  mismatched version/source, or a local checkout/PATH supplies the executable.

## Required hostile fixtures

Mixed paginated releases; runtime release ahead of app release; preview tag
without release; missing/extra asset; wrong source commit/ref; malformed
manifest; checksum mismatch; failed attestation/signature; API failure;
partial upload/retry; preview pointer rollback; hidden sentinel loss; dropped
APT suite; formula missing sibling; unsupported architecture; and rerun with
existing immutable assets.

This report intentionally does not approve source unit tests as proof of the
external product contract. Final approval requires actual producer release,
consumer feed/tap publication, and clean installer/upgrade evidence.

## Manifest arbitration and staged CI design

### One application authority

The producer must publish one canonical application manifest for each immutable
release candidate. It is the only authority for product/channel/version/source
identity and complete artifact inventory. A schema change is a breaking change:
consumers accept exactly the declared current schema and fail closed; they do
not fall back to an older schema that lacks inventory.

The canonical manifest should contain, at minimum:

```text
schema
product_id
channel
version
source_repository
source_ref
source_commit
release_tag
release_id
artifacts[{name,target,kind,sha256,size}]
components[{name,crate,version,binary,targets}]
```

APT and Homebrew select their required artifact projection from this same
manifest. Package-specific records are allowed only as subordinate transport
records. They must carry `parent_manifest_sha256` (or an equivalent immutable
manifest identifier), repeat and cross-check the parent identity, and never
become an alternate product authority. Existing APT `release-record.json` and
Homebrew package/archive records can remain subordinate if their roles are
explicit and their fields are checked against the parent.

Do not put `manifest_sha256` inside the bytes whose digest it names. Canonical
manifest bytes are hashed externally (sidecar, release record, feed/tap
publication record, or formula metadata). A subordinate identity may contain
the parent manifest digest; the parent must not contain a digest of that
identity. This prevents self-hash recursion and makes retries byte-stable.

The current Homebrew proposal must therefore change its three-way identity
model (`config/homebrew-release-contract.json` and
`docs/homebrew-release-contract.md`): `homebrew-manifest.json` should be a
consumer projection or disappear, and archive `manifest.json`/`identity.json`
must be checked as subordinate records against the canonical app manifest.

### Staged installer workflow

Use an acyclic four-stage chain. Each stage emits JSON evidence and immutable
links; later stages consume exact IDs/digests rather than rediscovering a
moving `main` or a mutable preview pointer.

1. **Producer stage:** build the declared target matrix, assemble deterministic
   archives/debs, emit the canonical manifest, compute its external digest,
   sign/attest every artifact and manifest, and upload an immutable release.
   Verify every manifest artifact row against the bytes before publishing the
   rolling channel pointer.
2. **Consumer staging stage:** APT discovery selects the canonical manifest,
   filters Linux amd64/arm64 rows, verifies source/ref/version/product and
   fetches packages. Homebrew updater selects the same manifest, filters
   native macOS rows, verifies sibling inventory/checksums, and renders stable
   or preview formulas. No consumer builds from source in the normal path.
3. **Publication stage:** publish the signed APT suite and tap commit/formulas
   only after the staged inputs pass. Preserve the other APT suite and current
   preview pointer. Record producer run, release ID, manifest digest, feed/tap
   revision, and formula/package rows.
4. **Clean-client stage:** install from the real published endpoint in
   disposable environments, then run upgrade/switch/uninstall cases. G2 hosted
   macOS tests do not invoke OrbStack, nested virtualization, or `host start`;
   actual Docker-backed host execution belongs to G4/G5. They must still prove
   all installed native binaries, sibling discovery, version/source identity,
   and a useful no-network smoke command.

### Exact current platform capability map

The generator's fixed runtime platform map is:

```text
Linux x86_64   -> ubuntu-24.04      -> Linux-X64
Linux aarch64  -> ubuntu-24.04-arm  -> Linux-ARM64
macOS arm64    -> macos-15          -> macOS-ARM64
```

This is encoded in `../velnor/crates/velnor-workflow/src/primitives/runtime_products.rs:70-148`.
The release workflow provisions Rust target names including both
`aarch64-apple-darwin` and `x86_64-apple-darwin`, but a cross target is not
native Intel evidence. Homebrew must advertise Intel only after a real
producer artifact and a matching native clean-client test; otherwise emit an
explicit unsupported result. Native macOS preflight supports Docker execution
but rejects Firecracker/KVM (`../velnor/crates/velnorctl/src/local_diagnostics.rs:1-5,76-152`).

### Evidence fields for `g0_checker`

Use the existing evidence envelope's `release` and `install` objects, with the
following machine-verifiable values. The checker already requires the starred
fields; the remaining fields should be retained in the evidence payload for
independent cross-checking:

```json
{
  "release": {
    "applicability": "required",
    "release_channel": "stable|preview",
    "release_version": "X.Y.Z or preview version",
    "tag_target_sha": "40-hex source commit",
    "release_id": "immutable provider release ID",
    "asset_digests": {"asset-name": "sha256:<64-hex>"},
    "apt_feed_revision_suite_and_candidate": "consumer-sha; suite; version; manifest-sha",
    "homebrew_tap_revision_and_formula": "consumer-sha; formula; version; manifest-sha",
    "manifest": {
      "schema": "exact current schema",
      "product_id": "configured product",
      "source_ref": "exact tag or refs/heads/main",
      "source_commit": "same 40-hex SHA",
      "manifest_sha256": "sha256:<64-hex>"
    }
  },
  "install": {
    "applicability": "required",
    "install_upgrade_test_environment": "OS image; architecture; runner; clean workspace/PATH",
    "installed_binary_identity": {
      "product_id": "configured product",
      "channel": "stable|preview",
      "version": "same release version",
      "source_sha": "same source commit",
      "manifest_sha256": "same canonical digest",
      "binaries": [
        {"name": "velnorctl", "path": "absolute installed path", "sha256": "sha256:<64-hex>"},
        {"name": "velnor-runner", "path": "absolute installed path", "sha256": "sha256:<64-hex>"},
        {"name": "velnor-workflow", "path": "absolute installed path", "sha256": "sha256:<64-hex>"}
      ]
    },
    "upgrade_from": "prior channel/version or null",
    "switch_from": "prior channel/version or null",
    "service_manager_result": "systemd success|not-applicable with reason",
    "functional_result": "success"
  }
}
```

`tag_target_sha` must equal the evidence record's actual checkout/source SHA;
installed `product_id`, version, and source SHA must equal the release. The
checker must reject missing sibling rows, empty/unsupported architecture,
manifest digest mismatch, stale parent records, local checkout/PATH fallback,
and a `functional_result` that is merely a formula or package test. APT
service success requires a genuine systemd-capable environment; Homebrew's
macOS G2 result is explicitly `not-applicable` for systemd and must not invoke
OrbStack.

The current checker implementation only validates the existing scalar release
and install fields (`../dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:374-415,2006-2198`);
unknown nested manifest/environment fields would otherwise be ignored. Before
using this as a gate, `g0_checker` must type and validate the manifest,
artifact, component, environment, operation, and service fields above (or
reject records that omit them). Do not treat the current free-form publication
strings or non-empty binary list as proof of channel/feed/formula/install
coherence.
