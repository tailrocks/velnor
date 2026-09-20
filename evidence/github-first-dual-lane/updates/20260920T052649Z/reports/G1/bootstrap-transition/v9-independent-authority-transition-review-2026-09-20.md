# Independent v9 authority-transition review

Reviewer: `authority_transition_review`  
Observed: 2026-09-20 UTC  
Verdict: **not approved; design contract inconsistent and execution externally blocked**

This is a read-only design review. No source, branch, ruleset, check, App,
trust, release, merge, dispatch, runner, or credential state changed. The
user must separately approve any later execution.

## Exact reviewed inputs

- Frozen plan Markdown:
  `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.md`
  SHA-256 `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`.
- Frozen plan JSON:
  `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.json`
  SHA-256 `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`.
- Owner v9 audit report SHA-256
  `1e920b0186c050d78757919ca39dc71ffb686f4de97e7b2e3cf77cbb83d02f0f`.
- Owner v9 audit results SHA-256
  `4ddeb3272b5456f8e790e925c4dba4a71ba4436eee38af0910bbc660fc196ca1`.
  It reports 33/33 structural checks, but also `external_blocked` and no
  mutation/authorization.
- Canonical v9 root manifest raw SHA-256
  `f88030e68a2d6c917cc1567508a6f0b0797ae119e04fb778cdf4b3c060b4ad14`;
  canonical no-self digest `7f888b746ca8c7a6f6e8342c7d0a450edd70a5bf766421f9a2d8ddd2cd9e517b`.
- v9 hostile transport fixture SHA-256
  `6e838b3b765ceb4570f1a86628d606dffa8cd03909c81dfc1d1abab7023f96be`.
- v8 canonical DAG consumed by v9 SHA-256
  `aa2f86a91cb0b8458ec8e101c3ad99859514c146bcaead655db9b041bddbbf5d`.
  Its own metrics still report 16 ambiguous S7 fields, 11 timing mismatches,
  24 permanent missing leaves, and 50 unmapped record leaves.

The separate v9 schema/DAG audit was also reviewed:
`v9-independent-contract-audit-2026-09-20/canonical-dag/v9-independent-audit-report.md`
SHA-256 `8aa9fdbd08f592f613aeb9f38bf90216df686e929893d01bc473ce0c2c5b6c54`;
results SHA-256
`02cd400676467a38579b0eb495cf57b8282efb393e6bf576d35ab8642e33dcaf`.
It independently reports 11 findings, including the S7 preimage, schema
identity, release-leaf, and executable-fixture defects below. The separate
validator-binding audit remains `unimplemented`; its report says no G1 gate
claim was made.

## Design improvements accepted as shape-only

V9 materially addresses several v8 shape defects: the caller permission
upper-bound union is now present; the publisher is declared
`workflow_call`-only behind a push/main caller guard; S7 is named as a
pre-provider artifact plus provider/verify-B phase; field-lineage orientation
is explicit; current-main evidence is separated from the historical parent;
historical predicate input is excluded; and draft-release-before-attestation
ordering is stated. These are declarative improvements. The actionlint
fixture and owner audit do not prove live GitHub execution, external provider
behavior, cryptographic verification, or freeze enforcement.

## Independent hard findings

### 1. Frozen Markdown and machine hashes disagree

The Markdown identifies the pre-record schema as
`0ba1873d3afe008a72c55ed9ac0856f46ac4f1c2be4a5a5c653600c0560f98a8` and the
canonical root digest as
`03826f8729e812b31156007b295c3e808effb299b67867f4342e5b24b356bc21`.
The bound JSON/root manifest use the actual current values
`f5d04aad1cb6ee764bdd70e13961ca7ec28924994e149c3a49933fdb9441e26c` and
`7f888b746ca8c7a6f6e8342c7d0a450edd70a5bf766421f9a2d8ddd2cd9e517b`.
The pair hashes themselves are correct, but its two contract descriptions are
not semantically coherent. Regenerate both documents and recompute the review
tuple before approval.

### 2. The root-bound machine input does not contain the v9 corrected closure

V9's `canonical_field_model` is only a correction string plus the old v8 DAG
hash. The root manifest binds that old DAG as an “independent machine
baseline”; it does not bind a new corrected machine DAG. Its unresolved
metrics therefore remain part of the only root-bound machine input.

The bound release-manifest schema has 14 leaves, but
`canonical_leaf_paths` has no `release_manifest.*` namespace. The map has 146
permanent and 124 pre-record paths, while the release schema's fields are not
represented as independently typed lineage leaves. The root also has no
positive release-manifest fixture. A schema file being listed in a root is not
the same as mapping and exercising every field.

