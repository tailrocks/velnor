# Exact secure raw-store quarantine re-review — `3840a9f1b809fecc951e3ba5a985834efa6082ca`

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: clean `/private/tmp/g3-raw-store` (`codex/g3-raw-store`)  
Compared with: `3e3aa993c0f0b648a3e07c1d90913f13a14c8a2b`  
Boundary: read-only bounded-store review; no source, producer, host, Docker, remote, or production-wiring edits.

## Verdict

**Reject for bounded cleanup acceptance.** Atomic source-to-quarantine rename
closes the earlier source-path unlink race for the schedules exercised, and
hostile FIFO/symlink/directory/public entries are preserved. A concrete crash
fixture shows that quarantine directories and entries are not recovered at all;
they become permanent unbounded debris. The quarantine directory and final
entry deletion also retain same-UID pathname ownership races. Production store
wiring remains a separate required gate.

## Exact verification

- `git rev-parse HEAD`: `3840a9f1b809fecc951e3ba5a985834efa6082ca`.
- `git status --short --branch`: clean, remote equal.
- `cargo test --locked --all-features --package velnor-tools --test
  github_raw_store -- --nocapture`: **17 passed**.
- `cargo test --locked --all-features --package velnor-tools -- --nocapture`:
  **277 passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --profile test --all-targets --all-features
  --package velnor-tools -- -D warnings`: passed.
- `git diff --check`: passed.
- Existing exact fixtures passed: regular source-name replacement during
  publication cleanup, FIFO/symlink/directory/public temporary entries left
  untouched, transaction recovery, orphan sweep, namespace replacement,
  hardlink refusal, and concurrent same-ID collision.
- Independent scratch fixture, run in detached `/private/tmp/g3-raw-store-harness`:
  `cargo test --locked --all-features --package velnor-tools --test
  quarantine_review -- --nocapture`: passed while asserting that crash-left
  `.velnor-raw-quarantine-crash/entry` directories in `sha256`, `original`,
  and `refs` remain after `RawObjectFileStore::new` reopens the store. This is
  a reproduced cleanup gap, not a source-test false claim.
- Exact implementer tree remained clean; scratch fixture was not committed.

## Fixed or demonstrated

1. `remove_private_named` now avoids unlinking symlinks, FIFOs, devices,
   directories, hardlinks, and public files. It moves a candidate with
   descriptor-relative no-clobber rename before any deletion
   (`src/github_raw_store.rs:1067-1121`).
2. `quarantine_remove` verifies the moved entry's inode, mode, link count, and
   optional bytes before deleting the entry in the quarantine directory
   (`:1142-1225`). Source-name replacement is therefore moved and retained on
   identity mismatch rather than deleted. The exact regular replacement race
   fixture passes.
3. `reconciliation_leaves_fifo_symlink_and_unknown_entries` confirms the
   current policy leaves FIFO, symlink, directory, and mode-0644 public file
   entries untouched (`tests/github_raw_store.rs:371-433`).

## Remaining findings

### R1 — crash-left quarantine is never recovered (P1)

The new cleanup creates `.velnor-raw-quarantine-*` directories
(`src/github_raw_store.rs:1228-1260`). A kill can occur after directory
creation, after the source entry is renamed into `entry`, or after entry
unlink but before directory removal. Startup scans any name beginning
`.velnor-raw-` and calls `reconcile_temporary`, but that function only removes
private regular files; it deliberately leaves directories
(`src/github_raw_store.rs:845-855,893-895,992-1018,1288-1308`). It does not
open, validate, or sweep quarantine `entry` contents. The independent scratch
fixture reproduced persistent quarantine directories and entries after reopen.

Required: persist enough ownership/identity metadata to distinguish a store
quarantine from an operator entry, recover every crash phase, and bound stale
quarantine count/bytes. Add kill/reopen fixtures at each rename, verify, entry
unlink, directory-sync, and directory-remove boundary. Until then an attacker
or repeated interruption can accumulate unbounded local evidence debris.

### R2 — quarantine directory adoption is not identity-bound (P1)

`create_quarantine_directory` succeeds with `mkdirat`, then reopens the name
and returns that descriptor without retaining or comparing the identity of the
directory created by this call (`:1228-1257`). A same-UID writer can rename
the created directory and install another directory before the reopen. The
store can then adopt the replacement as its “private” quarantine and move or
delete entries there. Mode `0700` limits other UIDs, not another process under
the same UID—the threat model already exercises same-UID namespace replacement.

Required: capture the created inode immediately, reopen descriptor-relatively,
compare device/inode/mode/link identity, and fail closed on any mismatch. Add
a deterministic create/reopen replacement fixture.

### R3 — final quarantine entry removal still has a pathname race (P1/P2)

After opening and verifying `quarantine/entry`, the code calls
`unlinkat(quarantine_fd, "entry", 0)` (`:1176-1220`). A same-UID writer can
replace that pathname after the final `fstat` and before `unlinkat`; the
replacement is then deleted. `remove_quarantine_directory` has the same
stat-then-`AT_REMOVEDIR` race for the quarantine directory itself
(`:1262-1285`). The atomic source rename protects the original source name,
not these later attacker-observable names.

Required: ensure the quarantine directory is outside the untrusted writable
namespace or leave uncertain entries/directories for operator cleanup. A
post-check pathname unlink cannot prove object identity under this threat model;
add replacement-after-final-check fixtures (regular, symlink, FIFO, and empty
directory) and assert no unowned entry is deleted.

### R4 — production API/wiring gap remains (P0 outside bounded scope)

The exact module remains test-only and still does not match production
`RawObject`/`RawObjectRef` fields; the live collector still imports the legacy
path-based store. This quarantine review does not approve production capture.
Resolve the prior report's API migration and run consumer-level tests before
any authoritative/production claim.

### R5 — Linux remains unproven (P1 for dual-platform claim)

This is a Darwin-arm64 run. Linux `renameat2`, directory sync, inode identity,
and crash recovery remain unobserved; retain the explicit no-Linux-proof
constraint from the prior exact review.

No source changes, live capture, host/Docker edits, or publication approval.
