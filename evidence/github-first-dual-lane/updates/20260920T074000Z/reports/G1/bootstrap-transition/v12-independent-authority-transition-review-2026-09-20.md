# Independent v12 authority-transition review

Date: 2026-09-20. Review is read-only. No GitHub, ruleset, branch, workflow, release, artifact, credential, source, or authority mutation occurred.

## Verdict

`NOT APPROVED`.

V12 is a materially better contract draft. The OLD_PRE input is now explicitly bound, root and fixture preimages are reproducible, the unsupported `integration_id` dependency is removed, workflow-call output names are declared, and custom versus automatic release attestation is stated as separate. Those are structural improvements only.

The proposal still has internal identity contradictions and non-executable transport/census rules. It also explicitly lacks the external provider, source publication, native closure, signing trust, and freeze/CAS capabilities needed to perform the transition. Therefore:

- design status: blocked pending the findings below;
- execution status: blocked, independently of design repair;
- `owner 44/44`, `adversarial 11/11`, and strict-positive `7` are structural fixture results, not approval or live evidence.

## Frozen input and reproducibility

The exact v12 tuple reviewed was:

| Input | SHA-256 |
|---|---|
| `v12-freeze-manifest.json` raw | `0693377186d05348585749f199450cb4f3e69c9641118157ad10a862f7887b69` |
| `v12-canonical-model-source.json` | `5c3c7722481adf5c025502e7ba0d0edc252d066dd1cafb219a4b4a208b3a5062` |
| `v12-canonical-field-dag.json` | `33df75eceaa11d94085e4766f0b73da32c82c7058542651a878237b0ebacc425` |
| `AUTHORITY-CHANGE-PLAN-2026-09-20-v12.json` | `127acef035c8fee8f729a48c4efa5ad8a107be2c8ea4c27ac2718e0ffea00390` |
| `AUTHORITY-CHANGE-PLAN-2026-09-20-v12.md` | `b3f8ec14a33ef61d507b329b9d169849ab8f0c97f9686b851fe7a52b3d539556` |
| `canonical-root-manifest.v5.json` raw | `546d79d955ba5b56b2143b08a0c20edca36a2cec84e6d056b39dd3d4bc9e0946` |
| canonical root digest | `e2b30dc21732f957ee9b69b45f270b2d72a995b624d4b19a448eb86985e404f8` |
| owner audit results | `1287d2e7d672669db115c15ad67d761f91ea38962120174eec85472561334ec2` |
| owner audit report | `aaaef33f2919b863be56891be042aaf5d51f40106a5e4d09f8f29c520dba433d` |
| adversarial results | `35207920676cc48e074a5aa2a859b67efadb2f4eb6bd95fa28b71c751c459125` |
| adversarial report | `72db4a7878d78896a3147f1cb84558658baefc6edfa67a772b493129f284f567` |

The root digest was recomputed from the canonicalized root manifest; bound-file and normalized fixture hashes match the root manifest. The following read-only facts are also exact in the frozen bytes:

- `authority_claim=false`, `execution_authorized=false`, `mutation_performed=false`;
- manifest status is `frozen_proposal_external_blocked`; `frozen_at=null`;
- observed main is `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`;
- root-bound OLD_PRE schema is `f5d04aad1cb6ee764bdd70e13961ca7ec28924994e149c3a49933fdb9441e26c`, explicitly marked historical input;
- the model records the source audit checkpoint as observed evidence with SHA `77e1ecc848e0a3ad73cdfd853b788054e8cda02e0aae38d076b7c59b0da130bd`, observed at `2026-09-20T03:12:50Z`, earlier than the manifest's `03:58:55Z` main observation; it is not one of the root manifest's bound files.

The fixture values remain synthetic. For example, the provider endpoint is `https://provider.invalid/v12/results`, the provider/signing identities are fixture values, and the resulting main is the placeholder `bbbb...`. None is live proof.

## Findings

### F1. Policy-bootstrap-B has three incompatible identities and is included despite self-exclusion

This is a hard contract contradiction.

The model excludes `Policy-bootstrap-B` from the terminal census (`exclude_nodes` and `exclusion_keys`), but `positive-verify-b.json` includes this row:

