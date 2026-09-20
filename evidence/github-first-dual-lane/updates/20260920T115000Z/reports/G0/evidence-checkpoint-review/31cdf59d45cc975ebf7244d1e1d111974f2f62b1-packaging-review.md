# Independent packaging-only review: `31cdf59d45cc975ebf7244d1e1d111974f2f62b1`

Review date: 2026-09-20. Read-only. No source edit, source checkout
mutation, Docker/OrbStack/image operation, workflow dispatch, release,
publication, remote mutation, authority action, or gate action was performed.
A fresh temporary Git clone was used for committed-object, manifest, source,
attribute, JSON, and secret-scan readback.

## Verdict

Bounded external-evidence packaging integrity: **PASS**.

The `20260920T110300Z` update is append-only and byte-closed for its declared
87-file package. The package preserves the parent checkpoint and keeps all
source-only, pending, changes-required, and unapproved boundaries explicit.
This is not a G0/G1 result, gate, attestation, source admission, authority
transition, release, publication, or merge approval.

## Candidate, parent, and scope

- Candidate: `31cdf59d45cc975ebf7244d1e1d111974f2f62b1`; exact parent:
  `264315ff5f6778d62d297ffb3f4df34e1ef27a8f`.
- Local `origin/evidence/github-first-dual-lane-20260919T181508Z` resolves to
  the candidate exactly. Candidate and fresh clone are clean;
  `git fsck --full --no-reflogs` passes.
- Subject: `evidence: append stable 110300 checkpoint`; commit time
  `2026-09-20T11:11:49Z`. Trailers are present:
  `Co-authored-by: Codex <codex@openai.com>` and
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`.
- Parent-to-candidate diff is 92 paths: 91 additions under exactly
  `evidence/github-first-dual-lane/updates/20260920T110300Z/` and one expected
  `.gitattributes` modification. The prior `20260920T094700Z` update is
  byte-for-byte unchanged. No product/source files, deletions, or renames.

## Inventory and byte closure

- `INVENTORY.tsv` declares **87** regular, non-symlink source files and
  **3,131,231** source bytes: G0 = **66 files / 2,891,703 bytes**; G1 =
  **21 files / 239,528 bytes**. Inventory SHA-256:
  `3d8eda88a2a630a614ab89b881a3429ebdf3bfca7aa5d2443441cc281b3ea7b6`.
- Fresh current-source reread found 87/87 regular, non-symlink paths with
  matching inventory size, mtime, and SHA-256; mismatches: **0**. The newest
  selected source mtime is `2026-09-20T11:02:22Z`, before the fixed
  `2026-09-20T11:03:00Z` cutoff.
- The update tree has **91** regular `100644` blobs: the 87 destinations plus
  `INVENTORY.tsv`, `SHA256SUMS`, `checkpoint.json`, and `checkpoint.md`.
  Symlinks and other modes: **0**. Fresh checkout destination readback and
  committed `git cat-file` object readback both match all **87/87** inventory
  rows; mismatches: **0**.
- Top-level `SHA256SUMS` has **88** rows and SHA-256
  `f9ea86b4a0cb1d02f33deb2315b9e14d52a78a82587cac7dc6678689a45a03cc`.
  `sha256sum --check --strict` passes **88/88**; fresh committed-object
  rehash passes **88/88**. The two selected subordinate `SHA256SUMS` files
  are not duplicated in the top-level manifest: canonical-gap control has
  **5/5** rows passing and workflow-inventory control has **57/57** rows
  passing in the fresh clone. Thus the layered controls cover every
  inventory destination; no byte gap exists. This omission is recorded here
  rather than silently treating the top-level 88-row manifest as a complete
  one-level census.
- `.gitattributes` disables `text`/`filter` conversion for the complete
  report tree. Fresh `git check-attr` over all **87** report files shows
  `text: unset` and `filter: unset`, with zero deviations. No historical
  blank-EOF, newline, trailing-whitespace, or other source bytes were
  normalized; exact hashes remain the evidence.

## Included source records

- The workflow-source inventory contains **58** files including its own
  subordinate manifest; its manifest states 160 source objects, 155 workflow
  objects, 5 reusable-action objects, 44 API objects, and 116 local objects.
  Its source-only/non-gate boundary and pending review status are preserved.
  Its nested SHA manifest passes **57/57**.
- The canonical mapping-gap artifact contains **6** files including its
  subordinate manifest; its nested SHA manifest passes **5/5**. It retains
  `actual_jobs: null`, unresolved joins, and no candidate/gate claim.
- The package's separate bootstrap image records preserve the restored
  original review and v2 proposal as distinct bytes:

  | Record | SHA-256 | Boundary |
  | --- | --- | --- |
  | restored original image review | `6f5ce0d29e9bcfda5bc20115e9a4ab3ea687645031a8bfc9ddfde463eca277ce` | **CHANGES REQUIRED**, no image accepted |
  | original proposal | `d08a0468066d2bf2331a6417f0c930806b5a384fa4de81a753b36f505c2aadb5` | design evidence only |
  | separate proposal v2 | `2aa209303a86a8fc099bd9d4667345e56089ef18c1ceabeacc451602ba47e545` | separate successor bytes |
  | v2 readiness review | `acdd5da1741ad81338c4f908c317256db7505508df569c75cb4f11a6279a6916` | **NOT READY**, no digest/publication |

  The restored `6f5` review is not replaced by the v2 proposal or readiness
  review. No image digest, attestation, hosted canary, or G1 approval is
  inferred.

## Missing input and exclusions

- Requested input/report digest
  `8fd6a3d0da3e9715cc31d4b43a915ba7ef900c56a42dc64c66780935fe824d07` is
  absent from the source tree; the follow-up explicitly records that absence.
  It is excluded rather than invented or substituted. A separately named
  independent follow-up file exists in the source workspace with a different
  SHA-256 (`ecbf65c91d44ce6abcb0bc1053acc35d81653eb97105e317684c112c902692a7`);
  it is not the requested `8fd6` input and is not silently used as one. The
  missing digest may reflect an upstream path/hash typo; this package makes
  no inference.
- No active collector path is present in the package. The three mutable roots
  are explicitly excluded: `G0/live-collector-20260920T095716Z-05e`,
  `G0/live-collector-20260920T101603Z-8d3`, and
  `G0/live-collector-20260920T103455Z-413`. No live writer/cache snapshot is
  treated as terminal evidence.
- Moving owner ledgers/WIP, raw logs, builds/targets, clones/source archives,
  credentials, and secrets remain excluded. Prior frozen updates remain
  untouched.

## Parsing, secrets, cutoff, and claim boundary

- All **66** committed `.json`, `.jsonl`, and `.ndjson` files parse with
  `jq`; parse failures: **0**. High-confidence GitHub-token, AWS-key,
  bearer-token, and private-key scan matches: **0**.
- Checkpoint metadata states cutoff `2026-09-20T11:03:00Z`, source two-read
  equality, destination/source reconciliation, cutoff enforcement, zero
  selected symlinks, and all source/GitHub/dispatch/merge/release/authority
  mutations false.
- Attestation is **none** and gate status is **not-evaluated**. Included
  records remain source-only or bounded historical evidence; no current
  execution, image, publication, authority, or G1 claim follows from
  packaging.

Final disposition: **PASS for this packaging scope only**, with the layered
checksum-control note above. Exact source bytes, committed objects, nested
manifests, cutoff, exclusions, and claim boundaries are preserved.
