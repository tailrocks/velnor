# Independent v12 canonical-repair review

Date: 2026-09-20. Scope is only `v12-canonical-repair-2026-09-20/` at manifest SHA `3d56b36ecfb73a382ee621b2c152704adf87ec17ae85f9361f61ea2a63980bfa`. V12 frozen bytes, GitHub state, authority, release, credentials, and source were not mutated.

## Verdict

The bounded model regeneration and declared S7b digest computation are reproducible at the semantic/fixture level. The repair is not an authority-transition approval and is not execution-ready.

One hard temporal protocol defect remains: the provider result signs `provider_check_status`/`provider_check_conclusion`, requires `provider_check_conclusion="success"`, and includes the conclusion in the S7b ordered digest, while the protocol says the same check is terminalized to `completed/success` only after the signed result and outer digest are stored. This is an unexecutable signing/terminalization cycle under the GitHub Checks API.

## Exact inputs and execution evidence

Manifest and input hashes:

| Item | SHA-256 |
|---|---|
| repair manifest | `3d56b36ecfb73a382ee621b2c152704adf87ec17ae85f9361f61ea2a63980bfa` |
| `canonical-model-v3.json` | `7996c852e749c32eafb338a38f0b7e865ee7ea22e0325de7d504db07c67b9aea` |
| `declared_preimage.py` | `2483866b234ef329fb5adac2622f53769d3ac3eb8ce02c79afba2cbe3e457b7a` |
| `build_repair_checkpoint.py` | `35a5de03a169bc795a108cc60a25ee752b725aeef0de6666bf2927dcb5a89047` |
| `independent_preimage_test.py` | `bccefbfd39281a632b1efca323b88c78b060a31c93e769e56e4562e50e8cf74b` |
| `audit_repair_checkpoint.py` | `4248ac9d606fb5f2e8680c9ab27c29d5396df75ed77c8ad614a48a69e0e03bba` |

I copied only the model and the four declared Python files to a fresh temporary directory. No v12 bundle, OLD_PRE schema, v2 model, canonical-model.json, or historical file was present. With Python `3.13.15`, `jsonschema==4.25.1`, and uv `0.11.29`, these commands passed:

```text
uv run --with jsonschema==4.25.1 python3 build_repair_checkpoint.py
uv run --with jsonschema==4.25.1 python3 independent_preimage_test.py
uv run --with jsonschema==4.25.1 python3 audit_repair_checkpoint.py
```

The temporary build produced only `canonical-model-v3.json` and seven emitted fixtures. All seven fixture hashes match the manifest. Strict schema validation, temporal lint, provider schema coverage, shared audit, and independent recomputation all pass. The semantic provider digest is:

```text
3590a056efa0be7bb719b31c2797bc8e8e4b7449049a334e58bd7afb6fb55516
```

The checked output results match the manifest hashes except for the checkpoint-results JSON noted below. Existing checkpoint results are still internally consistent with the manifest.

## Findings

### R1. Provider check signing and terminalization form a temporal cycle

The repair model says all of the following:

- `provider_contract.check_allocation.signed_check_fields` includes `provider_check_status` and `provider_check_conclusion`;
- `provider_contract.signature.signed_bytes` includes check `status` and `conclusion`;
- the provider schema requires `provider_check_status="completed"` and `provider_check_conclusion="success"`;
- `provider_check_conclusion` is in the 38-field S7b ordered preimage and therefore affects `provider_result_digest`;
- `check_allocation.terminalization` says to update the same check to `completed/success` after the signed result and outer digest are stored.

The documented Checks API shows allocation as `in_progress` with `conclusion=null`, then updates the same run to `completed` with a conclusion; providing a conclusion sets status to completed. If allocation instead creates `completed/success`, that violates the model's declared post-sign terminalization order. Under the declared sequence, there is no consistent completed-success value to sign. See [GitHub check-run API](https://docs.github.com/en/enterprise-cloud@latest/rest/checks/runs) and [GitHub checks guide](https://docs.github.com/en/rest/guides/using-the-rest-api-to-interact-with-checks).

Required repair must choose one executable ordering:

1. Allocate queued/in-progress check; sign a payload that excludes final status/conclusion; terminalize the same check after storage; verify final readback separately; or
2. Terminalize/read back the check first, then sign a payload that binds its final identity, with no later mutation.

The model must state exact fields in the signed payload, outer digest preimage, and post-sign readback. `provider_check_conclusion` cannot remain a final post-sign value in the signed/preimage input while terminalization is declared later.

### R2. Signed-payload boundary is underspecified around the signature field

The S7b outer preimage correctly excludes `provider_result_digest`, result ID, nonce, check run ID, external ID, status, and attestation digest. It includes `provider_result.signature_base64`. That is safe only if the provider signature's signed bytes explicitly exclude both `signature_base64` and the outer `provider_result_digest`.

