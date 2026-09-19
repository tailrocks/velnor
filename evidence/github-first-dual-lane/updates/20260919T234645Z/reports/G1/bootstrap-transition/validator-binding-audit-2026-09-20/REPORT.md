# Validator-only signed artifact binding adversarial review

Status: **unimplemented; no verifier result and no G1 gate claim**.

Observed 2026-09-20 UTC. This is an independent validation of the typed
Linux-X64 `velnor-workflow-policy-validator` design. It is not PR957 candidate
bootstrap validation. No source checkout, workflow, release, artifact,
dispatch, credential, or host state was changed.

## Finding

The proposed product and verifier are absent from the authoritative source.
`origin/main` is `1048337062ea625fada1b4f7c07f2feed75f60c7` (parent/design main
snapshot `b5a4b4afaa6ca807927cacc03659b570a895dd5c`, subject `fix: make
generated workflow rendering reproducible`). The local checkout
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` is retained only as a secondary
inspection context; PR957 is `92387e88c32f933a9061b819256e535662655cb2`.
Exact searches against authoritative `origin/main` found no
`velnor-workflow-policy-validator`, `artifact_service_zip_sha256`,
`upload_artifact_id_output`, `producer_binding_subject_sha256`,
`ci-policy-validator-products.yml`, `workflow-policy-validator.v1`, or
`artifact-binding.v1`. The expected typed workflow, generated source, setup
action, Rust product module, custom predicate verifier, and protected typed
publisher paths are absent. The current `ci-policy.yml` only filters a
candidate artifact by name and `expired=false` (line 112); that is not this
binding.

The authoritative runtime workflow does contain an existing full-SHA
`actions/upload-artifact` pin (`043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` at
line 192) and standard `gh attestation verify` calls (lines 275-276), but no
validator service-artifact custom predicate or raw-REST binding.

The existing `producer_workflow`/`workflow_run` logic in
`crates/velnor-workflow/src/s2/primitives/release.rs` is a source/release gate,
not an artifact-to-numeric-job binding. It does not verify an Actions artifact
ID, service ZIP digest, run attempt, check-run ID, upload step, custom
attestation, or raw archive bytes.

The result is therefore `unimplemented`, not pass, fail, or “conditionally
green”. The attached 34-case matrix is a required implementation contract.

## Revision reconciliation

The revised design fixes the previously identified specification gaps:

* draft release creation and nonzero `release.id` now precede `binding.json`,
  the raw-ZIP binding attestation, and the final manifest attestation;
* the custom subject is the exact raw REST ZIP name
  `velnor-workflow-policy-validator-artifact-<artifact-id>.zip` and its digest,
  not an inner binary digest or a `github-actions-artifact:<id>` alias;
* the binding predicate is
  `https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1`,
  the release predicate is
  `https://tailrocks.dev/velnor/attestation/validator-release/v1`, and the
  signed binding carries draft ID/tag/target;
* the exact officially resolved `actions/attest@v4` commit is
  `1e69f48acb82d1966a394da916b4c1698aa569d6`; its required contract is
  `contents:read`, `id-token:write`, `attestations:write`,
  `push-to-registry:false`, `create-storage-record:false`, with no
  `artifact-metadata:write`.

Remaining gaps are implementation/authority gaps, not silently accepted by
these fixtures: no typed producer, raw-ZIP verifier, or protected publisher
exists on authoritative `origin/main`; the validator workflow's exact
`actions/upload-artifact` full-SHA pin remains a requirement placeholder (the
existing runtime pin is not a validator pin); no live IDs, API objects, raw ZIP,
or cryptographic signatures were verified; and the protected pre-main
main-bound authority plus old-checker-to-B handoff remains absent. Therefore
the revision is specification-corrected but still not implementation-approved
or G1-complete. One event-mode decision remains explicit: the design lists
`workflow-dispatch-main` as allowed, while the positive fixture uses `push`;
dispatch acceptance requires a separately trusted protected-main authorization
claim, and an unprotected dispatch must reject. The implementation must either
prove that claim or narrow the publisher to push-only.

## Persisted evidence

All paths below are relative to this directory:

| File | Purpose | SHA-256 |
| --- | --- | --- |
| `binding-predicate.schema.json` | Strict revised custom predicate shape, including draft release binding | `4bcd53cdd4e1900d4edf240f46da4c14ab570fc7be09f37484a9f56402da3b06` |
| `validator-binding-fixtures.json` | Valid post-draft base plus 34 hostile JSON mutations against the revised contract | `3fa58aab41a0893fb386f55257474cce04036753440525d0d938f424226f1e6d` |
| `validation-results.json` | Exact design/source hashes, authoritative `origin/main` reconciliation, revised action/release contract, unavailable execution status, case IDs, gate disposition | `ca9fc2ebabb0e46aab1a79b0c022753f08221074fc2b3136ed020e7404bd2876` |
| `hostile-fixtures.index.json` | SHA-256 index for 34 materialized hostile JSON documents | `1652ebf312aa4ccf047b4cf93c77f2fe8e3feff72a490ccf022c528226fb5912` |
| `materialize-hostile-fixtures.py` | Deterministic JSON-pointer materializer (evidence helper only) | `6113b9083fdd859a944d6d014522a6f62b18fec348fca9ae012a6e5a4a104ea3` |

Design inputs were hashed before fixture creation:

| Input | SHA-256 |
| --- | --- |
| `../VALIDATOR-ONLY-DESIGN-2026-09-20.md` | `029f639a5cf7aa5e7df248abc714b3206e023bff8a4d23496392273b9c6c90eb` |
| `../VALIDATOR-ONLY-DESIGN-2026-09-20.json` | `925c19715d10082f5416f5e1e11486208eefbbf50097f23325d82256edd1ce4d` |
| `../bootstrap-artifact-feasibility-2026-09-20.md` | `3316446c8ca5715c1d75b198a6884a26f8fdd769fbcd0a036db9a8a55685070e` |
| `../artifact-producer-binding-research-2026-09-20.md` | `e30587f4e1acb8b47b006361af8a982b3c89da265bc698c018dda0696ea47c16` |

The fixture bundle has one positive structural base (`P00`) and 34 materialized
hostile JSON documents, each with an index-recorded SHA-256 and expected
reject decision. It was regenerated against the revised design hashes above.
The base now uses the raw service ZIP as the custom-attestation subject, the
new `tailrocks.dev` binding predicate, the exact full `actions/attest` SHA,
non-OCI permissions, and a signed nonzero draft release binding. Cases cover
forged job/run/attempt/artifact IDs, name-only selection, duplicate siblings,
service ZIP versus inner payload versus binary hash confusion, source
SHA/closure/ref, workflow ref/SHA, signer repository/workflow/event/signature
and predicate type, live run/jobs/step mismatches, expiry, namespace
substitution, and release lifecycle.

`jq empty` passed for both JSON files. A read-only Python contract check passed:
34 unique cases, all expected `reject`, nonzero reserved release ID, and
service/inner/binary digest layers agree in `P00`. This is only fixture
integrity; it is not verifier execution. P00's IDs (`artifact=77001`,
`run=88001`, `attempt=1`, `job=99001`, `check_run=99002`, `release=90001`) are
synthetic fixture values, not live GitHub evidence.

## Required signed predicate

The custom in-toto predicate is deliberately strict about identity shape but
does not treat predicate text as authoritative. It requires:

* product `velnor-workflow-policy-validator`, purpose `policy-validator`, and
  platform `Linux-X64`;
* source repository `tailrocks/velnor`, exact `refs/heads/main`, 40-hex source
  SHA, and 64-hex source closure;
* exact protected workflow path/ref, workflow SHA, `push` event (or an
  independently authorized protected-main `workflow_dispatch`), `main` branch,
  repository ID, run ID/database ID/attempt, fixed `build-linux-x64` job key,
  numeric job and check-run IDs;
* artifact numeric ID, exact closure-derived name, service/archive digest, and
  independently computed inner-payload digest;
* upload step ID, `artifact-id` output, and `artifact-digest` output;
* terminal-success build/upload steps resolved from the protected workflow
  source; raw binary SHA/size/executable mode/self-reported revision/closure;
  and the final manifest subject digest.

Dynamic equality is semantic, not JSON Schema magic. The real verifier must
compare each field against trusted main state, protected workflow source,
artifact REST, run REST, exact-attempt jobs REST, downloaded ZIP bytes, safe
extraction output, binary self-report, release API, and signed subjects.

The source closure must use the repository's canonical closure algorithm (sorted
`git ls-tree` inputs plus explicit closure version/features/profile footer),
with a typed validator domain/tag namespace that cannot collide with the
application runtime product. A self-reported `--closure` value or the first 16
hex tag characters is only a claim/locator until the full closure is recomputed
from the exact trusted source.

