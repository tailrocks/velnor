# Independent v8 authority-transition review

Reviewer: `authority_transition_review`  
Observed: 2026-09-20  
Verdict: **not approved; execution blocked**

This is an independent, read-only review. No source, ruleset, check, App,
trust, release, ref, merge, runner, or credential state was changed.

## Exact reviewed inputs

- Plan Markdown: `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v8.md`
  SHA-256 `503a564afcbe157e607215c0549480b2cade36525b6f3aeb41198418c6add639`.
- Plan JSON: `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v8.json`
  SHA-256 `87a5896eb8cf75e53b49f8eacb61226315a9f79c574af3078fdc34390252c321`.
- Owner structural audit report SHA-256
  `9100a2323635f6f299238773dfc49aeaddd48be46309d7a01064f0532b215f5f`.
- Owner structural audit results SHA-256
  `2e2764bdd84d6e5fbbf95046ed9e4391046f2ce26f45424325461dc9aaacb96c`.
- Hostile fixture set SHA-256
  `72026acbd960e57e226fc116dff72962561d32a8d12d5de0b86cf900bbbc41b7`.
- Current-main checkpoint SHA-256
  `77e1ecc848e0a3ad73cdfd853b788054e8cda02e0aae38d076b7c59b0da130bd`.
- Closure-input snapshot SHA-256
  `1b890ef899c8a02146e7d6ab1d664fcb92213924e8c7a722f8f57a2bfb55c072`.

The JSON correctly says `successor_draft_external_blocked`,
`execution_authorized=false`, `mutation_performed=false`, and independent
approval `not_approved`. No v8 freeze manifest was supplied; this report treats
the two hashes above as the review tuple, not as operational authorization.

## Closed at design level since v7

The following v7 defects are materially addressed in the proposal, but are not
runtime proof: workflow-call-only publisher with a guarded `ci-main` push/main
caller; no fabricated caller-job output re-export; S4 binding before release
asset and S6 release attestation after asset publication; explicit caller versus
called OIDC claim names; an acyclic named job chain; and a 128-leaf mapping that
matches the 128 leaf paths of the permanent-B schema. The owner audit proves
these JSON shape checks and hostile mutations, not GitHub execution or provider
behavior.

## Independent hard findings

### 1. Reusable-workflow caller permissions make B writes impossible

`main_b_publisher.ci_main_graph.caller_job.permissions` is only:

```text
actions:read, contents:read, attestations:read, checks:read, id-token:write
```

The called jobs require `contents:write` (`reserve-release`, `publish`),
`attestations:write` (`attest-binding`, `attest-release`), and `actions:write`
(`record-upload`). GitHub states that permissions passed by a caller to a
reusable workflow can only be downgraded, not elevated, and nested workflows
cannot increase them. Therefore the proposed called jobs cannot perform their
declared writes under this caller job. The owner audit checks each called job in
isolation and misses this caller upper bound.

Required correction: grant the caller job the exact upper-bound write scopes
(while keeping job boundaries and unprivileged build restrictions), or redesign
all side effects through an actually authorized actor. A called-workflow job
permission declaration alone cannot fix this.

