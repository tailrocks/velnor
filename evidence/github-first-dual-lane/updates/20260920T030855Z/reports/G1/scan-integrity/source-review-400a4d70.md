# G1 scan-integrity source review — 400a4d70

Status: independent read-only review. No source edits, generated-output
writes, merges, workflow dispatches, protected-policy writes, host/Docker
operations, or authority writes.

## Frozen scope

- Candidate: `400a4d70fd6876a43daba74965e9349e33d13bd5`,
  `codex/g0-estate-scope`, detached clean worktree
  `/tmp/velnor-scan-integrity-400-review`.
- Parent: `d88db98e42ccf5896dd03993f42cd8d1361524e8` (D19 `CLAUDE.md`
  regular-file fix); prior scan-integrity candidate: `820f6509`.
- Commit delta is source-only plus `Cargo.lock`, `Cargo.toml`, and the
  regular-file instruction conversion. No generated workflow/state output is
  changed.

## Verification

| Check | Result |
| --- | --- |
| `cargo test -p velnor-workflow --all-features --lib --locked -- --test-threads=1` | **1765 passed** (267.89s) |
| `cargo clippy -p velnor-workflow --all-features --all-targets --locked -- -D warnings` | **Pass** |
| `cargo fmt --all -- --check` | **Pass** |
| `git diff --check 820f6509 400a4d70` | **Pass** |
| Release closure command | **Pass**; reports `f5d937eacb53f6942088d20852f113d974d957d33b70f655f2c0df5d7fe500f0` |
| Integrated `--check` | **Fails**: generated `project.toml` and generator-state drift |

The exact integrated command was:

```text
target/debug/velnor-workflow --check --plain --providers github-hosted,velnor \
  --baseline-revision 400a4d70fd6876a43daba74965e9349e33d13bd5 .
```

It reports:

```text
error: generated files differ: .github/ci/project.toml, .github/ci/.github-actions-generator-state; rerun generate
```

The release binary was built with `--release --no-default-features --locked`
and `--closure`; no output files were generated.

## D19 closure evidence

The exact candidate tree contains:

```text
100644 blob bf256d9389fbe480672d15b7a0164eaa731a2287 1644 crates/velnor-workflow/CLAUDE.md
```

The previous `crates/velnor-workflow/AGENTS.md` blob is
`be65c6c8124f166ca643bda1499324d7a988dbce`, also 1644 bytes. A direct diff
shows only line wrapping; stripping whitespace gives identical content. Thus
the instructions are semantically preserved while `CLAUDE.md` is now a
regular blob.

The canonical release closure calculation over the 187 regular records from
`crates/velnor-workflow`, `Cargo.toml`, `Cargo.lock`, toolchain pins, and
`.cargo`, plus:

```text
closure-version:1
features:
profile:release
```

produces exactly:

```text
f5d937eacb53f6942088d20852f113d974d957d33b70f655f2c0df5d7fe500f0
```

The release binary reports the same value. The declared D19 pin remains
`fdeed261bd2247a38db6922a7726cd45d3d6f31e`; its tree still records
`crates/velnor-workflow/CLAUDE.md` as mode `120000` and computes closure
`81ba31f87a699c4e24e68baa1cf7cd0b5d765e5970934ec72c38b83772b274dc`.
`closure.rs:189-220` accepts only regular blob modes `100644`/`100755`, so the
published pin remains closure-incompatible; `81ba...` is only the raw shell
digest for comparison because the current validator rejects that tree before
minting a closure. Do not weaken that validator.

## Prior finding disposition

### Detector path TOCTOU — fixed for the reported symlink/path race

`s2/mod.rs:1351-1411,1798-1838` now captures raw regular-file bytes once,
passes those bytes into `scan_shape_with_detector_files`, and retains explicit
non-regular omissions. `s2/scan/mod.rs:74-185` materializes a private,
per-scan regular-file snapshot. Detector modules still call
`fs::read_to_string`, but their `root` is the private snapshot, never the
mutable target. `s2/mod.rs:9975-10030`
(`detector_snapshot_does_not_follow_replaced_path`) proves a target path
replaced by an outside symlink cannot redirect detector parsing.

