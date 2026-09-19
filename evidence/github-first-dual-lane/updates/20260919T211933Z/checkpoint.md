# External evidence checkpoint update

Update: `20260919T211933Z`  \
Captured: `2026-09-19T21:21:11Z` UTC  \
Evidence ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`  \
Parent checkpoint: `59779b08af7ae67d667379d39ba835dbbf3762f7`  \
Prior cutoff: `2026-09-19T19:24:28Z` UTC  \
Source base context: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

This append-only update adds 413 stable report/fixture files observed after
the prior cutoff. It was prepared in the existing evidence worktree after a
remote fetch; local and remote parent were both `59779b08...`. Source files
were read-only. No source checkout, workflow, dispatch, release, merge, or
package publication was changed.

Publication owner is `/root`; records owner is `/root/g0_records`. This
package is evidence only: `attestation=none`, `gate_status=not-evaluated`.
It does not establish G0-G7 success.

## Included

- G0 checker adversarial extension: final d9/b11 fixture inputs, exact b11
  copies, harness scripts, and the two final NDJSON result ledgers. The report
  preserves the false-green findings and explicitly blocks gate approval.
- G0 exact d9 G2 applicability fixture and case reports. STDERR logs remain
  excluded.
- G0 lane-compare reviews, fleet/check-context and dependency snapshots,
  records-contract review, checker/host/release reviews, estate-scope and
  scanner reviews, and other compact finalized implementation reviews.
- G1 bootstrap isolation/sandbox design reports, generated-recovery reports,
  hosted-provider reviews, scan-integrity reviews, and the two artifact
  feasibility/binding research reports.

Source observation was bounded: candidate files were regular report/fixture
files newer than the prior cutoff. A source size/mtime/SHA-256 snapshot was
taken before copy and rechecked after copy; all 413 source hashes, sizes, and
mtimes stayed equal, and every destination hash matched its source.

## Validation

- 360 JSON/manifest files parsed with `jq -e .`.
- 4 NDJSON files parsed; 78 nonempty lines were valid JSON.
- Refined secret-pattern scan: 0 hits.
- `INVENTORY.tsv` contains destination, source, bytes, and source SHA-256 for
  every copied file; `SHA256SUMS` covers every update file except itself.
- Prior updates were retained. No source branch was written.

## Excluded

The active `G0/hosted-config/report.md` changed during capture and was left
out. Mutable `session.json`, current/monitor/live/push/ledger-named state,
raw STDERR/STDOUT/log outputs, intermediate checker ledgers, clones, build
targets, binaries, caches, temporary trees, and credentials were excluded.
Only the two final checker NDJSON ledgers named in the source report were
selected from an `out/` directory.

The update is not a source release, attestation, or gate result. Earlier
checkpoint files remain preserved in their prior update directories.
