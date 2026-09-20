# Stable external evidence checkpoint 20260920T110300Z

- Capture cutoff: **2026-09-20T11:03:00Z** UTC. The cutoff is fixed before
  later writer activity. Package assembly used only source paths at or before
  this cutoff.
- Parent checkpoint: `264315ff5f6778d62d297ffb3f4df34e1ef27a8f`, preserved
  unchanged.
- Included: **87** regular, non-symlink files, **3,131,231** source bytes.
  G0: **66 files / 2,891,703 bytes**. G1: **21 files / 239,528 bytes**.
  Exact source paths, destination paths, sizes, source mtimes, and SHA-256
  values are in `INVENTORY.tsv` (SHA-256
  `3d8eda88a2a630a614ab89b881a3429ebdf3bfca7aa5d2443441cc281b3ea7b6`).
- Stability: every selected source file was a regular non-symlink; the source
  file list and `(size, mtime, SHA-256)` snapshot matched on reads bracketing
  copy, and destination bytes matched the source inventory.

## Included records

- **Actual immutable workflow inventory**
  `G0/workflow-source-inventory-20260920T102146Z/`: all 58 files, including
  the API body/tree objects, manifests, README, and owner handoff. Its source
  `SHA256SUMS` verified. The artifact explicitly says source-only/non-gate;
  review is **pending and not implied** by this package.
- **Canonical gap**
  `G0/canonical-mapping-gap-20260920T094508Z/`: all six files; source
  `SHA256SUMS` verified. It retains `actual_jobs: null`, unresolved joins,
  and no candidate/gate claim.
- Checker successor
  `G0/checker-v2-review/54ea2b09138a2218fee2aa0e9846def03ad9d675-successor-review.md`
  (`1724eafdaf08f31430d1db6503ab32d50702239f615adee5d990a8fe50b9c839`). Its
  approval is limited to the requested checker-test scope; it is not live
  collector, G0, authority, or gate approval.
- Native release-ID review
  `G1/reviews/native-id-9908296d-independent.md`
  (`b1ea37e593a2b487444b9245822f8f6e37669f6906e7decdcd9f81a3f6551887`). Its
  PASS is bounded to the exact source fix; publication is not approved.
- Bootstrap cross-job chain:
  `G1/reviews/bootstrap-artifact-cross-job-3ed0023b.md`
  (`fe4368a2394e696481a125e73d3b933ae29600b7412628ddbc37ede3c06abbc0`),
  independent review (`25759b7bf0e5633c967b3216f8e012693f38e68942563db0eef3f61240e8b2b7`),
  initial repair design (`834d3067be85f4261d180d013c5839edddf0465f128611e2a18defa19313803b`),
  final run-isolation decision (`5a59f867177de9e53275ff9fd4da43381a7ddea7aa262609a956f34fb4587fd0`),
  and the follow-up (`56bf72bca3ddf60503f567326c5a90399b562510130b784e75e0805e533e647f`).
  The follow-up records that requested input `8fd6a3d0da3e9715cc31d4b43a915ba7ef900c56a42dc64c66780935fe824d07`
  is absent from the shared source tree; it is not guessed or substituted.
  Designs remain unapproved; the original/independent reviews retain their
  changes-required boundaries.
- Hostile transport fixture review
  `G1/reviews/bootstrap-transport-hostile-fixtures-f2f1a3b0.md`
  (`4f6aed10d664730fcba212a3e2667ec29a9c01cc26410ff34291aefbcfb10850`) and
  independent owner review
  (`0c4d4b3b1f9c1773cd27d6dba61e5d7f07a17a518bb6cc349a838d55653d2095`).
  These are offline fixture evidence only; the cross-job case remains RED and
  no hosted/security approval follows.
