# Stable external evidence checkpoint 20260920T074000Z

- Capture cutoff: **2026-09-20T07:40:00Z** UTC. Capture metadata: **2026-09-20T07:48:10Z** UTC.
- Parent checkpoint: `1101eec44291c59992ead80a14c400bfb1bc7221`, preserved unchanged.
- Historical failed checkpoint: `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`, preserved unchanged. Its packaging-integrity failure remains authoritative historical evidence; it is not silently replaced.
- Included: **107** regular files, **3,401,260** source bytes; exact source/destination/hash/mtime rows are in `INVENTORY.tsv`.
- Groups: **G0 19 files**, **G1 88 files**.

## Stability and integrity

The selected source list was fixed at the cutoff. Every selected path was a regular non-symlink file at two source hash reads bracketing the copy. The complete source hash/size/mtime list matched between reads, and all 107 destination hashes and sizes matched the first source read. No source tree was edited.

This batch contains stable post-`1101eec` review reports, corrected source-contract inventories, the source integration queue, authority-transition repair/review material, and the terminal-census fixture suite. The v12 material remains design/fixture evidence only; its independent reviews report `NOT APPROVED`, and the terminal suite is synthetic evidence only.

## Preserved raw-byte correction chain

- `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d` remains the immutable packaging-integrity-failed predecessor.
- `1101eec44291c59992ead80a14c400bfb1bc7221` remains the immutable corrected raw-byte checkpoint. Its independent correction review is included here at `reports/G0/evidence-checkpoint-review/1101eec44291c59992ead80a14c400bfb1bc7221-packaging-review.md` (SHA-256 `f752d6b4134e553096d4b7e7e1a32b75912950857fe6fc2271ea11b606d4e38d`).
- Root `.gitattributes` from `1101eec` remains unchanged. It scopes `binary -filter` to raw response/body artifacts and `-text -filter` to normalized/snapshot/manifest identity artifacts. Fresh committed-object verification is required before publication review.

## Scope exclusions

Excluded deliberately: live `G0/g0-run-metadata-capture-*` trees and writer scripts; `G1/integration/execution-health-*.json` point captures; hosted-config/checker WIP; raw logs, raw `.response`/`.body` capture partitions, builds, source archives, repository clones and `.git` metadata; credentials and secrets; `__pycache__`; and paths created after the cutoff, including the v12 successor repair, source-review addendum, later Jackin/partition/APT/runtime reports. No moving file was copied.

The exact included set is the inventory, not a directory snapshot. No source admission, authority transition, workflow dispatch, release, publication, gate, or merge occurred.

Validation metadata: 73 JSON files and 1 NDJSON file parse; high-confidence secret-pattern scan finds no matches; source/destination hashes reconcile; and a fresh clone of the committed batch verifies the new `SHA256SUMS`, `INVENTORY.tsv` destination hashes, parent raw SHA256SUMS, and `.gitattributes` attributes. The final commit SHA is reported with the remote verification. Attestation: **none**. Gate status: **not-evaluated**.
