# G3 mapper corpus review: `f7818fb837caf8cd8697c56f60d0cff136bfebdc`

Date: 2026-09-20. This is a bounded review of the frozen mapper/corpus commit,
not G0 approval, live-authority approval, CAS approval, release/install proof,
or publication approval.

## Verdict

**Reject as acceptance-ready.** The commit correctly preserves an unknown
artifact attempt as `None` and fails closed when mapping it. It adds a
request/raw-ledger preflight and a frozen-provider replay. The replay is only a
scoped parser/ledger test, not a positive full-mapper test. The validator still
has provenance holes, and the exact commit fails the requested clippy gate.

## Frozen source and commands

- Isolated worktree: `/private/tmp/g3-corpus-test`.
- Frozen `HEAD`: `f7818fb837caf8cd8697c56f60d0cff136bfebdc`.
- Review ancestry: `6cdeae4701db29901a1250439f520d7c9b449cb3` plus `95af80e4`.
- Worktree clean; no source or fixture edits.
- At review time the moving `tmp/g3-combined2` ref was later than this frozen
  commit (`d4777d9f431432165819f1178d5b288dae2a1455`); that later state is not
  included in this verdict.
- `cargo test -p velnor-tools`: **360 passed, 1 ignored**.
- `cargo test -p velnor-tools g0_mapping -- --nocapture`: **14 passed, 1
  ignored**.
- `VELNOR_REAL_API_CORPUS=... cargo test -p velnor-tools
  frozen_real_api_corpus_preserves_raw_links_failures_and_unknown_artifact_attempt
  -- --ignored --nocapture`: **1 passed**.
- `cargo fmt --all -- --check`: **pass**.
- `cargo check --locked --all-features --package velnor-tools`: 0 errors, but
  one unused-import warning.
- `cargo clippy --locked --all-features --package velnor-tools -- -D warnings`:
  **failed**: unused `serde_json::Value` at
  `crates/velnor-tools/src/g0_live_mapping.rs:16`.

## Frozen corpus result

Corpus: `dual-lane-evidence/G0/real-api-fixture-corpus-20260920T080318Z`.
Independent JSON census of the captured bodies gives:

- 68 jobs;
- 2 artifacts;
- 70 check runs;
- 48 skipped jobs and 48 skipped check runs;
- 1 failed check run;
- 10 request records, 8 with bodies and 2 metadata-only records.

The ignored test verifies body digests, eight synthetic request/raw links, two
artifact records, `run_attempt == None`, 48 skipped jobs, and one failed check.
It does **not** assert the 68/70 totals itself and does not construct a
`LiveCollection`, manifest, bindings, or canonical mapped inventory.

More importantly, the test calls only `validate_request_raw_bindings`
(`g0_live_mapping.rs:3131-3133`) and `map_artifact_observation`; it never calls
`map_g0_inventory` or `map_g0_inventory_with_supplement`. The synthetic
`RequestRecord` page state uses
`has_next_page: Some(!record["has_next"])` (`g0_live_mapping.rs:3104-3111`),
which inverts the corpus's `has_next: false` values. It also invents request
IDs, API request IDs, timestamps, rate limits, scopes, and page item counts.
Therefore the passing replay is useful regression evidence for selected raw
bytes and artifact fail-closed behavior, but is not a full positive mapper
proof and does not validate pagination semantics.

## Artifact attempt behavior

The corrected behavior is fail-closed and remains necessary:

- `LiveArtifact.run_attempt` is explicitly optional because the list-artifacts
  API does not expose attempt identity (`github_live_collector.rs:224-235`).
- `parse_artifact` preserves `None` rather than stamping latest attempt
  (`github_live_collector.rs:2091-2135`), and the parser test asserts `None`
  (`github_live_collector.rs:3836-3837`).
- `map_artifact_observation` rejects the unknown attempt
  (`g0_live_mapping.rs:1173-1179`).

The frozen corpus therefore proves correct rejection, not artifact acceptance.
An authoritative per-run/attempt association is still required before an
authentic artifact-bearing collection can map. It must cover multiple attempts,
list/archive identity, and no latest-attempt self-fill.

## Request/raw-ledger validator audit

The top-level mapper now invokes `index_unique_requests` and
`validate_request_raw_bindings` before projection (`g0_live_mapping.rs:388-406`).
The new pass (`g0_live_mapping.rs:633-700`) correctly rejects:

- missing response raw reference;
- response/error raw ID absent from the merged ledger;
- raw object whose `request_id` differs from the referencing request;
- known-request raw object not referenced by that request;
- one raw ID referenced by different request IDs;
- provider/local duplicate raw identity through `merge_raw_object_sets`.

The focused test `request_raw_ledger_rejects_cross_bound_and_unlinked_objects`
covers cross-bound and known-request orphan behavior. The duplicate provider /
local identity test also passes.

Remaining false-green cases:

1. **Unknown-request provider orphan bypass.** For every raw object whose
   `request_id` is absent from `request_by_id`, the validator executes the
   `continue` at `g0_live_mapping.rs:682-687`. The comment assumes such rows are
   local model/workload objects, but the validator receives only the merged
   vector and does not retain provider/local provenance or check object kind.
   A provider raw object with a typo/foreign request ID is therefore accepted
   as an unbound raw object. Split provider and local validation, or carry an
   explicit trusted provenance class and reject unknown provider IDs. Add an
   unknown-request provider fixture.

2. **Same-request response/error alias.** The duplicate guard at
   `g0_live_mapping.rs:671-679` rejects reuse only when the previous request ID
   differs. A single raw ID can be both the same request's `response_raw_ref`
   and `error_raw_ref` and pass. Response and error roles must be distinct (or
   the schema must explicitly prove a valid dual-role), with a negative fixture.

3. **No direct missing-response/duplicate-role tests.** The implementation
   handles a missing response, but the committed focused suite does not test
   it. There is no test for same-request role alias, unknown-request orphan,
   duplicate raw ID in the validator input, or a duplicate request ID reaching
   the validator (the latter is caught only by the separate index pass).

4. **Common ledger relation is not endpoint provenance.** The validator checks
   IDs, request back-links, and completion later through consumers, but does not
   bind every raw object to exact endpoint, method, status, query/page, object
   kind, or subject. Existing mapper gaps remain for artifact list/archive,
   workflow/dependency rows, reconciliation/workflow bindings, and execution /
   job observations. The `map_repository` artifact path still allows only the
   generic `workflow_artifacts` kind (`g0_live_mapping.rs:1018-1028`); it does
   not prove `/actions/runs/{run}/artifacts` plus archive `/actions/artifacts/{id}/zip`.

5. **No mapper-wide subject duplicate policy.** Raw IDs are protected by the
   merge path, but artifact IDs/names, execution/job IDs, PR identities,
   workflow-binding tuples, and supplement key collisions are not all rejected
   at this boundary.

## CAS and live scope

The review does not credit CAS retention or `read_g0_rawadapter`; those are
explicitly blocked/owned by the ongoing `g0_runtime` work. This commit does not
wire the live CLI to a verified CAS-backed mapper or replace the unavailable
live authority. No live or publication claim follows.

## Required disposition

Keep `f7818fb8` rejected for G0/G2. Fix the clippy warning, add the missing
validator negatives, preserve provider/local provenance so unknown provider
request IDs fail closed, enforce response/error role uniqueness, and run the
full mapper against a real captured positive with correct pagination metadata.
Then separately review CAS/live-authority integration; this bounded corpus
replay cannot substitute for it.
