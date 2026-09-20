# G1 scan-integrity follow-up — 29d6a9c

Status: bounded independent read-only review. No source edits, generated-output
writes, merges, workflow dispatches, protected-policy writes, host/Docker
operations, or authority writes.

## Frozen scope

- Candidate: `29d6a9caf64d625799efa1ad52c3bba0e4f52db2`, detached clean
  worktree `/tmp/velnor-scan-integrity-29d6-review`.
- Parent: `400a4d70fd6876a43daba74965e9349e33d13bd5`.
- Delta: `crates/velnor-workflow/src/s2/mod.rs` only; 129 insertions and
  11 deletions.

## Verification

| Check | Result |
| --- | --- |
| Focused post-capture test | **Pass**: 1 passed, 1765 filtered |
| `cargo clippy -p velnor-workflow --all-features --all-targets --locked -- -D warnings` | **Pass** |
| `cargo fmt --all -- --check` | **Pass** |
| `git diff --check 400a4d70 29d6a9c` | **Pass** |

Focused command:

```text
rtk cargo test -p velnor-workflow --all-features --lib --locked -- \
  post_capture_failure_after_commit_preserves_recovery_journal --nocapture
```

## Fault-path evidence

Production `write_reviewed_file` now calls
`write_reviewed_file_observed_with_capture(..., capture_post_write_preimage)`
(`s2/mod.rs:8647-8664`). The regular-file branch performs the real staged
write, hard-link backup, pre-replacement checks, atomic `fs::rename`, and
backup cleanup before invoking the capture callback (`s2/mod.rs:8677-8772`).
An error from that callback is returned before the mutation is appended or
`journal.record` is called. `apply_generated_write_plan` routes that error to
`rollback_after_error` (`s2/mod.rs:7882-7894`), whose typed branch immediately
returns while preserving the durable journal (`s2/mod.rs:7958-7980`).

`post_capture_failure_after_commit_preserves_recovery_journal`
(`s2/mod.rs:21098-21198`) creates a real generated preimage, baseline, write
plan, and transaction journal. It then runs the real replacement path, not a
mock write, and injects a typed post-capture failure through the new callback
seam. Assertions prove:

1. the live path contains the new bytes after the atomic replacement;
2. `partial_apply_recovery_required` is retained;
3. the transaction journal remains and no backup is falsely treated as proof;
4. recovery rejects the unrecorded mutation with `cannot prove mutation identity`;
5. after the post-write identity is explicitly recorded, normal journal
   recovery restores the original bytes and removes the journal.

This is a meaningful regression for the exact prior gap: committed mutation,
post-observation error, no in-memory `AppliedMutation`, and no journal progress
must not be reported as a clean rollback or silently discard the journal.

## Fidelity limits

The injected callback returns `GeneratorError::partial_apply_recovery_required`
directly; it does not force `capture_post_write_preimage` itself to fail from a
real `capture_file_preimage` error or a real post-write byte mismatch. Thus the
test proves the production atomic-commit-to-typed-error/journal branch, but not
the lower-level filesystem-error mapping. A second fault seam would be needed
to exercise those concrete capture causes.

The “recovery after identity is recorded” portion calls private
`TransactionJournal::record` directly (`s2/mod.rs:6502-6517`). No normal CLI or
runtime path records the failed mutation after `write_reviewed_file` returns
the typed error; the next normal generation call only invokes
`recover_pending_transaction` (`s2/mod.rs:7565-7577`) and therefore remains
fail-closed at `cannot prove mutation identity` until an operator/tool records
the observed identity. The test is valid proof of safe refusal and recovery
mechanics once trusted progress exists, not proof of an end-user recovery
command or automatic recovery from the injected failure.

The test covers the first changed output with an empty prior-mutation list. A
multi-output case with earlier journal progress followed by a failed later
post-capture would strengthen ordering proof; the existing reverse journal
recovery logic is structurally compatible but that matrix is absent.

## Preserved integration blockers

This follow-up does not change the prior 400a verdict:

- D19 still declares old symlink-bearing pin
  `fdeed261bd2247a38db6922a7726cd45d3d6f31e`.
- Committed generated `project.toml` and generator-state remain stale against
  exact 400a/29d6 source; this review generated nothing.
- Protected policy authority/acquisition and caller/source/ruleset binding
  remain absent or unproven.

## Verdict

**Conditional source-level pass for the new regression; no full G1 approval.**
The test accurately exercises a real atomic replacement followed by a typed
post-capture error and proves journal preservation plus fail-closed recovery.
It should be described as injected typed-fault coverage, not as proof that a
real filesystem capture failure is automatically recoverable. The D19,
generated-state, authority, and recovery-entrypoint limitations remain.

