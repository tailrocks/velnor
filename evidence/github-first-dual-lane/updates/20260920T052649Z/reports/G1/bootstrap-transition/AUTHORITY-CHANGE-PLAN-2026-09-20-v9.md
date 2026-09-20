# Velnor authority-transition plan v9 — proposal only

Status: `successor_draft_external_blocked`. V8 remains immutable and preserved. V9 is a machine-contract correction, not authorization: no source, ruleset, App, release, merge, dispatch, or runner mutation occurred. The authority/freeze choice remains unselected.

The transition remains one bounded sequence: Tree A admits the source/generator change without a permanent B pin; resulting `main` publishes the typed validator through the real GitHub reusable workflow; live provider result and terminal census are verified; Tree B then adopts the permanent pin. Tree A admission, Main-B provider verification, and permanent Policy are distinct authorities.

## Current bound

Current live main is `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`, observed `2026-09-20T03:58:55Z`. Source PR 969 head is `2445c647dafa43acbc5285c873d3b04174ff75c6`. Generator emission revision is `54`; target generator revision is null. Old runtime/policy pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` is not a target.

Current evidence is outside source:

- checkpoint `G1/bootstrap-transition/current-main-checkpoint-89f82dd8-2026-09-20.json`, SHA `77e1ecc848e0a3ad73cdfd853b788054e8cda02e0aae38d076b7c59b0da130bd`;
- closure input `G1/bootstrap-transition/current-main-closure-inputs-89f82dd8-2026-09-20.json`, SHA `1b890ef899c8a02146e7d6ab1d664fcb92213924e8c7a722f8f57a2bfb55c072`;
- runtime run `35484968618`, macOS job `106009574341`, runner `macos-26` — rejected;
- Preview run `35484968732`, job `106009550596` — generated-tree drift;
- Main CI run `35484968706`, Policy `106009552370` — failed; required children were skipped.

Every current field in the v9 JSON resolves to the 89f/parent/tree/checkpoint tuple. The earlier `325719f1`/`e94b484` values are under `historical_base_snapshot` only, never `evidence.current_main`.

The independent canonical machine baseline is `G1/bootstrap-transition/v8-independent-contract-audit-2026-09-20/canonical-dag/canonical-field-dag.json`, SHA `aa2f86a91cb0b8458ec8e101c3ad99859514c146bcaead655db9b041bddbbf5d`. It measured 146 permanent-B leaves, 68 old record leaves, 16 ambiguous S7 fields, 11 timing mismatches, and 24 missing step leaves. V9 consumes that machine input and resolves S7 into unique producers; it does not call the old independent findings proof of runtime.

## Exact product and schema set

The typed product is exactly:

```text
product_id: velnor-workflow-policy-validator
schema:     velnor.workflow-policy-validator.v1
asset:      velnor-workflow-policy-validator-Linux-X64
platform:   Linux-X64
runner:     ubuntu-24.04
architecture: x86_64
features:   ""
mutable_latest: false
overwrite: false
application_namespace_reuse: false
```

The current Main-B binding schema is `authority-contract-separation-2026-09-20/permanent-b-product-binding.schema.json`, SHA `427cbae0d9a238ae5e22a1afac2c8f840e4a8ad136983ac28a863651e533aed7`. It has 146 concrete leaves, including every named step status/id/conclusion. The historical `validator-binding-audit-2026-09-20/binding-predicate.schema.json`, SHA `4bcd53cdd4e1900d4edf240f46da4c14ab570fc7be09f37484a9f56402da3b06`, is retained for old-fixture reproducibility and is forbidden as a current input.

V9 adds one root-bound schema set:

- `v9/policy-validator-b-pre-record.v2.schema.json`, SHA `0ba1873d3afe008a72c55ed9ac0856f46ac4f1c2be4a5a5c653600c0560f98a8`;
- `v9/policy-validator-b-provider-result.v1.schema.json`, SHA `4bb8a2a31867a720453219eae9624745c647f4483dd71f45b7cd759bd978bd2b`;
- `v9/policy-validator-b-release-manifest.v1.schema.json`, SHA `81c7599b3d29abe9c6fad2b3032a6207352ac72a7bbe2399cd913a0003ec6b74`;
- `v9/validator-pin-adoption.v1.schema.json`, SHA `078b038e862a7650c714d219d96099c112c97c34fad3a75b54ecce208e024f8b`;
- `v9/canonical-root-manifest.v2.json`, canonical digest `03826f8729e812b31156007b295c3e808effb299b67867f4342e5b24b356bc21`.

The root manifest binds these schemas, two positive fixtures, both actionlint fixtures, the current main tuple, and the canonical machine input. Positive fixture root references use the documented zero-root normalization so root and fixture hashes do not form a self-preimage. The historical schema is explicitly excluded.

## Tree A, Main B, Tree B

Tree A contains no `[policy.validator]`, permanent literal pin, candidate manifest, `--pin-build`, old setup/runtime identity, old macOS fallback, or compatibility route. All ordinary workloads and native `Linux-X64`, `Linux-ARM64`, and `xcode-27` obligations remain required.

The temporary required context is one external `Policy-bootstrap-A` verifier bound to exact PR number, head/base, synthetic tree, closure, and complete pre-merge check census. It can read and report only. It cannot merge, mutate refs/rulesets, publish, create releases, write packages, use secrets, or bypass checks. The old Policy context is not simultaneously required because the old 0dc parser rejects the transition before generation; this is an explicit temporary trust amendment, not an omitted workload.

After normal protected merge, the exact resulting main SHA is reread. `ci-main` invokes one reusable publisher on push to `refs/heads/main`; it has no standalone push/tag/PR/publishing dispatch. Main B produces the typed product above, immutable release manifest, raw asset digests, binding/release attestations, a pre-provider record artifact, and the actual producer IDs.

The external B provider downloads and validates the pre-provider record, then emits one `Policy-bootstrap-B` check and one signed provider-result record. `verify-B` reads that exact result, captures called-workflow identity, and computes the post-provider terminal census. No field is attributed to `record-upload or verify-B`; each has exactly one producer. Policy consumes `verify-B` outputs from the reusable workflow caller.

Only after this proof does Tree B add `.github/ci/validator-pin-adoption.json` and its sidecar. The adoption file is validated by the v9 schema and binds exact PR number/head/base/tree, base Policy blob/verifier/integration, generator config/template/static mapping, Main-B release/asset/provider-result/attestation digests, and the permanent trust-root schema hashes. Temporary A/B transition authorities are removed only after final Policy verification.

## Executable reusable-workflow contract

The actionlint-checked fixtures are:

```text
v9-executable-fixture/ci-main-caller.yml
v9-executable-fixture/ci-policy-validator-products.yml
```

Command and result:

```text
actionlint v9-executable-fixture/ci-main-caller.yml v9-executable-fixture/ci-policy-validator-products.yml
exit 0
```

The caller uses-job has the exact upper-bound permission union required by called jobs:

```text
actions:write, contents:write, attestations:write, id-token:write, checks:read
```

Called jobs downgrade to their least privilege. Build has only `contents:read/actions:read`; reserve/publish have only `contents:write`; attestation jobs have read plus OIDC/attestation write; pre-record upload has only `actions:write`; verify-B is read-only. No caller-job output re-export is declared.

The called `capture-caller-context.capture` step receives caller source/ref/SHA/workflow claims/run IDs through declared `workflow_call` inputs and runs the exact Contents API request for the caller workflow blob. This is the S0 producer; “GitHub context” alone is not treated as a transport edge.

## Unique field DAG

The JSON carries every field from the independent machine baseline, all 146 permanent schema leaf paths, and all v9 record/provider leaf paths with `{stage, producer, source, type, required}` objects. S7 is split as follows:

| Stage | Unique producer | Contract |
|---|---|---|
| S0 | `capture-caller-context.capture` | workflow_call inputs plus caller Contents blob response |
| S1 | `build-linux-x64` | artifact upload outputs; raw REST-derived values are not claimed until S2 |
| S2 | `artifact-verify` | producer run/job/check, raw artifact download, REST size/digest/expiry |
| S3 | `reserve-release` | draft release ID/tag/target |
| S4 | `attest-binding` | binding predicate, binding attestation, final manifest; no release asset |
| S5 | `publish` | release asset and immutable-release readback |
| S6 | `attest-release` | release predicate/subject and signer/OIDC certificate fields |
| S7a | `pre-record-upload` | strict pre-provider Actions artifact ID/name/digest and upload run/job/check |
| S7b | `external-provider-result`, `verify-B.capture-called-identity`, `verify-B.terminal-census` | provider check/result IDs, called workflow claims/blob, terminal census |

Lineage orientation is explicit: `producer_outputs_to_consumer_inputs`. `needs_edges_consumer_producer` separately records GitHub `needs` direction. The executable edges are capture→build, build→artifact-verify, artifact-verify→reserve, reserve→attest-binding, attest-binding→publish, publish→attest-release, attest-release→pre-record-upload, pre-record-upload→verify-B, and external-provider-result→verify-B. Every edge lists exact typed fields. Provider-result fields are external inputs to verify-B, not outputs consumed by their own producer.

## Two-phase record and terminal census

`pre-record-upload` creates `positive-pre-record.json`-shaped bytes and uploads `policy-validator-b-pre-record.zip`. Its strict v2 schema contains S0–S6 values only. The Actions artifact REST response is the sole producer of pre-record artifact ID/name/digest and upload run/attempt/job/check.

The external provider uses its own App credential to download that artifact, validate raw bytes and schema hashes, query exact main/run/job/check/release/asset/attestation identities, and create the signed provider-result record. It emits `Policy-bootstrap-B` only after those checks. Its v1 schema contains provider check, pre-record artifact binding, provider-result ID/digest, called-workflow claims, and terminal-census fields.

`verify-B` reads the provider result through its provider-result credential. Its `capture-called-identity` step obtains `job_workflow_ref/job_workflow_sha` and the exact workflow Contents blob; its `terminal-census` step queries every applicable upstream run/attempt/job/check and requires terminal `success`. It excludes only `verify-B`, Policy, the external B check, and the caller wrapper, preventing a self-cycle. Caller claims are `workflow_ref/workflow_sha`; called claims are `job_workflow_ref/job_workflow_sha`.

## Exact digest preimages

All preimages use canonical UTF-8 JSON, LF line endings, lexicographically sorted object keys, declared array order, decimal integers without leading zeroes, and lowercase hexadecimal digests. The JSON lists ordered field names, exclusions, subject bytes, and digest source; prose stage labels are not the acceptance rule.

- S4 binding preimage is exactly S0+S1+S2+S3 fields. It excludes all S4 own IDs/digests, every S5/S6 field, every S7a/S7b field, including binding record IDs/digests, called-workflow fields, provider IDs, and terminal fields.
- S6 release preimage is exactly S0+S1+S2+S3+S4+S5 fields. It includes release asset ID/digest and published release ID, but excludes release-attestation own IDs/digests and all S7 fields.
- S7 provider-result preimage is exactly S7a transport plus external provider-check/result fields. It excludes provider-result own ID/digest and terminal-census digest. Terminal-census digest is computed separately over exact successful upstream identities, excluding its own digest.
- Record artifact/upload IDs, name, run attempt, and upload step are never in their own digest preimage.

The release manifest schema requires draft-first, nonzero release ID, immutable tag/asset/readback, source/tree/closure, product identity, and manifest digest. Before release publication, verify `GET /repos/{owner}/{repo}/immutable-releases` returns HTTP 200 with `enabled:true`; record `enforced_by_owner` and its canonical response digest. After publication, query release/asset IDs and GitHub’s automatic release attestation. Retry queries the exact draft ID/tag/target and never creates a second candidate.

## Closure, ruleset, and native obligations

The generator closure must include `.github-gen/velnor-workflow.toml`, every source/generated ordinary workflow and action, both B publisher source/output files, generator state, actionlint config, nightly, and package signer. S2 remains neutral: no Velnor product/consumer hardcodes or literal pin. Dynamic old-identity reachability must be absent; a zero string grep is insufficient.

Ruleset ID is `19573071`; observed contexts are DCO, ci-required, Policy, with actor 5 always bypass. Full returned JSON body and canonical hash are required before/after each full-object PUT. Context identity is `{context,integration_id}`. No ETag/CAS behavior is assumed. The provider freeze/recovery mechanism and actor-5 threat model remain unresolved blockers; leases/proxies/watchdogs alone do not enforce against the administrator.

Required execution order: read-only census; provider freeze decision and disposable race/death proof; reread main/ruleset; Tree-A full PUT; Tree-A admission and all ordinary required jobs; normal merge and exact SHA check; Main-B real publisher/provider result; post-provider terminal census; Main-B full PUT; Tree-B normal PR/schema/sidecar; final Policy proof and temporary-authority deletion.

Native acceptance requires Linux-X64, Linux-ARM64, and `xcode-27`, plus all applicable unit, preview, release, nightly, signer, actionlint, DCO, ci-required, provider, and Policy checks. `macos-26`/macOS-15, empty matrices, skipped required jobs, wrapper-green/child-failed runs, wrong runner identities, stale checks, and unpublished local artifacts fail closed.

## Hard blockers and status

- Target generator revision is null; current 54/old 0dc are not targets.
- Current 89f main lacks proven B source/output and fixed-point generation.
- Real B App/provider IDs, credentials, verifier revision, and called-workflow identity are unresolved.
- Real verifier, raw artifact/attestation crypto checks, and provider result implementation are absent.
- Provider freeze/recovery excluding actor 5 is unproven.
- Current native evidence used forbidden `macos-26`; `xcode-27` proof is absent; D19 has no clean published candidate.
- Tree-B schema/trust-root evidence exists only as external v9 design artifacts, not a merged consumer.
- Independent approval is absent; `g0_reviewer` and `authority_transition_review` must review the machine DAG, actionlint fixture, strict positive/negative fixtures, current evidence, release setting, provider handoff, freeze, native matrix, and cleanup.

V9 is not approval-ready, execution-ready, or a G1 pass. It is a corrected machine-bound proposal only.
