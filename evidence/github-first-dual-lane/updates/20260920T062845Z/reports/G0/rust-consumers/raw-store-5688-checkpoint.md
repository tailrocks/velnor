# Raw-store 5688 bounded checkpoint

Observed 2026-09-20 (Asia/Ho_Chi_Minh) in isolated `/private/tmp/g3-raw-store`.

## Source and review

- Base/rejected review: `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36`.
- Review: `G0/checker-v2-review/5688af96-g3-raw-store-rereview.md`, report SHA
  `03cc0a51c0c0465d564735191680bb3cf62a65e4a0414081842f16b2061c7bff`.
- Corrected commits: `b90a8e7715677bc22447a50409e06c23e3c7ea52` (peak
  admission) and `024c5f612874c5ffa2dfd1520be4f0aa6edb9cd7` (lifecycle).
- Branch/remote: `codex/g3-raw-store`; local and `origin` resolve to the
  corrected commit; worktree clean.
- Owned files only: `crates/velnor-tools/src/github_raw_store.rs` and
  `crates/velnor-tools/tests/github_raw_store.rs`.

## Bounded fixes

- `.txn` is a compact metadata/digest journal. Recovery reconstructs the full
  sidecar only from descriptor-verified CAS bytes, avoiding payload-sized
  journal/sidecar duplication.
- Publication admission scans under the namespace lock and reserves payload,
  filesystem slack, source, and record metadata before creating a temporary
  inode. The same admission covers normal and recovery sidecar publication.
- Pending retention manifests persist the record-directory identity. Recovery
  keeps the opened descriptor, performs no-clobber rename, reopens the final
  directory, and compares directory identity, manifest identity, and bytes;
  valid wrong-record and symlink replacements fail closed into rejected
  retention evidence.
- Fresh and restart publication share the same post-rename verifier. Pending
  names must be the exact derived name for the manifest's final record name.
  Rejected identity-race records use a recognized, quota-accounted terminal
  namespace and survive a subsequent reopen.
- Deterministic fault hooks exercise manifest write, file sync, chmod,
  readback, directory sync, and pending publish rename boundaries. Partial
  state remains bounded and either recovers or fails closed.

## Verification

- Focused locked/all-features integration suite: **34 passed**.
- Full locked/all-features `velnor-tools` package: **294 passed**.
- Locked all-target/all-feature clippy with `-D warnings`: clean.
- `cargo fmt --all` and `git diff --check`: clean.
- Exact-max valid compact transaction recovery fixture passed with a 64 MiB
  safe payload.

Not claimed: physical crash/power-loss or disk-full proof, production
collector/publication wiring, Linux runtime proof, integration approval, merge,
release, or fleet rollout. The prior independent negative-control harness still
asserts the old transient-overquota behavior; it must be rewritten as a
positive non-overquota control by the reviewer.
