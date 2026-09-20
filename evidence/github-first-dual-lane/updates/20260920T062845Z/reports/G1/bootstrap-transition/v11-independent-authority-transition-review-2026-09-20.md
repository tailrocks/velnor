# Independent v11 authority-transition review

Reviewer: `authority_transition_review`  
Observed: 2026-09-20 UTC  
Verdict: **not approved; v11 is a better design draft, but it is not executable or authority-safe yet**

Read-only review. No source, branch, ruleset, check, App, trust root, release,
merge, dispatch, runner, credential, or ref state changed. A later execution
requires separate user approval.

## Exact frozen inputs

- Freeze manifest:
  `G1/bootstrap-transition/v11-freeze-manifest.json`
  SHA-256 `df14a907b29cf5839551dc0b4a32e77af5c61d8e927433530f9a061d75997c16`.
- Canonical model:
  `G1/bootstrap-transition/v11-canonical-model-source.json`
  SHA-256 `ff3d8018bb7707ef540b9be5b6e7b22262b93a1309733b59f9f19a9842dda930`.
- Generator SHA-256 `c554cdd41c7050536fd8a11382f0b6b73bf18ce6cb03c64ee7520c184e7aee44`.
- Root manifest SHA-256
  `35f8ae8a20cfe3ce774ed0d63ece8ed06d3e66d4e66693933ea5e5825e32a96d`.
- Canonical root digest, independently recomputed from sorted compact UTF-8 LF
  JSON: `deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017`.
- DAG SHA-256 `37e3d0cb65b5b793219d94894a672eae50e8181378481dfb846afff1c49e56e7`.
- Plan JSON SHA-256
  `4c50c8e591924cbdbccb7ec489e8a8bbb20576d5f5fbb207c42e52837f6d42c6`.
- Plan Markdown SHA-256
  `2fe0a7e534299e0b7dc83102706d29d91fd07e422a795ef726c1dccd5d119dde`.
- Owner audit: `59/59`, report SHA-256
  `b81a47a7951d2437701e0a5ec2664fcad84e71e2e1dedf79400f423864b66e24`, results
  SHA-256 `50160264a4e886a548b4c9494ce0e32f7a8f06f0704bf9daee4200a577fb8411`.
- Separate adversarial audit: `19/19`, report SHA-256
  `b877b8ad6ed9de30b0541ccdb99ebdaf061f35a46526709849540fed1f59a271`, results
  SHA-256 `972a5435150ac27b077b9edf390a20eb4e4db849f11571a23d1fd7c071d6c5b0`.

The two audit counts are structural/schema evidence only. They are not this
review's approval, live proof, cryptographic proof, or authority authorization.

The bound revision remains main
`89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent
`325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree
`22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`, observed at
`2026-09-20T03:58:55Z`. PR 969 head
`2445c647dafa43acbc5285c873d3b04174ff75c6` is candidate evidence only;
resulting-main SHA/tree are correctly null until protected-merge and
`refs/heads/main` readback. Freeze time is `05:53:29Z`, so the bound revision
must be reread before any operation.

## v10 defects that v11 actually closes

These are accepted as design-shape improvements, not execution evidence:

- strict Draft 2020-12 positive instances now validate;
- the release/pre-record/raw-artifact/provider/verify-B/adoption fixtures join
  one synthetic lifecycle;
- candidate PR head and resulting-main identity are separate;
- raw `sha256:<64>` artifact digest and canonical bare digest are distinct;
- documented Actions/check/artifact/OIDC fields replace unsupported
  `workflow_sha`/`integration_id` API assumptions;
- the caller upper-bound permissions, reusable-workflow inputs/secrets/outputs,
  and direct `needs.policy-validator-B.outputs.*` Policy transport are named;
- Main-B is reusable-workflow-only behind the `ci-main` push/main guard; and
- Tree-A/Main-B/Tree-B context names and no-permanent-B-before-proof ordering
  are explicit.

## Independent hard findings

### 1. “One canonical model source” is false for generated pre-record schema

`build_v11_bundle.py` imports and flattens
`authority-contract-separation-2026-09-20/v9/policy-validator-b-pre-record.v2.schema.json`
(`OLD_PRE`, generator lines 23 and 255), then uses its fields to generate
`pre_record.schema.json`. That file is not bound by the v11 root manifest and
has SHA-256
`f5d04aad1cb6ee764bdd70e13961ca7ec28924994e149c3a49933fdb9441e26c`.
The manifest binds the v9 vocabulary DAG (`902854e...`) but not this second
semantic input. A verifier cannot reproduce the claimed v11 projection from
the frozen model/generator/root tuple alone.

Move the pre-record contract into v11, or bind this exact historical schema
and state that it remains an authoritative input. Then regenerate and rehash
the complete bundle.

### 2. Permanent-binding security invariants are not positively validated

