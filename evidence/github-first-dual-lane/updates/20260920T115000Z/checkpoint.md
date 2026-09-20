# Stable external evidence checkpoint 20260920T115000Z

- Capture cutoff: **2026-09-20T11:50:00Z** UTC. This cutoff is fixed before
  later live-writer activity. No source or remote mutation occurred.
- Parent checkpoint: `31cdf59d45cc975ebf7244d1e1d111974f2f62b1`, preserved
  unchanged.
- Included: **158** regular, non-symlink files, **2,235,918** source bytes.
  G0: **146 files / 2,128,457 bytes**. G1: **12 files / 107,461 bytes**.
  Exact source paths, destination paths, sizes, source mtimes, and SHA-256
  values are in `INVENTORY.tsv` (SHA-256
  `62684adc16af51efd6ab68fb9d2c20fb05efe8abeb6a27d9b3fa1e6b7fb9472c`).
- Stability: every selected source file was regular and non-symlink; the
  source `(size, mtime, SHA-256)` snapshot matched on reads bracketing copy;
  destination bytes matched the inventory.

## Included records

- **Recovered cross-job independent review**
  `G1/reviews/bootstrap-artifact-binding-repair-final-independent-review-20260920.md`
  has SHA-256 `8fd6a3d0da3e9715cc31d4b43a915ba7ef900c56a42dc64c66780935fe824d07`.
  Recovery provenance was independently checked: original path
  `/Users/donbeave/Projects/tailrocks/dual-lane-evidence/G1/reviews/bootstrap-artifact-binding-repair-final-independent-review-20260920.md`
  and canonical path are byte-identical, both 15,522 bytes, same SHA. The
  canonical report remains an unapproved design review.
- **Corrected hosted runner matrix**: complete immutable
  `G0/hosted-config/runner-matrix-20260920T112021Z-successor/` (139 files,
  2,089,501 bytes; `manifest.sha256` SHA-256
  `eaeaf361842c3317218193f06888f082be7c939e02f8392bd3cbfafd913c324a`) plus
  independent successor review
  `G0/distribution-review/hosted-runner-matrix-20260920T112021Z-successor-review.md`
  (SHA-256 `8f96159e1b6026921be0ba29bf21178bdf925969cede8e2b3409024fab573e60`).
  Source manifest verification was clean. The predecessor review is retained
  separately for chronology; no entitlement, capacity, routing, or runtime
  claim follows.
- **Workflow inventory review**
  `G0/distribution-review/workflow-source-inventory-20260920T102146Z-review.md`
  (SHA-256 `48f14c76280fc9bb5c7620a09f89a74396f5cbebeeb2beda7fc59b5977810bc7`)
  independently checks the prior immutable inventory. It remains source-only,
  not G0/G2 or publication approval.
- **Native signer scope correction**: original independent report
  `G1/reviews/native-signer-contract-61cb7fdf-independent.md`
  (SHA-256 `642ba39bb48dad69ee6b5f8275ef1bed6fbf0cf8818d74dd6e1a581beedd3141`)
  and separate scope correction
  `...-independent-scope-correction.md`
  (SHA-256 `5ca0698841a992313c3c670a20f4d534fa5fe446508d19050700d0e5d7f03f9c`).
  Original bytes are preserved; correction is not an in-place supersession.
  Both reject generated caller/DAG approval.
- **Native reconciliation**
  `G1/reviews/native-id-9908296d-reconciliation.md` (SHA-256
  `d31052a65c0d3326aaefa1616755fe43f20b42a0dc61c87bff6e2a5f2b85db4d`)
  records the source-renderer versus stale checked-in generated-runtime
  mismatch. It grants no publication or provider approval.
- **Homebrew successor**: `G1/reviews/homebrew-86bc621-independent.md`
  (SHA-256 `c759150d4ae9b8db88d6b73a6f760dcf0e4665ab4f40afddd5a4452245675a76`)
  and predecessor `homebrew-257e7fc-independent.md` (SHA-256
  `3407be839d8982bc7673add926592a42d9e99fa899062b2b5a6e57658779225e`).
  Successor remains blocked for strict audit isolation; no install,
  publication, dispatch, or G2 approval.
