# Independent v9 schema/DAG adversarial audit

Status: design-only external audit. No authority, release, dispatch, verifier execution, or source mutation.
Owner 33/33 result is preserved as proposal evidence only; it is not treated as approval.

## Exact input/output hashes

- Frozen v9 plan Markdown: `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed` (expected `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`).
- Frozen v9 plan JSON: `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615` (expected `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`).
- Canonical v9 root digest: `7f888b746ca8c7a6f6e8342c7d0a450edd70a5bf766421f9a2d8ddd2cd9e517b` (expected `7f888b746ca8c7a6f6e8342c7d0a450edd70a5bf766421f9a2d8ddd2cd9e517b`).
- Hostile transport fixture: `6e838b3b765ceb4570f1a86628d606dffa8cd03909c81dfc1d1abab7023f96be` (expected `6e838b3b765ceb4570f1a86628d606dffa8cd03909c81dfc1d1abab7023f96be`).
- Independent hostile fixture set: `8af355978a6c3ec33bc7d39f9c19caf11e245eccc4af729c64c6125b8eb3f0b5` (8 cases; all expected reject; unexecuted).
- Independent materialized field DAG: `902854e6591687095856fdf507115e933cb4d57ade8437b8875ca19d104303b9` (146 permanent leaves, 124 record leaves, 93 typed transport fields).
- Independent script: `4797485a4441442712941a3c24649aa915db4da3ec0e9deee96c194259ad773b`.
- Independent result JSON: `d010e465c84d8552453af3612e950bc41715ee19a5fadd63070b0774944ae88a`.

## Independent result

- Checks: 22; pass 18; fail 3; unimplemented 1; unavailable 0.
- Findings: 12. Authority claim: `False`.

## Findings

- `V9-LEAF-RELEASE-001` (high): The current canonical_leaf_paths map has no release_manifest namespace; all 14 leaves of a bound current schema are unrepresented.
- `V9-DAG-S7B-001` (medium): S7b aggregate producer_job is a slash-joined two-producer claim; unique producer is only recoverable from separate prose/group metadata.
- `V9-DAG-S7B-002` (medium): Six S7b fields are verify-B-local/API captures and have no field_lineage_edges producer transport; the contract must keep their API derivation explicit.
- `V9-PREIMAGE-001` (critical): S7_provider_result lists provider_result_id/digest in ordered_fields while also excluding them; digest_source says exactly ordered_fields.
- `V9-PREIMAGE-002` (critical): S7_terminal_census lists terminal_census_digest in ordered_fields and excluded_fields, and orders terminal_census_id (its own ID).
- `V9-PREIMAGE-003` (critical): Terminal-census ID is produced at S7b/verify-B and is consumed in its own digest preimage; no_self_or_future_preimage=true is false unless ID is allocated before digest and explicitly specified.
- `V9-SCHEMA-ID-001` (high): Permanent binding/pre-record use velnor-workflow-policy-validator/Linux-X64 while release-manifest uses velnor-policy-validator/ubuntu-24.04; a record cannot satisfy both current schemas.
- `V9-SCHEMA-BIND-001` (high): Schema accepts a syntactically valid but wrong canonical root digest; verifier must enforce equality to the measured root, not only the 64-hex pattern.
- `V9-FIXTURE-RELEASE-001` (medium): Current root requires positive fixtures but binds no positive release-manifest fixture; its 14-leaf schema is not exercised by a positive instance.
- `V9-FIXTURE-OUTPUT-001` (high): The actionlint fixture is not an executable realization of the declared typed output graph; actionlint pass proves syntax only.
- `V9-FIXTURE-FAKE-001` (high): Fixture emits synthetic IDs/digests or invalid provider URL; no real verifier or REST/artifact/attestation lineage is executed.
- `V9-IMPL-001` (critical): No typed Main-B verifier/publisher source exists at the fresh live main commit; v9 positive/hostile checks are contract-only and cannot be called execution success.

## Check statuses

- `pass` `frozen-plan-hashes`
- `pass` `owner-result-hashes`
- `pass` `canonical-root-no-self-digest`
- `pass` `hostile-fixture-hash`
- `pass` `root-bound-file-hashes`
- `pass` `schema-files-and-plan-hashes`
- `pass` `plan-canonical-schema-hashes`
- `pass` `146-permanent-and-124-record-leaves`
- `pass` `canonical-leaf-metadata`
- `pass` `field-availability-and-needs-DAG`
- `pass` `unique-field-provenance`
- `pass` `S7a-S7b-transport-partition`
- `fail` `preimage-disjointness-and-field-availability`
- `pass` `S4-S6-preimage-boundaries`
- `fail` `cross-schema-product-identity`
- `pass` `strict-positive-schema-validation`
- `pass` `positive-cross-file-root-and-schema-hashes`
- `pass` `actionlint-fixture-syntax`
- `fail` `fixture-output-lineage-coverage`
- `pass` `independent-negative-fixture-contract`
- `pass` `fresh-live-main-tuple`
- `unimplemented` `fresh-live-typed-verifier-implementation`

## Independent hostile contract fixtures

All listed cases are expected `reject`; none was sent to a live verifier.

- `terminal-census-own-id` — include terminal_census_id in S7_terminal_census ordered_fields (own identity cannot be hashed before allocation).
- `terminal-census-overlap` — keep terminal_census_digest in both ordered_fields and excluded_fields (exact ordered preimage and exclusion contradict).
- `provider-result-overlap` — keep provider_result_id/digest in both ordered_fields and excluded_fields (exact ordered preimage and exclusion contradict).
- `release-schema-unmapped` — omit release_manifest from canonical_leaf_paths (all current schema leaves need typed lineage).
- `release-identity-mismatch` — compose old permanent product with new release-manifest product (cross-schema identity must be equal).
- `schema-root-hash-pattern-only` — replace canonical_root_manifest_sha256 with zeros (verifier must compare measured root digest).
- `s7b-ambiguous-producer` — use one slash-joined producer for external-provider-result and verify-B fields (each field requires one concrete producer).
- `fixture-output-subset` — execute actionlint fixture as typed implementation (declared 146-leaf transport has missing job/workflow outputs).
