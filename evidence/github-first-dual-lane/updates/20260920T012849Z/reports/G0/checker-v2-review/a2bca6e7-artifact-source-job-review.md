# Exact checker review: a2bca6e767aa038818a2ffc991401609118600f3

## Bounded verdict

The source-job requirement and basic artifact census are present, but this
commit is not an acceptable exact artifact-identity gate. Its artifact run
binding still validates a run ID, attempt, and head independently. A row can
combine a valid attempt from one captured run with a valid head from another
captured run, or use a different attempt for the same run, without an
artifact-specific finding. The checker is currently fail-closed for both
offline and `--live`, so the fixture cannot authorize G0 today; the defect
would matter when trusted live reconciliation is wired.

This is a bounded exact-source review, not G0/G3 approval.

## Exact source and checks

- Detached tree: `/private/tmp/velnor-checker-review-a2`
- Exact commit: `a2bca6e767aa038818a2ffc991401609118600f3`
- Parent: `e68f3fb120baca17c39607a2a8c5f0a52d7131e0`
- Product source was not edited.

Checks against the detached tree passed:

- `cargo test --locked --all-features --package velnor-tools -- --nocapture` — 250 passed.
- `cargo fmt --all -- --check` — pass.
- `cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings` — pass.
- `cargo build --locked --all-features --package velnor-tools --bin velnor-tools` — pass.
- `git diff --check e68f3fb1..a2bca6e7` — pass.

## Independent public-CLI harness

Harness script:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/run_harness.py`

Result:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/a2-results.json`

The fixture is synthetic, offline, and contacts no network. It adds typed
source jobs, one artifact per repository, artifact request/raw objects, and
recomputed outer bytes/CAS. It then exercises omitted rows, source/job
substitution, run identity, request URL, digest, raw kind/reference, CAS, and
live self-authorship cases.

Observed a2 cases:

- `missing-source-jobs`: `g0-source-job-inventory`.
- `missing-artifacts`: `g0-artifact-inventory`.
- `source-job-target-mismatch`: `g0-source-job-plan` and
  `g0-source-job-derivation`.
- `source-bytes-derived-mismatch`: `g0-workflow-plan` and
  `g0-source-job-derivation`.
- Wrong URL, digest, raw kind/reference, and CAS tampering each produced the
  expected structural/storage finding.
- `mismatched-run-attempt` (`99`), `mismatched-known-head` (a head valid for a
  different captured PR run), and `mismatched-known-run` (PR run ID/head while
  retaining the main-run request) produced only
  `offline-validation-only`; no artifact identity/request finding.
- `raw-payload-mismatch` rewrote the artifact API bytes and consistently
  recomputed raw digest, typed artifact digest, outer snapshot, and CAS. It
  produced only `offline-validation-only`; the checker does not parse or bind
  artifact row identity to response content.
- Every offline case failed with `offline-validation-only`.
- `--live`, including the locally authored fresh fixture, failed before file
  use with: `--live is unavailable: trusted authenticated collector/current-API
  reconciliation is not wired; offline files cannot authorize a gate`.

## Required follow-up

Before this checker can support a trusted artifact gate, bind each artifact to
an exact captured `(run_id, run_attempt, run_head_sha)` producer tuple, then
bind its typed ID/name/run fields to a parsed artifact API response whose raw
bytes, request, digest, and CAS bytes are the same verified object. The
artifact census also needs an independently captured expected response set or
count; non-empty plus unique rows does not prove that artifacts were not
omitted. No approval is issued for producer integration.
