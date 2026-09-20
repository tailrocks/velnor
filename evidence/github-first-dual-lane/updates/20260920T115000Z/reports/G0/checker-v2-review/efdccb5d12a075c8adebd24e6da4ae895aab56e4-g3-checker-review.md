# Independent exact checker review: `efdccb5d12a075c8adebd24e6da4ae895aab56e4`

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

Review tree: `/private/tmp/g3-checker-efd-review.6rX`

Exact `HEAD`: `efdccb5d12a075c8adebd24e6da4ae895aab56e4`

Parent: `54ea2b09138a2218fee2aa0e9846def03ad9d675`

Tree: `f1d569ede992b69a6f994ead5eb62c31970136b0`

Changed source blob: `e4a012679e94c5b0d18654d0b8c1a7d14075d4f3`

Only the owner file `crates/velnor-tools/src/evidence_check.rs` changes from
the parent. The detached tree was clean before and after review. No source,
collector, mapper, host, GitHub, live-capture, gate, or remote state changed.

## Bounded verdict

**Changes required before checker/mapper composition approval.** The checker-
only changes are directionally correct and their bounded tests pass: typed
model/workload objects now require exact JSON round trips and object kinds,
access rows require a successful repository API response, workflow/dependency
and graph rows reject generic raw kinds, PR bindings resolve to a captured
workflow path/revision, and the old `github-response` kind is gone from the
exact source/tests. This is not G0 or live-authority approval.

The exact checker has a concrete composition blocker with current collector
commit `413189538fa03e069bb82a92f69d75a70beaf72d` (its mapper blob is
`de000cb2138d06ca6c09742cf29cedd65bcd8544`). The full collector-to-mapper-
checker chain was not run; the following is a source-bound compatibility
finding that must be resolved before claiming that chain.

### F1 — Closed endpoint map rejects legitimate mapper raw kinds (blocker)

`check_g0_request_provenance` resolves every request's response raw object
through `g0_endpoint_contract` (`evidence_check.rs:2055-2073`). The new map
accepts only `repository`, `check_run`, `check_suite`, `job`,
`workflow_artifacts`, `workflow_run`, and `app`
(`evidence_check.rs:4252-4372`). Current collector/mapper output retains
additional provider response kinds, including `auth.viewer`,
`pull_request`, `default_branch.commit`, `rulesets`, `ruleset`, `workflows`,
`workflow.dependency`, `workflow.source`, and
`workflow.dependency.source`. The collector explicitly emits
`workflow.source` for `/repos/{repo}/contents/{path}?ref={sha}` and
`workflow.dependency.source` for the corresponding dependency contents
endpoint (`github_live_collector.rs:1164-1199, 2784-2804`); the mapper
preserves each raw object's kind (`g0_live_mapping.rs:986-1022`).

For such a real mapped request, the new checker emits endpoint-contract and
query/incomplete findings before source validation. The positive checker
fixture avoids this path: its `raw-workflow-*` objects share a repository
request ID but are not response references for any source request
(`evidence_check.rs:9083-9113`). Add exact contracts for every retained
provider endpoint/object kind, or explicitly separate and validate local
objects, then run a real mapped positive and malformed/unknown endpoint
negatives. Do not claim current collector compatibility from this candidate.

### F2 — Source/graph raw references can remain orphaned or cross-endpoint

`source_has_raw_binding` checks only referenced raw ID, expected kind, raw
SHA, and equal bytes (`evidence_check.rs:3076-3094`). Global provenance checks
that a provider raw object's `request_id` appears in the request set, but does
not require every provider raw object to be the matching request's
`response_raw_ref` (`evidence_check.rs:2214-2265`). Therefore a caller can
attach a same-byte `workflow.source` object captured for another repository or
path and have the typed source pass; graph helpers are weaker still:
`g0_has_workflow_source_raw` and `g0_graph_node_raw_binding` check category
only (`evidence_check.rs:5620-5644`). The new negative fixture changes a raw
kind to generic `repository`, but does not test an orphan, wrong endpoint,
same-byte cross-repository source, or source-SHA/graph mismatch.

Require provider raw refs to be exact request response refs and bind source
repository/path/revision to the request endpoint (including dependency source
identity). Require each graph node/edge raw source to match its typed source
identity, not merely a permitted category. Add serialized negatives for
orphan, cross-repository, wrong-path, and stale-source graph substitutions.

The new local-kind exemption also allows `model.session`, `workload.source`,
and `workload.artifact` raw objects whose `request_id` collides with a
provider request. The mapper rejects that collision, but the checker does
not; require a disjoint producer-local namespace or equivalent binding at
this boundary.

## Verification

Focused source/negative tests:

- `g0_source_join_bindings_are_typed_and_repository_specific`: **1 passed**
  (wrong model/artifact kind, cross-repository access, generic graph/workflow
  raw substitution, and missing PR workflow source binding).
- `complete_g0_fixture_round_trips_through_public_check_paths`: **1 passed**
  (serialized public path; real DCO raw chain mutation; synthetic 32-row
  fixture remains synthetic).
- `endpoint_contract_is_closed_and_coverage_specific`: **1 passed**;
  `github-response` is absent from the exact source tree.
- `paginated_request_missing_raw_response_fails_closed`: **1 passed**.
- `g0_graph_dangling_edge_and_wrong_model_fail_closed`: **1 passed**.
- `immutable_pr_subject_positive_and_head_mutation_negative`: **1 passed**.

Bounded commands:

```text
rtk cargo test --locked --all-features --package velnor-tools --bin velnor-tools \
  evidence_check::tests:: -- --nocapture
  cargo test: 46 passed, 218 filtered out

rtk cargo test --locked --all-features --package velnor-tools
  cargo test: 264 passed

rtk cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings
  cargo clippy: No issues found

rtk cargo fmt --all -- --check
  clean
```

`git diff --check 54ea2b09138a2218fee2aa0e9846def03ad9d675..HEAD` passed.

No full collector-to-mapper-to-checker invocation, live capture, current
fleet reconciliation, G0 completion, execution, release, install, or
publication claim follows.
