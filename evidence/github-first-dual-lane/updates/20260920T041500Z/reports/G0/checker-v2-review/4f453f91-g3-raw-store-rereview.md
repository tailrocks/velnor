# Exact raw-store bounded-retention re-review — `4f453f91d38ed488f55f3e5f58262a6f9b1e0f52`

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: `/private/tmp/g3-raw-store`, branch `codex/g3-raw-store`  
Compared with: `0d7713cd15cefb554fda5954e2d3320968aa31e7`  
Boundary: read-only bounded-store review; no source, collector, host, Docker,
remote, or production-wiring edits.

## Verdict

**Reject for bounded retention and authoritative capture.** This commit
correctly removes duplicate retained payload copies: retention records now
point to descriptor-verified source qdirs, and replay checks source entry FD
identity, length, and digest. It also adds metadata/entry accounting and
strict unknown/debris rejection. The global quota is still not admitted before
all cleanup mutations: `.txn` payloads and public orphan candidates are not
included in the pre-mutation usage scan. They can be moved into qdirs, then
cause quota failure, leaving an over-budget state. The new store remains
test-only; the live collector still imports the legacy store. No authoritative
capture or dual-platform claim is approved.

## Exact verification

- `git rev-parse HEAD`: `4f453f91d38ed488f55f3e5f58262a6f9b1e0f52`.
- `git status --short --branch` was clean/remote-equal before tests. Test
  execution leaves only an untracked `.github-raw-store-fixtures/` directory;
  no committed/source files changed.
- `rtk cargo test --locked --all-features --package velnor-tools --test
  github_raw_store -- --nocapture`: **23 passed**.
- `rtk cargo test --locked --all-features --package velnor-tools --
  --nocapture`: **260 unit passed; integration 22 passed, 1 failed** in
  `cleanup_leaves_replaced_regular_temporary_name_instead_of_unlinking_it`.
  This is not a clean 283-test proof.
- `rtk cargo fmt --all -- --check`: passed.
- `rtk cargo clippy --locked --profile test --all-targets --all-features
  --package velnor-tools -- -D warnings`: passed.
- Independent scratch harness at
  `/private/tmp/raw-store-adversarial-0d` includes the exact source by path.
  Positive/hostile checks passed: same-content source-entry replacement is
  rejected on replay; a 120-MiB source qdir plus 8-MiB orphan is moved before
  quota rejection; and a max-sidecar `.txn` is moved before rejection.
- The max-sidecar `.txn` fixture uses `2*64 MiB + 4096 = 134221824` bytes.
  After failed reopen, `du -sk` measured **131076 KiB (134221824 bytes)**,
  above the `128 MiB` quota, with the transaction moved to
  `refs/.velnor-raw-quarantine-*/entry` and no completed retention record.
- Linux check remains unavailable before crate compilation:
  `openssl-sys` cannot find `x86_64-linux-gnu-gcc`. No Linux proof is made.

## Improvements over `0d7713cd`

1. Retained records are metadata-only pointers; source qdir bytes are no
   longer copied into the retention record. The owner disk test confirms
   `duplicate_entry_bytes == 0` (`tests/github_raw_store.rs:1257-1334`).
2. `RetentionUsage` now counts source qdir/temp payload lengths, fixed
   namespace/record overhead, unique source/record keys, and strict metadata;
   128-entry and 128-MiB logical limits are enforced (`src/github_raw_store.rs:
   1943-2070`).
3. Retained-source replay is descriptor-bound. `validate_manifest_entry` now
   compares the recorded source FD identity as well as byte length/digest
   (`:1752-1806`). Unknown generated names, malformed records, FIFO/symlink,
   and partial manifests fail closed without reclaiming operator entries.

## Remaining findings

### R1 — pre-mutation quota misses `.txn` and public orphan bytes (P0/P1)

`reconcile_namespace` calls `retention_usage` before scanning transaction
files (`:1031-1054`). That usage scan handles only retained records,
`.velnor-raw-...*.tmp`, and `.velnor-raw-quarantine-...` source dirs
(`:1943-2069`); a `raw_id.txn` is not counted. `quarantine_admission` reserves
only one fixed source overhead (`:1526-1538`), not the candidate's FD length.
The public-orphan path passes a max length but admits before the candidate is
renamed (`:1287-1327`); transaction recovery does the same at `:1057-1101`.