- Bootstrap image evidence: inventory
  `G1/bootstrap/bootstrap-image-inventory-3ed0023b.md`
  (`5ccb0c5e62f39c03dd9a3543a6d32fbb111e8409446c9388e492c239e4099c6c`),
  restored original review
  (`6f5ce0d29e9bcfda5bc20115e9a4ab3ea687645031a8bfc9ddfde463eca277ce`),
  original proposal (`d08a0468066d2bf2331a6417f0c930806b5a384fa4de81a753b36f505c2aadb5`),
  v2 proposal (`2aa209303a86a8fc099bd9d4667345e56089ef18c1ceabeacc451602ba47e545`),
  and readiness review (`acdd5da1741ad81338c4f908c317256db7505508df569c75cb4f11a6279a6916`).
  The readiness review is **unapproved/not ready**; no image digest or
  publication claim is transferred.
- Source-integration/read-only bootstrap records:
  `G1/integration/source-integration-readiness-f6-vs-main-20260920.json`
  (`dcdfb99cf9a076ac9cf53c71ace73bae82ae144a7c4f9565e95c7fdf8224b09d`),
  `source-integration-merge-checkpoint-20260920.json`
  (`4bc3ecd88d587d9f5e83be6d5fbf0b82bd4cffb0db7b63680a0a2100962c9e6d`),
  and the `9e5c0eb` feasibility probe (`43b5013a44442dae787592e403f9bf5ae8e82f270c0464515876a7c8a65dfbe4` Markdown and
  `cbe034c98c66fb36a07e5b1ca7e38ca3f7d986308d7872fccf7ec17bc594b310` JSON).
  The merge record
  references source integration `7df0481e`; all records state no merge,
  generated-tree adoption, pin change, dispatch, or gate action.
- Executable 4fa/7df admission probe pair
  (`2780e3f22efdbd2692033792f4a3fe42b6647f6e0a3ba8ee281876864a420b23`,
  `c43041d5dd9b6cfe6c5718f85485d6aa1464a3e8675b3e2f4cddf5b4f789848b`) is
  included as read-only historical probe evidence. It records the old
  validator rejecting `xcode-27`; it is not candidate execution or approval.
- APT bounded source review
  `G1/reviews/apt-5ef7c9a-60a-independent.md`
  (`9cffec3fe928996aff330590df4f6b56f5100c4be1519ca8d43fc969a80201c0`):
  source-fix PASS only; publication/install approval is not granted.
- Documentation correction review
  `G0/docs-review/review-23b69394243bb430b4b261fec24fa0e94488e688.md`
  (`0fc47e550eb6a9d8072e28f98850eb93fcea3fb22488db4dc8ad444e2d75c6bf`):
  narrow documentation PASS only.
- Dependency delta
  `G1/dependency-graph-refresh-20260920T085949Z/delta-20260920T102957Z.json`
  (`5329357b91303d77d7445b5a0ec7bc448b0b00b2c684811490600d297ea7b3d3`):
  current-main baseline correction only; every gate remains not passed.

## Excluded

- Missing `8fd6a3d...` cross-job input report: absent at cutoff, excluded
  rather than invented.
- Active/mutable collector roots, always excluded:
  `G0/live-collector-20260920T095716Z-05e`,
  `G0/live-collector-20260920T101603Z-8d3`, and
  `G0/live-collector-20260920T103455Z-413`. No live writer/cache snapshot is
  included.
- Other moving owner ledgers/WIP, raw logs, builds/targets, clones, source
  archives, credentials, and secrets are excluded. No active root was treated
  as a terminal record.
- Prior frozen updates, including the parent checkpoint, are untouched.

## Integrity and claim boundary

- Source manifests for the workflow inventory and canonical gap passed their
  own SHA-256 checks before copy. JSON/NDJSON validation, destination/source
  reconciliation, and the high-confidence secret scan are required before
  push and recorded in the handoff.
- `.gitattributes` scopes `-text -filter` to this update's complete report
  tree. Fresh committed-object verification must recheck that rule and all
  manifests.
- No source edit, workflow dispatch, source admission, authority transition,
  merge, release/publication, install, or gate decision occurred.
- Attestation: **none**. Gate status: **not-evaluated**.