The `59/59` validator checks six positive schema instances. It does not
validate a positive `permanent_binding` instance. The only full 303-field
object, `positive-full-typed-output-fixture.json`, is a metadata wrapper whose
test checks types, not the permanent schema's constraints. Direct comparison
shows these typed values are `true` while `permanent_binding.schema.json`
requires `false`:

- `permanent.product.mutable_latest`;
- `permanent.product.overwrite`; and
- `permanent.product.application_namespace_reuse`.

`permanent.trust.temporary_key_used` and several action booleans are merely
unconstrained booleans; no machine rule proves trusted signing isolation.
Add and strictly validate a positive permanent-binding instance, including
all const/equality/security invariants. If temporary signing is forbidden,
encode `temporary_key_used: false`, not prose.

### 3. Positive cryptographic fields are placeholders, not recomputed proofs

The model defines S7c as SHA-256 over canonical ordered terminal-census rows
(UTF-8, LF, lexicographically sorted object keys, declared array order). For
the frozen positive verify-B rows, that digest independently computes to
`dcb4660aad604cf15c4b16d3691de795b03b8b661030bbca29423afb107fc4ad`, while
the fixture declares `terminal_census_rows_digest` as 64 `9` characters and
`terminal_census_digest` as 64 `1` characters. The synthetic provider digest
and binding/release preimage digests are likewise not recomputed by the audit.

Non-live fixtures may use placeholders, but they must be marked as such and
must not be called cryptographic positive proof. Add a deterministic
preimage/hash test, or make every placeholder explicitly non-evidence and
exclude it from any approval criterion.

The provider signature also excludes `provider_result_id` and
`provider_result_digest`. The contract does not bind the authenticated
response URL/result ID to the signed payload, so a valid signed payload can
be replayed under another result ID unless the provider supplies a signed
outer envelope/nonce or equivalent anti-replay binding. Provider result
records also omit persisted raw `name`, `external_id`, and `status` from the
Checks readback despite requiring them in prose. Store those exact fields (or
a precisely signed readback envelope) and bind them to the provider result.

### 4. Release-attestation contract conflates GitHub automatic and custom attestations

S6 calls its source the “automatic release attestation API” but requires the
custom predicate URI
`https://velnor.dev/attestations/velnor-policy-validator-release/v1` and
custom predicate path `attestations/velnor-policy-validator-release.v1.json`.
GitHub's immutable-release attestation uses the documented in-toto release
predicate and binds the release tag, target commit, and release assets. A
custom URI/path requires an explicit attestation action/step and its own
source, permissions, certificate policy, and API readback; it is not the
automatic immutable-release attestation.

The contract also lacks immutable-release-setting readback and does not bind
the complete release subject list (tag, target commit, every asset, and the
manifest asset). Signer fields are unconstrained strings; the fixture uses
synthetic identities. Choose one executable design: bind the actual automatic
immutable-release predicate/object and setting readback, or add the explicit
custom attestation producer and bind its exact source/certificate/predicate
bytes. Do not call one the other.