Independent exact-source fixtures demonstrate the mutation order:

1. A 120-MiB source qdir plus an 8-MiB unreferenced private object passes the
   initial usage check. Reopen returns an error only after moving the orphan
   into a new source qdir. `du -sk` is 131072 KiB and the moved candidate
   remains; no pre-mutation reservation rejected it.
2. A max-sidecar 134221824-byte invalid `.txn` is ignored by the initial
   usage scan, moved into `refs/.velnor-raw-quarantine-*/entry`, then
   `materialize_retained_record` discovers the quota overflow and returns an
   error. The resulting 131076-KiB state is over the 128-MiB limit.

The final error is fail-closed, but mutation already occurred and leaves an
over-budget recovery namespace. Required: include transaction payloads and
every candidate FD in one lock-held reservation before rename; account the
full source/temporary/metadata state; and make admission plus transfer
atomic. Add tests for max-size `.txn`, orphan cleanup at the boundary, two
concurrent candidates, and disk/allocation measurement rather than only
`metadata.len()` arithmetic.

### R2 — fixed overhead is an estimate, not an allocation proof (P1)

The new accounting uses fixed 64-KiB source, record, and namespace overheads
(`:41-47`) and logical file lengths. It does not measure filesystem blocks,
extended metadata, sparse/CoW behavior, or allocation failure. The max-sidecar
fixture crosses the quota through an omitted candidate, but the committed
`retention_disk_usage_does_not_duplicate_source_bytes` test checks only logical
lengths and constants; it does not inspect `st_blocks`/`du` or inject disk
failure. Define the filesystem allocation contract and prove its bound on
each supported filesystem, or use a conservative reservation that covers all
owned states without relying on an unverified estimate.

### R3 — partial publication is fail-closed but not atomic/recoverable (P1)

`materialize_retained_record` creates the record directory and writes only
`manifest.json` (`:1637-1681`). `write_private_file` can leave a created or
partially written file when write/sync/chmod/readback fails; there is no
record-level temporary name, complete marker, rollback, or bounded partial
state. Restart rejects a malformed record, as shown by
`partial_retention_record_fails_closed_without_reclaim`, but that test only
manually writes a truncated manifest. No injected write/fsync/disk-full fault
proves every partial-write boundary or recovery outcome.

Required: stage and atomically publish a complete metadata record, or encode a
bounded journal state that recovery can distinguish, account, and safely
finish/preserve. Exercise write, chmod, sync, directory-sync, and crash
boundaries.

### R4 — production store wiring remains absent (P0 outside bounded scope)

The exact module is still compiled by the integration test via `#[path]`.
`crates/velnor-tools/src/github_live_cli.rs:9,77-79,120-123` imports
`super::live_transport::{GithubHttpTransport, RawObjectFileStore}`, which is
the legacy `github_transport.rs` implementation. Live collection therefore
does not use this quota/provenance contract.

### R5 — full-suite race failure remains (P1)

Focused 23-test coverage passes, but the full package still fails the
replacement-race test. Stabilize the race fixture or fix the synchronization
contract before using the claimed 283-test result as acceptance evidence.

### R6 — Linux implementation remains unproven (P1 for cross-platform claim)

All runtime evidence is Darwin arm64. Linux `renameat2`, directory durability,
descriptor identity, quota admission, allocation, and crash recovery remain
unobserved because the target cannot compile. Run the hostile, quota,
partial-write, replay, and restart suite on a real Linux runner.

## Acceptance constraints

- Include `.txn`, `.tmp`, qdirs, retained metadata, partial records, and every
  public orphan candidate in one lock-held finite quota/reservation model.
  Reject before rename/mutation when candidate bytes would exceed it.
- Bound actual owned allocation, not only logical lengths and guessed fixed
  overhead. Add deterministic disk/full, allocation, and concurrent-admission
  fixtures.
- Keep descriptor-bound source replay: replacement with different or identical
  bytes must fail when the manifest claims the original FD identity; unknown
  names and malformed records remain untouched and fail closed.
- Publish complete records atomically or model partial records explicitly and
  prove recovery at each write/sync boundary.
- Wire this module into the live collector and remove the legacy path before
  any authoritative raw-evidence claim.
- Re-run full tests deterministically plus Linux proof before approval.

No source changes, live capture, host/Docker edits, publication, or approval.
