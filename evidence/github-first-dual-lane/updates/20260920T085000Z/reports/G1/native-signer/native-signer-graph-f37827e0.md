# Native product signer graph design

Status: design-only; implementation is blocked on bounded review.

## Evidence anchor

- Source worktree: `/private/tmp/dual-lane-native-product-v3`
- Source branch: `codex/github-first-native-product-v3`
- Source base/HEAD: `f37827e09415ba5733b37e97a16aef716e553322`
- HEAD subject: `fix(native): keep runtime debs out of product assets`
- Source worktree was clean at inspection.
- Existing source owners: `crates/velnor-workflow/src/s2/primitives/native_product.rs`,
  `crates/velnor-workflow/src/s2/primitives/release.rs`, and
  `crates/velnor-workflow/templates/release-package-signer.yml`.

This document is a producer/consumer interface proposal for review by the APT
owner and independent native-product reviewer. It is not implementation,
generated-workflow output, publication evidence, or a claim that Intel/macOS
publication is ready.

## Existing mismatch

`render_native_product_steps` currently resolves a provider release ID and
assembles `product-assets` inside the final publisher, then directly invokes
`actions/attest-build-provenance`. The existing reusable
`ci-release-package-signer.yml` is used by the Debian signer only and accepts
`artifact-name`, `subject-path`, and `source-ref`; it has no explicit source
digest input and no output.

APT's provider check requires every subject to be attested by the exact shared
`ci-release-package-signer.yml`, with the source ref/commit, GitHub OIDC
issuer, SLSA predicate, hosted runner, and one exact subject digest. Product
metadata (`release-attestation.json`) is not a cryptographic substitute.

## One graph, one asset publisher

Stable `release.yml` and preview `preview.yml` remain the only workflows. No
`workflow_run`, dispatch fallback, or second publisher is allowed.

```text
admit-product-release (identity reservation only)
             |
native-product-build + runtime/debian artifacts
             |
native-product-assemble [read-only]
             |
sign-native-product[N] [shared reusable signer]
             |
publish [only release asset uploader/draft flipper]
```

The provider admission job is not an asset publisher. It may GET the exact
tagged candidate, or POST one draft when and only when the GET is a classified
404. It never uploads assets or changes draft state. This reservation is
necessary because the canonical manifest must contain the numeric provider
release ID before it is signed. It removes the provider-ID/manifest
self-bootstrap cycle.

### Stable machine jobs

| Job | `needs` | permissions | role/outputs |
| --- | --- | --- | --- |
| `admit-product-release` | `admit-provider`, `verify`, `build` | `contents: write` only; protected release environment if policy requires | Strict tag/ref admission or draft reservation. Outputs all strings: `release_id`, `release_url`, `assets_url`, `release_tag`, `source_ref`, `source_commit`, `target_commitish`, `phase=draft\|published`. |
| existing `native-product-build` | existing build admission | `contents: read` | Typed target/component build. Intel remains an explicit blocked target; no silent target removal/fallback. |
| `native-product-assemble` | `admit-product-release`, `native-product-build`, `debian` (plus any required read-only metadata job) | `contents: read` only; no `id-token`, `attestations`, or provider write | Downloads source-bound build/debian artifacts; validates typed census; emits `native-product-assets`. Runs only when provider phase is `draft`. |
| `sign-native-product` | `native-product-assemble` | caller grants only `contents: read`, `id-token: write`, `attestations: write` | Matrix calls `./.github/workflows/ci-release-package-signer.yml`; no provider write and no asset upload. |
| existing `publish` | provider, runtime, Debian, image/metadata, assemble, signer jobs | `contents: write`, `packages: read`; no direct attestation permissions | `if: always()` plus explicit result/phase gate. Sole release asset uploader and draft flipper. |

`publish` must not rely on GitHub's implicit skipped-`needs` behavior. Its
condition is conceptually:

```text
always()
&& needs.admit-product-release.result == success
&& needs.native-product-build.result == success
&& (phase == published ||
    (phase == draft
     && needs.native-product-assemble.result == success
     && needs.sign-native-product.result == success))
```

For `phase=published`, assembly and signer jobs are intentionally skipped;
the publisher executes the complete remote verification path. For
`phase=draft`, any skipped/failed assembly or signer job is fatal. Unknown
phase is fatal.

### Preview machine jobs

Preview uses the same graph and permissions. Differences are only typed
identity values:

- `release_tag = preview-${commit}`;
- default-branch source ref, e.g. `refs/heads/main` from config;
- prerelease/draft policy remains the existing immutable preview policy.

Preview must not revert to rolling Debian-only publication or direct
attestation. The existing preview publisher remains the sole asset writer.

## Provider admission and race rules

Admission must classify API results; `gh api ... || true` is forbidden.

1. GET exact tag.
2. Accept only a real 404 as absence.
3. On absence, create exactly one draft with exact tag, source commit,
   name, and stable/preview flags.
4. Re-read the exact tag and emit only the re-read identity.
5. If another creator wins the race, re-read and accept only an exact identity
   match; mismatched ID/tag/target/name/flags fails closed.
6. Existing published release emits `phase=published`; it is immutable and
   must not be changed.
