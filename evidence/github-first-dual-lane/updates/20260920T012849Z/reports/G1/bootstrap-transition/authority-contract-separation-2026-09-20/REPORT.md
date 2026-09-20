# Authority contract separation — 2026-09-20

Status: **contract-only; unexecuted; no G1 claim**.

The current owner plan v3 is approval-required and mutation-free. Its live
SHA-256 is
`eb130052633ba5a2fec067d835a6a2c2d4e7c89c567563d618f7902a4ca54871`.
It supersedes v2 (`8609f29fafb092608696c825553a0a85d2fa53d277c749026aa5d2eb7b0e229e`).
Authoritative `origin/main` is
`d20d4d1d17590cca85b501d982cbaad70d42c641` (parent
`1048337062ea625fada1b4f7c07f2feed75f60c7`).

## Separation

`Policy-bootstrap-A` is the one-use external Tree-A semantic admission. Its
strict schema permits only operator/build-image/head/base/prospective-tree,
lease, transaction, review-digest, and workload-census evidence. It binds
`refs/pull/<PR>/merge`; it carries no Actions run/job/artifact/release ID, no
artifact subject, no product identity, no permanent-main claim, no release or
attestation write authority, and no permanent B pin. Its positive fixture has
synthetic values and no fabricated GitHub producer IDs.

`Policy-bootstrap-B` is separate. Its strict permanent product schema requires
the actual `refs/heads/main` source and tree/closure, trusted publisher
workflow/ref/SHA, repository ID, run/database/attempt, producer job/check-run,
Actions artifact REST ID/name/digest/size/expiry, upload outputs, raw REST ZIP
digest/size, independently extracted inner-payload digest/size, binary
revision/closure, draft release ID/tag/target, manifest digest, custom
predicate subject/path/type, signer identity, terminal step census, and live
verification flags. It forbids temporary authority, lease, build image, PR
source, or temporary key use. It also requires a distinct read/checks-only
`Policy-bootstrap-B` verifier App/integration identity; the Tree-A App cannot
emit Main-B. Its positive fixture is synthetic and does not prove that B
exists.

The staged context spelling is exact:

```text
Tree A:  Policy-bootstrap-A
Main-B:  Policy-bootstrap-B
Tree B:  Policy
```

The bundle contains 30 hostile fixtures: 26 expected schema rejects and 4
expected semantic rejects. It includes forged numeric identities, rerun
attempt, artifact/name, raw service-ZIP digest, inner-payload/binary digest,
source SHA/ref/closure, workflow/publisher identity, subject name/path,
temporary authority, and draft-first lifecycle cases. Structural testing
passed with Python `jsonschema` 4.26.0 Draft 2020-12 plus date-time format
checking. The four semantic cases were schema-accepted as intended; they must
be rejected by the eventual verifier after live API, raw ZIP, and signature
checks.

The prior 34-case validator-binding audit and its
`validator-binding-audit-2026-09-20/binding-predicate.schema.json` remain
byte-preserved historical evidence only. `schema-resolution.json` makes the
replacement explicit: current fixtures resolve only the two schemas in this
directory. Neither fixture set is a release, merge, dispatch, authority
approval, or G1 pass.

## Publisher and attestation contract

Official read-only Git refs and `action.yml` inputs were checked:

```text
actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
  refs: HEAD, refs/heads/main, refs/tags/v7.0.1
  outputs: artifact-id, artifact-digest

actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6
  refs: refs/tags/v4, refs/tags/v4.2.2
  inputs: subject-name, subject-digest, predicate-type, predicate-path,
          push-to-registry, create-storage-record
```

These are official action requirements, not evidence of a Velnor run. The v3
plan still requires a separately reviewed validator-workflow upload-artifact
pin; the resolved upload commit is not evidence that such a workflow exists.
The
custom binding command contract is:

```yaml
subject-name: velnor-workflow-policy-validator-artifact-<ARTIFACT_ID>.zip
subject-digest: sha256:<RAW_SERVICE_ZIP_SHA256>
predicate-type: https://velnor.dev/attestations/policy-validator-binding/v1
predicate-path: attestations/velnor-policy-validator-binding.v1.json
```

The v3 plan fixes the artifact predicate namespace/path above. Its release
predicate URI/path is not explicit; this contract uses the derived
`https://velnor.dev/attestations/policy-validator-release/v1` and
`attestations/velnor-policy-validator-release.v1.json` as a review-required
placeholder, not a verified implementation fact. The attestation job has
`contents:read`, `actions:read`, `id-token:write`, and `attestations:write`;
build/verify has only `contents:read` and `actions:read`; draft reservation and
publication have only `contents:write`. `push-to-registry=false`,
`create-storage-record=false`, and no OCI artifact metadata write are required.
The action default for `create-storage-record` is true, so explicit false is
mandatory.

The publisher must fetch the immutable artifact by numeric ID, compare REST
metadata and upload output, hash the raw REST ZIP bytes, safely extract and
independently hash the inner payload and binary, verify the custom predicate,
then cross-check the exact run/attempt/jobs/steps and source/ref/closure. A
name, run ID, extracted payload, or standard binary provenance alone is not a
pass. Draft release creation precedes final manifest/predicate signing: require
nonzero `release.id`, exact immutable tag/target, then publish only after all
assets and attestations verify.

## Permanent trust versus temporary key

Permanent B trust is GitHub OIDC/Sigstore-based and independent of the
temporary bootstrap App key. Exact permanent constraints are issuer
`https://token.actions.githubusercontent.com`, repository `tailrocks/velnor`,
workflow
`tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml`, its
immutable `@refs/heads/main` ref and exact workflow commit, source
`refs/heads/main`, repository ID, product/schema/purpose/platform, closure,
raw ZIP subject/digest, and both custom predicate types. `temporary_key_used`
must be false.

`permanent-b-trust-storage-contract.json` defines the required reviewed Tree-B
consumer paths and external append-only evidence root, with policy revision,
OIDC certificate/signing records, raw artifact/release evidence, digest files,
rotation history, and revocation history. Tree-B or a later normal reviewed PR
rotates permanent policy revisions; old revisions and historical verification
records remain retained during overlap. Cleanup revokes and archives the
temporary App/key only after permanent B trust is proven. It never removes the
permanent B publisher or trust policy.

## Verifier status

Read-only inspection recorded in `origin-main-verifier-inventory.json` of
`origin/main` at the exact SHA above found existing
provenance-signing and legacy candidate/runtime binding code, but no typed
permanent-B verifier, no bootstrap-admission verifier, and no
`.github/workflows/ci-policy-validator-products.yml` publisher. Therefore:

```text
real verifier:       UNIMPLEMENTED
live API evidence:   NOT RUN
cryptographic proof: NOT RUN
gate claim:          FALSE
```

The exact hashes and machine-readable results are in
`authority-contract-separation-results.json`. Canonical-root provenance and
the absent previous-root check are in `canonical-root-manifest.json`.
