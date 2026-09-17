# Docker Multi-Arch Publish: Design-Challenge Memo

Area D, `velnor-workflow`. Base: velnor2 HEAD `33688938`.
Evidence consulted structurally only (job names + line count, no bodies):
Jackin clone `/tmp/jackin` ref `pr994`,
`.github-gen/sources/workflows/construct.yml` (539 lines:
`changes` paths-filter, `build` digest matrix, `publish-manifest`
plus PR rehearsal, required gate). No names, paths, or bytes from
that source appear in this change.

## 1. Is the capability generic?

Yes. Multi-arch OCI publication is the same contract in every
repository: one immutable version tag, one native builder per
platform, content-addressed digest transport, and a single manifest
assembly over a verified-complete digest set. Nothing about that
contract depends on Rust, Debian, or the Velnor job image.

The current tree already proves the generic shape exists: the
`native` identity lane renders admission, per-arch staging builds,
and one `imagetools create` index job. What is *not* generic is its
binding: a hardcoded job Dockerfile, a hardcoded workflow package
compiled into the image, record-bound labels, and a Debian consumer.
A repository with only a consumer-owned Dockerfile cannot use any
of it; the non-identity fallback is a single-arch, single-job
`build-push-action` with no admission, no caches, no digest
transport, no resume, and no manifest verification.

So the change has two halves, both extensions of existing
machinery:

1. A standalone `docker` release publisher for consumer-owned
   Dockerfiles: `image` + optional `dockerfile`/`context`/
   `platforms`, rendering admission, a per-platform push-by-digest
   matrix with BuildKit caches and SBOM/provenance, and one
   manifest job that verifies the complete digest set before
   creating the version tag.
2. Hardening of the existing `native` image lane to the same
   invariants where it is behind: SBOM/provenance on the platform
   builds (with the `id-token`/`attestations` permissions that make
   them real) and exact-set verification of the assembled index.

Dockerfiles and image contents stay consumer-owned throughout:
the generator references the declared Dockerfile path and build
context; it never emits image contents.

## 2. What is already supported?

The `native` identity lane (`primitives/release.rs`) already owns
most of the protocol, and the new publisher reuses its shape:

- Multi-arch matrix: `image_platform_matrix` (amd64+arm64, native
  runners, `ubuntu-24.04-arm` for arm64).
- BuildKit caches: registry `buildcache-<arch>` plus `gha` scope
  cache, `mode=max` on export.
- Digest transport: per-arch `image-<arch>.digest` artifacts
  (`retention-days: 2`), `imagetools inspect` with
  attestation-manifest filtering and `sha256:` validation.
- Manifest assembly: `imagetools create --tag <version>` over the
  staging refs, then re-inspection proving the tag references the
  verified digests.
- Resumable publication: `existing-image-digest` dispatch input;
  admission adopts a present tag only with a matching release or
  the explicitly supplied recovery digest.
- Reconcile: absent creates, coherent re-verifies without
  mutation, anything else fails closed (`refusing to adopt unknown
  bytes`, `refusing a fail-open publish`, tag-moved guards).
- Retention and provenance: short artifact retention, `attest` and
  the shared package-signer template on the package lanes,
  `provenance`/`sbom` on the plain (non-identity) image publisher.

Gaps this change closes:

- No generic multi-arch path: the plain publisher is single-arch
  with no admission, caches, digest transport, resume, or manifest
  verification.
- No SBOM/provenance in the `native` platform builds (the one
  lane that publishes the consumed index lacks exactly what the
  plain lane has).
- No shared digest-set gate: each shell fragment re-validates
  ad hoc. New `release verify-digests` runtime command validates
  one directory of `image-<arch>.digest` files against the
  declared arch set (present, single `sha256:` token, 64 lowercase
  hex, 4 KiB cap, no extras) so "complete verified set" is one
  tested unit instead of scattered shell.
- Index verification proves inclusion but not exclusivity. Both
  manifest jobs now also prove the assembled tag carries exactly
  the verified platform set (attestation manifests excluded).

Explicitly not changed: `scan/docker.rs` (the verify lane already
pairs arch Dockerfiles and fails closed on unknown hosts; publish
is release-side), `ir.rs` collapsed verify rendering, and the
`runtime.rs` selection closure.

## 3. What bug class is removed?

Split-brain publication: a version tag that names a *subset* of
platforms, or two concurrent publishers racing to create the same
tag with different bytes. The enabling condition is tag creation
as a multi-writer operation over an unverified set: platform jobs
that push tags, plus a manifest step that assembles whatever
happened to arrive.

The change removes the condition structurally:

- Platform jobs are digest producers, never tag writers. The new
  publisher pushes by digest with no `tags:` at all; the `native`
  lane keeps its commit-scoped staging tags but the consumer
  version tag still has exactly one writer. Per-platform artifact,
  exactly one authorized publisher; per version tag, exactly one
  manifest job.
- Admission reconciles before anything builds: absent opens the
  lane, a coherent present tag (recovery digest match, or matching
  release on the `native` lane) re-verifies without mutation, and
  anything else is a conflict failure, never an overwrite.
- The manifest job assembles only over a complete verified set:
  every declared arch present, every digest strictly validated,
  no extra files, and a post-create inspection proving the tag
  carries exactly that set. A partial matrix cannot publish a
  partial tag.
- One concurrency group per ref (`release-${{ github.ref }}`,
  `cancel-in-progress: false`) serializes publishers so resume
  and fresh publication cannot interleave on the same tag.

Secondary class removed: unattested native images. Platform
builds now emit SBOM and provenance attestations, so the index
consumers pull is traceable to a verified build per platform.

## Evidence

- `crates/velnor-workflow/src/scan/docker.rs`: local `docker
  build` verify lane (unchanged; cited as the verify-side half).
- `crates/velnor-workflow/src/primitives/release.rs`:
  `image_platform_matrix`, `render_image_admission_job`,
  `render_image_platform_job`, `render_image_index_job`,
  `render_native_publish_job` reconcile, `native_image_jobs`.
- `crates/velnor-workflow/src/runtime.rs`: `release verify-tag`
  (VERSION immutable guard), `package-binary`/`package-deb`
  sidecar discipline; new `release verify-digests`.
- Structural evidence: `construct.yml` job shape above; no
  content copied.

## Declared Contract (TOML Example)

```toml
schema = 1

[generator]
repository = "example/app"

[release]
enabled = true
reason = "multi-arch container publication for the service image"
kind = "docker"
image = "ghcr.io/example/app"
dockerfile = "Dockerfile"
context = "."
platforms = ["linux/amd64", "linux/arm64"]
```

`dockerfile` defaults to `Dockerfile`, `context` to `.`, and
`platforms` to both Linux architectures when omitted. Any other
platform value is a configuration error. The rendered
`release.yml` publishes exactly the declared platform set under
one immutable version tag, resumable via the
`existing-image-digest` dispatch input.
