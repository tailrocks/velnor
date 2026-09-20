# Exact raw-store bounded-retention re-review — `0d7713cd15cefb554fda5954e2d3320968aa31e7`

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: `/private/tmp/g3-raw-store`, branch `codex/g3-raw-store`  
Compared with: `c9b16046a3ff399b9e9e18d9cd377a4f0bd6a221`  
Boundary: read-only bounded-store review; no source, collector, host, Docker,
remote, or production-wiring edits.

## Verdict

**Reject for bounded retention and authoritative capture.** This commit fixes
the previous unbounded, pathname-cleanup sink in important ways: retention has
a typed `deny_unknown_fields` manifest, FD-derived identity/digest/length
checks, anchored private directories, count/logical-byte admission, and
fail-closed recovery. It still does not provide an actual bound on retained
disk state: source quarantine directories and their entries remain in place
while copies are materialized under retention, and pending quarantine bytes are
not admitted against the quota. The new module is also not wired into the live
collector; production still imports the legacy store. No authoritative capture
or dual-platform claim is approved.

## Exact verification

- `git rev-parse HEAD`: `0d7713cd15cefb554fda5954e2d3320968aa31e7`.
- Commit diff is limited to `github_raw_store.rs` and its tests. The test run
  leaves an untracked `.github-raw-store-fixtures/` directory; no source or
  committed files changed.
- `rtk cargo test --locked --all-features --package velnor-tools --test
  github_raw_store -- --nocapture`: **20 passed** on the final focused run.
- Focused hostile/recovery tests passed: crash-left quarantine recovery,
  unknown retention child fail-closed, count admission/reopen fail-closed,
  replaced-temporary preservation, and symlink/hardlink replacement race.
- One earlier full-package run had **19 passed, 1 failed** in
  `cleanup_leaves_replaced_regular_temporary_name_instead_of_unlinking_it`;
  isolated rerun passed. This is a timing-flaky test signal, not a clean
  280-test proof.
- `rtk cargo fmt --all -- --check`: passed.
- `rtk cargo clippy --locked --profile test --all-targets --all-features
  --package velnor-tools -- -D warnings`: passed.
- `git diff --check`: passed.
- Independent scratch harness at
  `/private/tmp/raw-store-adversarial-0d`: **2 passed** offline. Its two
  60-MiB pending quarantine entries were both retained and source copies were
  preserved; `du -sk` measured **245808 KiB** despite the implementation's
  approximately 128-MiB `MAX_RETAINED_BYTES` logical quota.
- Linux cross-target check was unavailable before crate compilation because
  `openssl-sys` could not find `x86_64-linux-gnu-gcc`; no Linux proof is made.

## Improvements over `c9b16046`

1. Retention is now root-anchored and uses a strict manifest containing
   source namespace/name/parent, quarantine identity, entry digest/length, and
   retained FD identity (`src/github_raw_store.rs:40-91,1717-1748`). Unknown
   retention children and malformed records fail closed before public-object
   reconciliation (`:982-1000,1952-1971`).
2. Source and retained files are opened and rechecked through descriptors with
   private-regular/single-link, inode, size, and digest checks. Hostile FIFO,
   symlink, hardlink, and replacement entries are preserved rather than
   removed by pathname.
3. Admission now refuses at 128 records or the logical retained-byte limit
   before creating a retained record (`:1471-1483,1626-1633`). Crash-left
   quarantine directories are retained and reopen stably.

## Remaining findings

### R1 — quota does not bound actual retention state (P0/P1)

`quarantine_admission` counts current retained records plus the number of
pending qdirs, but does not count pending qdir bytes (`:1471-1483`).
`materialize_retained_record` adds only the copied entry and a fixed manifest
to `RetentionUsage` (`:1562-1633`), then copies the manifest back into the
source qdir and deliberately leaves the qdir and entry in place
(`:1666-1682`). `retention_usage` counts only retained record manifest and
entry logical lengths (`:1952-1971`), not source copies, directory metadata,
filesystem blocks, sparse/padding allocation, or partial records.

