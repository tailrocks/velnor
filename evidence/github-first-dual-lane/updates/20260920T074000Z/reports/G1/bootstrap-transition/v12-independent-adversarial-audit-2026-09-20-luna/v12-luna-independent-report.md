# Independent v12 strict-schema/dataflow audit — Luna

Design evidence only. No source, authority, release, workflow, dispatch, merge, runner, or credential mutation occurred.

Result: 21 pass, 7 fail, 4 unimplemented checks; authority_claim=false.

## Material findings

- S7b emitted provider_result_digest does not equal the declared 56-field S7b preimage digest. The generator hashes its provider object (including excluded provider identity/nonce/status fields) instead.
- The field registry assigns 19 permanent.verification/verifier leaves to external-provider-result, but provider_result.schema.json has no output leaves for them.
- pre_record.release.asset_id/asset_digest are staged S3 and transported to attest-binding although the release asset is produced at S5; final-manifest values are likewise assigned before their declared S6 computation.
- typed transport edges use one abstract label, not executable output/artifact/record channels; dropped-field transport cannot be independently rejected.
- build_v12_bundle.py directly loads the historical v9 OLD_PRE schema to generate v12 pre_record leaves. This is a second contract input, despite its hash being bound.
- verify-B terminal rows have schema minItems=15 but no maxItems, unique workflow/check identity, exact workflow enum, or cross-field head/ref/repository constraints.
- raw ZIP, inner payload, binary, and release asset bytes are absent; digest/size claims are not byte identity proof. Provider/check/live workflow verification is unimplemented at observed main.
- Hostile fixtures: 19 isolated JSON cases; 7 are rejected by strict schemas, while 12 semantically forged cases remain schema-valid and therefore are unimplemented without a real verifier.

## Checks

- `pass` `frozen-root-digest` — recomputed=e2b30dc21732f957ee9b69b45f270b2d72a995b624d4b19a448eb86985e404f8; frozen=e2b30dc21732f957ee9b69b45f270b2d72a995b624d4b19a448eb86985e404f8
- `pass` `model-byte-identity` — sha256=5c3c7722481adf5c025502e7ba0d0edc252d066dd1cafb219a4b4a208b3a5062
- `pass` `generator-byte-identity` — sha256=1a7ed9ab4bcb099cdb03d66d132e535ca9d0974c3e733990e13bd630e76c948c
- `pass` `root-no-self-digest` — canonical root object has no self-digest member
- `pass` `root-bound-byte-hashes` — all root entries independently measured
- `pass` `strict-seven-positives` — validated=7
- `pass` `independent-schema-leaf-count` — counts={'permanent_binding': 145, 'pre_record': 72, 'pre_record_transport': 11, 'provider_result': 30, 'verify_b': 22, 'release_manifest': 14, 'adoption': 17}; total=311
- `pass` `leaf-registry-bijection` — registry=311; schema_leaves=311
- `pass` `typed-output-strictness` — typed_fields=311; invalid=0
- `pass` `synthetic-not-live` — typed fixture metadata and every leaf are non-live
- `pass` `permanent-binding-constants` — immutable Linux-X64 and temporary-key constants
- `pass` `permanent-no-old-integration-id` — legacy integration_id absent
- `pass` `release-fourteen-leaves` — leaves=14
- `pass` `id-namespace-separation` — Actions artifact ID is distinct from release asset ID
- `pass` `registry-stage-producer-closure` — fields=311; violations=0
- `pass` `needs-is-acyclic-and-later` — workflow needs edges respect stage order
- `fail` `release-and-final-manifest-producer-timing` — S5 asset and final-manifest values must not be claimed by earlier producers
- `pass` `preimage-future-field-exclusion` — ordered digest fields are before their producer and disjoint from exclusions
- `pass` `binding-preimage-byte-identity` — declared=8ec6eb53dceda0d90f06e043e9360518535a54138cda408fea633a8dc363a7e7; measured=8ec6eb53dceda0d90f06e043e9360518535a54138cda408fea633a8dc363a7e7
- `pass` `release-preimage-byte-identity` — declared=ecbe6e70cb562f3ae1161a27ddc0f380d4746cc32f839cce8a8da9439d5838ea; measured=ecbe6e70cb562f3ae1161a27ddc0f380d4746cc32f839cce8a8da9439d5838ea
- `fail` `provider-preimage-byte-identity` — declared=01efb3de3486e97d42765983ee77edeadfee979a01711dafe11d98bb59c6f708; declared-preimage-measured=a8378ea1897e8024563cbedd50f47131d3733dd6f198d860588ca84135da5768; ordered_fields=56
- `fail` `provider-own-fields-excluded-from-emitted-digest` — emitted generator payload must not hash provider-owned/post-signature fields
- `fail` `provider-producer-schema-coverage` — every external-provider-result field must have a provider output leaf
- `pass` `terminal-census-positive-predicate` — positive has one terminal row per required workflow and recomputed digest
- `fail` `terminal-census-schema-cardinality` — strict schema has minItems only; exact 15/unique workflow identity is semantic
- `fail` `typed-transport-concrete-channel` — typed_edges=56; unique_transport_labels=1
- `pass` `field-edge-coverage` — edge_fields=311; registry=311
- `unimplemented` `provider-check-terminalization-order` — model allocates/read-backs check before signing, signs status/conclusion, then terminalizes; no executable provider/verifier exists to prove this temporal contract
- `unimplemented` `provider-check-identity-cross-binding` — provider_result_id/external_id/run/app/head equalities are not expressible in provider_result.schema.json and no verifier implementation is present
- `unimplemented` `raw-byte-identity` — bundle has digest/size claims but no raw service ZIP, inner payload, binary, or release-asset bytes for independent hashing
- `fail` `no-old-pre-hidden-input` — v12 generation must not derive strict pre_record leaves from OLD_PRE
- `unimplemented` `observed-main-source-closure` — no typed publisher/verifier source is present at observed origin/main; live authority cannot be tested

## Frozen input hashes

- freeze manifest: `0693377186d05348585749f199450cb4f3e69c9641118157ad10a862f7887b69`
- canonical model: `5c3c7722481adf5c025502e7ba0d0edc252d066dd1cafb219a4b4a208b3a5062`
- generator: `1a7ed9ab4bcb099cdb03d66d132e535ca9d0974c3e733990e13bd630e76c948c`
- DAG: `33df75eceaa11d94085e4766f0b73da32c82c7058542651a878237b0ebacc425`
- root digest: `e2b30dc21732f957ee9b69b45f270b2d72a995b624d4b19a448eb86985e404f8`

## Boundary

Owner 44/11 labels were not used as approval. The seven strict positive validations passing only establish schema shape. Semantic hostile cases accepted by schemas are recorded as unimplemented until a real producer/verifier exists. V12 and prior frozen bundles remain unchanged.