7. Final `publish` revalidates provider identity and tag ref immediately before
   upload, validates complete remote census/bytes, flips once, then revalidates
   the published state/census/bytes. No API atomicity is claimed.

The numeric provider ID is serialized as a positive decimal **string** in the
canonical manifest, archive `parent_manifest_id`, and release attestation.
No JSON-number coercion is allowed.

## Assembly artifact and canonical manifest

The sole CI transport artifact is `native-product-assets`:

```text
product-manifest.json
product-manifest.json.sha256
release-attestation.json
<typed binary subjects>
<typed archive subjects>
```

Debian packages stay in the release-root/debian artifact. They remain rows in
the canonical product inventory but are not copied into `product-assets`; the
existing `sign-deb` jobs provide their shared-signer attestations.

The product manifest is the one producer-owned application manifest. It must:

- use the exact provider `release_id` string;
- contain the complete typed target/component/artifact census;
- omit itself and `product-manifest.json.sha256` from `artifacts`;
- contain no duplicate basename or unsafe path;
- have a basename-only checksum sidecar;
- produce `release-attestation.json` only after its digest exists;
- set `release-attestation.assets == manifest.artifacts` and preserve the exact
  APT 13-field attestation schema.

`native_product.rs` owns a typed `NativeProductPlan`/subject inventory. The
renderer consumes that plan; it must not hard-code product, crate, binary,
component, or target names in `release.rs`. The plan partitions subjects into:

```text
product-manifest.json
product-manifest.json.sha256
release-attestation.json
typed non-APT binary rows
typed archive rows
APT package rows (covered by existing sign-deb)
```

The generated matrix and assembly census must derive from the same plan. The
blocked Intel target remains explicit in the plan and keeps the product lane
fail-closed; it is not silently omitted from a claimed ready matrix.

## Shared signer contract

Extend `crates/velnor-workflow/templates/release-package-signer.yml` with a
required `source-digest` input. The signer validates lowercase 40-hex input and
requires it to equal its own `${{ github.sha }}`. It validates the exact safe
source ref, artifact basename, and subject basename before download.

Native callers pass:

```yaml
artifact-name: native-product-assets
subject-path: ${{ matrix.subject }}
source-ref: refs/tags/${{ github.ref_name }} # stable
source-digest: ${{ github.sha }}
```

Preview passes the exact configured default-branch ref and the same
`${{ github.sha }}` source digest. The signer has no provider API output; its
GitHub attestation record is the output. Every caller, including existing
`sign-deb`, must supply the new required input.

The signer matrix is generated from the typed subject inventory:

```text
product-manifest.json
product-manifest.json.sha256
release-attestation.json
<every unblocked typed native binary>
<every unblocked typed native archive>
```

APT package subjects are not duplicated in this matrix: `sign-deb` is the same
shared signer and covers them.

## Source-ref and digest semantics

For a stable tag run, `github.sha` is the tagged commit and the caller passes
`refs/tags/<release-tag>`. For preview, `github.sha` is the exact default
branch commit and the caller passes its exact `refs/heads/<branch>` ref. The
source contract, provider target, manifest, release attestation, and signer
verification must all compare against that same pair. A caller-provided digest
that differs from the signer run's `github.sha` fails before attestation.

## Final publisher verification

Before upload and after publication, `publish` verifies every native subject
with the shared signer selector and exact source binding:

```text
--signer-workflow "$GITHUB_REPOSITORY/.github/workflows/ci-release-package-signer.yml"
--source-ref <admitted source ref>
--source-digest <admitted source commit>
--cert-oidc-issuer https://token.actions.githubusercontent.com
--predicate-type https://slsa.dev/provenance/v1
--deny-self-hosted-runners
--format json
```

It independently verifies provider API identity, tag ref, complete asset
census, exact bytes, manifest/sidecar, release-attestation semantics, and
archive parent IDs. Metadata never self-authenticates provider identity.

## Review tests required before implementation

1. Signer template rendering: required input, source-digest mismatch/non-hex,
   source-ref rejection, exact subject/artifact validation, minimal perms.
2. Graph rendering: exact `needs`, one asset writer, no `workflow_run`, no
   dispatch fallback, no native direct attestation, and correct published-path
   skip behavior under `always()`.
3. Provider admission shell fixtures: 404-only creation, existing draft,
   existing published, create race, mismatched tag/target/name/flags, changed
   tag ref, and string-vs-number release ID rejection.
4. Assembly fixtures using actual generated bytes: target/component census,
   duplicate/unsafe basenames, missing sibling, digest/size/mode failure,
   no self-hash, exact sidecar/attestation, valid Mach-O and malformed/MH_OBJECT
   negatives.
5. Final publisher fixtures: pre-upload signer check, remote census/byte
   mutation, post-flip race, and failure on skipped/failed draft prerequisites.
6. Exact APT/Homebrew consumer runs against the actual producer artifact, not a
   handwritten fixture; verify APT package subjects remain covered by
   `sign-deb` and native subjects by `sign-native-product`.
7. Regenerate all owned workflows through the supported generator and run
   full rendered actionlint. Do not hand-edit generated YAML.

No publication, dispatch, install, host runtime, Mac runtime, or generated
workflow mutation is part of this design checkpoint.
