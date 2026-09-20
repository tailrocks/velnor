# Independent correction packaging review: `1101eec44291c59992ead80a14c400bfb1bc7221`

Review date: 2026-09-20. Read-only. No source checkout mutation, runtime,
Docker, host, install, build, dispatch, release, publication, remote mutation,
or authority action was performed. A temporary local clone was used only for a
fresh checkout and checksum readback, then removed.

## Verdict

Correction-scope packaging integrity: **PASS**.

This is an append-only raw-byte correction. It preserves the prior
`de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d` checkpoint as immutable historical
evidence and adds the independent failure report plus the complete corrected
281-file skills partition. It does not convert the prior full package into a
pass and is not a gate, attestation, approval, or source admission.

## Candidate, parent, and refs

- Evidence repository: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`.
- Candidate: `1101eec44291c59992ead80a14c400bfb1bc7221`; exact parent:
  `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`.
- Local and local `origin` tracking refs for
  `evidence/github-first-dual-lane-20260919T181508Z` both resolve to the
  candidate. Configured remote is `git@github.com:tailrocks/velnor.git`; no
  network operation was used.
- Observed shared worktree: branch `codex/github-first-records`, HEAD
  `b1649ef90a27655af37ecf5119a34b6331f47973`, clean. Candidate was audited
  through immutable objects and a temporary clone; the shared worktree was not
  changed.
- Candidate and parent both carry `Signed-off-by: Alexey Zhokhov
  <alexey@zhokhov.com>` and `Co-authored-by: Codex <codex@openai.com>`.

## Append-only and historical-failure preservation

- Parent-to-candidate diff: **287 additions**: root `.gitattributes` plus 286
  files under `updates/20260920T065140Z/`. No modification, deletion, rename,
  path escape, prior `20260920T062845Z` update change, `PLAN.md` change, or
  `STATUS.md` change.
- The prior de326 update remains unchanged. Its independent failure report is
  copied by hash and explicitly referenced by the correction checkpoint:
  `G0/evidence-checkpoint-review/de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d-packaging-review.md`,
  SHA-256
  `919fafed1586e2ccfec9fa1a186a156674e764ec5a1a1790b3ed3ce03e7773b7`.
  The preserved failure is the 105 LF-normalized response blobs and lost
  2,860 CR bytes; the correction does not rewrite that evidence.

## Corrected source and Git-blob bytes

- Correction update tree: **286/286** regular `100644` blobs: 282 selected
  source files plus `INVENTORY.tsv`, `SHA256SUMS`, `checkpoint.json`, and
  `checkpoint.md`.
- `INVENTORY.tsv`: 282 rows and **2,350,246** declared source bytes. Every
  external source path matched its recorded size, mtime, and SHA-256; every
  committed destination blob matched the same bytes and SHA-256. Mismatches:
  **0**.
- Corrected skills partition: **281 files / 2,345,007 bytes**. All 105 raw
  `.response` files match source bytes, including **2,860 CRLF carriage-return
  bytes**. The first corrected response is 2,822 bytes with 28 CRLFs and
  SHA-256 `beb76e4ec10f7d15c7046b78c0bab864166809d91be1e1797d41d9b5b706477a`.
- `SHA256SUMS` has **285** entries, exactly all update blobs except itself.
  Missing entries: 0. Extra entries: 0. Git-object rehash mismatches: 0.
  Manifest SHA-256: `83032a1c5e61c530397e760cc7c0cb1e83a7189827e4f5d093b5ca1dec65a210`.
- Fresh local clone detached at the candidate and checked with
  `sha256sum --check --strict SHA256SUMS`: **exit 0**, 285/285 lines passed.
  Fresh checkout raw bytes retained 2,822 bytes and 28 CRLFs.
- All 45 package JSON files parse; JSONL parsing succeeds for 153 records. No
  symlinks were selected or dereferenced. Secret-pattern scan: 0 matches.

## Attributes and filters

Candidate root `.gitattributes` SHA-256 is
`7c992be681a7ab156207d155094143bd76a953157a5db201f9e29d772e9bd940`.
Its scoped rules were verified with `git check-attr` in the fresh clone:

- raw paths: `binary=set`, `text=unset`, `filter=unset`, `diff=unset`;
- normalized, snapshot, manifest, and auth paths: `text=unset`,
  `filter=unset`.

The host has global LFS filter definitions and `core.autocrlf=input`, but the
scoped evidence paths explicitly disable filters/text conversion. The
committed bytes and fresh-checkout bytes confirm that this policy is active.

## Cutoff, failure pointer, and boundary

- Correction checkpoint JSON SHA-256:
  `de577c97147a87770550243695a7a79226cf7245c3087a52d2edf1a8175161e0`.
- Correction cutoff and capture time: `2026-09-20T06:52:33Z`. Maximum selected
  source mtime: `2026-09-20T06:48:38Z`; no selected source is newer than the
  correction cutoff.
- Checkpoint supersession points to parent de326, its exact prior update,
  independent failure report/hash, and corrected update. `attestation=none`;
  `gate_status=not-evaluated`. The gate statement explicitly establishes no
  G0-G7 success, authority, source admission, publication, or release.

## Disposition

**PASS for this correction scope only:** raw source bytes, committed blobs,
attributes, fresh checkout, and SHA manifest reconcile. **de326 remains an
immutable integrity-failed historical checkpoint.** No gate, attestation,
approval, source edit, commit, or push was made by this review.
