# Independent bounded review: b50c50744c747d4e38d5b3a62c8850e4a95ca908

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

## Scope and verdict

Reviewed exact detached commit `b50c50744c747d4e38d5b3a62c8850e4a95ca908`
(`fix(raw-store): fail closed on cleanup races`), whose first parent is
`5c2877dd80c825a8cfa9438a004f0f5e141537c5`. The retention change is in the
two intended files only:

- `crates/velnor-tools/src/github_raw_store.rs`
- `crates/velnor-tools/tests/github_raw_store.rs`

Scoped semantic verdict: **PASS for the bounded successful-cleanup and
retention-race contract under the store's namespace-lock/descriptor model**,
with the two residual findings below. **Integration approval: NOT APPROVED**
until the new clippy failure is fixed or explicitly dispositioned. This is
also not a G0, live-capture, or G3 approval.

## What the exact source proves

- `remove_exact_file_with_disposition` verifies the observed descriptor and
  bytes, then invokes the source-disappearance hook before quarantine. An
  `ENOENT` from `rename_no_clobber` now goes to `retain_quarantine_result`, so
  an observed journal disappearing after verification cannot become a
  successful discard. The empty private quarantine directory and retention
  record are retained when capacity permits.
- The same fail-closed path applies during recovery. A missing quarantine
  pathname or `AT_REMOVEDIR` `ENOENT` returns `false`; the caller takes the
  retention path instead of reporting successful cleanup.
- Successful journals use the discard disposition only after the moved entry
  is reopened and identity, regular-file mode, link count, and bytes are
  rechecked. `discard_verified_quarantine` performs a second complete
  `quarantine_entry_matches` immediately before `unlinkat`.
- The qdir and entry operations are descriptor-relative. A replacement that
  fails the identity/content check is retained; source pathnames are not
  blindly unlinked. qdir identity is checked before `AT_REMOVEDIR`.
- The successful path maps both `Retained` and `Left` to `RawStorageError::Refused`.
  Error branches that use `let _ = retain_quarantine_result(...)` return the
  original error immediately; they do not convert retention failure to
  success. The recovery-only `let _ = quarantine_remove(...)?` propagates
  errors and ignores only its enum result for the retain disposition, leaving
  the qdir as evidence when it cannot be materialized.

## Exact race and capacity fixtures

The focused integration suite exercises actual `RawObjectFileStore::store`
and `RawObjectFileStore::new` cleanup/recovery paths, not private helper calls:

- `successful_cleanup_source_disappearance_fails_closed_and_retains_incident`
  removes the observed transaction before quarantine; store returns an error,
  the empty source qdir remains, one retention record is materialized, and
  reopen succeeds.
- `recovery_cleanup_source_disappearance_fails_closed_and_retains_incident`
  repeats the race through constructor recovery; constructor returns an error,
  qdir/record remain, and a later reopen succeeds.
- `successful_transaction_cleanup_retains_replacement_evidence` replaces the
  moved entry before the first discard recheck; the replacement bytes and
  retention record remain, and the store fails closed.
- `successful_cleanup_postcheck_replacement_is_not_deleted` replaces the
  entry immediately before the new final recheck; the replacement remains
  readable and is not deleted, with a retention record and failed operation.
- `successful_transaction_cleanup_releases_retention_capacity` runs 130
  successful captures while 63 real failure records already exist; successful
  journals/source qdirs leave no retained failure records and do not displace
  the 63 records.
- `retention_admission_is_bounded_and_reopens_fail_closed` materializes 128
  records, leaves the 129th qdir pending, and keeps the store refusing reopen
  rather than dropping evidence.

The exact focused command passed **39/39**:

```text
rtk cargo test --locked --all-features --package velnor-tools --test github_raw_store -- --nocapture
```

The package command passed **371 tests, 1 ignored**:

```text
rtk cargo test --locked --all-features --package velnor-tools -- --nocapture
```

`rtk cargo fmt --all -- --check` and `git diff --check HEAD^ HEAD` passed.
An external scratch harness compiled the exact b50 source and performed **130
distinct 256-KiB source/safe pairs** (different bytes, so no payload
deduplication shortcut); it ended with `txn=0 source_qdirs=0 retained=0`.
The scratch harness is not product evidence and made no repository changes.

## Findings / limits

1. **P2 quality finding:**
   `rtk cargo clippy --locked --all-features --package velnor-tools --bin velnor-tools -- -D warnings`
   fails only on the new source function
   `discard_verified_quarantine` (`github_raw_store.rs:2097`),
   `clippy::too_many_arguments` (10/7). No functional test failed, but the
   exact candidate is not warning-clean.

2. **Strict hostile same-UID TOCTOU boundary:** the final descriptor/content
   check and descriptor-relative `unlinkat` are separate syscalls. A fully
   uncooperative same-UID actor that replaces `entry` in that narrow interval
   could still make the unlink remove the replacement. The namespace lock
   prevents cooperating store instances, the qdir FD prevents traversal to an
   external directory, and the supplied hook proves replacement immediately
   before the final check is retained. This review does not claim an atomic
   check-and-delete primitive that POSIX does not provide.

No source, branch, remote, live capture, or G0 artifact was modified by this
review. The detached tree was clean before and after verification.
