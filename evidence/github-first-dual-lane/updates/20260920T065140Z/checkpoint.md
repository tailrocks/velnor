# Corrected raw-byte evidence checkpoint 20260920T065140Z

- Correction cutoff: **2026-09-20T06:52:33Z** UTC.
- Parent: `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d` (`de326`), preserved unchanged as an integrity-failed historical checkpoint.
- Prior evidence cutoff: **2026-09-20T06:28:45Z** UTC.
- Independent failure review: `G0/evidence-checkpoint-review/de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d-packaging-review.md`; SHA-256 `919fafed1586e2ccfec9fa1a186a156674e764ec5a1a1790b3ed3ce03e7773b7`.
- Included: **282** regular files, **2350246** source bytes.

## Explicit supersession

`de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d` is **not packaging-integrity PASS**. Its append-only structure was valid, but Git text conversion changed all 105 raw `.response` blobs, losing 2,860 CR bytes and invalidating 105 checksum rows. This correction does not rewrite or delete de326.

The corrected raw partition is appended under `reports/G0/fleet/skills-raw-20260920T061548Z/`. Exact source bytes, source SHA-256 values, and post-copy destination hashes reconcile for all 281 files, including all 105 `.response` files.

Root `.gitattributes` now scopes `binary -filter` to raw artifacts and `-text -filter` to normalized/snapshot/manifest identity artifacts. No source, plan, status, authority, dispatch, release, merge, or gate state changed. Attestation: **none**. Gate status: **not-evaluated**.