Required correction: emit and hash one successor machine DAG containing the
corrected S7 and release-manifest fields, bind it in the root, and validate
positive and hostile instances against that exact DAG.

### 3. Current schemas disagree on product identity

Permanent binding and pre-record require
`velnor-workflow-policy-validator`, `Linux-X64`, and asset
`velnor-workflow-policy-validator-Linux-X64`. The bound release-manifest schema
requires `velnor-policy-validator`, namespace `velnor.policy-validator`,
platform `ubuntu-24.04`, and tag `velnor-policy-validator-<sha>`.
One Main-B record cannot satisfy both current schemas. Unify the identity, or
define a rooted, explicit release-to-product mapping and its verifier rule.

### 4. S7 preimages are impossible as written

`S7_provider_result.ordered_fields` contains both
`provider_result_id` and `provider_result_digest`, while `excluded_fields`
excludes both. `S7_terminal_census.ordered_fields` contains
`terminal_census_id` and `terminal_census_digest`, while the digest is also
excluded. The digest sources say the preimage contains exactly the ordered
fields, yet the JSON claims `no_self_or_future_preimage=true`.

This is not an implementation choice: a self field cannot simultaneously be
inside and outside an exact preimage. The provider-result schema also requires
root/schema hashes, the full pre-record object, called-workflow identity,
terminal census, composed binding, and immutable-release readback, but these
are absent from the S7 ordered field list. The digest therefore does not bind
the required result even after the self-field contradiction is removed.

The pre-record contract further mixes `pre_record_artifact_*` and
`record_artifact_*` fields, while the pre-record schema defines only its
`artifact.*` fields. The producer, wire name, schema path, and digest meaning
for the second group are not defined.

Required correction: publish exact disjoint field partitions, one canonical
serialization, and explicit bytes for every signed/result digest. No self ID,
future census field, or required schema field may be omitted.

### 5. S7 timing still contains a provider-result/census cycle

The Markdown says the external provider emits the signed provider result, then
`verify-B` captures called-workflow identity and computes the post-provider
terminal census. The provider-result schema nevertheless requires
`terminal_census`, and the positive provider fixture embeds it in the provider
result. The JSON assigns terminal-census production to
`verify-B.terminal-census` while calling the provider result a signed result.

Either the provider must independently compute, sign, and transport the full
census before emitting its result, or verify-B must emit a separate
post-provider result with a second unambiguous signature/digest. The current
single record cannot be both pre-census provider output and post-census
verify-B output.

### 6. External provider trust and credential transport are placeholders

Provider App ID, installation ID, integration ID, verifier revision, result
URL, provider key, and key trust root remain placeholders. The JSON has no
workflow-call secret declaration or exact secret name/transport; only the
actionlint fixture declares `secrets: inherit` and a synthetic
`PROVIDER_RESULT_TOKEN`. The fixture is not product source or a live proof.

The provider-result schema calls the result “signed” but contains no signature,
algorithm, public-key/fingerprint, certificate, or verified-key binding. A
`credential_key_id` string is not cryptographic evidence. The external App
must also prove checks-write authority and least privilege separately from the
Actions caller token.

Required correction: specify the exact reusable-workflow secret contract,
provider endpoint/authentication, key/trust-root and signed-byte envelope,
App installation/permissions, and fail-closed verification. No build job may
receive the provider credential.

### 7. B check identity is claimed, not read back and bound

The provider schema has a context constant and claimed provider/integration/App
IDs plus `resulting_main_sha`, but no required Checks API `head_sha`, check
name/context readback, App identity readback, or status/conclusion response.
`check_run_id` plus a signed claim is insufficient. The external verifier must
read the exact check run and require the `head_sha` to equal the actual
resulting main commit, and require the exact `{context, integration_id, app}`
identity and terminal success. Tree-A admission has the same missing explicit
check-run/head binding.

### 8. Caller source/run provenance trusts inputs too broadly

`capture-caller-context` receives source/ref/SHA/workflow/run values as
workflow-call inputs and only queries the Contents blob at the supplied SHA.
The fixture does not query the caller run API to prove that run ID/attempt,
event, ref, head SHA, workflow path/ref/blob, and tree are mutually consistent.
The caller guard is declarative; another local caller could otherwise supply
claims that look like `ci-main`. Require exact run/run-attempt API readback and
protected caller-workflow source binding before any side effect.

### 9. Declared output graph is not an executable implementation

