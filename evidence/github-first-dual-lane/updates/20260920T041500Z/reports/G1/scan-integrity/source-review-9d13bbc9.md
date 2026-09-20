# G1 scan-integrity source review — 9d13bbc9

Status: independent read-only review. No source edits, generated-output
writes, merges, workflow dispatches, protected-policy writes, host/Docker
operations, or authority writes.

## Frozen scope

- Candidate: `9d13bbc986cbd422a2a2422de97f30b5f03a1a32`, detached clean
  worktree `/private/tmp/velnor-scan-integrity-9d13-review`.
- Compared with: `cec04dd1faef785a85e4c8dee6aecb4229ef57a2`.
- Delta commits: `eedae2e1` (dispatch route), `5f35627b` (CLI test),
  `762a26ac` (snapshot authentication), `993ce685` and `9d13bbc9`
  (bounded fsync/crash-test documentation).

## Verification

| Check | Result |
| --- | --- |
| `cargo test -p velnor-workflow --all-features --test recover_transaction_cli --locked -- --nocapture` | **2 passed** |
| snapshot tamper/symlink unit filter | **1 passed** |
| post-capture filter | **2 passed** |
| durable-transaction filter | **2 passed** |
| rollback filter | **4 passed** |
| generation-lock filter | **2 passed** |
| `cargo clippy -p velnor-workflow --all-features --all-targets --locked -- -D warnings` | **Pass** |
| `cargo fmt --all -- --check` | **Pass** |
| `git diff --check cec04dd1 9d13bbc9` | **Pass** |

The lib suite contains 1768 tests. Parallel full-suite attempts were not
stable in this shared host: one run was `1767 passed, 1 failed` in the
pre-existing local shallow-clone fixture (the focused test passes); another
was `1766 passed, 2 failed` in unrelated runtime selection/stall fixtures.
A serial run reached the chatty runtime stall test and was stopped after more
than two minutes. These failures do not touch the transaction delta. Do not
report the full 1768 as green from this environment.

## Provider-pairing failures: harness precondition, not six provider regressions

`cargo test -p velnor-workflow --all-features --test provider_pairing --locked
-- --nocapture` reports six failures:

- `all_providers_emit_one_caller_per_unit_per_provider`
- `automatic_all_gates_pair_except_fork_pr_trust_exclusion`
- `hosted_only_emits_only_hosted_callers`
- `swift_explicit_hosted_opt_out_stays_hosted`
- `swift_is_excluded_from_local_providers_by_platform`
- `velnor_only_emits_only_velnor_callers`

All six stop at `tests/provider_pairing.rs:112` with the identical error:

```text
baseline unavailable: supply --baseline-revision <full-sha> or explicitly select --local-no-baseline
```

The test helper at `provider_pairing.rs:101-109` invokes the binary without
either baseline option. The file is unchanged from `cec04dd1`, and no
provider-pairing assertion runs. Classify these as a pre-existing fixture
contract failure; update the temporary-fixture helper to pass
`--local-no-baseline` in a separate test-harness change. They are not
evidence of a 9d13 provider-rendering defect.

## Real CLI route fixed and independently exercised

`s2/dispatch.rs:49-68` now recognizes `--recover-transaction` before
legacy parsing. `recover_transaction_cli`
(`s2/mod.rs:6156-6193`) then requires a local schema-2 target before taking
the output-root lock.

The new subprocess test `tests/recover_transaction_cli.rs:40-75` passes:

- schema-2 target reaches operator recovery without an unrelated
  `--providers` switch and reports the expected missing-journal error;
- schema-1 target reaches the explicit schema gate, not legacy “unexpected
  argument” parsing.

Manual exact-binary probes agree:

```text
--recover-transaction --plain <schema-2 target>
error: no pending transaction journal ...; nothing to recover

--recover-transaction --plain <schema-1 target>
error: transaction recovery requires a local schema-2 target ...
```

The earlier cec04 CLI blocker is fixed.

## Journal safety delta

The candidate materially closes the previous snapshot/path findings:

- `open_transaction_file` uses `O_NOFOLLOW` and compares opened versus
  observed identity (`s2/mod.rs:6792-6823`); `read_transaction_file`
  rechecks identity and final length after reading
  (`6825-6842`).
- Before-images are copied as bytes, mode-restored, and synced rather than
  hard-linked to live outputs (`6921-6935`). After-images are length- and
  digest-recorded in schema-2 manifest entries
  (`6966-6983`).
- Snapshot reads validate the recorded length and digest and preserve the
  journal on mismatch (`7172-7201`).
- Journal creation/progress/cleanup sync the journal and output-root
  directories (`6756-6777, 6885-7000, 7266-7279`).
- `transaction_journal_binds_snapshot_bytes_and_rejects_symlinks` passes
  after mutating an after-image and replacing a before-image with an outside
  symlink (`s2/mod.rs:21441-21519`).
- Existing rollback identity/mode tests and generation-lock tests pass. The
  real post-capture single-output and later-output multi-output tests remain
  green.

No outside-file clobber was observed in the tested static symlink, snapshot
tamper, identity-replacement, or cooperative-lock cases.

## Residual threat-model limits

These do not recreate the earlier ordinary snapshot-path bug, but prevent an
absolute hostile-concurrency claim:

1. `O_NOFOLLOW` protects the final journal file component. Parent directory
   resolution, `sync_transaction_directory`, `write_transaction_bytes`,
   and live output capture/rename remain path-based. An external attacker who
   swaps a journal/output ancestor after inspection can still race those
   operations. The generation lock serializes cooperating Velnor writers; no
   hostile concurrent swap test exists.
2. The recorded digest is the repository's stable FNV-1a `u64`
   (`s2/mod.rs:8865-8875`), and the manifest is not authenticated. This
   detects ordinary snapshot-byte tampering, but a principal able to rewrite
   both manifest and snapshot can rebind the evidence. Treat the journal as
   operator-local unless a cryptographic authenticated manifest is required.
3. There is no power-loss/restart proof. Commits `993ce685` and
   `9d13bbc9` explicitly bound the `sync_all` claim to normal ordering,
   not universal filesystem crash semantics. Do not claim power-loss recovery
   from the unit suite.

## Existing integration blockers retained

- D19 published pin `fdeed261bd2247a38db6922a7726cd45d3d6f31e` still carries
  the symlink-mode `crates/velnor-workflow/CLAUDE.md`; closure validation must
  not be weakened.
- Generated `project.toml`/generator-state drift and protected
  policy-authority/source/ruleset binding remain unproven from prior reviews.

## Verdict

**Source-level transaction review: conditional pass; integrated G1: not
approved.** The cec04 dispatch defect is fixed, real CLI routing now passes,
snapshot copies/length-digests/nofollow reads and hostile static journal tests
pass, and no tested cooperative recovery path clobbers outside files. The six
provider-pairing failures are unchanged harness baseline failures, not
candidate behavior. Full-suite green status and power-loss durability remain
unproven; D19, generated-state, and authority blockers remain decisive.