Important GitHub limitation: the jobs API exposes step names/status/conclusion,
not workflow step IDs. Therefore the verifier must fetch the exact protected
workflow source at `workflow_sha`, map `upload-validator` to its expected step
name, then compare that name and terminal result in the exact-attempt jobs
response. A signed step ID or display name alone is not enough.

## Concrete producer contract (not executed)

Resolve and record immutable full SHAs before implementation. The following
official GitHub refs were resolved read-only on 2026-09-20; the `actions/attest`
ref was also checked against its official `action.yml` and README:

* `actions/attest@v4` → `1e69f48acb82d1966a394da916b4c1698aa569d6`
* `actions/upload-artifact@v7` → `043fb46d1a93c77aae656e7c1c64a875d1fc6a0a`
* `actions/checkout@v5` → `fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09`

These are official resolved references, not a validator-product pin or design
approval: no validator workflow exists on authoritative `origin/main`. A
future producer must re-resolve and review the exact full commit it pins.

The revised fixed uploader/attestation shape is:

```yaml
permissions:
  contents: read
  actions: read
  id-token: write
  attestations: write

steps:
  - id: upload-validator
    uses: actions/upload-artifact@<reviewed-full-40-hex-sha>
    with:
      name: policy-validator-${{ env.CLOSURE16 }}-Linux-X64
      path: staging/policy-validator/
      if-no-files-found: error
      overwrite: false

```

After the draft release returns its nonzero ID and the publisher verifies the
raw ZIP/API/job facts, the publisher creates the binding attestation:

The build job's permissions above are read-only for repository/actions plus
OIDC/attestation writes. The separate protected release publisher requires
`contents:read`, `contents:write`, `actions:read`, `id-token:write`, and
`attestations:write` for the draft/assets/attestation transaction. Neither job
gets `artifact-metadata:write`.

```yaml
uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6
with:
  subject-name: velnor-workflow-policy-validator-artifact-<artifact-id>.zip
  subject-digest: sha256:<raw-service-zip-sha256>
  predicate-type: https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1
  predicate-path: <isolated>/binding.json
  push-to-registry: false
  create-storage-record: false
```

`subject-digest` must be the upload action's service/archive digest, not the
inner binary hash. The subject name must use the exact numeric artifact ID;
`create-storage-record` and `push-to-registry` are both explicitly false for
this Actions-artifact subject. No `artifact-metadata: write` permission is
allowed: this is not an OCI subject, and that permission would not prove
artifact-to-job binding.

The build job must query the upload output by ID and compare it to the REST
artifact before signing. The protected publisher must fetch the exact object
and enclosing run/jobs attempt, then download the exact archive:

```sh
artifact="$(gh api \
  -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/actions/artifacts/$ARTIFACT_ID")"
printf '%s\n' "$artifact" > artifact.json
gh api \
  -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/actions/artifacts/$ARTIFACT_ID/zip" \
  --output service-artifact.zip
gh api \
  -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/actions/runs/$RUN_ID"
gh api \
  -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/actions/runs/$RUN_ID/attempts/$RUN_ATTEMPT/jobs?per_page=100"
sha256sum service-artifact.zip
```

The draft reservation is a separate, earlier API transaction. The following
is the command contract only; it was not executed:

```sh
TAG="velnor-workflow-policy-validator-v1-${CLOSURE16}"
existing="$(gh api -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/releases/tags/$TAG" 2>/dev/null || true)"
# If an existing release/draft is returned, compare tag, target, assets, and
# attestations exactly; do not delete, reuse, or overwrite a mismatch.
release="$(gh api --method POST -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/releases" \
  -f tag_name="$TAG" -f target_commitish="$SOURCE_SHA" \
  -F draft=true -F prerelease=false)"
RELEASE_ID="$(jq -er '.id | select(type == "number" and . > 0)' <<<"$release")"
gh api -H 'Accept: application/vnd.github+json' \
  "/repos/tailrocks/velnor/releases/$RELEASE_ID"
```

After the raw-ZIP binding is verified, use the same pinned action and the
separate release predicate for the manifest subject:

```yaml
uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6
with:
  subject-path: <isolated>/manifest.json
  predicate-type: https://tailrocks.dev/velnor/attestation/validator-release/v1
  predicate-path: <isolated>/release-predicate.json
  push-to-registry: false
  create-storage-record: false
```

