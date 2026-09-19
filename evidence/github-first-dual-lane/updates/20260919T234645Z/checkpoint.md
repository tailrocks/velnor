# External evidence checkpoint update

Update: `20260919T234645Z`  
Captured/new cutoff: `2026-09-19T23:51:00Z` UTC  
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  
Parent checkpoint: `c14dffabe5e5cfabea6f3e8978c6047e8e3e7014`  
Prior cutoff: `2026-09-19T22:33:18Z` UTC  
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only update adds 20 stable regular source entries totaling
524,664 bytes. It includes the requested exact
`G1/bootstrap/source-review-b61f19e7.md`, three source-derived workload
contracts (fleet ten, distribution eight, and the remaining six), the
validator-only admission/design/preflight records, the validator-binding
adversarial review indexes/results, and independent exact-head PR961/PR963
reviews. The b61 report SHA-256 is captured in `INVENTORY.tsv`.

## Integrity and limits

- Initial source snapshot: `2026-09-19T23:48:46Z`; post-copy snapshot:
  `2026-09-19T23:49:39Z`; final reread/cutoff: `2026-09-19T23:51:00Z`.
- Source bytes/hashes matched before copy, after copy, and at final reread.
- JSON files parsed: 10. NDJSON: none. Refined secret-pattern hits: 0.
- No source checkout, source branch, workflow, dispatch, release, merge,
  ruleset, package publication, credential, host, or binary state changed.
- No symlink entries were selected; no symlink was dereferenced.
- `attestation=none`, `gate_status=not-evaluated`; this package establishes no
  G0–G7 success.

## Included

- Exact bootstrap source review `b61f19e7` (blocked source-checkpoint review;
  not G1 approval).
- Source-derived workload contracts for disjoint ten repositories,
  distribution consumers, and the remaining six repositories.
- Validator-only admission probe, design, protected-transition preflight,
  authority-transition review, and typed validator-binding adversarial report
  with JSON indexes/results.
- Independent exact-head PR961 and PR963 review reports.

## Excluded

Mutable `session.json`, active/live/current/monitor/push-named recovery
snapshots, raw logs and streams, research clones, build targets, binaries,
caches, temporary trees, credentials, and reports not proven finished in this
bounded post-cutoff selection remain excluded.

`INVENTORY.tsv` records each destination, source-relative path, byte count,
source SHA-256, source mtime UTC, and kind. `SHA256SUMS` covers every regular
file in this update except itself. Prior updates remain unchanged.
