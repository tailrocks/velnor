# Exact checker delta review: c0329b892e51706f5cca1c90a73744c41766cb68

## Bounded verdict

The c032 delta fixes the principal a2 artifact-binding defect: artifact rows
now require an exact captured `(run_id, run_attempt, run_head_sha)` tuple, and
the referenced raw object must resolve to a successful artifact request for
that exact run. It also requires every source-job row to reference the
immutable workflow source object. These corrections were independently
tested. The result remains a bounded source review, not G0/G3 or producer
integration approval.

## Exact source and checks

- Detached tree: `/private/tmp/velnor-checker-review-c032`
- Exact commit: `c0329b892e51706f5cca1c90a73744c41766cb68`
- Parent: `a2bca6e767aa038818a2ffc991401609118600f3`
- Product source was not edited.

Checks against the exact tree, with a fresh target directory:

- `cargo test --locked --all-features --package velnor-tools -- --nocapture` — 250 passed.
- `cargo fmt --all -- --check` — pass.
- `cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings` — pass.
- `cargo build --locked --all-features --package velnor-tools --bin velnor-tools` — pass.
- `git diff --check a2bca6e7..c0329b89` — pass.

## Independent public-CLI evidence

The harness is synthetic, offline, and contacts no network:

- Script: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/run_harness.py`
- Exact result: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/c032-review-results.json`

The harness uses the actual public CLI and recomputes every mutated outer
snapshot/digest/CAS object. Results:

- `mismatched-run-attempt` now emits `g0-artifact-identity`.
- `mismatched-known-head` now emits `g0-artifact-identity`.
- `mismatched-known-run` now emits `g0-artifact-request` because the request
  endpoint remains bound to the main run.
- Wrong URL, digest, raw kind/reference, and CAS tampering retain their
  expected findings.
- Missing artifacts/source jobs and source-job target/source-byte substitutions
  retain their expected inventory/plan/derivation findings.
- `artifact-response-ref-mismatch` creates a second valid raw object for the
  same request and points the request's `response_raw_ref` at that decoy while
  the artifact row references the other raw object. It produced only
  `offline-validation-only`: the artifact-specific request check finds the
  request by `raw.request_id` but does not require
  `request.response_raw_ref == artifact_raw_id`.
- `raw-payload-mismatch` rewrites artifact API bytes to a different artifact
  identity and consistently recomputes raw digest, typed digest, outer bytes,
  and CAS. It produced only `offline-validation-only`: c032 verifies bytes,
  digest, request, and CAS, but does not parse/bind response content to the
  typed artifact ID/name/run fields.
- The complete synthetic baseline and every offline hostile case fail with
  `offline-validation-only`.
- `--live` fails closed before reading caller files with:
  `--live is unavailable: trusted authenticated collector/current-API
  reconciliation is not wired; offline files cannot authorize a gate`.

## Remaining bounded risks

The c032 implementation is a meaningful correction, but two artifact-content
bindings remain absent before any trusted producer gate:

1. The artifact row must point to the request's exact `response_raw_ref`, not
   merely to any raw object carrying the same request ID.
2. The checker or trusted collector must parse the verified artifact response
   and prove that each typed artifact row's ID/name/run fields came from those
   response bytes. Digest/CAS self-consistency alone proves only that the
   caller rewrote all copies coherently.

Also, the census is only non-empty with unique IDs/names; it has no
independently captured expected artifact set/count, so omission of one member
of a multi-artifact response is not proven impossible.

These residuals are structural follow-ups. Since offline validation remains
non-authoritative and live is explicitly unavailable, this review issues no
G0/G3 approval or producer-integration approval.