The verifier must compare REST `id`, `name`, `digest`, `expired`, expiry,
`workflow_run.id`, repository IDs, branch, and head SHA; upload outputs; the
raw ZIP hash; safely extracted inner payload hash; and all signed predicate
fields. It must reject if any API request is stale, duplicate, queued, failed,
canceled, expired, or ambiguous.

After downloading the archive, verify the custom binding and signer constraints
against the exact ZIP (not only an extracted member):

```sh
gh attestation verify service-artifact.zip \
  --repo tailrocks/velnor \
  --signer-repo tailrocks/velnor \
  --signer-workflow tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml \
  --source-ref refs/heads/main \
  --source-digest "$SOURCE_SHA" \
  --predicate-type https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1 \
  --format json > binding-verification.json
```

Then enforce the JSON output's immutable certificate/timestamp fields and
user-controlled predicate against live API facts. `gh attestation verify`
itself warns that predicate fields can be falsified by a compromised workflow;
the protected fixed uploader plus API cross-check is therefore required. A
standard binary SLSA attestation cannot replace this service-artifact binding.
The final manifest gets a separate
`https://tailrocks.dev/velnor/attestation/validator-release/v1` predicate over
the manifest subject after the draft ID exists; it is not interchangeable with
the raw-ZIP binding predicate.

## Release-ID lifecycle correction

The design requires a real nonzero draft release ID before creating the custom
binding predicate. The fixture enforces this corrected sequence:

1. Query the immutable closure-derived tag. If a published release exists,
   require exact target/manifest/assets/attestations; if a draft exists,
   require the exact expected target/tag and no conflicting assets.
2. Create one unique draft release with the exact source target. Require a
   nonzero numeric ID, exact tag/target, `draft=true`, and no published time.
3. Download the Actions artifact by numeric ID, hash the raw service ZIP, and
   safely extract/re-hash the inner payload and binary.
4. Write `binding.json` with the exact draft `release.id`, producer IDs,
   workflow/source fields, raw ZIP digest, inner payload digest, and closure.
5. Attest the raw ZIP with the exact custom subject/predicate contract, then
   verify signer, subject, predicate, and all live API facts.
6. Generate the canonical manifest containing the now-existing release ID,
   tag, target, producer binding, and digests; attest it with the separate
   release predicate; upload immutable assets and verify the draft API object.
7. Publish only after every check; never accept `release.id=0`, infer an ID
   from a name/tag, use a stale draft, move `latest`, or overwrite bytes.

H30–H32 exercise placeholder ID, stale/wrong ID, and target mismatch. This
phase ordering must be implemented and independently reviewed before any
release evidence can count.

## Official primary evidence

Consulted via Firecrawl developer/scrape on 2026-09-20:

* [Actions artifacts REST](https://docs.github.com/en/rest/actions/artifacts):
  artifact metadata has numeric ID, name, service digest, expiry, and
  `workflow_run`; it does not expose producer job ID, attempt, uploader, step,
  or attestation ID.
* [Actions workflow jobs REST](https://docs.github.com/en/rest/actions/workflow-jobs):
  jobs expose numeric job/run/attempt/head SHA and step name/status/conclusion,
  but no artifact ID/digest.
* [`actions/upload-artifact`](https://github.com/actions/upload-artifact):
  `artifact-id` is the REST lookup key and `artifact-digest` is the uploaded
  artifact ZIP SHA-256; displayed size is ZIP size.
* [`actions/attest`](https://github.com/actions/attest/blob/main/README.md)
  and [action.yml](https://github.com/actions/attest/blob/main/action.yml):
  custom attestation requires subject digest, predicate type, and exactly one
  predicate/predicate path; OIDC and attestation permissions sign/persist it;
  storage records are separate.
* [`gh attestation verify`](https://cli.github.com/manual/gh_attestation_verify):
  `--repo`, `--signer-workflow`, `--source-ref`, `--source-digest`, and
  `--predicate-type` enforce actor/source/predicate policy, while predicate
  fields still require trusted-builder and live API checks.
* [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations):
  attestations bind subject digest and predicate type; custom types require
  explicit verification.

## Required disposition

Do not mark this design approved or G1-passing. Implement the typed product,
strict schema/semantic verifier, protected one-uploader publisher, raw ZIP
verification, draft-first release binding, and real signed fixtures. Then run
all 34 mutations with a real verifier and report each case as pass, reject,
unavailable, or error. Any unavailable real verifier is an incomplete gate,
not a hostile-test success.
