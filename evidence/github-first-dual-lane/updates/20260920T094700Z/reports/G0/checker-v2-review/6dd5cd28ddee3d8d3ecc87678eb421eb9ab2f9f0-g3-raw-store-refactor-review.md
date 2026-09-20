# Independent bounded review: 6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

## Scope and verdict

Reviewed exact detached commit `6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0`
(`refactor(raw-store): group cleanup expectations`), parent
`b50c50744c747d4e38d5b3a62c8850e4a95ca908`.

**Bounded refactor approval: PASS.** The delta is one source file only,
`crates/velnor-tools/src/github_raw_store.rs`, with 30 insertions and 31
deletions. It adds `QuarantineExpectation<'a>` containing exactly the prior
four typed values (`FileIdentity`, `Option<usize>`, `Option<&[u8]>`, and
`Option<u64>`) and passes that group to the two existing verification
functions. No tests, call paths, cleanup disposition, error mapping, or
filesystem operation changed. The grouped fields are `Copy`, and the exact
candidate compiles and passes all exercised paths.

**Integration approval: BLOCKED by repository-wide lint debt.** The
candidate's target clippy is clean; all-targets clippy still fails on 15
pre-existing mapper-test panic lints in
`crates/velnor-tools/src/g0_raw_store_adapter.rs` (not this delta): 11
`expect()` on `Result`, 3 `expect()` on `Option`, and 1 `expect_err()`.

This is not a G0, live-capture, or G3 approval.

## Verification

Exact detached tree was clean before and after all checks.

```text
rtk cargo test --locked --all-features --package velnor-tools --test github_raw_store -- --nocapture
cargo test: 39 passed (1 suite, 75.47s)

rtk cargo test --locked --all-features --package velnor-tools -- --nocapture
cargo test: 371 passed, 1 ignored (2 suites, 47.11s)

rtk cargo clippy --locked --all-features --package velnor-tools --bin velnor-tools -- -D warnings
cargo clippy: No issues found

rtk cargo fmt --all -- --check
PASS

rtk git diff --check b50c50744c747d4e38d5b3a62c8850e4a95ca908..HEAD
PASS
```

The required full-target lint was also run:

```text
rtk cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings
exit 101: 15 errors, 0 warnings
```

All reported locations are the mapper test file above; no diagnostic points
at `github_raw_store.rs` or the new expectation type.

## No-behavior-drift check

The exact diff changes only these interfaces:

- `quarantine_entry_matches(quarantine, entry_name, expectation)` now reads
  the same identity, link-count, byte-limit, and expected-byte values from a
  typed group.
- `discard_verified_quarantine(..., expectation, ...)` forwards the same
  values to that verifier.

The construction occurs at the existing successful-discard branch, after the
same first verification and before the same final verification. The retain,
fail-closed, qdir, `unlinkat`, and recovery branches are byte-for-byte
unchanged outside this argument grouping. Focused store tests and the full
package suite therefore cover the refactor's cleanup behavior without adding
new acceptance claims.

No source, branch, remote, live capture, or G0 artifact was modified by this
review. The evidence report is the only external artifact created.
