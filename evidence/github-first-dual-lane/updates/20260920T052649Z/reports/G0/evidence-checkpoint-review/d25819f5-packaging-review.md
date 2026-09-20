# Independent packaging-only review: `d25819f5b4a9f1e7724456693e9d8061182cdd1a`

Review date: 2026-09-20. Read-only. No source checkout, runtime, Docker, host, install, build, dispatch, release, publication, or repository mutation was performed.

## Verdict

Packaging integrity: **PASS**.

This is an append-only, hash-consistent historical evidence snapshot. It is **not** a G0--G7 approval, runtime result, release attestation, or current-source assertion. The candidate commit is remote-equal and contains no non-additive changes. The retained fleet artifact is inventory-only and is bound to the final pass of its captured API reread, not to live state after the capture cutoff.

## Candidate and append-only proof

- Candidate: `d25819f5b4a9f1e7724456693e9d8061182cdd1a`; parent: `ec9fc3c9a000d99086089fb1a3f4f038460789f8`; tree: `5666089e8f036c8cd01e44355f108b2f53475474`.
- Remote `origin` is `git@github.com:tailrocks/velnor.git`; `refs/heads/evidence/github-first-dual-lane-20260919T181508Z` resolves to the candidate exactly.
- Parent-to-candidate diff has 93 paths, all `A`, all under `evidence/github-first-dual-lane/updates/20260920T041500Z/`; non-additions: 0; paths outside that update: 0.
- Git tree modes: 93/93 regular `100644` blobs; symlinks/special files: 0.

The immutable checkpoint identifies the same parent/ref and source-base observation (`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9) at `evidence/github-first-dual-lane/updates/20260920T041500Z/checkpoint.md:3-7` and `checkpoint.json:127-132,141`.

## Manifest and byte integrity

Evidence paths below are relative to the exact candidate tree unless noted.

- `INVENTORY.tsv`: 89 data rows, 5,000,169 summed source bytes, no malformed rows, duplicate source/destination paths, missing destinations, destination-size mismatches, or destination SHA-256 mismatches. Header and first rows are visible at `INVENTORY.tsv:1-5`; final row is `INVENTORY.tsv:90`.
- `SHA256SUMS`: 92 well-formed, unique entries. It covers every regular update file except itself: tree-minus-manifest is only `SHA256SUMS`; manifest-minus-tree is empty. Every listed content hash matches: 0 mismatches. Head/tail coverage is visible at `SHA256SUMS:1-8,85-92`.
- The update contains 93 files total: 89 selected evidence files plus `INVENTORY.tsv`, `checkpoint.json`, `checkpoint.md`, and `SHA256SUMS`. The checkpoint's G0/G1 split is 74/15 and sums to 89 (`checkpoint.json:16-21`).
- All 70 selected JSON files parse successfully. The extra JSON file in the 93-file tree is the packaging `checkpoint.json`; this reconciles the checkpoint's `JSON files: 70` statement.
- Secret-pattern scan over all 93 extracted files: 0 hits. No credential/private-key/token filenames. No symlinks were dereferenced; the checkpoint records the same policy at `checkpoint.json:5-12` and `checkpoint.md:26-28`.

## Retained raw-64 pages and final fleet binding

The retained API capture is internally complete and hash-bound:

- `reports/G0/fleet/live-default-branches-open-prs-20260920T035717Z-raw-pages/manifest.json` has 64 unique entries: 32 `initial`, 32 `final`, one page/index per each of 32 repositories, all `status=ok`, HTTP 200. Manifest structure and per-entry raw/normalized digests are shown at `manifest.json:1-28` and `manifest.json:870-898`.
- All 64 `raw_response_sha256` values match the retained page files: 0 mismatches; all manifest paths exist; all retained JSON bodies parse; manifest record counts equal actual array lengths. Initial and final record totals are both 87.
- Raw page bodies total 3,976,628 bytes. The raw manifest is 37,403 bytes; together the raw-page directory is 4,014,031 bytes, reconciling the fleet report's total at `reports/G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.md:22-25`.
- Final manifest repository set exactly equals the final fleet JSON repository set: 32/32. The final JSON declares exact 32-repository scope, `status=inventory_only_not_gate`, `gate_status=not_evaluated`, and 64 retained pages at `reports/G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json:1-16,85-90`.
- Final fleet checks: 32/32 complete rows; 32/32 initial and final metadata/ref reads; 32/32 initial and final PR reads; 87 initial and 87 final PR records; 0 errors; 0 branch/ref-SHA churn; 0 open-set/head/base/merge changes. The source report records these as inventory/reconciliation only at `...pr-reread-final.md:27-35,45-49`.
- Capture interval is `2026-09-20T03:57:17Z`--`04:00:39Z`; package cutoff is `2026-09-20T04:18:47Z`. All 64 raw-page timestamps and the final fleet end are before the cutoff. “Final” therefore means the final pass inside this historical capture, not current live state.

## Attestation and gate boundary

- `checkpoint.json:3-4,14-15` and `checkpoint.md:3-7,30-33` state `attestation: none`, `gate_status: not-evaluated`, and explicitly establish no G0--G7 success.
- Individual copied reports retain their own reject/block/conditional/pass dispositions; the checkpoint explicitly says no disposition is promoted to authority (`checkpoint.md:21-24`). The fleet JSON likewise remains `inventory_only_not_gate`.
- The commit has DCO trailers (`Co-authored-by: Codex <codex@openai.com>` and `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`), but `%G?` is `N`; neither trailer nor the Git commit is a cryptographic release attestation.

## Historical-source boundary

This review checked the immutable candidate tree, its remote ref, and the copied evidence. It did not re-fetch or compare the original external evidence paths against present filesystem contents. The package intentionally excludes moving/current/live ledgers and the moving integration map (`checkpoint.json:5-12`), so no claim is made about source or GitHub state after the recorded cutoff. No packaging defect was found; the remaining limitation is deliberate historical/inventory-only scope, not an external access failure.