The independent two-by-60-MiB fixture produced four 60-MiB files (two source
qdirs plus two retained copies), measuring 245808 KiB while the logical quota
is about 128 MiB. Repeated pending qdirs can also consume all 128 slots and
then permanently refuse new recovery; there is no bounded reclamation or
operator handoff policy. This remains relocation with a logical counter, not
bounded retention.

Required: reserve and account for every pending and retained state before
mutation, including metadata/actual allocation policy; or perform a proven
descriptor-owned transfer that removes only the original owned bytes. Bound
count, bytes, and crash leftovers together, and test above-quota restart and
concurrent admission.

### R2 — partial retained records can become unaccounted durable debris (P1)

`materialize_retained_record` creates the retained directory, writes `entry`,
then writes/syncs `manifest.json` (`:1635-1682`). A write, chmod, sync, or
identity failure after directory/entry creation can leave an incomplete record.
Strict restart then fails closed through `read_retained_record`, but the
partial bytes are outside valid `retention_usage` accounting and have no
bounded recovery/journal state. The implementation needs an atomic complete
record protocol, or an explicitly bounded partial-record state with recovery
and crash-boundary tests.

### R3 — source qdir entry provenance is content-only on replay (P1)

The source qdir entry is read and FD-verified at `:1570`, but
`validate_manifest_entry` checks only byte length and digest and ignores the
actual `FileIdentity` tuple (`:1775-1789`). The manifest's entry identity is
the newly copied retained-record FD, not the source qdir entry. A same-byte
replacement in the source qdir is therefore accepted on replay. This may be
an intentional content-provenance policy, but it is not an immutable source FD
binding; declare that policy or bind/recheck source identity at publication and
test replacement at every sync boundary.

### R4 — new store is not production-wired (P0 outside bounded-store scope)

The exact module is compiled by `tests/github_raw_store.rs` via a test
`#[path]`. `crates/velnor-tools/src/github_live_cli.rs:8-9,77-79,120-123`
still imports `super::live_transport::{GithubHttpTransport, RawObjectFileStore}`
from legacy `github_transport.rs`. The live collector therefore does not use
this retention contract, and no authoritative raw-capture claim can rely on
this commit.

### R5 — race-test reliability is unresolved (P1)

The focused race test passes in isolation and in the final focused run, but a
full package run previously failed the same timing-sensitive test. Make the
replacement race deterministic or fix the synchronization contract before
counting the full suite as evidence.

### R6 — Linux implementation remains unproven (P1 for cross-platform claim)

All runtime evidence is Darwin arm64. `renameat2`/fallback behavior, directory
durability, inode checks, quota accounting, and crash recovery on Linux remain
unobserved because the cross target cannot compile. Run the same hostile,
quota, race, and restart suite on a real Linux runner.

## Acceptance constraints

- Keep unknown retention names, malformed manifests, invalid digest lengths,
  wrong FD identity/mode, and quota overflow fail-closed before any public
  cleanup mutation.
- Make retention a finite, measured state machine: pending qdirs, retained
  records, partial/crash records, metadata, and filesystem allocation must all
  fit one admitted budget; admission must reserve before mutation.
- Make publication/recovery atomic and crash-testable; no malformed partial
  record may consume untracked space indefinitely.
- Define whether replay provenance is content-bound or source-FD-bound. If
  source-FD-bound, compare the source identity; if content-bound, encode and
  test that explicit contract. Reject duplicate/non-canonical manifest keys if
  the manifest is authoritative JSON.
- Wire this module into the live collector and remove the legacy store path;
  then rerun production integration, not only test-only module tests.
- Re-run deterministic hostile replacement, FIFO/symlink/hardlink, quota,
  crash/reopen, and Linux tests before any G0/G1 authoritative evidence claim.

No source changes, live capture, host/Docker edits, publication, or approval.
