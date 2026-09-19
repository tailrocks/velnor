# External evidence checkpoint update

Update: `20260919T223252Z`
Captured and new cutoff: `2026-09-19T22:33:18Z` UTC
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`
Parent checkpoint: `eda33aee9d7185052ea3f359b5b3755d7115a0a6`
Prior cutoff: `2026-09-19T21:21:11Z` UTC
Source base context: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only update adds 121 stable source entries: 118 regular files and
three preserved symlink fixture entries, totaling 21,461,729 source bytes.
The selection includes the requested exact 5552cb8, b2546b46, 0712a549,
ce894a81, 4f68c47d, and PR960/de70b482 review evidence, stable transition and
XCFramework handoff reports, and the completed ce894a81 public CLI
harness/fixture set. The earlier `2026-09-19T19:24:28Z` cutoff is retained
only in the older checkpoint; it is not repeated as this update's cutoff.

The source root was read-only. A source lstat/size/SHA snapshot before copy
matched the post-copy snapshot for every entry, and all regular destination
hashes matched their source. No source checkout, workflow, dispatch, release,
merge, package publication, or ruleset changed.

Publication owner is `/root`; records owner is `/root/g0_records`. This
package is evidence only: `attestation=none`, `gate_status=not-evaluated`.
It does not establish G0-G7 success.

## Included

- Exact bootstrap policy review `5552cb8` and hostile-producer fixture review
  `b2546b46`.
- Exact schema-2 APT handoff `0712a549`, including its independent review.
- Exact checker `ce894a81` review, harness scripts/results, serialized
  adversarial fixtures, CAS objects, and preserved symlink fixture entries.
- Exact action scanner review `4f68c47d`.
- PR960 hosted-provider review at exact target `de70b482`, plus the stable
  XCFramework/product handoff review and G1 bootstrap transition report.

## Validation

- 66 JSON/manifest files parsed with `jq -e .`.
- No NDJSON files were selected in this update.
- Refined secret-pattern scan: 0 hits.
- `INVENTORY.tsv` records destination, exact source path, bytes, source mtime
  UTC, kind, and source SHA-256 or exact symlink target for all 121 entries.
- `SHA256SUMS` covers every regular update file except itself; symlinks are
  not dereferenced.
- Prior updates remain unchanged. No source branch was written.

## Excluded

Mutable `session.json`, the active `G0/hosted-config/report.md`, other
current/monitor/live/push-named state, raw STDERR/STDOUT/log outputs, research
clones, build targets, binaries, caches, temporary trees, credentials, and
non-requested reports not proven stable in this bounded capture remain
excluded.

This update is not a source release, attestation, or gate result. Earlier
checkpoint files remain preserved in their prior update directories.