Reference: [GitHub reusable-workflow permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations#supported-keywords-for-jobs-that-call-a-reusable-workflow).

### 2. S7 record/provider handoff is still temporally cyclic

The graph is `record-upload -> verify-B`. The strict record schema requires
`provider_check` (including the B check ID, provider/integration IDs and
resulting main SHA) and `terminal_census`. Yet `record-upload` creates the
record artifact before the external B check exists, has only `actions:write`,
and has no provider/check credential. The external provider must first read that
artifact before emitting `Policy-bootstrap-B`. The post-run census is also
specified after record upload and provider verification.

The machine `field_provenance` leaves all sixteen record/provider/called fields
with the ambiguous producer/source `record-upload or verify-B`. This does not
define a transportable producer and permits a self/forward reference. A GET
endpoint for an artifact is not a callback or workflow transport channel.

Required correction: split the artifact into an explicitly pre-provider record
and a post-provider signed result/census (or add a real post-provider upload),
with exact producer, credential, artifact ID transport, binding, and strict
schema for each phase. No field may have an `or` producer.

### 3. Canonical schema contract is internally inconsistent and incompletely bound

- The strict record schema requires `caller_workflow.oidc_job_workflow_sha` and
  `producer.oidc_job_workflow_sha`. V8's corrected machine contract uses
  standard caller `workflow_sha` and called/producer `job_workflow_sha`; no
  mapping or successor schema reconciles these names.
- The strict record schema requires `canonical_release_schema_sha256`, but the
  v8 canonical schema list has no release-schema path/hash. It cannot be
  validated as written.
- V8 lists the historical
  `validator-binding-audit-2026-09-20/binding-predicate.schema.json` as a
  current canonical schema, while `schema-resolution.json` explicitly says it
  is audit-only and not a current schema.
- `canonical-root-manifest.json` binds the temporary/permanent schemas and
  fixtures but does not bind the new record/provenance schema. Its referenced
  hash alone is not a root-bound contract.
- No positive strict record fixture is supplied; the existing permanent-B
  positive fixture is not a record artifact.

Required correction: publish one successor schema set, name the exact release
schema (or remove that required field), remove the historical schema from the
current list, add all current schemas/fixtures to the canonical root manifest,
and validate a positive record plus hostile provider/transport cases.

### 4. Field-lineage direction and S0 source transport are undefined

All seven `field_lineage_edges` name the consumer as `from_job` and producer as
`to_job`; for example `from_job=artifact-verify, to_job=build-linux-x64`,
while the `needs` edge is consumer-to-producer. The JSON never declares this
non-obvious orientation or validates that each field is produced by the named
producer and consumed by the named consumer. A field-edge checker can therefore
pass with reversed or mismatched types.

S0 names `ci-main caller job` as producer, but the caller `uses` job has no
shell steps and no outputs. The called workflow can see caller context, but the
plan does not identify the exact job/step/API response that obtains and
transports each caller workflow blob/claim into S0. “GitHub context and
Contents API” is a source class, not an executable edge.

Required correction: define one edge orientation, validate producer output and
consumer input sets/types, and name the real producer/step/API response for
caller identity and source fields.

### 5. Preimage partitions are not machine-complete

The JSON gives stage `produces`/`consumes`, prose exclusions, and an acyclic
digest graph, but no canonical `binding_fields` or `release_fields` set, no
canonical serialization/UTF-8/number handling, no exact preimage bytes, and no
algorithm-input record. “S4 is S0-S3” does not prevent an implementation from
omitting or adding same-stage fields while satisfying the owner audit. The
contract needs exact ordered field paths, canonical bytes, and digest source for
S4, S6, and S7.

### 6. Revision-bound evidence has a stale SHA collision

`revision_bound_facts.main_sha` and the checkpoint/closure inputs bind
`89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, with parent/base
`325719f1e05d3d46322c9fd3eeb9ad545e175638`. But the same JSON's
`evidence.current_main` and `evidence.current_parent` fields still label
`325719f1...` and `e94b484...` as current. The Markdown says current evidence is
for 89f82. This is not a harmless historical label: consumers can select the
stale `evidence.current_main` field and bind checks to the wrong revision.

Required correction: relabel the old value as the base/parent snapshot or remove
it; make every current-main field resolve to one exact SHA/tree/checkpoint tuple.
Recompute the pair hash after correction.

## Remaining execution blockers (not merely missing run IDs)

- Target generator revision is null; current revision 54 and old `0dc` are not
  targets. B source/output and fixed-point closure are absent at 89f82.
- The only observed runtime used forbidden `macos-26`; full `xcode-27` native
  and supporting workload census is absent; D19 has no clean published runtime.
- Real B App installation, provider/integration/verifier identities, credential
  proof, live verifier/API/crypto implementation, and called-workflow identity
  evidence are unresolved placeholders.
- Provider freeze is unproven: actor 5 remains `always` bypass, ruleset updates
  are documented full-object `PUT` without an If-Match/CAS claim, and the
  proxy/lease/watchdog/death/recovery exercise is absent. The plan correctly
  leaves the freeze choice unselected; this is an execution hard stop, not an
  atomic transaction.
- The release text says “immutable tag” but does not bind the repository/org
  immutable-release setting and readback. GitHub immutable releases lock tags
  and assets only on publication and automatically create a release attestation;
  the plan must prove the setting and include that attestation in the exact
  record/census.
- Tree-B adoption remains a prose `validator-pin-adoption.v1` object without a
  schema path/hash, exact PR head/base/tree/lease fields, or a durable permanent
  trust-root hash before temporary authority removal.

References: [GitHub OIDC reusable-workflow claims](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/secure-your-work/security-harden-deployments/oidc-with-reusable-workflows), [immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases), and [ruleset update API](https://docs.github.com/en/rest/repos/rules#update-a-repository-ruleset).

## Approval requirements

Do not execute or mutate authority from v8. A conditionally reviewable
successor must, at minimum, fix the caller permission upper bound; make S7
record/provider/census transport acyclic and single-producer; reconcile and
root-bind the strict schemas; define field-edge orientation and exact preimage
bytes; normalize all revision evidence; and provide provider freeze/recovery,
real identities, target source/output, native matrix, verifier, and Tree-B
durable-trust evidence. User must separately select the freeze threat model and
approve execution. No future run IDs are required for a design review, but no
execution approval is supportable without the live identities and evidence.
