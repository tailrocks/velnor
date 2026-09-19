# Homebrew contract checkpoint

Status: implementation-ready consumer proposal; producer handoff still
required. No published URL, checksum, target, or formula is being advertised
from this checkpoint.

Implementation: Homebrew branch `codex/github-first-homebrew`, corrected
source commit `c772971de3df714b33bffb55febfcfe478428175`.

## Authority

`tailrocks/velnor` owns one canonical `velnor.product-manifest/v1` JSON
record. Its exact top-level fields are:

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

Canonical release IDs use the shared grammar
`[A-Za-z0-9][A-Za-z0-9._:/-]*`; slash is valid for provider namespaces and
must be accepted consistently by APT and Homebrew.

Homebrew consumes only the projection where `kind` is `homebrew-archive` and
the target is `aarch64-apple-darwin`. The supported release contains exactly
one such artifact named
`velnorctl-<version>-aarch64-apple-darwin.tar.gz`. Linux artifact rows remain
available to the APT consumer and are ignored by this updater.

The package archive has exactly these root members:

```text
identity.json
manifest.json
velnor-runner
velnor-workflow
velnorctl
```

The three binary members are one product release and source commit, while
their component records retain explicit crate versions. Archive
`manifest.json` and `identity.json` are subordinate records. They repeat the
canonical identity and carry `parent_manifest_id == release_id`; they do not
become a competing authority. The generated formula carries the external
SHA-256 of `product-manifest.json`. This keeps the graph acyclic because the
canonical manifest hashes the archive and the archive contains the subordinate
record.

Stable requires `X.Y.Z`, `refs/tags/vX.Y.Z`, and `vX.Y.Z`. Preview requires
`X.Y.Z-preview.N+<sha7>`, `refs/heads/main`, and
`preview-<full-source-sha>`. Both formula channels install all three binaries
and subordinate records. Stable/preview versions are monotonic per channel;
switching is uninstall-then-install; rollback is a prior tap revision.

SemVer core and numeric prerelease identifiers reject leading zeros; comparison
supports arbitrary-length numeric identifiers. Same-version reruns must match
the prior generated formula's canonical manifest SHA-256, in addition to source,
tag, release ID, and archive checksum.

The producer handoff also requires exactly one
`release-attestation.json` (`velnor.github-release-attestation/v1`). It binds
GitHub provider/release URL and immutable release ID, resolved source ref and
commit, canonical manifest SHA-256, and the exact canonical artifact census.
`target_commitish` is diagnostic only; unresolved source evidence is rejected.

## Capability boundary

Native macOS arm64 is the only supported Homebrew target. Intel remains an
explicit unsupported result until a native artifact and matching clean-client
test exist. The package is an operator surface; host execution controls Linux
jobs through Docker/OrbStack and does not claim native macOS Actions,
Firecracker, or KVM.

Generated formulas carry a target-support marker. The updater refuses to
replace the historical source-build formula with an arm64-only formula unless
`VELNOR_ALLOW_ARM64_FORMULA_REPLACEMENT=1` is explicitly set. This is a
migration guard, not Intel support.

## Producer dependency

The native arm64 build already proves all three required binaries at
`abe9ad82`, but the producer handoff does not yet contain the app archive and
canonical product manifest. The updater therefore renders no checked-in
formula. Integration waits for the producer's exact `product-manifest/v1`
record and immutable archive bytes; the updater derives checksums and URLs from
those bytes and has no fallback to old package manifests or runtime releases.

## Local proof

From the Homebrew worktree:

```text
bash -n scripts/package-update.sh
bash -n scripts/test-package-update.sh
./scripts/test-package-update.sh
ruby -c Formula/velnorctl.rb.template
ruby -c Formula/velnorctl-preview.rb.template
git diff --check
```

The fixture executes hostile leading-zero and component-version cases, stable
upgrade/rollback, same-version canonical-digest mutation, preview
ordering/rollback, missing canonical authority and attestation, archive
checksum tampering, parent identity mismatch, all sibling binaries and
version/source identity, Linux projection filtering, historical formula
replacement guarding, generated formula Ruby syntax, and temporary
install/uninstall smoke. It intentionally does not claim a clean hosted macOS
install or published GitHub provenance; those remain integration gates.
