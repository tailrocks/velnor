# Homebrew contract checkpoint

Status: implementation-ready consumer proposal; producer handoff still
required. No published URL, checksum, target, or formula is being advertised
from this checkpoint.

Implementation: Homebrew branch `codex/github-first-homebrew`, corrected
source commit `6520aad7bd66d53349e040508146956e9c4f0c1e` (pushed).

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
components[{name,crate,version,binary,feature,identity,targets}]
```

Canonical component rows additionally carry `feature` (`null` or
`release-build`) and `identity` (`version` or `revision`). The Velnor
profile is exact: `velnorctl` and `velnor-runner` use
`release-build`/`version`; `velnor-workflow` uses `null`/`revision`.
Archive component rows carry the same fields and must equal their canonical
parent rows.

Canonical release IDs use the shared grammar `^[1-9][0-9]*$`: positive
decimal provider IDs with no leading zero. Repository-qualified tags and
locally invented slash IDs are rejected. APT must adopt this same producer
grammar; this Homebrew change does not modify the APT checkout.

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

The handoff also carries `product-manifest.json.sha256`, an externally
computed standard checksum sidecar with exactly one row naming
`product-manifest.json`. The updater checks the row against the manifest,
checks the sidecar's provider release asset size/digest, and invokes provider
attestation verification on the sidecar. Path-prefixed or malformed checksum
rows fail closed; producer output must normalize to this basename contract.

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

The exact producer comparison remains blocked at the serializer boundary.
Native producer commit `b2a31c1a` adds the provider-bound attestation, but its
release renderer still emits `sha256  product-assets/product-manifest.json`.
The producer verifier and this consumer intentionally require the basename
`product-manifest.json`; Homebrew must not accept the producer's path-bearing
row. The post-fix fixture will be admitted only from a pinned producer
serializer/golden with its source and executable digests recorded, plus a
pre-fix path-bearing negative proving the old bytes fail closed.

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

The native product source contract is audited at
`2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`; the latest producer attestation
renderer is `b2a31c1a`, but its sidecar serialization is not yet compatible.
The producer handoff does not yet contain the app archive, canonical product
manifest, or an accepted post-fix golden. The updater
therefore renders no checked-in formula. Integration waits for the producer's
exact `product-manifest/v1` record and immutable archive bytes; the updater
derives checksums and URLs from those bytes and has no fallback to old package
manifests or runtime releases. Release-attestation production and preview
publication semantics remain producer-owned dependencies; this checkpoint adds
no local substitute.

## Local proof

From the Homebrew worktree:

```text
jq -e . config/homebrew-release-contract.json
bash -n scripts/package-update.sh
bash -n scripts/test-package-update.sh
./scripts/test-package-update.sh
ruby -c Formula/velnorctl.rb.template
ruby -c Formula/velnorctl-preview.rb.template
git diff --check
```

The fixture executes hostile SemVer and positive-release-ID cases, component
feature/identity mismatches, sidecar digest/path tampering, stable
same-version rerun and canonical-digest mutation, preview ordering/rollback,
missing canonical authority and attestation, archive checksum tampering,
parent identity mismatch, all sibling binaries and version/source identity,
Linux projection filtering, historical formula replacement guarding, generated
formula Ruby syntax, and temporary install/uninstall smoke. It intentionally
does not claim a clean hosted macOS install or published GitHub provenance;
those remain integration gates.