- **Bootstrap prefetch prototype**: proposal
  `G1/bootstrap/bootstrap-prefetch-prototype-proposal-20260920.md` (SHA-256
  `33fbfa6d62012fcf3bbb242f85f446a0f6c15d497d319e6c67ba141d3fa54f23`) and
  independent review of exact `b24989d3`
  `bootstrap-prefetch-prototype-review-b24989d3.md` (SHA-256
  `3d87c13f1f339f2dc19ef8785c676dec6533dd986fa0ca5c19d1f3935d9435a4`).
  Review disposition is changes required; no image/network/bootstrap
  acceptance evidence.
- **Isolation incident/design chain**: independent run-isolation follow-up
  review `G1/reviews/bootstrap-artifact-binding-run-isolation-followup-independent-review-20260920.md`
  (SHA-256 `ecbf65c91d44ce6abcb0bc1053acc35d81653eb97105e317684c112c902692a7`)
  and canary-validity contract (SHA-256
  `1946681a5ed28d81b8721e913a5b9e1faab537f1d8a607acb24c95f9c88e737b`) are
  read-only design evidence. No live canary was authorized or run.
- **Bootstrap transport/source reviews**: `G1/bootstrap/source-review-80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0.md`
  (SHA-256 `a17ceb67f4d44c7e9d116724620c9e6e49e46b406228b8ae280bcf3c8d39970b`)
  remains changes-required; `G1/integration/review-7df0481e3f38f9f662d25cd963c99d24bc32357b.md`
  (SHA-256 `bab4d4d23a3599cec5d0144e7f2d9bc29884d03467c08b002bb89198c54eed44`)
  remains changes-required for source→generated fixed point.
- **Current completed bounded reports**: APT consumer review
  `G0/distribution-review/apt-consumer-4cb7490-exact-review.md` (SHA-256
  `b79cdcbc28017d1cce09a340f9c29c3fa54ea2691655d72acc4fcd08e4af8766`),
  prior and successor runner reviews (`23ec799d7577e92992b0aff003ffafa4567325c4caea0fd0da0512310caef324`
  and `8f96159e1b6026921be0ba29bf21178bdf925969cede8e2b3409024fab573e60`),
  checker review `efdccb5d12a075c8adebd24e6da4ae895aab56e4-g3-checker-review.md` (SHA-256
  `82ccff4164185c69536b99bfbaa5422c178cd588fcffe19688cbb5146cdf6738`),
  docs review `review-409668e2a0f988938ed507455b77d91bea5cd00f.md` (SHA-256
  `376a949ee591873b84f6efddf3e1241c1fe4dd21943276eccc70b629ad063d2b`),
  and prior packaging review `31cdf59d45cc975ebf7244d1e1d111974f2f62b1-packaging-review.md` (SHA-256
  `6af6031067a13db28b3a3e7c965a0542bad57e6f9c1e8c646bbfdc2635e9f296`) are
  preserved as bounded independent evidence only.

## Excluded

- Active/mutable collector and live roots, including
  `G0/live-collector-20260920T095716Z-05e`,
  `G0/live-collector-20260920T101603Z-8d3`,
  `G0/live-collector-20260920T103455Z-413`, both external-action-gap roots,
  and mutable `G0/hosted-config/report.md`.
- `G1/integration/source-integration-merge-checkpoint-20260920.json` was
  previously frozen at SHA-256
  `4bc3ecd88d587d9f5e83be6d5fbf0b82bd4cffb0db7b63680a0a2100962c9e6d`, but
  the source path now differs (`9109595eae5c46254413407f9f399e8c67f8168bda982d85cdcd0363719edcd2`);
  current bytes are excluded rather than silently superseding the old packet.
- Moving owner ledgers/WIP, raw logs outside the bounded runner capture,
  builds/targets/clones/source archives, credentials, and secrets.
- Prior checkpoint packets remain untouched.

## Integrity and claim boundary

- Runner successor `manifest.sha256` passed source verification. Destination
  inventory and SHA-256 manifest are generated mechanically after copy.
- JSON/NDJSON validation, high-confidence secret scan, fresh committed-object
  readback, and raw-byte attribute verification are required before push.
- No source edit, workflow dispatch, ruleset/check mutation, authority change,
  merge, release/publication, install, or gate decision occurred.
- Attestation: **none**. Gate status: **not-evaluated**.
