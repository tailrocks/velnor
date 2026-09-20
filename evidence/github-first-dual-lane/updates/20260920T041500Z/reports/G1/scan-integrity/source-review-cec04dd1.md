# G1 scan-integrity source review — cec04dd1

Status: independent read-only review. No source edits, generated-output
writes, merges, workflow dispatches, protected-policy writes, host/Docker
operations, or authority writes.

## Frozen scope

- Candidate: `cec04dd1faef785a85e4c8dee6aecb4229ef57a2`, detached clean
  worktree `/tmp/velnor-scan-integrity-cec04-review`.
- Parent: `29d6a9caf64d625799efa1ad52c3bba0e4f52db2`.
- Commit delta: `crates/velnor-workflow/src/s2/mod.rs` plus the two S2 TUI
  test constructors. `s2/dispatch.rs` is unchanged.

## Verification

| Check | Result |
| --- | --- |
| `cargo test -p velnor-workflow --all-features --lib --locked` | **1767 passed** (32.95s) |
| `post_capture_failure_after_commit_preserves_recovery_journal` | **Pass**; 1 test |
| `later_post_capture_failure_preserves_prior_journal_progress` | **Pass**; 1 test |
| `post_capture` filter | **2 passed** |
| `durable_transaction` filter | **2 passed** |
| `generation_lock_serializes_mutating_writers` | **2 passed** |
| `generation_refuses_symlinked` filter | **7 passed** |
| `cargo clippy -p velnor-workflow --all-features --all-targets --locked -- -D warnings` | **Pass** |
| `cargo fmt --all -- --check` | **Pass** |
| `git diff --check 29d6a9c cec04dd1` | **Pass** |

## Blocking finding: the advertised CLI is unreachable through normal dispatch

The new flag exists only in the S2 parser (`s2/mod.rs:383-447, 5978-6005`)
and is dispatched by `s2::dispatch::run_if_s2`. The dispatcher was not changed
in this commit (`s2/dispatch.rs:41-91`): it routes S2 only when it sees
`--providers` or can parse the legacy `Cli`. The legacy parser rejects the new
flag before it can inspect the local target.

Reproduction on the exact built binary:

```text
$ target/debug/velnor-workflow --help | rg recover
# no output; help has no --recover-transaction

$ target/debug/velnor-workflow --recover-transaction --plain \
    crates/velnor-workflow/tests/fixtures-s2/polyglot
error: unexpected argument '--recover-transaction' found
```

The same failure occurs for the repository root and a local S2 fixture. Adding
the unrelated `--providers github-hosted` flag forces S2 and reaches the new
code, which then correctly reports no pending journal:

```text
$ target/debug/velnor-workflow --providers github-hosted \
    --recover-transaction --plain crates/velnor-workflow/tests/fixtures-s2/polyglot
error: no pending transaction journal ...; nothing to recover
```

The tests at `s2/mod.rs:21287-21541` call `recover_transaction_cli(&Cli {...})`
directly and therefore do not exercise the real binary parser/dispatcher.
This is a release-blocking regression, not a documentation gap. Fix the
schema dispatcher to recognize `--recover-transaction` before legacy parsing,
then add an actual subprocess/dispatch regression for a local S2 target and
for a normal legacy target (the latter must fail with the intended schema gate,
not “unexpected argument”).

## What the commit does correctly

- `GenerationLock::acquire` is taken before journal inspection in
  `s2/mod.rs:7208-7216`; existing lock serialization passes.
- Operator recovery checks every unrecorded record's current bytes and Unix
  mode against its journaled after-image before writing `progress`
  (`7204-7249`). A mismatch returns the refusal error and leaves the journal.
- The production multi-output apply loop receives the real post-write capture
  callback (`7885-8048`). The two new tests use actual atomic replacement and
  actual `capture_post_write_preimage`, not a fabricated error. The later
  output failure preserves the first output's progress and recovers both after
  restoring the second after-image.
- The CLI rejects generation/baseline combinations and remote shorthand; exact
  forced-S2 probes returned the intended errors. Existing symlinked managed
  directory/output-root tests pass, and `--output /tmp` (a symlink on this
  host) is rejected before journal inspection.

## Remaining transaction-integrity limits

These are independent of the dispatch blocker and prevent a “no TOCTOU /
durable journal” claim:

1. `transaction_snapshot_bytes` (`s2/mod.rs:7026-7047`) performs
   `symlink_metadata` and then path-based `fs::read`. An external replacement
   between those calls can turn a checked regular snapshot into a symlink or
   another file. Recovery also checks the live output, then later stages and
   renames it (`7049-7110`), so an external writer can change the output after
   the comparison and before the restoring rename. The generation lock only
   serializes cooperating Velnor writers. Use no-follow descriptor reads and
   an fd-relative compare-and-replace/lock protocol (or explicitly scope the
   threat model and test it); add a race regression rather than relying on
   metadata checks.
2. Journal bytes are not bound by a digest. The manifest stores path, kind,
   and mode only (`6825-6838`); `before` regular snapshots are hard links to
   live files (`6784-6794`) and `after` bytes are trusted by operator
   recovery. A mutable journal can therefore be edited to make arbitrary
   current bytes look like the after-image and to supply arbitrary rollback
   bytes. At minimum copy before bytes, record a digest/length/mode for every
   snapshot, and validate it before progress or rollback.
3. “Durable” tests are process-local. `write_transaction_bytes` fsyncs files
   and `sync_transaction_directory` fsyncs only the top journal directory
   (`6687-6723, 6840-6845`); creation of the journal entry in the output-root
   parent and entries in `before/` and `after/` are not separately synced.
   There is no crash/restart test. Do not claim power-loss durability until
   parent/subdirectory ordering is made explicit and exercised.

## Existing integration blockers retained

This review does not alter prior G1 blockers:

- D19 published pin `fdeed261bd2247a38db6922a7726cd45d3d6f31e` still carries
  the symlink-mode `crates/velnor-workflow/CLAUDE.md`; do not weaken closure
  validation.
- The exact integrated generated `project.toml` and generator-state remain
  stale against the candidate unless regenerated by the owning workflow.
- Protected policy authority/acquisition and caller/source/ruleset binding are
  absent or unproven; local self-reported closure is not authority proof.

## Verdict

**Do not approve cec04dd1 for integrated G1.** The source-level post-capture
and multi-output recovery behavior is materially improved and all 1767 tests,
clippy, format, and diff checks pass. The real CLI is currently unreachable
without an unrelated `--providers` switch; dispatch must be fixed and
covered. The journal snapshot/TOCTOU and crash-durability limits remain
explicit follow-up integrity work, while the prior D19/authority blockers
remain.