References: [immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases),
[repository attestations API](https://docs.github.com/en/rest/repos/attestations),
[in-toto release predicate](https://github.com/in-toto/attestation/blob/main/spec/predicates/release.md).

### 5. Called-workflow source readback is named but not transportable

The caller contract gives a concrete Contents API URL only for
`.github/workflows/ci-main.yml`. It requires
`called_workflow_file_blob_sha` and says it is checked “separately through
Contents API”, but specifies no called-workflow path/ref/commit URL or source
blob equality. The field is therefore a typed claim with no producer/API
transport. The same gap affects the artifact upload source, whose
`source_commit_sha` remains null.

Add an exact Contents request for
`.github/workflows/ci-policy-validator-products.yml` at the trusted called
workflow commit, bind returned blob SHA/path/ref, and bind the exact
run-attempt job. Also require direct equality of Actions response `id` and
`run_attempt` to the selector inputs, plus trusted repository identity. A
selector alone is not evidence.

### 6. First-main-job timing and full terminal closure are not enforceable

The push/main guard prevents a standalone reusable-workflow trigger, but no
generated `ci-main` source, job `needs` graph, concurrency rule, or exclusion
proof establishes that `policy-validator-B` is the first relevant main job or
that all other main consumers wait for its verified outputs. GitHub
`workflow_call` jobs run as part of the caller; the plan must bind the actual
caller source and ordering, not infer a standalone B run.

The 15-workflow consumer list is not a closure matrix. Census rows contain
workflow path/SHA, run/attempt, job/check IDs, head SHA, status, and
conclusion, but no job key/name, check name/context, App ID, repository,
event/ref, or explicit self-node marker. The declared exclusions
(`verify-B`, `Policy`, `Policy-bootstrap-B`, caller wrapper) cannot be applied
to those rows. The plan therefore cannot prove all DCO/CI/unit/preview/release,
runtime, signer, maintenance, and generated children had terminal successful
attempts, nor that the verifier counted itself.

Add a typed per-consumer expected job/check/context/App/event/ref matrix,
attempt identity, explicit exclusion keys, and exact source/output mapping.
Bind Policy's adoption input to the complete release/provider/census record;
the seven workflow-call outputs alone do not transport permanent verifier
inputs.

### 7. A/B/permanent authority and true check-equivalence remain under-specified

Ruleset ID `19573071` and desired context strings are present, but no full
ruleset readback binds target refs, enforcement, bypass actors, actor-5 state,
or the pre/post object. `Policy-bootstrap-A` has no concrete external actor,
App, permission bound, PR head/base/tree predicate, expiry, or removal API.
Permanent `Policy` has no concrete App/verifier/source identity. The generated
permanent schema still requires legacy `verifier.integration_id`, while the
documented Checks API contract correctly says no `integration_id` is read;
provider-side App/installation mapping is not supplied.

Consequently, replacing old `Policy` with B and later permanent `Policy` is
not proven equivalent. Require exact check name/external ID/head SHA/App ID,
source commit/blob, evaluator revision, permissions, and all required
contexts for each phase. Require a real Tree-A admission record and a
machine-checked Tree-B adoption record binding PR number/head/base, resulting
main/tree, provider result, release manifest, census, workflow/blob/schema,
and permanent trust root.

### 8. Temporary removal is a boolean, not a safe authority transition

`temporary_authority_removed: true` and `permanent_trust_root` are schema
fields, not API evidence. The plan does not identify the temporary A/B App,
installation, exact removal calls, removal readback, or a durable permanent
trust root that remains valid after temporary credential removal. Do not
remove either authority until permanent Policy verification and durable trust
readback succeed. The final Policy check must not depend on the removed App
secret/key.

### 9. Freeze, branch races, and rollback still have no executable primitive

The plan honestly leaves `provider_enforced_capability: null`, but its
required “exact base/head/tree lease”, conditional ruleset update, actor-5
exclusion, crash recovery, and non-clobbering rollback are requirements, not
operations. The GitHub ruleset update contract is a full update operation; the
plan supplies no tested provider CAS/ETag/If-Match primitive or independently
controlled exclusive writer. A signed lease, watchdog, ordinary operator
consent, or post-mutation mismatch check cannot prevent a concurrent writer.

The executable plan must acquire a provider-enforced or independently
controlled freeze, reread branch/base/head/tree immediately before Tree-A
admission, expire/reject advanced PR heads, use the protected merge response
and subsequent `refs/heads/main` readback, and require the first main run's
`head_sha` to equal that resulting SHA. Rollback must be a conditional ref/
ruleset restoration only; it must never dispatch/re-run the forbidden old
checker or old OS, and must refuse to clobber a newer revision. If no such
capability exists, name the required operator/external authority instead of
claiming an atomic transaction.

Reference: [GitHub ruleset update](https://docs.github.com/en/rest/repos/rules#update-a-repository-ruleset).

### 10. Current source, native closure, identities, and freshness remain external blockers

The bound current-main closure says the typed publisher source/output is
absent, target generator revision is unresolved, fixed-point renderer closure
is unproven, and no live verifier/provider/artifact/attestation exists. The
checkpoint remains negative: runtime-products run `35484968618` succeeded but
used forbidden `macos-26`; preview run `35484968732` failed generated-tree
drift; ci-main run `35484968706` failed Policy/ci-required. Required
xcode-27/Linux-X64/Linux-ARM64 closure is absent.

Provider App ID, installation ID, verifier revision, authenticated endpoint,
credential name, signing algorithm/key/trust root, external A identity, and
permanent Policy identity remain null or synthetic. These cannot be supplied
by fixture IDs. The main observation is 03:58:55Z while the frozen manifest
was written 05:53:29Z; require a fresh read of every SHA/ref/source/ruleset
before lease acquisition.

## Exact approval prerequisites

No conditional execution approval is supportable until all of these are
attached to a new immutable bundle and independently checked:

1. Bind every generator input, including the historical pre-record schema, and
   validate a strict positive `permanent_binding` plus recomputed cryptographic
   preimages.
2. Bind actual generated `ci-main` and called-publisher source at immutable
   commits/blobs, per-job permissions, caller `needs`/concurrency ordering,
   exact workflow-call transport, and the complete typed consumer census.
3. Supply distinct external-A and Main-B/permanent Policy identities, App IDs,
   installations, source/revision, least-privilege permissions, signed result
   envelope/replay binding, and durable trust roots.
4. Resolve release-attestation semantics (automatic immutable release versus
   explicit custom predicate), immutable-release setting/readback, complete
   subjects, and exact OIDC/certificate policy.
5. Provide an enforceable freeze/CAS/exclusive-writer and recovery/rollback
   mechanism, with fresh base/head/tree/ruleset reads and resulting-main/first-
   run SHA proof. No PR head substitution, branch-advance tolerance, or
   post-hoc mismatch acceptance.
6. Prove Tree-A admission, protected merge, Main-B publication/verification,
   Tree-B adoption, true check-equivalence, durable trust before removal, and
   removal readback. Execution remains a separate explicit user decision.

