# Stable external evidence checkpoint 20260920T094700Z

- Capture cutoff: **2026-09-20T09:47:00Z** UTC. This cutoff is fixed before later live/WIP files; package assembly and final readback completed at approximately 2026-09-20T09:55Z UTC.
- Parent checkpoint: `339527872a63d5e36ed172fee267d500fdde9510`, preserved unchanged.
- Included: **134** regular non-symlink files, **1,665,184** source bytes. G0: **125 files / 1,619,850 bytes**. G1: **9 files / 45,334 bytes**. Exact source paths, sizes, source mtimes, and SHA-256 values are in `INVENTORY.tsv`.
- Stability: every selected source path was a regular non-symlink file; the complete source list and each `(size, mtime, SHA-256)` row matched on reads bracketing copy. Destination bytes matched the source inventory.

## Included reviewed records

- Previous checkpoint packaging review: `G0/evidence-checkpoint-review/339527872a63d5e36ed172fee267d500fdde9510-packaging-review.md`, SHA-256 `9ee9368085dd6648b36a0eb97ecac18d662031aa4be2c6782c682bdd76519a69`. It is packaging-only and does not approve G0/G1, authority, release, or publication.
- Documentation checkpoint review: `G0/records-contract-review/review-3e6126ddbfb9f52d8eaaf447c29c98256a62ca64.md`, SHA-256 `b010c42f2a9afa08dc9b9f285719caeccf65df86a3d141b938b4bc115579c5b4`. Its PASS is limited to the exact PLAN/STATUS documentation delta.
- Real API fixture predecessor and corrected successor: the original `G0/real-api-fixture-supplement-20260920T085311Z/` includes all **72** raw files plus README/manifest/relationship metadata; its independent review SHA-256 is `cc207ebf8ff0e0e43aeb1e7d520df773e7002016629e865f5af82f0c2a47da2d`. The corrected metadata-only successor `...085311Z-corrected-20260920T090934Z/` retains raw-source references and manifest SHA-256 `fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e`; its corrected independent review SHA-256 is `d4830c96fcf1b34f593a951ffbe424f847946d74bc46b11e4867b16abc30df6c`. The original raw bytes are not rewritten or substituted. Both reviews are read-only fixture evidence, not execution or gate authority.
- Checker successor review `G0/checker-v2-review/101aa26b849d507d6bdc702d55619bb830c65706-successor-review.md`, SHA-256 `19ebb74504a7ea30dc27466e3ae21130f9fe513b2e7c81041d4514e90df095c0`: changes required; collector/CAS remains unwired/fail-closed.
- Raw-store retention chain: `b50c50744c747d4e38d5b3a62c8850e4a95ca908...`, SHA-256 `3b6202c7063a4f027c96c30062d4b9298ad0ba2a57644b92c12bacb2d783ae73`, and clarified `6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0...`, SHA-256 `2f1baf4700fa165d4e8a4bc8de82fdafbd86c84368997148b761dabc6c4666f4`. The bounded refactor passes its scoped review; integration remains blocked by lint debt.
- APT and native bounded reviews: `G1/reviews/apt-3bfcd8a9-independent.md`, SHA-256 `220b9b98991061b939f2be17a65b0d56d02e6183c878b6980f54c6c7ad36c99e`, is provider-identity scoped and blocks full publication authority; `G1/reviews/native-647c6bc2cfc73ed2b93075bd822257f6709a1165-independent.md`, SHA-256 `d0df4743c4d3f0f74c9216d468cad3bf3ebbd398076fc13c08235f8e6c06eb80`, rejects publication due to preview census/generator and signer-boundary residuals. The APT producer discovery review `G0/distribution-review/apt-discovery-3b669d8-exact-review.md`, SHA-256 `d39272de8431e35a037f25d5ba96ba9ef2c207a5db8fca8e08866ad77de62413`, rejects complete producer/G2 approval.
- Draft regeneration records: original `G1/integration/draft-regeneration-f6.json` SHA-256 `dc8f01c18a8ab793bd32fe03cbcb935f2c0c9f607fe46d3d5be3ba62851b57c3`; corrected aggregate `draft-regeneration-f6-aggregate-correction.json` SHA-256 `05d50ce9ffa162a47a15516bbf5025813f569dac9380a291b59a53932866b5ef`; bounded reviews `review-disposable-render-f6...md` SHA-256 `bd0f0f16856243fb3212484bbfede5812e612a15771d8870337f0f63a334f522` and `review-draft-regeneration-f6-aggregate-correction.md` SHA-256 `d072b0fa00e85de9b922cbc768aaab622063f70a4fbea33951d1bc5e14791443` are retained with chronology and no authority claim.
- Current PR identity snapshot `G0/fleet/20260920T093002Z/` is included because the independent review arrived before cutoff: review SHA-256 `87714d0cd584e8f2aa816b7cda54fc2f6f6f2844b109c91dbee74732286681cb`; source `manifest.sha256` SHA-256 `27325132cc58d532fb597df659327f35dc97df798c4647781456135a950447b5`. It is bounded identity/freshness evidence only. The source manifest validates 40 API files; the directory has 41 files including the manifest. It makes no current-check, execution, merge, release, or gate claim.

## Missing and excluded

- Requested shorthand `sourcef64b203` had no matching regular file, filename, or content reference in the source evidence tree at cutoff; it is excluded rather than guessed. Requeue when the exact stable path/hash is supplied.
- Later or moving hosted/bootstrap/WIP files, active capture writers, huge captures, builds/targets/archives/clones, credentials/auth material, and raw logs outside the two bounded raw/API sets are excluded. The full current PR capture is bounded to the independently reviewed 40-file manifest plus manifest itself.
- Frozen prior updates and rejected/corrected predecessor versions remain untouched. No source edit, workflow dispatch, source admission, authority transition, release/publication, gate, merge, installation, or attestation occurred.

## Integrity controls

- `.gitattributes` scopes `-text -filter` to the copied fixture supplement raw tree and `G0/fleet/20260920T093002Z/**`; fresh committed-object verification must recheck these attributes and all manifests.
- JSON/NDJSON parsing, high-confidence secret scan, destination/source hash reconciliation, and fresh-clone verification are required before push.
- Attestation: **none**. Gate status: **not-evaluated**.
