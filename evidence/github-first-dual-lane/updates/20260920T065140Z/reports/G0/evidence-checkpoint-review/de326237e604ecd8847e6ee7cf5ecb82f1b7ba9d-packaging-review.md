# Independent packaging-only review: `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`

Review date: 2026-09-20. Read-only. No source checkout, runtime, Docker, host,
install, build, dispatch, release, publication, remote mutation, or repository
authority action was performed. The candidate was audited from immutable Git
objects; it was not checked out over the observed worktree.

## Verdict

Append-only structure: **PASS**. Byte-exact package integrity: **FAIL**.

The candidate adds one update path, but the committed package does not preserve
105 raw response files byte-for-byte. Their recorded CRLF source bytes were
stored as LF-only Git blobs, removing 2,860 carriage-return bytes. The same 105
entries in `SHA256SUMS` therefore fail against the committed package blobs.
This is a packaging failure, not a source or authority approval.

## Candidate and worktree boundary

- Evidence repository: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`.
- Candidate: `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`; parent:
  `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`.
- Both local and local `origin` tracking refs for
  `evidence/github-first-dual-lane-20260919T181508Z` resolve to the candidate.
  The configured remote is `git@github.com:tailrocks/velnor.git`; no fetch or
  other network operation was used.
- The evidence worktree was on `codex/github-first-records` at
  `23942744a8da521f72d8cf83b7c2d6820e384664`, with observed modifications in
  `docs/ci/github-first-dual-lane/PLAN.md` and `STATUS.md`; those files were
  not touched. Candidate objects were read directly.
- Candidate and parent both carry `Signed-off-by: Alexey Zhokhov
  <alexey@zhokhov.com>` and `Co-authored-by: Codex <codex@openai.com>`.

## Append-only and tree checks

- Parent-to-candidate diff: **336 additions**, all under
  `evidence/github-first-dual-lane/updates/20260920T062845Z/`; no deletion,
  modification, rename, or path escape.
- Tree: **336/336** regular `100644` blobs: 332 selected source files and
  four controls (`INVENTORY.tsv`, `SHA256SUMS`, `checkpoint.json`,
  `checkpoint.md`).
- `git diff --check` exits 2 with 163 output lines corresponding to preserved
  trailing-whitespace/blank-EOF diagnostics in copied Markdown/API-response
  bytes. This is consistent with
  raw-byte preservation intent, but does not prove preservation; the hashes
  below disprove it for the 105 response blobs.

## Manifest and source-byte reconciliation

- `INVENTORY.tsv`: 332 rows; declared source bytes **3,615,060**, matching the
  checkpoint declaration. The committed source blobs total **3,612,200** bytes
  (delta **2,860**).
- All 332 current external source files match their inventory size/hash and
  recorded integer mtime. The destination comparison fails exactly for the
  105 `.response` files in
  `reports/G0/fleet/skills-raw-20260920T061548Z/raw/`.
- Each affected source has CRLF lines while its package blob has the same LF
  line count without CR bytes. Example:
  `00001-auth-user.response` source/inventory is 2,822 bytes with SHA-256
  `beb76e4ec10f7d15c7046b78c0bab864166809d91be1e1797d41d9b5b706477a`, while
  the committed destination is 2,794 bytes with SHA-256
  `e20d80d3d79e3beedfa6bd197fef358126c192dfd4bfa38253803118dc9a601a`.
- The corrected skills partition has the claimed **281 files**, but committed
  bytes are **2,342,147**, not the declared **2,345,007**; the same 2,860-byte
  newline delta accounts for the difference.
- `SHA256SUMS` has 335 entries for all package blobs except itself; no missing
  or extra entry exists, but **105 entries fail** against the committed bytes,
  exactly the affected response files. The manifest is therefore not a valid
  checksum closure for the Git package.
- All 74 package `.json` files parse (73 selected JSON files plus
  `checkpoint.json`); all JSONL parses succeed with 153 records. Secret-pattern
  scan found 0 matches. No package symlinks were selected or dereferenced.

## Cutoff and v11 closure

- Checkpoint declares `new_cutoff_utc=2026-09-20T06:28:45Z`; the requested
  millisecond boundary `06:28:45.332Z` is not retained in package metadata.
  No selected source mtime is newer than the second-precision cutoff; maximum
  is `2026-09-20T06:27:22Z`. `captured_at_utc=06:35:20Z` is later package
  assembly metadata, not evidence observation time.
- Independent recomputation confirms the 20 bound v11 files, zero bound-file
  mismatches, raw root-manifest SHA-256
  `35f8ae8a20cfe3ce774ed0d63ece8ed06d3e66d4e66693933ea5e5825e32a96d`, and
  canonical root digest
  `deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017`.
- Result summaries remain separate: owner **59/59**, earlier independent
  **19/19**, Luna **24/38** with 9 failed and 5 unimplemented. Authority flags
  are false. Checkpoint status is `proposal_only_external_blocked`,
  `attestation=none`, and `gate_status=not-evaluated`.

## Disposition

The update is append-only and structurally well-formed, but it is **not
packaging-integrity PASS**. Repackage the selected raw response bytes without
newline conversion and regenerate the inventory/checksum controls before any
later packaging review. No repair, gate, attestation, approval, or source edit
was made here.
