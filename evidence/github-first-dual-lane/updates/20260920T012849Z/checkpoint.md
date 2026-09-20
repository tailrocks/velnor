# External evidence checkpoint update

Update: `20260920T012849Z`  
Captured/new cutoff: `2026-09-20T01:28:49Z` UTC  
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  
Parent checkpoint: `2aff30fb95c1227d50cd22d2f39b38eb813ce802`  
Prior cutoff: `2026-09-19T23:51:00Z` UTC  
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only update adds 63 stable regular source entries totaling 1,335,680 bytes. It retains the frozen authority-plan v2–v5 chain, canonical authority-contract-separation bundle, corrected workload/index artifacts, and finalized checker/store/native/scan/bootstrap and independent reviews. `ca18d011` is included as requested.

## Integrity and limits

- Source hashes matched before copy, after copy, and at final reread; destination hashes match source.
- JSON files parsed: 21. NDJSON lines: 0. Secret-pattern hits: 0.
- No source checkout, branch, workflow, dispatch, release, merge, ruleset, package publication, credential, host, or binary state changed.
- No symlinks selected or dereferenced.
- `attestation=none`, `gate_status=not-evaluated`; no G0–G7 success claim.

## Boundary

Finalized reports/plans/contracts only. Rejected findings remain evidence, never approval. Live/current/monitor/push snapshots, session state, raw logs, generated harness trees, builds, targets, binaries, caches, clones, credentials, and mutable ongoing files remain excluded.

`INVENTORY.tsv` records destination, source-relative path, byte count, source SHA-256, source mtime UTC, and kind. `SHA256SUMS` covers every regular file except itself. Prior updates remain unchanged.
