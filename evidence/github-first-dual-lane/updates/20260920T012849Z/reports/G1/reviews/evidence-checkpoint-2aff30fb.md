# Independent review: external evidence checkpoint `2aff30f`

Status: **PASS for checkpoint packaging integrity only**. This is not a
G0--G7 gate approval. The checkpoint declares `attestation: none`,
`gate_status: not-evaluated`, and explicitly says that it establishes no gate
success.

## Exact object and provenance

- Git checkpoint: `2aff30fb95c1227d50cd22d2f39b38eb813ce802`
- Parent and declared prior checkpoint: `c14dffabe5e5cfabea6f3e8978c6047e8e3e7014`
- Ref: `refs/heads/evidence/github-first-dual-lane-20260919T181508Z`
- Update: `evidence/github-first-dual-lane/updates/20260919T234645Z/`
- Cutoff/capture: `2026-09-19T23:51:00Z`
- Prior cutoff: `2026-09-19T22:33:18Z`
- Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

The detached review worktree was clean at the exact checkpoint. An
independent `git ls-remote` query returned the exact reviewed SHA for the
declared evidence ref. The checkpoint's `remote_before_copy` and parent both
match `c14dff...`; the append-only update then advances the ref to `2aff30...`.

## Independent checks

All checkpoint checks below were read-only in detached worktree
`/private/tmp/evidence-checkpoint-2aff`; no source or checkpoint branch was
written. This review report is the only external report file created.

- The checkpoint commit has 24 regular files and no symlinks. Every changed
  path is below this update directory; no source path is in the commit delta.
- `SHA256SUMS` passed for all 23 listed files (20 selected source files plus
  `INVENTORY.tsv`, `checkpoint.json`, and `checkpoint.md`); its self-exclusion
  is consistent with the declared scope.
- `INVENTORY.tsv` has 20 source records totaling 524,664 bytes. Independent
  source-relative byte/SHA-256 comparison found 20/20 matches. All selected
  source mtimes precede the cutoff; the latest is `2026-09-19T23:47:24Z`.
  The recorded before/after/final source snapshots also agree and report no
  source mutation during copy.
- All 10 selected source JSON files and the control `checkpoint.json` parse;
  no selected NDJSON exists. No selected symlink, log, stdout, or stderr was
  found. The selected inventory contains none of the excluded mutable/live,
  build, cache, credential, binary, or clone paths.
- The exact included b61 source review has SHA-256
  `b84aab36db74aac24cfed6c842ddb0d41e0fec79553e307de113bf7e4857af23`,
  matching its inventory record.

These checks establish durable copy/inventory integrity. They do not establish
that excluded files were absent from the source; the exclusion list is an
explicit scope boundary.

## Attestation, gate, and substantive evidence limits

The package contains no cryptographic/provider-backed attestation. The SHA-256
manifests provide file-integrity checks only. `checkpoint.json` records
`attestation=none`, `gate_status=not-evaluated`, and the explicit no-G0--G7
statement. The included b61 report is itself a **REJECT** as a G1 source
checkpoint, with four P0 defects and no hosted canary, hostile binary probe,
or Docker execution. The authority-transition review remains hard-blocked and
approves no authority change.

Mutable session/active/live/current/monitor/push snapshots, raw logs/streams,
research clones, targets, binaries, caches, temporary trees, credentials, and
unproven reports remain intentionally excluded. Therefore this checkpoint must
not be used as publication, installation, release, hosted acceptance, source
approval, or any G0--G7 success claim.

## Verdict

**Approve the exact checkpoint as an append-only, internally durable evidence
package.** Keep all substantive gates and hosted/release claims blocked; no
attestation or gate approval is present.
