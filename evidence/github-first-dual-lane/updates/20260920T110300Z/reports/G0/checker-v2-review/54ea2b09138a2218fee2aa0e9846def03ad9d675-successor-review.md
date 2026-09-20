# Exact checker successor review: `54ea2b09138a2218fee2aa0e9846def03ad9d675`

Review date: 2026-09-20

Review tree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`

Commit parent: `32100eba010a2db137db4e7df624d6af8b114de7`

Tree: `47035673b8deed55720255e0fed180103b9ffb85`

This is an isolated, read-only source review pinned to the exact remote
commit. No checker/product source, fixture, host, GitHub, gate, or rollout
state was changed. No live-authority, publication, or merge approval is
implied.

## Bounded verdict

**Approved for the requested checker-test scope.** This test-only successor
closes the prior public-chain gap: the real DCO check-run, check-suite, and App
raw objects are now attached to the typed PR producer that the serialized
`check_paths` invocation validates. A rehashed wrong-suite/App identity fails
through that public path with `g0-check-raw-evidence`, not merely a byte digest
failure. The new missing-raw paginated-request regression also asserts both
request invalidation and raw-reference failure. The endpoint/coverage contract,
`filter=all` check-inventory rule, and exact captured-query behavior remain
intact. Full fleet/live/current-reconciliation authority is intentionally not
claimed.

## What passes

- `complete_g0_fixture_round_trips_through_public_check_paths` changes the
  first PR's manifest/snapshot/typed inventory to the captured DCO obligation,
  attaches `real-public-checks-raw`, `real-public-suite-raw`, and
  `real-public-app-raw` to one `G0CheckProducer`, serializes/deserializes the
  collector, and sends that typed producer through `check_paths`
  (`evidence_check.rs:9950-10110`). The baseline public report has no
  `g0-check-raw-evidence`, endpoint-contract, query, or raw-object finding.
- The semantic negative rehashes the captured suite body while replacing its
  App ID/slug with SonarCloud, refreshes the outer typed-inventory bytes, and
  reruns serialized `check_paths`; it requires `g0-check-raw-evidence`
  (`evidence_check.rs:10201-10246`). This proves semantic provider binding
  survives body rehashing and is not just CAS-integrity checking.
- Real captured query provenance remains exact: DCO check-runs use
  `per_page=100&filter=all&page=1`, while direct suite/App requests use an
  empty query. The prior source-correct Homebrew `filter=latest` capture is
  still rejected for check-inventory authorization and is not relabeled.
- `paginated_request_missing_raw_response_fails_closed` constructs a known
  check-runs page request with no matching raw object and asserts both
  `g0-request-incomplete` and `g0-raw-reference`
  (`evidence_check.rs:11495-11520`).
- The closed endpoint-kind/coverage-purpose regression and all prior
  artifact/member, pagination, duplicate, cross-scope, query-order, and
  unknown-path checks remain present and pass.
- The producer authority seam remains unavailable by design. No concrete
  `VerifiedRawStore` or authenticated closing collector is implemented; the
  default live path remains fail-closed. This review makes no live, all-32,
  recursive, checkout, or current-reconciliation claim.

## Bounded limitations

- The public test uses a synthetic 32-repository fixture with one real DCO PR
  chain inserted. It proves the requested serialized semantic path, not a
  fresh all-32-repository capture or a live closing reconciliation.
- The missing-raw regression directly exercises
  `check_g0_request_provenance`, not a filesystem `check_paths` invocation;
  the production path is shared, but this test does not claim public live
  authority.
- The real public negative mutates the suite's embedded App identity. The
  separate real-App body mutations remain covered by the helper-level
  supplement test; no broader provider/fleet claim follows.

## Verification

- `CARGO_TARGET_DIR=/private/tmp/velnor-checker-54ea-target rtk cargo test --locked --package velnor-tools`: **263 passed**.
- Focused serialized public real-chain/tamper test: **1 passed**, 262 filtered.
- Focused missing-raw pagination regression: **1 passed**, 262 filtered.
- Focused closed endpoint/coverage-purpose test: **1 passed**, 262 filtered.
- Focused real corpus and App/suite supplement tests: **1 passed each**, 262 filtered.
- Focused unwired-live rejection: **1 passed**, 262 filtered.
- `CARGO_TARGET_DIR=/private/tmp/velnor-checker-54ea-target rtk cargo clippy --locked --package velnor-tools --all-targets -- -D warnings`: **No issues found**.
- `rtk cargo fmt --all -- --check`: clean.
- `rtk git diff --check 32100eba010a2db137db4e7df624d6af8b114de7 54ea2b09138a2218fee2aa0e9846def03ad9d675`: clean.
- Exact branch was clean at `54ea2b09138a2218fee2aa0e9846def03ad9d675`.
- No live dispatch, API mutation, host operation, gate, rollout, or merge operation.