The JSON declares 12 reusable-workflow outputs. The actionlint fixture declares
only three outputs and emits synthetic IDs/digests (`1`, zero hashes, and
`provider.invalid`). Its actionlint pass proves YAML syntax only. Current main
`89f82dd8b287f46a3cf4c0920f341f6ca6c736db` has no typed B publisher source or
workflow matching the declared graph. The separate schema audit therefore
correctly marks output lineage unimplemented, not green.

### 10. Terminal census is not a complete closure proof

`terminal_census.successful_runs` has only `minItems: 1`; each item lacks
workflow ref/blob SHA, event, tree/closure, runner label, artifact/manifest,
release, and attestation identities. The pre-record
`required_step_conclusions` object has no required keys or minimum property
count, so `{}` validates. This cannot prove every applicable CI, preview,
release, nightly, signer, DCO, Policy, Linux-X64/Linux-ARM64, and xcode-27
consumer completed terminal success. Bind the exact expected closure and all
run attempts/jobs/checks, excluding only explicitly typed self nodes.

### 11. Digest encodings and automatic release attestation are underspecified

The preimage contract says digests are lowercase hex, but schemas mix bare
64-hex values with `sha256:<64-hex>` values. No field-level normalization or
prefix-preservation rule exists; canonical preimage equality is therefore
ambiguous.

The release protocol says GitHub's automatic immutable-release attestation is
queried and included in S6/provider evidence. The current release/provider
schemas have no automatic-attestation ID, digest, predicate, or signed-byte
field; `release_attestation_required: true` is only a boolean. Add the exact
attestation object and binding, or remove the claim.

### 12. Main merge identity and freeze are not executable atomic operations

`ruleset_transition.merge_method` and its API transcript state
`sha=PR_HEAD_SHA`. A normal protected PR merge may create a merge or squash
commit; PR head SHA is not the resulting main SHA. The plan must bind the
actual merge response, reread `refs/heads/main`, and start/accept Main-B only
for that exact observed push/run `head_sha`, with base/head/tree and lease
checks. A post-mutation mismatch check alone does not prevent a race.

Ruleset mutation is documented as full-object `PUT`; the plan explicitly makes
no ETag/CAS assumption. Actor 5 remains an `always` bypass, and provider
freeze, coordinator, lease, watchdog, death recovery, and conditional restore
are unproven placeholders. A proxy or signed lease cannot enforce against an
administrator without a provider-enforced capability. User freeze/threat-model
choice is still pending and is an execution blocker.

References: [reusable-workflow permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations),
[reusable-workflow outputs](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows#using-outputs-from-a-reusable-workflow),
[OIDC claims for reusable workflows](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/secure-your-work/security-harden-deployments/oidc-with-reusable-workflows),
[check runs API](https://docs.github.com/en/rest/checks/runs),
[ruleset update API](https://docs.github.com/en/rest/repos/rules#update-a-repository-ruleset),
[immutable-release setting API](https://docs.github.com/en/rest/repos/repos), and
[immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).

## Remaining external execution blockers

- Target generator revision is null; current `54` and historical `0dc` are not
  an approved target.
- B source/output and fixed-point generated closure are absent at main `89f82`.
- Provider App, installation/integration/check identities, credential/trust
  root, verifier revision, and live verifier are absent.
- Existing runtime evidence used forbidden `macos-26`; required `xcode-27`,
  Linux-X64/Linux-ARM64 closure, and full consumer census are absent.
- D19 has no clean published candidate; real artifact/attestation/crypto
  lineage is not executed.
- Tree-B adoption and durable permanent trust-root proof are design artifacts,
  not merged consumer evidence. Temporary App removal cannot precede proof of
  a durable permanent trust root.
- User has not selected the provider-freeze threat model or approved execution.

## Approval requirements

Do not execute v9. A reviewable successor must first regenerate the frozen pair
with coherent Markdown/JSON/schema/root hashes; bind a corrected machine DAG
and all release leaves/fixtures; unify product identity; repair S7 preimages and
provider/census timing; specify provider signature, check head binding, secret
transport, and exact run/source readbacks; replace `PR_HEAD_SHA` with the real
resulting main SHA protocol; and provide an enforceable freeze/lease/recovery
mechanism. Then supply target B source/output, real identities, verifier,
artifact/attestation proofs, full native/consumer census, and Tree-B durable
trust evidence. User approval remains a separate gate; no future run IDs are
required merely to review a coherent design, but no execution approval is
supportable from this v9 pair.
