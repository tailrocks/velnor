# Bootstrap hostile transport fixtures

Date: 2026-09-20

This is a test-only fixture review. It is not G1 approval, hosted proof, or a
security attestation.

## Identity and ownership

- Fixture branch: `codex/bootstrap-transport-hostile-fixtures`
- Fixture worktree: `/private/tmp/bootstrap-transport-hostile-fixtures`
- Fixture commit: `f2f1a3b0c45bc2d69e7a581719d29f66e6373a64`
- Fixture file: `crates/velnor-workflow/tests/bootstrap_transport_hostile.rs`
- Source baseline exercised: `3ed0023b038335d7b22dfa2758457e3808f777ee`
- Owner-tip validation: isolated detached worktree at
  `80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0`, cherry-picked as
  `cacd384298cda3780e4926011275ea043672b1ff`
- The fixture branch was pushed normally to
  `origin/codex/bootstrap-transport-hostile-fixtures`.
- No production or generated source file was edited.

## Executable fixture coverage

The tests generate workflows with the actual `velnor-workflow` binary, extract
the generated Python or shell body, and execute that body against local,
harmless archives and fake API/tool commands. They do not run a candidate,
Docker, a hosted runner, or an upload endpoint.

- `bootstrap_transport_hostile.rs:174`: generated candidate ZIP validator;
  actual ZIP symlink, non-regular special mode, traversal, duplicate,
  4097-member, and declared-oversize fixtures.
- `:201`: generated workflow TAR scanner; actual TAR symlink, hardlink,
  traversal, duplicate, 4097-member, and declared-oversize fixtures.
- `:224`: generated execute-source TAR census with the same hostile members.
- `:252`: generated workflow namespace scanner with an executable
  `bash ../candidate-source/evil.sh` in both base and head contracts.
- `:289`: generated bounded download helper with deterministic replacement of
  `.partial` by a symlink to an outside marker during the write.
- `:465`: generated acquire shell with a producer job and an ordinary job in
  one run. The producer-bound baseline succeeds; the final same-name artifact
  is then recreated with a new documented artifact ID, valid service digest,
  and timestamps in the overlapping ordinary-job window. The fixture uses no
  invented `uploader_job_id` field: the documented artifact REST object does
  not provide one.

## Results

| Source | ZIP/TAR parser fixtures | Host-command scanner | Partial-path race | Cross-job replacement |
|---|---:|---:|---:|---:|
| `3ed0023b` | pass | intentional red | intentional red | intentional red |
| owner `80ceab4b` | pass | pass | pass | intentional red |

The three parser suites each passed all six hostile cases on both source
snapshots. On exact `3ed0023b`, the scanner test reports the generated scanner
admitted `bash ../candidate-source/evil.sh`; the race test reports the outside
marker changed from `sentinel` to empty bytes. These are executable regression
failures, not static string checks.

On owner tip `80ceab4b`, the scanner and race tests pass. The cross-job test
still fails at the assertion that the replacement must be rejected, after its
producer-bound baseline succeeds. This is the remaining source-bound gap:
the generated acquire path binds the artifact by static name, run, digest,
timestamps, and producer-job window, but the documented REST metadata does not
bind the final artifact object to the upload job. The red test must remain red
until the owner architecture supplies an independently documented binding or
rejects this ambiguity.

## Source-bound evidence

- Owner scanner rejection is emitted by the generated source template at
  `crates/velnor-workflow/src/s2/mod.rs:5219-5230` and rejects both traversal to
  `candidate-source` and shell/interpreter execution of it.
- Owner bounded writers use `os.open(... O_EXCL ...)` at
  `crates/velnor-workflow/src/s2/mod.rs:303` and `:347`; the exact 3ed writer
  used `open(..., "wb")`, which the race fixture demonstrates is unsafe.
- The replacement fixture is intentionally not a claim that GitHub exposes an
  uploader-job field. It records the unsupported binding and must not be
  converted into a green result by weakening the assertion or by an unrelated
  image/API failure.

## Verification commands

Owner-tip isolated worktree:

```text
rtk cargo fmt --all -- --check
rtk cargo clippy --locked -p velnor-workflow --test bootstrap_transport_hostile -- -D warnings
rtk cargo test --locked -p velnor-workflow --test bootstrap_transport_hostile generated_candidate_zip_validator_rejects_actual_hostile_members -- --nocapture
rtk cargo test --locked -p velnor-workflow --test bootstrap_transport_hostile generated_workflow_tar_scanner_rejects_actual_hostile_members -- --nocapture
rtk cargo test --locked -p velnor-workflow --test bootstrap_transport_hostile generated_execute_source_census_rejects_actual_hostile_members -- --nocapture
rtk cargo test --locked -p velnor-workflow --test bootstrap_transport_hostile generated_namespace_scanner_rejects_candidate_source_host_command -- --nocapture
rtk cargo test --locked -p velnor-workflow --test bootstrap_transport_hostile generated_bounded_download_rejects_replaced_partial_path -- --nocapture
```

All commands above pass on owner tip. The cross-job command is intentionally
red on both snapshots for the unsupported binding described above.