The raw-inventory and detector-view regressions at
`s2/mod.rs:9841-9973` also prove untracked regular files are included,
symlinks are retained as inventory evidence, and non-regular entries become
typed limitations rather than detector inputs. Rust toolchain resolution now
uses the same byte map (`s2/scan/rust.rs:29-54`), closing the old secondary
path read.

Residual boundary: `capture_file_preimage` validates identity and length, not
a content hash, so an in-place same-inode/same-length write concurrent with
the initial read is not independently detected. Atomic replacement and the
reported path/symlink race are covered; a hostile same-inode mutation fixture
is absent. This is not a reason to reopen the old path-following finding, but
it prevents claiming an absolute concurrent-content snapshot guarantee.

### Fixed-point bound — materially fixed and regression-tested

`s2/mod.rs:1736-1765,1798-1840` centralizes a three-pass bounded helper and
compares next ownership plus rendered output, shape, and raw-input fingerprint.
`exact_baseline_outputs_exclude_only_exact_bytes` proves modified/unknown
files are not hidden by baseline ownership, and
`cyclic_renderer_fails_closed_after_three_passes` proves exactly three passes
then the `detector_fixed_point_unreachable` error. No generated writes occur
on the failure path.

### Post-write journal gap — fail-closed terminal path added; end-to-end
injection proof remains missing

`s2/mod.rs:8656-8675` turns post-write identity/read/content failures into
typed `partial_apply_recovery_required` errors. `s2/mod.rs:7960-7980` keeps
the durable transaction journal when such an error arrives, and all rollback
inspection/staging/rename failures at `7982-8047` use the same typed terminal
state. `recover_transaction_record` (`7004-7065`) refuses to recover a
committed mutation without durable progress identity, rather than guessing.

This removes the prior silent journal-loss/success risk. The actual committed
write can still fail after backup cleanup and before `journal.record`; in that
case the typed error preserves the journal but intentionally leaves no
progress record, so later recovery stops with “cannot prove mutation identity”
and requires operator handling. That is safe fail-closed behavior, not an
automatic rollback proof. The only new direct regression,
`partial_recovery_error_preserves_transaction_journal`
(`s2/mod.rs:20967-20989`), injects the typed error before a mutation; there is
no failure-injection test at the real commit-to-post-capture boundary.

### Rollback identity/mode — fixed for tested mutation races

`AppliedMutation` retains before/after identity and bytes; rollback compares
the current post-image before restoring. `rollback_restores_reviewed_file_mode`,
`rollback_refuses_same_bytes_replacement_after_write`, and
`rollback_refuses_recreated_missing_preimage` pass (`s2/mod.rs:20840-20965`).
The new typed recovery path now classifies rollback inspection, remove, stage,
mode, and rename failures as terminal partial recovery.

### Git-object/source closure hostile cases — fixed in source and tests

`immutable_baseline_rejects_git_object_overrides`
(`s2/mod.rs:9592-9692`) exercises replace refs, grafts, and alternates.
`closure.rs:189-220` rejects non-blob records, symlinks, malformed IDs, and
ambiguous paths; `closure_of_tree_rejects_tracked_symlink_record`
(`closure.rs:343-394`) is the integrated tracked-symlink regression.

## Verdict

**Do not approve full G1.** The exact source candidate passes 1765 tests,
clippy, format, diff checks, and the release closure self-report. The prior
detector path race, fixed-point bound, raw-inventory omission, object-override,
and tested rollback-identity findings are materially addressed.

Blocking integration evidence remains:

1. The published D19 revision is still the old symlink-bearing
   `fdeed261...`, not the regular-file source/pin required by this candidate.
2. Committed generated `project.toml` and generator-state are stale against
   exact `400a4d70`; no generated outputs were regenerated in this review.
3. Protected policy authority/acquisition and its caller/source/ruleset
   binding remain absent or unproven in this source; local operator baseline
   validation is not a substitute.
4. A real commit-to-post-capture failure-injection matrix is still needed
   before claiming complete transaction-recovery proof.