The current model describes signed bytes in prose, not as an ordered field list. No computed self-hash cycle was observed in the fixture: the outer digest is distinct from the whole-object digest, and the independent test passes. Still, the signed-field set must be made machine-exact and disjoint from the signature and outer-digest fields. Otherwise a provider can interpret “canonical provider-result payload” as including its own signature.

### R3. Digest mutation coverage is strong but the declared negative suite is not executed

Independent recomputation confirms:

- 38 ordered S7b fields produce the emitted digest;
- whole-object hashing produces a different digest;
- mutating ordered `provider_result.source_record_artifact_digest` changes the selected digest;
- mutating excluded `provider_result.provider_result_id` leaves the selected digest unchanged;
- expanded mutation over all 256 excluded fields present in the six preimage roots leaves the selected digest unchanged;
- all ordered-field mutations change the selected digest except `provider_result.canonical_root_manifest_sha256`, which is intentionally normalized to zeroes.

The model declares nine repair negative cases (`old_pre_hidden_input`, provider whole-object/future/own-field cases, premature release/final-manifest cases, unbound provider leaf, duplicate terminal workflow, and wrong source), but no script executes those named cases. The existing independent script executes only one excluded-field mutation and one ordered-field mutation. The positive repair checkpoint therefore proves the declared digest function, not the full nine-case hostile suite.

Required repair: bind concrete mutated inputs and expected rejection outputs for all nine cases, especially the provider check temporal cycle and terminal source/identity predicates.

### R4. “Isolated reproduction” output is path-dependent

The regenerated fixtures, reports, independent results, and audit results are byte-identical to the checkpoint values. `repair-checkpoint-results.json` is not byte-identical because the generator embeds absolute `model_path` and `fixture_directory` values.

Observed regenerated result SHA: `1540f6cc8047c7edd3fb36c930d73a67405bf4ed8de29b36aaa5ce9beecb7b9c`.

Manifest/checkpoint result SHA: `315aaeb6db10cc477a9de5053fd78b9d6a2506c212f1455ef6b32ce05fb19883`.

The only semantic difference is the temporary path. If the result hash is evidence, paths must be relative/canonicalized or omitted before hashing. Otherwise isolated reproduction proves semantic equality only, not byte-for-byte checkpoint regeneration.

### R5. Declared-input scope is clean for generation, but independent loading is broader than declared

Static inspection plus the clean-directory run found no reads of OLD_PRE, v2, `canonical-model.json`, or the frozen v12 bundle. `build_repair_checkpoint.py` reads `canonical-model-v3.json` and imports code; its temporary output contains only the model and generated fixtures. `audit_repair_checkpoint.py` reads the model and six explicit fixture names.

`independent_preimage_test.py` instead loads `(ROOT / "fixtures").glob("*.json")`, then selects six known stems. Today the directory contains only those six fixtures, so the digest result is unaffected. A future or stale extra JSON fixture would nevertheless be read, violating strict declared-input accounting. Replace the glob with the six model-declared schema names.

The base Python environment lacked `jsonschema`; the run required an unbound external environment (`jsonschema==4.25.1`, Python 3.13.15, uv 0.11.29). Pin the validator/runtime or include a lock/hash if strict reproducibility is required.

## What the repair does establish

- model version `12-repair-3` and model hash are exact;
- v12 base model is referenced only as declared metadata, not read from disk;
- all seven embedded schemas and fixtures validate;
- schema leaves and canonical registry are bijective for all seven schemas (145 permanent, 72 pre-record, 11 transport, 30 provider, 14 release, 22 verify-B, 17 adoption);
- provider fields have schema coverage;
- S4, S6, S7b, and S7c ordered/excluded sets have no overlap;
- no ordered field is from a later stage than its preimage stage under the declared stage order;
- provider own digest/result IDs are excluded from the outer digest.

These are static contract/fixture results. They do not prove typed workflow transport, terminal consumer semantics, raw ZIP/inner-payload/binary/release equality, provider identity/signing trust, freeze/CAS/recovery, or GitHub production execution. The manifest itself retains those external gates.

## Required disposition

Keep this repair checkpoint external-blocked. Before any successor plan or authority action:

1. Repair R1 with an executable check/sign/terminal state machine and exact signed-field set.
2. Make checkpoint evidence path-independent and bind the runtime/dependency environment.
3. Execute all nine declared repair negative cases with hash-bound mutated fixtures.
4. Keep prior v12 transport, 15-consumer census, provider identity, native closure, signing, freeze/CAS, rollback, and permanent-adoption blockers open until independently proven.
5. Obtain separate user approval before any trust/ruleset/branch/release mutation.

No authority or production state is approved by this report.