```text
workflow_path   .github/workflows/ci-policy-validator-products.yml
job_key          policy-validator-B
check_name       Policy-bootstrap-B
check_external_id terminal-check-13
check_run_id     7613
run_id           7413
```

The provider fixture separately allocates another `Policy-bootstrap-B` check: run/check `7301`, external ID `provider-result-v12-fixture`. `verify_b.policy_check_run_id` is `7402`; that is the `ci-policy` census row's `run_id`, not its check-run ID (`7602`), the census B check ID (`7613`), or the provider check ID (`7301`). The field is explicitly defined as a Check Runs API ID.

The static validator only checks cardinality/path/shape and positive integer IDs. The adversarial identity check also does not assert exclusion membership, ID uniqueness, expected job/check mappings, or equality of `policy_check_run_id` to the canonical B check. The result is not a selectable real-world check identity.

Required repair:

1. Define one canonical Main-B policy check identity: exact App ID, name, external ID, check-run ID, head SHA, status, conclusion, and producer.
2. Decide explicitly whether the provider check is the required context or whether a caller check is; do not create two same-name authorities.
3. Remove the B verifier's own row from the census, or change the exclusion model so the included row is intentionally required and machine-bound. A name-only or ambiguous exclusion is forbidden.
4. Bind `policy_check_run_id` to the actual Check Runs API `id` for that identity and test rejection of a workflow-run ID.

### F2. Most field transport remains prose, not executable transport

The frozen DAG has 69 edges:

- 56 `typed_output_transport` edges;
- all 56 use the same generic string: `explicit producer output, immutable artifact, or persistent record; needs is scheduling only`;
- 10 `producer_outputs_to_consumer_inputs` edges have no transport value;
- one persistent adoption edge and two precise workflow-call output edges.

The two workflow-call edges name seven output fields. That closes only the verify-B-to-caller-to-Policy boundary. It does not define the build → verify artifact → reserve → binding attestation → publish → release attestation → pre-record → provider → verify-B edges that carry the other typed fields.

Required repair for every edge:

- exact producer job/output name or persisted artifact name and ID;
- exact file/path/schema and digest binding when persistent;
- exact consumer input/output mapping and `needs` dependency;
- source workflow commit/blob and step/action ID/name;
- fail-closed handling for missing, duplicate, stale, or replaced values.

Generated workflow source must validate these mappings. A namespace list or generic transport sentence is not an implementation.

### F3. The 15-consumer census is not enforceable as written

The 15 required workflow paths and row fields are present, and the positive rows digest recomputes. That proves fixture shape only. The model has no machine-owned expected `(workflow path, workflow SHA, job key, check name, external ID, App ID)` matrix. It also does not enforce:

- one unique run/attempt, job ID, check-run ID, or external ID per row;
- repository/event/ref equality for every row;
- source workflow SHA equality to the trusted generated source;
- exact exclusion of `verify-B`, `Policy`, `Policy-bootstrap-B`, and caller-wrapper;
- per-row reusable-workflow source identity.

All positive rows use synthetic `workflow_sha=555...`, `event=push`, `ref=refs/heads/main`, `check_app_id=7303`, and `head_sha=bbbb...`; this cannot establish the real 15-job closure. The fixture simultaneously violates the declared B exclusion (F1).

The Actions attempt Jobs API exposes job/check/step status fields but not the complete caller/called workflow source identity required by the row model. GitHub documents `prepared_workflow_job` audit data for `job_workflow_ref`, `calling_workflow_refs`, and corresponding SHAs. The plan mentions OIDC and audit readback but does not bind a per-row join from those source identities to each census row. See [GitHub reusable-workflow audit data](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/reuse-automations/reuse-workflows).

Required repair: publish a machine-readable 15-row expected identity map, collect real run/attempt/job/check/App responses, join each reusable job to OIDC or `prepared_workflow_job` source identity, and reject every duplicate, excluded self row, wrong source SHA, wrong ref/event/head, nonterminal state, or non-success conclusion.

### F4. Release attestation is named but not executable

V12 correctly says the custom predicate and GitHub automatic immutable-release attestation are separate. The binding is still incomplete:

