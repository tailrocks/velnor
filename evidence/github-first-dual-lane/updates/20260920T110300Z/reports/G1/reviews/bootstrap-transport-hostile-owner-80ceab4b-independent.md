# Independent hostile-bootstrap fixture review

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

Disposition: **fixture valid; owner source remains RED on the cross-job
provenance case**. This is an offline generated-transport review only. It is
not hosted execution, Docker proof, G1 approval, or a security attestation.

## Exact composition

- Fixture commit: `f2f1a3b0c45bc2d69e7a581719d29f66e6373a64`
- Fixture parent: `3ed0023b038335d7b22dfa2758457e3808f777ee`
- Fixture file SHA-256: `d0ea27b6bf802ff170040e0cac253ed0755a9e2516cf02bb7e4beb0f8b2bc962`
- Owner source: `80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0`
- Owner source tree: `35355785c307329a7c4e2567dd26dca2c9177ec9`
- Isolated worktree: `/private/tmp/velnor-g1-bootstrap-hostile-review`
- Test-only composition index tree: `3a716529a36a4ce40b10854938c4b3541f652a6f`

The composition has exactly one staged file, the fixture test. Owner source
worktree `/private/tmp/velnor-g1-bootstrap` was not edited. No remote action,
upload, Docker daemon, hosted runner, or token was used.

## Verification

Command:

```text
CARGO_TARGET_DIR=/private/tmp/velnor-g1-bootstrap-hostile-review-target \
  rtk cargo test --locked --all-features -p velnor-workflow \
  --test bootstrap_transport_hostile -- --nocapture
```

Result: **5 passed, 1 failed, 0 ignored**, 9.57 seconds.

Passed:

- `generated_candidate_zip_validator_rejects_actual_hostile_members`
- `generated_workflow_tar_scanner_rejects_actual_hostile_members`
- `generated_execute_source_census_rejects_actual_hostile_members`
- `generated_namespace_scanner_rejects_candidate_source_host_command`
- `generated_bounded_download_rejects_replaced_partial_path`

Failed intentionally by assertion:

- `generated_acquire_rejects_same_name_artifact_recreated_by_other_job`

The complete RTK log is `$HOME/Library/Application Support/rtk/tee/1789901931_cargo_test.log`.
`rtk cargo fmt --all -- --check` passed. The targeted locked all-features
clippy command with `-D warnings` also exited zero.

## Fixture quality

The test invokes the actual generated `velnor-workflow` binary, parses the
generated YAML, extracts the exact generated Python heredocs/shell helper, and
executes them. ZIP/TAR members are real Python-produced archives, including
symlink, hardlink/special-mode, traversal, duplicate, excess-member, and
declared-size cases. The namespace case executes the generated scanner against
an actual generated workflow TAR. The partial-file case races the generated
writer with a replacement symlink and checks an outside sentinel. These are
runtime checks, not substring-only assertions.

The acquire case also runs the complete generated acquire shell. Its baseline
scenario completes successfully before the test changes only the documented
artifact response to a new ID/digest-valid same-name object. Therefore the
positive fixture is valid and the negative assertion reaches the real acquire
path.

The fake tools are intentionally offline adapters, so this does not prove the
GitHub API or runner. They route some requests by URL substring, return fixed
action-archive digests, make `git fetch` a no-op, and report a bounded `du`
value. Those choices limit endpoint/object-size coverage, but do not bypass the
cross-job assertion: the generated acquire script consumes the scenario's
producer/ordinary jobs and replacement artifact, and it accepts the replacement.
No production source check is replaced by a test-side success assertion.

## Remaining source failure

The producer job is `800` (`00:01`–`00:10`); ordinary job `801` overlaps it
(`00:02`–`00:09`). The replacement object has ID `1002`, valid archive/service
digest, and timestamps `00:08`–`00:08:30`. The generated acquire path checks
static name, enclosing run, expiry, digest, and timestamp bounds against the
producer job, but the documented artifact object has no uploader-job field.
The replacement therefore passes those predicates, and the test panics at
line 483 with:

```text
generated acquire accepted an artifact recreated by ordinary job 801 while producer job is 800
```

This is a real source-bound provenance gap, not a hostile-fixture false
negative. Preserve the test RED until an independently trusted artifact-ID
binding exists, or until ambiguity is rejected closed. Do not weaken the
assertion or claim hosted/security completion. The fixture commit can be
cherry-picked as a test-only change after the owner chooses that source
mechanism.
