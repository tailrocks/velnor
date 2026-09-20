# Independent packaging-only review: `339527872a63d5e36ed172fee267d500fdde9510`

Review date: 2026-09-20. Read-only. No source edit, source checkout
mutation, runtime/Docker/host/install execution, workflow dispatch, release,
publication, remote mutation, or authority action was performed. A fresh
temporary Git clone was used only for committed-object, attribute, and
checksum readback.

## Verdict

Bounded external-evidence packaging integrity: **PASS**.

The `20260920T085000Z` update is append-only and byte-closed for its declared
48-file source package and 28-file real-API corpus. The full G0 capture raw
tree is intentionally excluded; its inventory manifest is retained only as a
commitment to the excluded inventory, not as embedded raw-byte proof. This is
not a G0/G1 result, gate, attestation, source admission, authority transition,
release, or publication approval.

## Candidate, parent, and refs

- Candidate: `339527872a63d5e36ed172fee267d500fdde9510`; exact parent:
  `c391d96a3d1e3cc434f83c5481949f25bdbe49d6`.
- Local `origin/evidence/github-first-dual-lane-20260919T181508Z` resolves to
  the candidate exactly. `HEAD`/remote equality, merge-base equality, and
  bidirectional ancestry checks pass.
- Candidate subject is `evidence: append reviewed runtime and API records`,
  committed `2026-09-20T08:55:48Z`, after the evidence cutoff. It carries
  `Co-authored-by: Codex <codex@openai.com>` and
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`.
- Parent-to-candidate diff is 53 paths: 52 additions under exactly
  `evidence/github-first-dual-lane/updates/20260920T085000Z/` and one expected
  root `.gitattributes` modification. Deletions, renames, or prior-update
  modifications: zero.

## Inventory and committed bytes

- `INVENTORY.tsv` declares 48 regular non-symlink source files and
  3,034,649 source bytes: G0=39 files/2,964,223 bytes and G1=9
  files/70,426 bytes. Independent two-read source verification found
  48/48 rows stable; source size, SHA-256, mtime, and destination size/hash
  all match. Mismatches: **0**.
- The update tree has 52 regular `100644` blobs: those 48 destinations plus
  `INVENTORY.tsv`, `SHA256SUMS`, `checkpoint.json`, and `checkpoint.md`.
  Symlinks and other non-regular entries: **0**. No destination path is
  absolute or escapes with `..`.
- `SHA256SUMS` has exactly 51 rows. Its own SHA-256 is
  `fe734d300b66ef56343237a2670677b23949b11865af53fa9bada8d6598b4639`.
  `sha256sum --check --strict` passes **51/51** in the original detached
  checkout, the fresh clone, and the committed-object rehash.
- The fresh clone at the exact candidate is clean; `git fsck --full` passes.
  The root `.gitattributes` change adds scoped `-text -filter` rules only for
  the new real-API raw tree and G0 raw-manifest paths; prior rules remain.

## Real API raw-byte closure

- `manifest.json` declares exactly 28 raw files and 969,403 bytes. Independent
  manifest-to-tree comparison has zero missing and zero extra paths. Every
  raw file's manifest size/SHA-256 matches both its fresh-checkout bytes and
  `git cat-file` committed-object bytes: **28/28**, mismatches **0**.
- Fresh-clone `git check-attr` reports `text: unset` and `filter: unset` for a
  representative raw response and the captured G0 `raw-manifest.json`.
  This prevents checkout conversion from changing byte-sensitive evidence.
- The six JSON files in this update parse successfully; no JSONL/NDJSON file
  is present. High-confidence GitHub token, AWS key, bearer-token, and private
  key scan matches: **0**.

## Source cutoff and cited records

- Checkpoint source cutoff is `2026-09-20T08:50:00Z`; metadata capture is
  `08:53:31Z`. The newest selected source mtime is `08:49:20Z`; selected
  source paths newer than cutoff: **0**.
- The direct real-API corpus was captured at `08:03:18Z`. Included records'
  cited review cutoffs are before the source cutoff, including
  `2719c8cd...` at `08:44:48Z` and `907261d9...` at `08:48:43Z`. Their
  committed citation bytes are covered by the 51-row checksum manifest.
- The checkpoint's cited review/source hashes were rechecked through the
  inventory and fresh object clone. The package records remain bounded
  observations; cited reviews do not become gate evidence by inclusion.

## Deliberate full-G0-raw exclusion

- The checkpoint records the full G0 capture root as approximately
  **225,468 KiB / 7,037 files**, including approximately
  **115,952 KiB / 7,005 raw files**. It is intentionally not copied into this
  bounded update.
- Only `raw-manifest.json` is included for that capture: 1,931,936 bytes,
  SHA-256
  `6cc98c2886ad23a72bd17cf72f0fb5cea9038da8f01090579360affe62d53637`.
  Fresh-tree inspection finds zero G0 capture raw objects and one manifest
  object. Therefore this update proves packaging of the inventory commitment,
  not the omitted capture bodies, HTTP envelopes, stderr, or their raw-byte
  contents.

## Boundary

`checkpoint.json` records `attestation=none` and `gate_status=not-evaluated`.
No workflow dispatch, source admission, authority/ruleset transition,
release/publication, merge, installation, or gate mutation occurred. Exact
raw HTTP fixtures intentionally contain trailing whitespace; a generic
`git diff --check` reports those preserved source bytes, but this is not a
packaging defect because all source, object, checkout, and manifest hashes
reconcile exactly.

Final disposition: **PASS for this packaging scope only; omitted full G0 raw
is not claimed as embedded proof; no gate or attestation.**