- custom producer source commit is `null`;
- S6 names producer job `attest-release`, while the release contract names `.github/workflows/ci-release-package-signer.yml`; no exact source/action/trigger/needs join is bound;
- automatic readback is only prose: `immutable release setting and tag/target/assets are read back before acceptance`;
- no exact automatic-attestation endpoint, response fields, predicate bytes, certificate identity, release ID/tag/target/asset equality, or retry/expiry rule is transportable;
- the custom fixture only proves synthetic field equality.

GitHub documents `GET /repos/{owner}/{repo}/immutable-releases` with `enabled` and `enforced_by_owner`, and immutable releases protect the tag and assets after publication. The plan must bind that setting readback, the release object and asset bytes/digests, and the attestation API response to the exact release ID and manifest. See [immutable-release API](https://docs.github.com/en/enterprise-cloud@latest/rest/repos/repos), [release API](https://docs.github.com/en/rest/releases/releases), and [immutable-release semantics](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).

### F5. Freeze, race, and rollback are still unavailable

`freeze_contract.provider_enforced_capability` is `null`. The contract requires an exact base/head/tree lease, conditional ruleset update or independently controlled exclusive writer, actor-5 bypass exclusion, coordinator-death recovery, and non-clobbering rollback, but supplies none of them.

The workflow graph only declares `concurrency_group: velnor-main-${{ github.ref }}` and omits `cancel-in-progress`. GitHub's documented default allows the running job to continue while replacing only a pending run; a group is not an immutable SHA lease. See [GitHub concurrency](https://docs.github.com/en/actions/concepts/workflows-and-actions/concurrency) and [workflow concurrency syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).

The repository ruleset API is a PUT update/readback operation. V12 does not specify a supported ETag/If-Match/CAS transaction, complete ruleset object hash, exclusive actor, lease TTL, watchdog, or recovery actor. Do not infer atomic merge + ruleset change + first main job + release from separate API calls. See [Update a repository ruleset](https://docs.github.com/en/enterprise-cloud@latest/rest/repos/rules).

The observed main read is stale relative to the manifest observation and `frozen_at` is null. Before any future execution proposal, require a fresh read of main/base/head/tree, trusted workflow blobs, ruleset object/bypass actors, and all source revisions. Every branch/ruleset/release write needs a bound revision check; rollback must be conditional on the same revision and must abort on newer state.

### F6. Authority and permanent-trust identities remain null

The provider contract leaves `provider_app_id`, installation ID, verifier revision, endpoint, signer algorithm, key ID, and trust root unresolved. The positive provider values are fixture strings/IDs and a fake base64 signature. The external Tree-A `Policy-bootstrap-A` actor/App, Main-B provider installation, and permanent Tree-B `Policy` verifier App/source/trust identity are not concrete.

The adoption schema only requires a nonempty `permanent_trust_root`; the positive value is `fixture-trust-root-v11`. `temporary_authority_removed=true` is a synthetic boolean, not an API readback. The permanent binding schema also permits generic strings such as `permanent_trust_policy_sha256`, and the positive permanent fixture uses `fixture-0136` rather than a digest. Its `verification.temporary_authority_used=true` is accepted by the strict schema, so the final permanent record does not prove that no temporary authority remains in the permanent trust path.

Required repair:

- bind external App/install IDs, verifier revision, signing key/trust root, check creation/update/readback identity, and least-privilege permissions;
- bind Tree-A admission to exact PR head/base/tree, expiry, one-use actor, and no permanent-B key;
- bind permanent Tree-B verifier source/workflow SHA/blob/App/OIDC trust and exact adoption template/schema mapping;
- require a durable permanent trust-root readback before uninstall/removal of the temporary App/key;
- make the permanent record's final authority state and trust-policy digest constrained and equality-checked, not generic fixture strings;
- prove true check equivalence across required contexts and all consumer jobs. No old checker, skipped/native wrapper, or candidate semantic authority may satisfy it.

### F7. Native/source closure and publisher/signing isolation remain unproven

The current checkpoint records:

- runtime-products run `35484968618` succeeded while using forbidden `macos-26`;
- preview run `35484968732` failed generated-tree drift against pinned generator `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`;
- ci-main run `35484968706` failed its Policy prerequisite;
- typed Main-B publisher source/output are absent on observed main.

The v12 model declares Linux-X64, Linux-ARM64, xcode-27, publisher, artifact, attestation, release, and 15-consumer stages, but no live generated source, runner labels, action IDs, run attempts, job/check IDs, artifact IDs, manifest digests, or byte equality exists. Build, verify, attest, and publish credential separation is a permission table, not proof of the actual workflow. The forbidden macOS-26 path cannot be used as a green wrapper or fallback.

### F8. “22 negative cases complete” is label completeness, not hostile validation

`negative-fixtures.json` contains the 22 expected case names. The owner audit checks set equality between those names and `model.required_negative_cases`; it does not load a mutated fixture for each case or show a rejection result. The separate adversarial run executes only eight schema mutations plus structural predicates. Its `terminal-row-identity` predicate checks row count, unique workflow paths, and positive IDs; it does not test the F1 exclusion/identity contradiction.

For independent reproducibility, each required negative case needs a bound mutated input, validator version/source hash, expected rejection predicate, and observed rejection output. The root manifest binds the generator/model/schemas/fixtures but does not bind `audit_v12_bundle.py` or `adversarial_v12_bundle.py`. Hashing result reports alone cannot establish independent execution.

## Approval prerequisites

No execution approval should be requested until all items below are concrete and re-frozen as one immutable tuple:

1. User selects the exact Tree-A PR head/base/tree, target generator revision, generated source/output commits/blobs, and ruleset revision. Resulting main SHA remains unknown until a protected merge and must never be substituted with PR head.
2. An external operator/provider supplies a real one-use Tree-A admission App/check, Main-B provider App/install/verifier/signing trust, and permanent Tree-B verifier identity, with exact permissions, expiry, and source/OIDC bindings.
3. A provider-enforced exclusive freeze/lease or proven conditional writer exists. It must cover branch, ruleset, bypass actors, first main run, release writes, lease expiry, coordinator death, and conditional rollback. Ordinary concurrency, operator consent, or an unverified ETag assumption is insufficient.
4. Generated workflow source is present at the reviewed revision, workflow-call-only publisher is guarded by the caller's push/main context, and the full native closure passes on Linux-X64, Linux-ARM64, and xcode-27 with no macOS-26/skipped/native-wrapper substitute.
5. The 15-consumer expected identity matrix and exact per-edge transport are machine-validated from actual run/job/check/artifact/manifest/API responses. All DCO/CI children and generated/source/setup/report/actionlint consumers must be terminal completed success at the exact resulting main SHA.
6. F1 is resolved with one canonical Policy-bootstrap-B check identity, exact `check_run_id` semantics, explicit self exclusion, and exact Main-B-to-Policy output transport.
7. Provider signed bytes, raw artifact ZIP/inner payload/binary/release equality, release ID/tag/target/assets, custom predicate, and automatic immutable-release attestation are read back through documented endpoints and bound to the same source/tree/resulting-main tuple.
8. The first-main timing proof captures caller run identity, called-workflow OIDC/audit source identity, all required workflow attempts, and no post-merge source substitution. Cross-workflow handoff must use actual reusable-workflow outputs/artifacts/persistent records, not invented standalone called-run IDs.
9. Tree-B adoption proves exact PR head/base/tree, base workflow/blob/template/schema mapping, permanent trust-root readback, permanent Policy check equivalence, and successful removal readback. Temporary App/key removal occurs only after durable permanent trust is independently verified.
10. Re-run the complete independent structural and hostile suite from a hash-bound validator bundle, including concrete instances for all 22 negative cases; record external input hashes and user approval separately from execution evidence.

Only after these prerequisites are satisfied may the user make the separate final execution approval. V12 itself grants no authority and must remain unchanged.

## External inputs

- [GitHub concurrency](https://docs.github.com/en/actions/concepts/workflows-and-actions/concurrency)
- [Workflow syntax and `cancel-in-progress`](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [Reusable-workflow audit identity (`prepared_workflow_job`)](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/reuse-automations/reuse-workflows)
- [Repository ruleset update API](https://docs.github.com/en/enterprise-cloud@latest/rest/repos/rules)
- [Immutable-release setting API](https://docs.github.com/en/enterprise-cloud@latest/rest/repos/repos)
- [Release API](https://docs.github.com/en/rest/releases/releases)
- [Immutable-release protections](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
